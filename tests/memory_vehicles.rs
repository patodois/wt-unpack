//! Loading vehicles one after another keeps nothing alive: after each is dropped, the allocator's live bytes
//! are those of the provider's indexes, however many vehicles went by (what `check-bridge` relies on to stay
//! flat). Runs only with `WT_GAME_DIR` set, and measures with the macOS allocator's statistics (elsewhere it
//! only loads and drops). It is the only test of this file, so nothing else allocates while it measures.

#![cfg(feature = "bridge")]

use std::path::PathBuf;

use rewrite_bridge::{AssetKind, Provider};
use wt_unpack::bridge::WarThunder;

#[cfg(target_os = "macos")]
fn live_bytes() -> usize {
    #[repr(C)]
    #[derive(Default)]
    struct Stats {
        blocks_in_use: u32,
        size_in_use: usize,
        max_size_in_use: usize,
        size_allocated: usize,
    }
    extern "C" {
        fn malloc_zone_statistics(zone: *mut std::ffi::c_void, stats: *mut Stats);
    }
    let mut stats = Stats::default();
    // Safety: a null zone means every zone; the struct is the one libmalloc fills in
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
    stats.size_in_use
}

#[cfg(not(target_os = "macos"))]
fn live_bytes() -> usize {
    0
}

#[test]
fn vehicles_leave_nothing_behind_once_dropped() {
    let Some(dir) = std::env::var_os("WT_GAME_DIR").map(PathBuf::from) else { return };
    assert!(dir.join("aces.vromfs.bin").exists(), "WT_GAME_DIR does not look like the game folder");

    let wt = WarThunder::new(&dir).with_textures(false);
    let mut vehicles = wt.list("").unwrap();
    vehicles.retain(|a| a.kind == AssetKind::Vehicle);
    assert!(vehicles.len() > 100);

    // The first load builds the indexes; measure after it
    let id = &vehicles[0].id;
    let first = wt.load_animated(id).unwrap();
    // Without textures the materials keep their names and alpha modes but hold no images
    assert!(!first.model.materials.is_empty());
    assert!(first.model.materials.iter().all(|m| m.base_color.is_none() && m.normal.is_none()));
    drop(first);
    let baseline = live_bytes();

    for v in vehicles.iter().skip(1).take(60) {
        let loaded = wt.load_animated(&v.id).unwrap();
        assert!(!loaded.model.meshes.is_empty());
    }
    wt_unpack::memory::release_freed_memory();
    let after = live_bytes();
    // Some slack for the indexes' lazily built parts, none for 60 vehicles (tens of MB each)
    assert!(after <= baseline + (32 << 20), "live bytes grew from {} to {} MB over 60 vehicles", baseline >> 20, after >> 20);

    // With textures the same vehicle carries images, and they go away with it too
    let wt = WarThunder::new(&dir);
    let textured = wt.load_animated(id).unwrap();
    assert!(textured.model.materials.iter().any(|m| m.base_color.is_some()));
}
