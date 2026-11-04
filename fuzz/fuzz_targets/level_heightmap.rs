#![no_main]
//! `Heightmap::parse` (the `HM2` block) and the grid built from it.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(hm) = wt_unpack::level::Heightmap::parse(data) {
        wt_unpack_fuzz::use_heightmap(&hm);
    }
});
