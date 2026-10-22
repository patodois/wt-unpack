//! Tests against an installed copy of War Thunder. They are skipped unless `WT_GAME_DIR` points at the
//! game's `Resources/game` folder, e.g.
//! `WT_GAME_DIR=/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game cargo test`.
//! No game data is stored in this repository.

use std::fs;
use std::path::PathBuf;

use wt_unpack::{blk, gltf, vehicle, vromfs::Vromfs};

fn game() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("WT_GAME_DIR")?);
    assert!(dir.join("aces.vromfs.bin").exists(), "WT_GAME_DIR does not look like the game folder");
    Some(dir)
}

#[test]
fn data_archives_unpack_and_their_blks_parse() {
    let Some(game) = game() else { return };
    for name in ["char.vromfs.bin", "game.vromfs.bin", "wwdata.vromfs.bin"] {
        let vfs = Vromfs::parse(&fs::read(game.join(name)).unwrap()).unwrap();
        assert!(vfs.md5_ok, "{name}: md5 mismatch");
        assert!(!vfs.files.is_empty(), "{name}: no files");

        let shared = vfs.get(wt_unpack::vromfs::SHARED_NAMEMAP_NAME).map(|nm| {
            let (mut ctx, dict) = blk::SharedContext::from_namemap(&nm.data).unwrap();
            ctx.dict = vfs.get(&dict).map(|f| f.data.clone());
            ctx
        });
        let mut parsed = 0;
        for f in vfs.files.iter().filter(|f| f.name.ends_with(".blk") && blk::is_binary(&f.data)) {
            blk::parse(&f.data, shared.as_ref()).unwrap_or_else(|e| panic!("{name}:{}: {e:#}", f.name));
            parsed += 1;
        }
        assert!(parsed > 0, "{name}: no binary BLKs");
    }
}

#[test]
fn dyn_model_desc_has_the_sherman() {
    let Some(game) = game() else { return };
    let root = blk::parse(&fs::read(game.join("content/base/res/dynModelDesc.bin")).unwrap(), None).unwrap();
    let sherman = root.blocks.iter().find(|b| b.name == "m4a1_76w_sherman").unwrap();
    assert!(sherman.blocks.iter().any(|b| b.name == "matR"));
    assert!(sherman.blocks.iter().any(|b| b.name == "tex"));
}

#[test]
fn sherman_assembles_and_exports() {
    let Some(game) = game() else { return };
    let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
    let v = vehicle::load(&game, &pack, "m4a1_76w_sherman", 0).unwrap();

    // 126,420 rigid triangles plus 5,272 in the tracks and the flexible antenna
    let triangles: usize = v.meshes.iter().map(|m| m.indices.len() / 3).sum();
    assert_eq!(triangles, 131_692);
    assert_eq!(v.materials.len(), 20);
    for m in &v.meshes {
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.uvs.len());
        assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
        assert!(m.material < v.materials.len());
    }
    // Roughly Sherman-sized: about 7 m long with the gun, under 4 m tall with the antenna
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in v.meshes.iter().flat_map(|m| &m.positions) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size: Vec<f32> = (0..3).map(|k| hi[k] - lo[k]).collect();
    assert!((6.5..7.5).contains(&size[0]) && (3.0..4.0).contains(&size[1]) && (2.5..3.0).contains(&size[2]), "{size:?}");
    // The hull's colour texture is found, with the olive camouflage baked in
    let hull = v.materials.iter().find(|m| m.color_key.as_deref().is_some_and(|k| k.contains("body_c__us_camo_olive"))).unwrap();
    assert!(hull.color.is_some() && hull.metal_rough.is_some() && hull.normal.is_some());

    let out = std::env::temp_dir().join(format!("wt-unpack-test-{}", std::process::id()));
    gltf::write(&v, &out).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&fs::read(out.join("m4a1_76w_sherman.gltf")).unwrap()).unwrap();
    assert_eq!(doc["meshes"][0]["primitives"].as_array().unwrap().len(), v.meshes.len());
    for image in doc["images"].as_array().unwrap() {
        assert!(out.join(image["uri"].as_str().unwrap()).exists());
    }
    fs::remove_dir_all(&out).unwrap();
}
