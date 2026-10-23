//! FSB5 Vorbis: rebuilding the stripped Vorbis headers, repacking to Ogg, and decoding.
//!
//! FMOD stores only the audio packets of a Vorbis stream, each behind a `u16` size (0 or 0xFFFF ends
//! the stream). The three headers that a Vorbis decoder needs are rebuilt:
//!
//! - **Identification**: from the sample's channels and rate. FMOD always encodes with block sizes of
//!   256 and 2048.
//! - **Comment**: empty, with our vendor string.
//! - **Setup**: the codebooks, floors and residues. FMOD's encoder uses a fixed set of setup headers,
//!   and each sample names its own by CRC32 in its Vorbis chunk. The set ships in
//!   `vorbis_setups.bin.zst`.
//!
//! `vorbis_setups.bin.zst` is the zstd-compressed table `"FVS1"`, `u32` count, then per entry
//! `u32` CRC32, `u32` length and the setup packet (161 entries, 620 KB unpacked). The packets were
//! extracted from FMOD by [python-fsb5](https://github.com/HearthSim/python-fsb5) (MIT) and are taken
//! from vgmstream's `vorbis_codebooks_fsb.h` ([vgmstream](https://github.com/vgmstream/vgmstream),
//! ISC licence: "Copyright (c) 2008-2025 Adam Gashlin, Fastelbja, Ronny Elfert, bnnm, Christopher
//! Snowhill, NicknineTheEagle, bxaimc, Thealexbarney, CyberBotX, EdnessP, et al. Permission to use,
//! copy, modify, and distribute this software for any purpose with or without fee is hereby granted,
//! provided that the above copyright notice and this permission notice appear in all copies.").
//! vgmstream's full licence text is in `src/sound/LICENSE-vgmstream`. The same set is in
//! [fsbex](https://github.com/astral4/fsbex) (MIT OR Apache-2.0).
//!
//! Repacking to Ogg is lossless: the packets are copied into Ogg pages after the rebuilt headers,
//! with granule positions counted from each packet's block size. Decoding uses
//! [lewton](https://github.com/RustAudio/lewton) (MIT OR Apache-2.0).

use std::collections::HashMap;
use std::io::Write;
use std::sync::OnceLock;

use anyhow::{anyhow, ensure, Context, Result};
use lewton::audio::{get_decoded_sample_count, read_audio_packet_generic, PreviousWindowRight};
use lewton::header::{read_header_ident, read_header_setup, IdentHeader, SetupHeader};
use lewton::samples::InterleavedSamples;

use super::Sample;
use crate::bytes::Reader;

const SETUPS: &[u8] = include_bytes!("vorbis_setups.bin.zst");

/// Block size exponents FMOD encodes with: 256 and 2048.
const BLOCKSIZES: (u8, u8) = (8, 11);

const VENDOR: &[u8] = b"wt-unpack";

/// CRC32 to (start, length) of each setup packet in the unpacked table.
type SetupIndex = HashMap<u32, (usize, usize)>;

/// The setup packet FMOD's encoder used for a CRC32, if it is in the table.
pub fn setup_packet(crc: u32) -> Option<&'static [u8]> {
    static TABLE: OnceLock<(Vec<u8>, SetupIndex)> = OnceLock::new();
    let (raw, index) = TABLE.get_or_init(|| {
        let raw = zstd::bulk::decompress(SETUPS, 1 << 20).expect("embedded setup table is valid zstd");
        let index = parse_table(&raw).expect("embedded setup table is well formed");
        (raw, index)
    });
    index.get(&crc).map(|&(start, len)| &raw[start..start + len])
}

fn parse_table(raw: &[u8]) -> Result<SetupIndex> {
    let mut r = Reader::new(raw);
    ensure!(r.bytes(4)? == b"FVS1", "bad setup table magic");
    let count = r.u32()?;
    let mut index = HashMap::new();
    for _ in 0..count {
        let crc = r.u32()?;
        let len = r.u32()? as usize;
        let start = r.pos();
        r.skip(len)?;
        index.insert(crc, (start, len));
    }
    Ok(index)
}

