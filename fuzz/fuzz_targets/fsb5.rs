#![no_main]
//! `Fsb5::parse`, then every sample decoded and written as WAV (and as Ogg when it is Vorbis).
use libfuzzer_sys::fuzz_target;
use wt_unpack::sound::Fsb5;

fuzz_target!(|data: &[u8]| {
    let Ok(bank) = Fsb5::parse(data) else { return };
    for s in bank.samples.iter().take(16) {
        let _ = s.duration();
        if let Ok(pcm) = bank.decode(s) {
            let _ = pcm.write_wav(std::io::sink());
        }
        let _ = wt_unpack::sound::vorbis::write_ogg(s, bank.data(s), std::io::sink());
    }
});
