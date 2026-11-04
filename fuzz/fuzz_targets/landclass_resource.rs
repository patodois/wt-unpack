#![no_main]
//! `landclass::resource_detail`: a land class resource, and the material read from its detail block.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(detail) = wt_unpack::landclass::resource_detail(data) {
        let _ = wt_unpack::landclass::LandClassMaterial::from_block("class", &detail);
    }
});
