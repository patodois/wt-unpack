#![no_main]
//! The small blocks of a level (`eVER`, `RqRL`, `DxP2`, `TEX`, `WBBX`, `LMp2`): the first byte picks the tag,
//! the rest is the payload, wrapped in a valid DBLD container.
use libfuzzer_sys::fuzz_target;
use wt_unpack::level::Level;

const TAGS: [&[u8; 4]; 7] = [b"eVER", b"RqRL", b"DxP2", b"\0TEX", b"WBBX", b"LMp2", b"RIGz"];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, payload)) = data.split_first() else { return };
    let tag = TAGS[usize::from(pick) % TAGS.len()];
    let mut file = b"DBLD3x64".to_vec();
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&((payload.len() + 4) as u32).to_le_bytes());
    file.extend_from_slice(tag);
    file.extend_from_slice(payload);
    file.extend_from_slice(&4u32.to_le_bytes());
    file.extend_from_slice(b"\0END");
    let level = Level::parse(&file).expect("a well-formed container");
    let _ = level.export_info();
    let _ = level.required_resources();
    let _ = level.texture_refs();
    let _ = level.inline_textures();
    let _ = level.world_bbox();
    let _ = level.phys_map_info();
    if let Ok(layers) = level.rendinst_layers() {
        for l in layers.iter().take(4) {
            wt_unpack_fuzz::use_layer(l);
        }
    }
});
