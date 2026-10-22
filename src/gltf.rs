//! Minimal glTF 2.0 writer: one mesh primitive per material, PNG textures next to the `.gltf`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::dynmodel::{DynModel, Lod};

#[derive(Default)]
struct Primitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

pub struct Material {
    pub name: String,
    /// File name of the base color PNG, relative to the output directory
    pub color_png: Option<String>,
    pub double_sided: bool,
    pub alpha_mask: bool,
}

/// Column-major 4x4 node transform
fn transform(m: &[f32; 16], p: [f32; 3], w: f32) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r] * w)
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 { v.map(|x| x / len) } else { [0.0, 1.0, 0.0] }
}

/// Dagor is left-handed with Y up; glTF is right-handed. Mirror Z and flip the winding.
fn to_gltf(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[1], -p[2]]
}

pub fn write(
    model: &DynModel,
    lod: &Lod,
    nodes: &HashMap<String, [f32; 16]>,
    materials: &[Material],
    out_dir: &Path,
    name: &str,
) -> Result<()> {
    const IDENTITY: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    let mut prims: HashMap<usize, Primitive> = HashMap::new();
    let mut missing_nodes = Vec::new();
    for rigid in &lod.rigids {
        // "@root" is the model origin
        let tm = nodes.get(&rigid.node).unwrap_or_else(|| {
            if rigid.node != "@root" {
                missing_nodes.push(rigid.node.as_str());
            }
            &IDENTITY
        });
        for e in &rigid.elems {
            let prim = prims.entry(e.material).or_default();
            let vd = &model.vdata[e.vdata];
            let mut remap: HashMap<u32, u32> = HashMap::new();
            let tri_indices = &vd.indices[e.first_index..e.first_index + e.triangles * 3];
            for tri in tri_indices.chunks_exact(3) {
                // Reverse the winding to match the mirrored axis
                for &i in &[tri[0], tri[2], tri[1]] {
                    let src = i + e.base_vertex as u32;
                    let next = prim.positions.len() as u32;
                    let dst = *remap.entry(src).or_insert_with(|| {
                        let v = model.vertex(e.vdata, src as usize);
                        prim.positions.push(to_gltf(transform(tm, v.pos, 1.0)));
                        prim.normals.push(to_gltf(normalize(transform(tm, v.normal, 0.0))));
                        prim.uvs.push(v.uv.map(|x| x / 4096.0));
                        next
                    });
                    prim.indices.push(dst);
                }
            }
        }
    }
    if !missing_nodes.is_empty() {
        eprintln!("warning: parts without a skeleton node, left untransformed: {missing_nodes:?}");
    }

    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut gl_prims = Vec::new();
    let mut push_view = |bin: &mut Vec<u8>, bytes: &[u8], target: u32| -> usize {
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        views.push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len(), "target": target}));
        bin.extend_from_slice(bytes);
        views.len() - 1
    };

    let mut keys: Vec<_> = prims.keys().copied().collect();
    keys.sort();
    for mat in keys {
        let p = &prims[&mat];
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &p.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        let floats = |v: &[[f32; 3]]| v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        let pos_view = push_view(&mut bin, &floats(&p.positions), 34962);
        let nrm_view = push_view(&mut bin, &floats(&p.normals), 34962);
        let uv_bytes: Vec<u8> = p.uvs.iter().flatten().flat_map(|f| f.to_le_bytes()).collect();
        let uv_view = push_view(&mut bin, &uv_bytes, 34962);
        let idx_bytes: Vec<u8> = p.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let idx_view = push_view(&mut bin, &idx_bytes, 34963);

        let n = p.positions.len();
        let base = accessors.len();
        accessors.push(json!({"bufferView": pos_view, "componentType": 5126, "count": n, "type": "VEC3", "min": lo, "max": hi}));
        accessors.push(json!({"bufferView": nrm_view, "componentType": 5126, "count": n, "type": "VEC3"}));
        accessors.push(json!({"bufferView": uv_view, "componentType": 5126, "count": n, "type": "VEC2"}));
        accessors.push(json!({"bufferView": idx_view, "componentType": 5125, "count": p.indices.len(), "type": "SCALAR"}));
        gl_prims.push(json!({
            "attributes": {"POSITION": base, "NORMAL": base + 1, "TEXCOORD_0": base + 2},
            "indices": base + 3,
            "material": mat.min(materials.len().saturating_sub(1)),
        }));
    }

    let mut images = Vec::new();
    let mut gl_materials = Vec::new();
    for m in materials {
        let mut pbr = json!({"metallicFactor": 0.0, "roughnessFactor": 0.85});
        if let Some(png) = &m.color_png {
            images.push(json!({"uri": png}));
            pbr["baseColorTexture"] = json!({"index": images.len() - 1});
        }
        let mut mat = json!({"name": m.name, "pbrMetallicRoughness": pbr, "doubleSided": m.double_sided});
        if m.alpha_mask {
            mat["alphaMode"] = json!("MASK");
        }
        gl_materials.push(mat);
    }
    let textures: Vec<Value> = (0..images.len()).map(|i| json!({"source": i, "sampler": 0})).collect();

    let bin_name = format!("{name}.bin");
    let doc = json!({
        "asset": {"version": "2.0", "generator": "wt-unpack"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"name": name, "mesh": 0}],
        "meshes": [{"name": name, "primitives": gl_prims}],
        "materials": gl_materials,
        "textures": textures,
        "images": images,
        "samplers": [{"wrapS": 10497, "wrapT": 10497}],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"uri": bin_name, "byteLength": bin.len()}],
    });
    fs::create_dir_all(out_dir)?;
    fs::write(out_dir.join(&bin_name), &bin)?;
    fs::write(out_dir.join(format!("{name}.gltf")), serde_json::to_vec_pretty(&doc)?)?;
    Ok(())
}
