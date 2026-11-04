#![no_main]
//! `rodb::parse`: a read-only data block dump (the BLK form inside resources).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(b) = wt_unpack::rodb::parse(data) {
        let _ = b.to_text();
        let _ = b.to_json();
    }
});
