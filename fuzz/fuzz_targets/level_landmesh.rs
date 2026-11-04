#![no_main]
//! `LandMesh::parse` (the `lmap` block) and everything read from it.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(lm) = wt_unpack::level::LandMesh::parse(data) {
        wt_unpack_fuzz::use_landmesh(&lm, None);
    }
});
