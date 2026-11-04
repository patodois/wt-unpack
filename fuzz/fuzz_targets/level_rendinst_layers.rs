#![no_main]
//! `RendInstLayer::parse_all` (the `RIGz` block), the counters and the instances.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(layers) = wt_unpack::level::RendInstLayer::parse_all(data) {
        for l in layers.iter().take(4) {
            wt_unpack_fuzz::use_layer(l);
        }
    }
});
