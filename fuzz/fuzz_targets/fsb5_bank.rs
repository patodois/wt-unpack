#![no_main]
//! `fsb5_ranges`: the FSB5 banks inside an FMOD `.bank` (RIFF/FEV) file, each parsed where it was found.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(ranges) = wt_unpack::sound::fsb5_ranges(data) {
        // As the CLI does (`&map[range.clone()]`): a range outside the file panics here and is a finding, not skipped.
        // Only the first four banks are parsed, to keep an execution short
        for (i, r) in ranges.iter().enumerate() {
            let bank = &data[r.clone()];
            if i < 4 {
                let _ = wt_unpack::sound::Fsb5::parse(bank);
            }
        }
    }
});
