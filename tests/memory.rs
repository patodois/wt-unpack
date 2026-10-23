//! Memory of loading a level: the loader keeps nothing alive after the scene is dropped, and its peak stays
//! under a ceiling. Runs only with `WT_GAME_DIR` set, and measures with the macOS allocator's statistics
//! (elsewhere it only loads and drops). It is the only test of this file, so nothing else allocates in the
//! process while it measures.

#![cfg(feature = "bridge")]

use std::path::PathBuf;

use rewrite_bridge::Provider;
use wt_unpack::bridge::WarThunder;

#[cfg(target_os = "macos")]
mod malloc {
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

    /// Bytes in live malloc blocks now
    pub fn live() -> usize {
        let mut stats = Stats::default();
        // Safety: a null zone means every zone; the struct is the one libmalloc fills in
        unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
        stats.size_in_use
    }
}

#[cfg(not(target_os = "macos"))]
mod malloc {
    pub fn live() -> usize {
        0
    }
}

#[test]
fn berlin_leaves_nothing_behind_once_dropped() {
    let Some(dir) = std::env::var_os("WT_GAME_DIR").map(PathBuf::from) else { return };
    assert!(dir.join("aces.vromfs.bin").exists(), "WT_GAME_DIR does not look like the game folder");

    // Sample the live bytes while loading, to see the peak
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler = {
        let (stop, peak) = (stop.clone(), peak.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                peak.fetch_max(malloc::live(), std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })
    };

    let before = malloc::live();
    let wt = WarThunder::new(&dir);
    let scene = wt.load_scene("levels/avg_berlin.bin").unwrap();
    let loaded = malloc::live();
    assert!(scene.instances.len() > 500_000);
    drop(scene);
    let dropped = malloc::live();
    wt.clear_caches();
    let cleared = malloc::live();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sampler.join().unwrap();

    let mb = |b: usize| b >> 20;
    eprintln!("live: {} MB before, {} MB loaded, {} MB after the drop, {} MB after clear_caches; peak {} MB", mb(before), mb(loaded), mb(dropped), mb(cleared), mb(peak.load(std::sync::atomic::Ordering::Relaxed)));
    if cfg!(target_os = "macos") {
        // The scene is about 850 MB; the peak was 1,110 MB when the loader kept a second copy of the
        // textures and the whole of riDesc.bin
        assert!(mb(loaded - before) > 500 && mb(loaded) < 1000, "{} MB loaded", mb(loaded));
        assert!(mb(peak.load(std::sync::atomic::Ordering::Relaxed)) < 1000, "peak {} MB", mb(peak.load(std::sync::atomic::Ordering::Relaxed)));
        // Dropping the scene frees it; the provider's indexes are all that is left, and they go with the caches
        assert!(mb(dropped) < 60, "{} MB still live after dropping the scene", mb(dropped));
        assert!(mb(cleared) < 5, "{} MB still live after clear_caches", mb(cleared));
    }
}
