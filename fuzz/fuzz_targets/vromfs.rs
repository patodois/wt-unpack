#![no_main]
//! `Vromfs::parse` and what the CLI does with an archive's files: the shared name map and its dictionary,
//! then every binary BLK in it.
use libfuzzer_sys::fuzz_target;
use wt_unpack::{blk, vromfs};

fuzz_target!(|data: &[u8]| {
    let Ok(vfs) = vromfs::Vromfs::parse(data) else { return };
    let shared = vromfs::shared_context(&vfs).ok().flatten();
    for f in vfs.files.iter().take(64) {
        if f.data.len() <= 1 << 16 && blk::is_binary(&f.data) {
            let _ = blk::parse(&f.data, shared.as_ref());
        }
    }
});