fn ident_packet(channels: u8, rate: u32) -> Vec<u8> {
    let mut p = vec![1];
    p.extend_from_slice(b"vorbis");
    p.extend_from_slice(&0u32.to_le_bytes()); // version
    p.push(channels);
    p.extend_from_slice(&rate.to_le_bytes());
    p.extend_from_slice(&[0; 12]); // bitrate max, nominal, min: unknown
    p.push(BLOCKSIZES.1 << 4 | BLOCKSIZES.0);
    p.push(1); // framing
    p
}

fn comment_packet() -> Vec<u8> {
    let mut p = vec![3];
    p.extend_from_slice(b"vorbis");
    p.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    p.extend_from_slice(VENDOR);
    p.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    p.push(1);
    p
}

struct Headers {
    ident_packet: Vec<u8>,
    setup_packet: &'static [u8],
    ident: IdentHeader,
    setup: SetupHeader,
}

fn headers(s: &Sample) -> Result<Headers> {
    let crc = s.vorbis_setup.context("Vorbis sample has no setup header CRC32")?;
    let setup_packet = setup_packet(crc).with_context(|| format!("unknown Vorbis setup header {crc:08X}"))?;
    let ident_packet = ident_packet(s.channels, s.rate);
    let ident = read_header_ident(&ident_packet).map_err(|e| anyhow!("identification header: {e:?}"))?;
    let setup = read_header_setup(setup_packet, s.channels, BLOCKSIZES).map_err(|e| anyhow!("setup header {crc:08X}: {e:?}"))?;
    Ok(Headers { ident_packet, setup_packet, ident, setup })
}

/// The audio packets of a sample.
pub fn packets(data: &[u8]) -> impl Iterator<Item = Result<&[u8]>> {
    let mut r = Reader::new(data);
    std::iter::from_fn(move || {
        if r.pos() + 2 > data.len() {
            return None;
        }
        match r.u16() {
            Ok(0 | 0xFFFF) | Err(_) => None,
            Ok(n) => Some(r.bytes(n.into()).context("Vorbis packet runs past the sample data")),
        }
    })
}

/// Decodes a sample to interleaved 16-bit PCM, with the channels in WAV order.
pub fn decode(s: &Sample, data: &[u8]) -> Result<Vec<i16>> {
    let h = headers(s)?;
    let mut pwr = PreviousWindowRight::new();
    let mut out = Vec::with_capacity(s.frames as usize * usize::from(s.channels));
    for (i, packet) in packets(data).enumerate() {
        let block: InterleavedSamples<i16> = read_audio_packet_generic(&h.ident, &h.setup, packet?, &mut pwr)
            .map_err(|e| anyhow!("Vorbis packet {i}: {e:?}"))?;
        out.extend_from_slice(&block.samples);
    }
    if let Some(order) = wav_order(s.channels) {
        for frame in out.chunks_exact_mut(order.len()) {
            let vorbis: Vec<i16> = frame.to_vec();
            for (dst, &src) in frame.iter_mut().zip(order) {
                *dst = vorbis[src];
            }
        }
    }
    Ok(out)
}

/// For each WAV channel, the Vorbis channel it comes from, when the orders differ (Vorbis I spec 4.3.9).
fn wav_order(channels: u8) -> Option<&'static [usize]> {
    Some(match channels {
        3 => &[0, 2, 1],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 5, 3, 4],
        7 => &[0, 2, 1, 6, 5, 3, 4],
        8 => &[0, 2, 1, 7, 5, 6, 3, 4],
        _ => return None,
    })
}

