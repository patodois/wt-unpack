//! Minimal glTF 2.0 writer for a [`Vehicle`]: one mesh primitive per material, PNG textures next to
//! the `.gltf`.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::vehicle::{Image, Vehicle};

fn write_png(path: &Path, img: &Image) -> Result<()> {
    let mut buf = Vec::new();
    let mut enc = png::Encoder::new(&mut buf, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&img.rgba)?;
    writer.finish()?;
    fs::write(path, buf)?;
    Ok(())
}

pub fn write(vehicle: &Vehicle, out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    let name = &vehicle.name;

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

    for m in &vehicle.meshes {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        let floats3 = |v: &[[f32; 3]]| v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        let pos_view = push_view(&mut bin, &floats3(&m.positions), 34962);
        let nrm_view = push_view(&mut bin, &floats3(&m.normals), 34962);
        let uv_bytes: Vec<u8> = m.uvs.iter().flatten().flat_map(|f| f.to_le_bytes()).collect();
        let uv_view = push_view(&mut bin, &uv_bytes, 34962);
        let idx_bytes: Vec<u8> = m.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let idx_view = push_view(&mut bin, &idx_bytes, 34963);

        let n = m.positions.len();
        let base = accessors.len();
        accessors.push(json!({"bufferView": pos_view, "componentType": 5126, "count": n, "type": "VEC3", "min": lo, "max": hi}));
        accessors.push(json!({"bufferView": nrm_view, "componentType": 5126, "count": n, "type": "VEC3"}));
        accessors.push(json!({"bufferView": uv_view, "componentType": 5126, "count": n, "type": "VEC2"}));
        accessors.push(json!({"bufferView": idx_view, "componentType": 5125, "count": m.indices.len(), "type": "SCALAR"}));
        gl_prims.push(json!({
            "attributes": {"POSITION": base, "NORMAL": base + 1, "TEXCOORD_0": base + 2},
            "indices": base + 3,
            "material": m.material,
        }));
    }

    let mut images: Vec<Value> = Vec::new();
    let mut add_image = |file: String, img: &Image| -> Result<usize> {
        let path = out_dir.join(&file);
        if !path.exists() {
            write_png(&path, img)?;
        }
        if let Some(i) = images.iter().position(|v| v["uri"] == file.as_str()) {
            return Ok(i);
        }
        images.push(json!({ "uri": file }));
        Ok(images.len() - 1)
    };
    let mut gl_materials = Vec::new();
    for m in &vehicle.materials {
        let mut pbr = json!({"metallicFactor": 0.0, "roughnessFactor": 0.85});
        if let (Some(img), Some(key)) = (&m.color, &m.color_key) {
            pbr["baseColorTexture"] = json!({"index": add_image(format!("{key}.png"), img)?});
        }
        let mut mat = json!({"name": m.name, "doubleSided": m.double_sided});
        if let (Some(mr), Some(n), Some(key)) = (&m.metal_rough, &m.normal, &m.pbr_key) {
            pbr["metallicRoughnessTexture"] = json!({"index": add_image(format!("{key}_metal_rough.png"), mr)?});
            pbr["metallicFactor"] = json!(1.0);
            pbr["roughnessFactor"] = json!(1.0);
            mat["normalTexture"] = json!({"index": add_image(format!("{key}_normal.png"), n)?});
        }
        mat["pbrMetallicRoughness"] = pbr;
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
    fs::write(out_dir.join(&bin_name), &bin)?;
    fs::write(out_dir.join(format!("{name}.gltf")), serde_json::to_vec_pretty(&doc)?)?;
    Ok(())
}
