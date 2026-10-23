//! Loads a level through the bridge provider, drops the scene, and reports the process's memory at each
//! step, to check that a consumer gets its memory back.
//!
//! ```sh
//! /usr/bin/time -l cargo run --release --features bridge --example scene_memory -- $GAME levels/avg_berlin.bin
//! ```
//!
//! Per step it prints the memory the allocator holds for live blocks (macOS: what the program really
//! uses; it should fall back to almost nothing once the scene is dropped), the resident set size (via
//! `ps`) and the physical footprint (macOS). The last two include pages that were freed but that the
//! allocator and the OS have not taken back yet, and the OS compresses idle pages, so they move between
//! runs and stay high after a drop until the system needs the memory; `clear_caches()` asks the allocator
//! to release them. `/usr/bin/time -l` adds the peaks ("maximum resident set size" and "peak memory
//! footprint").

use std::hash::{Hash, Hasher};
use std::process::Command;

use rewrite_bridge::{Provider, Scene, SceneModel};
use wt_unpack::bridge::WarThunder;

/// Resident set size now, in MB (asks `ps`; 0 when that is not available)
fn rss_mb() -> u64 {
    let out = Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    out.ok().and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.trim().parse::<u64>().ok()).map_or(0, |kb| kb / 1024)
}

/// Physical footprint now, in MB (macOS: resident plus compressed pages; 0 elsewhere)
#[cfg(target_os = "macos")]
fn footprint_mb() -> u64 {
    // Safety: proc_pid_rusage writes a rusage_info_v2 through the pointer it is given
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let ptr = (&mut info as *mut libc::rusage_info_v2).cast::<libc::rusage_info_t>();
        if libc::proc_pid_rusage(std::process::id() as i32, libc::RUSAGE_INFO_V2, ptr) == 0 {
            info.ri_phys_footprint >> 20
        } else {
            0
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn footprint_mb() -> u64 {
    0
}

/// A hash of everything in the scene, to see that a change to the loader leaves its output alone
fn digest(scene: &Scene) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    scene.name.hash(&mut h);
    for m in &scene.models {
        let SceneModel::Inline(m) = m else { continue };
        m.name.hash(&mut h);
        for n in &m.nodes {
            (&n.name, n.parent, n.translation.map(f32::to_bits), n.rotation.map(f32::to_bits), n.scale.map(f32::to_bits)).hash(&mut h);
        }
        for x in &m.meshes {
            (&x.name, x.node, x.material, x.skin).hash(&mut h);
            for p in x.positions.iter().chain(&x.normals) {
                p.map(f32::to_bits).hash(&mut h);
            }
            for p in &x.uvs {
                p.map(f32::to_bits).hash(&mut h);
            }
            x.indices.hash(&mut h);
        }
        for x in &m.materials {
            (&x.name, x.base_color_factor.map(f32::to_bits), x.metallic.to_bits(), x.roughness.to_bits(), x.double_sided, format!("{:?}", x.alpha)).hash(&mut h);
            if let Some(i) = &x.base_color {
                (i.width, i.height, &i.rgba).hash(&mut h);
            }
        }
    }
    for i in &scene.instances {
        (i.model, i.transform.translation.map(f32::to_bits), i.transform.rotation.map(f32::to_bits), i.transform.scale.map(f32::to_bits)).hash(&mut h);
    }
    h.finish()
}

/// Bytes in live malloc blocks (macOS; 0 elsewhere)
#[cfg(target_os = "macos")]
fn live_mb() -> u64 {
    #[repr(C)]
    #[derive(Default)]
    struct MallocStats {
        blocks_in_use: u32,
        size_in_use: usize,
        max_size_in_use: usize,
        size_allocated: usize,
    }
    extern "C" {
        fn malloc_zone_statistics(zone: *mut std::ffi::c_void, stats: *mut MallocStats);
    }
    let mut stats = MallocStats::default();
    // Safety: a null zone means every zone; the struct is the one libmalloc fills in
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
    (stats.size_in_use >> 20) as u64
}

#[cfg(not(target_os = "macos"))]
fn live_mb() -> u64 {
    0
}

fn report(step: &str) {
    println!("{step:<34} live {:>5} MB   resident {:>5} MB   footprint {:>5} MB", live_mb(), rss_mb(), footprint_mb());
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let game = args.next().ok_or_else(|| anyhow::anyhow!("usage: scene_memory <game folder> [levels/avg_berlin.bin]"))?;
    let id = args.next().unwrap_or_else(|| "levels/avg_berlin.bin".to_string());
    report("start");
    let wt = WarThunder::new(game);
    let scene = wt.load_scene(&id).map_err(|e| anyhow::anyhow!("{e}"))?;
    report(&format!("loaded ({} instances)", scene.instances.len()));
    if std::env::var_os("WT_DIGEST").is_some() {
        println!("digest {:016x}", digest(&scene));
    }
    drop(scene);
    report("scene dropped");
    if std::env::var_os("WT_SKIP_CLEAR").is_none() {
        wt.clear_caches();
        report("clear_caches()");
    }
    if let Ok(secs) = std::env::var("WT_HOLD_SECS") {
        // Time to look at the process with `footprint` or `vmmap`
        std::thread::sleep(std::time::Duration::from_secs(secs.parse().unwrap_or(0)));
    }
    drop(wt);
    report("provider dropped");
    Ok(())
}
