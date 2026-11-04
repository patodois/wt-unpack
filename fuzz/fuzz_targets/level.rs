#![no_main]
//! `Level::parse` (the DBLD container) and every accessor that reads a block, then what the CLI builds from
//! the landmesh, the heightmap and the rendinst layers.
use libfuzzer_sys::fuzz_target;
use wt_unpack::level::Level;

fuzz_target!(|data: &[u8]| {
    let Ok(level) = Level::parse(data) else { return };
    let _ = level.export_info();
    let _ = level.required_resources();
    let _ = level.texture_refs();
    let _ = level.inline_textures();
    let _ = level.world_bbox();
    let _ = level.phys_map_info();
    let hm = level.heightmap().ok().flatten();
    if let Some(hm) = &hm {
        wt_unpack_fuzz::use_heightmap(hm);
    }
    if let Ok(Some(lm)) = level.landmesh() {
        wt_unpack_fuzz::use_landmesh(&lm, hm.as_ref());
    }
    if let Ok(layers) = level.rendinst_layers() {
        for l in layers.iter().take(4) {
            wt_unpack_fuzz::use_layer(l);
        }
    }
});