/// Writes a sample as an Ogg Vorbis file, copying its packets unchanged.
pub fn write_ogg<W: Write>(s: &Sample, data: &[u8], w: W) -> Result<()> {
    let h = headers(s)?;
    // Count each packet's samples first: the last one carries the end-of-stream flag. A packet
    // completes the samples from the centre of the previous block to the centre of its own.
    let mut packets_and_ends = Vec::new();
    let (mut total, mut previous) = (0u64, None);
    for (i, packet) in packets(data).enumerate() {
        let packet = packet?;
        let block = block_size(&h, packet).with_context(|| format!("Vorbis packet {i}"))?;
        if let Some(previous) = previous {
            total += (previous + block) / 4;
        }
        previous = Some(block);
        // The header's length trims the end of the last block
        packets_and_ends.push((packet, total.min(u64::from(s.frames))));
        // FMOD's streams can run on for packets that only decode past that length. They are
        // dropped, since some decoders (FFmpeg) only trim samples from the last packet.
        if total >= u64::from(s.frames) {
            break;
        }
    }
    ensure!(!packets_and_ends.is_empty(), "Vorbis sample has no packets");

    let mut ogg = OggWriter::new(w, s.vorbis_setup.unwrap_or(0) ^ s.frames);
    ogg.packet(&h.ident_packet, 0, false)?;
    ogg.flush()?; // the identification header sits alone on the first page
    ogg.packet(&comment_packet(), 0, false)?;
    ogg.packet(h.setup_packet, 0, false)?;
    ogg.flush()?; // audio starts on a fresh page
    let last = packets_and_ends.len() - 1;
    for (i, (packet, granule)) in packets_and_ends.into_iter().enumerate() {
        ogg.packet(packet, granule, i == last)?;
    }
    ogg.w.flush()?;
    Ok(())
}

/// Block size of an audio packet: 256 or 2048.
fn block_size(h: &Headers, packet: &[u8]) -> Result<u64> {
    // Lewton does not expose the modes, but its sample count tells the block sizes apart: a short
    // block always spans 128 samples, while a long one spans 576 to 1472 depending on its neighbours.
    let span = get_decoded_sample_count(&h.ident, &h.setup, packet).map_err(|e| anyhow!("{e:?}"))?;
    let short = 1u64 << BLOCKSIZES.0;
    Ok(if span as u64 == short / 2 { short } else { 1 << BLOCKSIZES.1 })
}

/// Ogg page writer: packets are laced into pages of about 4 KB.
struct OggWriter<W: Write> {
    w: W,
    serial: u32,
    sequence: u32,
    segments: Vec<u8>,
    body: Vec<u8>,
    /// Granule position of the last packet that ended on the current page.
    granule: Option<u64>,
    /// The current page starts with the rest of a packet from the previous page.
    continued: bool,
    eos: bool,
}

const PAGE_TARGET: usize = 4096;

impl<W: Write> OggWriter<W> {
    fn new(w: W, serial: u32) -> Self {
        OggWriter { w, serial, sequence: 0, segments: Vec::new(), body: Vec::new(), granule: None, continued: false, eos: false }
    }

    fn packet(&mut self, data: &[u8], granule: u64, eos: bool) -> Result<()> {
        let mut rest = data;
        loop {
            if self.segments.len() == 255 {
                self.flush()?;
                self.continued = true;
            }
            // A packet ends with a segment shorter than 255 bytes, which may be empty
            let n = rest.len().min(255);
            self.segments.push(n as u8);
            self.body.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if n < 255 {
                break;
            }
        }
        self.granule = Some(granule);
        self.eos = eos;
        if eos || self.body.len() >= PAGE_TARGET {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.segments.is_empty() {
            return Ok(());
        }
        let flags = u8::from(self.continued) | u8::from(self.sequence == 0) << 1 | u8::from(self.eos) << 2;
        let mut page = Vec::with_capacity(27 + self.segments.len() + self.body.len());
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(flags);
        // -1: no packet ends on this page
        page.extend_from_slice(&self.granule.unwrap_or(u64::MAX).to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(self.segments.len() as u8);
        page.extend_from_slice(&self.segments);
        page.extend_from_slice(&self.body);
        let crc = ogg_crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        self.w.write_all(&page)?;
        self.sequence += 1;
        self.segments.clear();
        self.body.clear();
        self.granule = None;
        self.continued = false;
        Ok(())
    }
}

/// Ogg's CRC32: polynomial 0x04C11DB7, not reflected, zero start and no final XOR.
fn ogg_crc(data: &[u8]) -> u32 {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = (i as u32) << 24;
            for _ in 0..8 {
                c = if c & 0x8000_0000 != 0 { c << 1 ^ 0x04C1_1DB7 } else { c << 1 };
            }
            *e = c;
        }
        t
    });
    data.iter().fold(0, |crc, &b| crc << 8 ^ table[((crc >> 24) as u8 ^ b) as usize])
}

