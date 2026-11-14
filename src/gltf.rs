//! Minimal glTF 2.0 writer for a [`Vehicle`]: one mesh primitive per node and material, PNG textures next to
//! the `.gltf`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::paths;
use crate::vehicle::{Alpha, Image, Vehicle};

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

/// Writes `<name>.gltf`, `<name>.bin` and the PNG textures into `out_dir`. The model's name and its texture
/// names come from game data, so each goes through [`paths::stem`] before it names a file: nothing is
/// written outside `out_dir`, and ordinary names give the files they always gave.
pub fn write(vehicle: &Vehicle, out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    let name = &vehicle.name;
    let stem = paths::stem(name);

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
    // Image index by texture name, so a texture several materials share is written once
    let mut by_name: HashMap<String, usize> = HashMap::new();
    // File names in use. A texture whose name needs no change keeps `<name>.png`, as before; these are all
    // set aside first, so that a name that had to be changed (`a/b` to `a_b`) is the one that gets a number
    // when the two would share a file.
    let texture_names = vehicle.materials.iter().flat_map(|m| {
        let pbr = m.pbr_key.iter().flat_map(|key| [format!("{key}_metal_rough"), format!("{key}_normal")]);
        m.color_key.iter().cloned().chain(pbr)
    });
    let mut taken: HashSet<String> = texture_names.filter(|t| paths::stem(t) == *t).map(|t| format!("{t}.png")).collect();
    let mut add_image = |texture: String, img: &Image| -> Result<usize> {
        if let Some(&i) = by_name.get(&texture) {
            return Ok(i);
        }
        let safe = paths::stem(&texture);
        let mut file = format!("{safe}.png");
        let mut n = 2;
        while safe != texture && !taken.insert(file.clone()) {
            file = format!("{safe}_{n}.png");
            n += 1;
        }
        let path = out_dir.join(&file);
        if !path.exists() {
            write_png(&path, img)?;
        }
        by_name.insert(texture, images.len());
        images.push(json!({ "uri": file }));
        Ok(images.len() - 1)
    };
    let mut gl_materials = Vec::new();
    for m in &vehicle.materials {
        let mut pbr = json!({"metallicFactor": 0.0, "roughnessFactor": 0.85});
        if let (Some(img), Some(key)) = (&m.color, &m.color_key) {
            pbr["baseColorTexture"] = json!({"index": add_image(key.clone(), img)?});
        }
        let mut mat = json!({"name": m.name, "doubleSided": m.double_sided});
        if let (Some(mr), Some(n), Some(key)) = (&m.metal_rough, &m.normal, &m.pbr_key) {
            pbr["metallicRoughnessTexture"] = json!({"index": add_image(format!("{key}_metal_rough"), mr)?});
            pbr["metallicFactor"] = json!(1.0);
            pbr["roughnessFactor"] = json!(1.0);
            mat["normalTexture"] = json!({"index": add_image(format!("{key}_normal"), n)?});
        }
        mat["pbrMetallicRoughness"] = pbr;
        match m.alpha {
            Alpha::Opaque => {}
            Alpha::Mask(cutoff) => {
                mat["alphaMode"] = json!("MASK");
                mat["alphaCutoff"] = json!(cutoff);
            }
            Alpha::Blend => mat["alphaMode"] = json!("BLEND"),
        }
        gl_materials.push(mat);
    }
    let textures: Vec<Value> = (0..images.len()).map(|i| json!({"source": i, "sampler": 0})).collect();

    let bin_name = format!("{stem}.bin");
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
    fs::write(out_dir.join(format!("{stem}.gltf")), serde_json::to_vec_pretty(&doc)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vehicle::{Material, Mesh, Node};

    #[test]
    fn writes_a_consistent_document() {
        let red = Image { width: 1, height: 1, rgba: vec![255, 0, 0, 255] };
        let vehicle = Vehicle {
            name: "box".into(),
            meshes: vec![Mesh {
                positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
                normals: vec![[0., 0., 1.]; 3],
                uvs: vec![[0., 0.], [1., 0.], [0., 1.]],
                indices: vec![0, 1, 2],
                ..Mesh::default()
            }],
            materials: vec![Material {
                name: "paint".into(),
                color: Some(red),
                metal_rough: None,
                normal: None,
                double_sided: true,
                alpha: Alpha::Opaque,
                color_key: Some("paint_c".into()),
                pbr_key: None,
            }],
            nodes: vec![Node { name: String::new(), parent: None, pivot: [0.0; 3] }],
        };
        let out = std::env::temp_dir().join(format!("wt-unpack-gltf-test-{}", std::process::id()));
        write(&vehicle, &out).unwrap();
        let doc: Value = serde_json::from_slice(&fs::read(out.join("box.gltf")).unwrap()).unwrap();
        assert_eq!(doc["accessors"][0]["count"], 3);
        assert_eq!(doc["accessors"][0]["max"], json!([1.0, 1.0, 0.0]));
        assert_eq!(doc["materials"][0]["doubleSided"], true);
        assert_eq!(doc["images"][0]["uri"], "paint_c.png");
        // Positions, normals, UVs and indices: 36 + 36 + 24 + 12 bytes, each view 4-byte aligned
        assert_eq!(fs::read(out.join("box.bin")).unwrap().len(), 108);
        assert!(out.join("paint_c.png").exists());
        fs::remove_dir_all(&out).unwrap();
    }

    fn textured(name: &str, keys: &[(&str, Option<&str>)]) -> Vehicle {
        let px = |v: u8| Image { width: 1, height: 1, rgba: vec![v, 0, 0, 255] };
        Vehicle {
            name: name.into(),
            meshes: vec![Mesh { positions: vec![[0.; 3]; 3], normals: vec![[0., 0., 1.]; 3], uvs: vec![[0.; 2]; 3], indices: vec![0, 1, 2], ..Mesh::default() }],
            materials: keys
                .iter()
                .enumerate()
                .map(|(i, (color, pbr))| Material {
                    name: format!("m{i}"),
                    color: Some(px(i as u8)),
                    metal_rough: pbr.map(|_| px(100)),
                    normal: pbr.map(|_| px(200)),
                    double_sided: false,
                    alpha: Alpha::Opaque,
                    color_key: Some((*color).into()),
                    pbr_key: pbr.map(String::from),
                })
                .collect(),
            nodes: vec![Node { name: String::new(), parent: None, pivot: [0.0; 3] }],
        }
    }

    /// Every file under `dir`, as paths relative to it with `/`
    fn files_under(dir: &Path) -> Vec<String> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let name = format!("{prefix}{}", e.file_name().to_string_lossy());
                if e.path().is_dir() {
                    walk(&e.path(), &format!("{name}/"), out);
                } else {
                    out.push(name);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn texture_and_model_names_from_game_data_cannot_leave_the_output_folder() {
        let root = std::env::temp_dir().join(format!("wt-unpack-gltf-names-{}", std::process::id()));
        let out = root.join("a").join("b").join("out");
        // Texture names as a description file could give them, and a model name of the same kind
        let vehicle = textured(
            "../model",
            &[("../../escape", Some("..\\up")), ("c:drive", None), ("con", Some("nul")), ("a/b", None), ("a_b", None), ("plain_c", Some("plain_n"))],
        );
        write(&vehicle, &out).unwrap();
        let files = files_under(&root);
        assert!(files.iter().all(|f| f.starts_with("a/b/out/") && f.matches('/').count() == 3), "{files:?}");
        let doc: Value = serde_json::from_slice(&fs::read(out.join(".._model.gltf")).unwrap()).unwrap();
        assert_eq!(doc["buffers"][0]["uri"], ".._model.bin");
        let uris: Vec<&str> = doc["images"].as_array().unwrap().iter().map(|i| i["uri"].as_str().unwrap()).collect();
        // One file per image, each a plain name that exists; names that collide once made safe are kept apart
        assert_eq!(uris.len(), 12);
        assert_eq!(uris.iter().collect::<HashSet<_>>().len(), 12, "{uris:?}");
        for uri in &uris {
            assert!(!uri.contains(['/', '\\', ':']) && out.join(uri).is_file(), "{uri:?}");
        }
        assert_eq!(uris[0], ".._.._escape.png");
        assert_eq!(uris[3], "c_drive.png");
        // A device name on Windows, an ordinary name elsewhere
        assert_eq!(uris[4], if cfg!(windows) { "_con.png" } else { "con.png" });
        assert!(uris.contains(&"plain_c.png") && uris.contains(&"plain_n_normal.png"));
        // "a/b" and "a_b" are different textures: two files with their own pixels, and the one whose name
        // needed no change keeps it
        let pixel = |uri: &str| {
            let mut reader = png::Decoder::new(std::io::BufReader::new(fs::File::open(out.join(uri)).unwrap())).read_info().unwrap();
            let mut buf = vec![0; reader.output_buffer_size().unwrap()];
            reader.next_frame(&mut buf).unwrap();
            buf[0]
        };
        assert_eq!((uris[7], uris[8]), ("a_b_2.png", "a_b.png"));
        assert_eq!((pixel(uris[7]), pixel(uris[8])), (3, 4));
        fs::remove_dir_all(&root).unwrap();
    }
}
