#![no_main]
//! `SharedContext::from_namemap`: the `\xff?nm` file of an archive.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wt_unpack::blk::SharedContext::from_namemap(data);
});
