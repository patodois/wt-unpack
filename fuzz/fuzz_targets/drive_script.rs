#![no_main]
//! `tank::parse_script`: the `--script` text of a scripted drive.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = wt_unpack::tank::parse_script(s);
    }
});
