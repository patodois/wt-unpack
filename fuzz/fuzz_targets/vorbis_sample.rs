#![no_main]
//! The Vorbis path of a sample: the audio packets are the input; the header bytes pick a setup header from the
//! table (a CRC32 cannot be guessed), the channel count, the rate and the claimed frame count.
use libfuzzer_sys::fuzz_target;
use wt_unpack::sound::{vorbis, Sample};

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let crcs = wt_unpack_fuzz::setup_crcs();
    let crc = crcs[usize::from(u16::from_le_bytes([data[0], data[1]])) % crcs.len()];
    let rates = [8000, 22050, 44100, 48000, 0, 1, u32::MAX];
    let s = Sample {
        name: None,
        rate: rates[usize::from(data[2]) % rates.len()],
        channels: data[3] % 10,
        frames: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        loop_points: None,
        vorbis_setup: Some(crc),
        data: 0..0,
    };
    let packets = &data[8..];
    let _ = vorbis::decode(&s, packets);
    let _ = vorbis::write_ogg(&s, packets, std::io::sink());
    for p in vorbis::packets(packets) {
        let _ = p;
    }
});