/// Splits an Ogg stream back into its packets, checking the page CRCs.
#[cfg(test)]
fn read_ogg(mut data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let (mut packets, mut partial) = (Vec::new(), Vec::new());
    while !data.is_empty() {
        ensure!(data.len() >= 27 && &data[..4] == b"OggS", "bad page");
        let n = usize::from(data[26]);
        let lacing = &data[27..27 + n];
        let body_len: usize = lacing.iter().map(|&l| usize::from(l)).sum();
        let page = &data[..27 + n + body_len];
        let mut zeroed = page.to_vec();
        zeroed[22..26].fill(0);
        ensure!(ogg_crc(&zeroed) == u32::from_le_bytes(page[22..26].try_into().unwrap()), "page CRC mismatch");
        let mut body = &page[27 + n..];
        for &l in lacing {
            partial.extend_from_slice(&body[..usize::from(l)]);
            body = &body[usize::from(l)..];
            if l < 255 {
                packets.push(std::mem::take(&mut partial));
            }
        }
        data = &data[page.len()..];
    }
    Ok(packets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc32(data: &[u8]) -> u32 {
        !data.iter().fold(!0u32, |mut c, &b| {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { c >> 1 ^ 0xEDB8_8320 } else { c >> 1 };
            }
            c
        })
    }

    #[test]
    fn every_setup_packet_matches_its_crc_and_parses() {
        let raw = zstd::bulk::decompress(SETUPS, 1 << 20).unwrap();
        let index = parse_table(&raw).unwrap();
        assert_eq!(index.len(), 161);
        for (&crc, &(start, len)) in &index {
            let packet = &raw[start..start + len];
            assert_eq!(crc32(packet), crc, "setup {crc:08X}");
            assert_eq!(&packet[..7], b"\x05vorbis");
        }
        // Lewton accepts a rebuilt header pair, here a stereo 48 kHz one
        let s = Sample { name: None, rate: 48000, channels: 2, frames: 0, loop_points: None, vorbis_setup: Some(0xA722_97FF), data: 0..0 };
        let h = headers(&s).unwrap();
        assert_eq!((h.ident.blocksize_0, h.ident.blocksize_1, h.ident.audio_sample_rate), (8, 11, 48000));
    }

    #[test]
    fn ogg_crc_check_value() {
        assert_eq!(ogg_crc(b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn ogg_pages_round_trip_packets_of_any_size() {
        let sizes = [0usize, 1, 254, 255, 256, 510, 4000, 65_000, 65_535, 3];
        let packets: Vec<Vec<u8>> = sizes.iter().enumerate().map(|(i, &n)| (0..n).map(|b| (b + i) as u8).collect()).collect();
        let mut out = Vec::new();
        let mut ogg = OggWriter::new(&mut out, 7);
        for (i, p) in packets.iter().enumerate() {
            ogg.packet(p, i as u64, i + 1 == packets.len()).unwrap();
        }
        assert_eq!(read_ogg(&out).unwrap(), packets);
        // The last page carries the end-of-stream flag and the last granule
        let last = out.windows(4).rposition(|w| w == b"OggS").unwrap();
        assert_eq!(out[last + 5] & 4, 4);
        assert_eq!(u64::from_le_bytes(out[last + 6..last + 14].try_into().unwrap()), 9);
    }

    #[test]
    fn packets_stop_at_the_end_marker() {
        let data = [2, 0, 0xAA, 0xBB, 1, 0, 0xCC, 0, 0, 9, 9];
        let got: Vec<_> = packets(&data).map(Result::unwrap).collect();
        assert_eq!(got, [&[0xAA, 0xBB][..], &[0xCC][..]]);
        assert!(packets(&[5, 0, 1]).next().unwrap().is_err());
    }
}
