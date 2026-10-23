//! Loads vehicles one after another through the bridge provider, as `check-bridge` does, and reports the
//! process's memory after each one: what `check-bridge` must keep flat over thousands of vehicles.
//!
//! ```sh
//! cargo run --release --features bridge --example vehicle_memory -- $GAME [filter] [count] [skip]
//! WT_NO_RELEASE=1 ...   # do not hand the allocator's free pages back between vehicles
//! WT_CLEAR_EVERY=10 ... # also call `clear_caches()` after every 10th vehicle
//! ```
//!
//! Per vehicle it prints the memory held by live blocks (macOS: what the program really uses; it falls
//! back to the indexes once the vehicle is dropped), the resident set size and the physical footprint.
//! The last two include pages the allocator has freed but not returned, which is what grew without bound
//! in `check-bridge` before it released them.

use rewrite_bridge::{AssetKind, Provider};
use wt_unpack::bridge::WarThunder;

#[cfg(target_os = "macos")]
fn memory_mb() -> (u64, u64, u64) {
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
    // Safety: proc_pid_rusage writes a rusage_info_v2 through the pointer it is given
    let (resident, footprint) = unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let ptr = (&mut info as *mut libc::rusage_info_v2).cast::<libc::rusage_info_t>();
        if libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, ptr) == 0 {
            (info.ri_resident_size >> 20, info.ri_phys_footprint >> 20)
        } else {
            (0, 0)
        }
    };
    ((stats.size_in_use >> 20) as u64, resident, footprint)
}

#[cfg(not(target_os = "macos"))]
fn memory_mb() -> (u64, u64, u64) {
    (0, 0, 0)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let game = args.next().ok_or_else(|| anyhow::anyhow!("usage: vehicle_memory <game folder> [filter] [count] [skip]"))?;
    let filter = args.next().unwrap_or_default();
    let count: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(20);
    let skip: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    let release = std::env::var_os("WT_NO_RELEASE").is_none();
    let clear_every: usize = std::env::var("WT_CLEAR_EVERY").ok().and_then(|a| a.parse().ok()).unwrap_or(0);

    let wt = WarThunder::new(game);
    let mut vehicles = wt.list(&filter).map_err(|e| anyhow::anyhow!("{e}"))?;
    vehicles.retain(|a| a.kind == AssetKind::Vehicle);
    println!("{:>4}  {:<44} {:>9} {:>9} {:>9}", "#", "vehicle", "live MB", "resident", "footprint");
    for (i, v) in vehicles.iter().skip(skip).take(count).enumerate() {
        let triangles = match wt.load_animated(&v.id) {
            Ok(m) => m.model.meshes.iter().map(|x| x.indices.len() / 3).sum::<usize>(),
            Err(e) => {
                println!("{}: {e}", v.id);
                0
            }
        };
        if release {
            wt_unpack::memory::release_freed_memory();
        }
        if clear_every > 0 && (i + 1) % clear_every == 0 {
            wt.clear_caches();
        }
        let (live, resident, footprint) = memory_mb();
        println!("{:>4}  {:<44} {live:>9} {resident:>9} {footprint:>9}  ({triangles} triangles)", skip + i + 1, v.id);
    }
    wt.clear_caches();
    let (live, resident, footprint) = memory_mb();
    println!("{:>4}  {:<44} {live:>9} {resident:>9} {footprint:>9}", "", "after clear_caches()");
    Ok(())
}
