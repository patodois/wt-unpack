//! Reader for FMOD Studio sound banks, the `.bank` files in War Thunder's `sound/` folder.
//!
//! Every bank comes in a pair: `name.bank` holds the events, buses and mixer data, and
//! `name.assets.bank` holds the sample audio. `masterbank.strings.bank` holds the event path strings.
//! Only the sample audio is read here.
//!
//! Bank container (RIFF, little-endian, chunks padded to even sizes):
//!
//! ```text
//! "RIFF" u32 size "FEV "
//!   "FMT "  u32 format version (0x8E in War Thunder 2.59), u32 oldest compatible version
//!   "LIST" "PROJ"
//!     "BNKI" bank id and flags
//!     "LIST" ...   bus, event, timeline, parameter tables (empty in .assets.bank files)
//!     "SNDH" u32 unknown (0x0008_0003 with one SND chunk, 0x0008_0005 with two),
//!            then per SND chunk: u32 file offset of its FSB5, u32 FSB5 size
//!     "STDT" "STBL" "HASH" "DEL " "MUTE" "REFI" "PLAT"
//!   "SND "  zero padding up to a 32-byte aligned file offset, then one FSB5 sample bank
//!   "SND "  ...  (a bank can carry several; SNDH lists them all)
//! ```
//!
//! FSB5 sample bank (little-endian):
//!
//! ```text
//! "FSB5" u32 version (1; version 0 has 4 more header bytes)
//! u32 sample count, u32 sample header bytes, u32 name table bytes, u32 sample data bytes
//! u32 codec        one for the whole bank, see `Codec`
//! u32 zero, u32 flags, [16] hash, [8] unknown           header ends at 0x3C (0x40 in version 0)
//! sample headers, one per sample:
//!   u64 bits  0      more chunks follow
//!             1..4   rate index: 4000 8000 11000 11025 16000 22050 24000 32000 44100 48000 96000
//!             5..6   channels: 1, 2, 6, 8
//!             7..33  data offset / 32, from the start of the sample data
//!             34..63 length in sample frames
//!   chunks, while the previous one says more follow:
//!     u32 bits 0 more chunks follow | 1..24 size | 25..31 type, then `size` bytes:
//!     1 channels (u8), 2 rate (u32), 3 loop (u32 start, u32 end),
//!     11 Vorbis (u32 CRC32 of the setup header, u32 seek table bytes, seek table)
//! name table: u32 offset per sample (from the start of the table), then NUL-terminated names
//! sample data: a sample runs from its offset to the next sample's offset (or the end)
//! ```
//!
//! Codec data:
//!
//! - **PCM**: interleaved little-endian samples (8-bit is signed).
//! - **FADPCM**, FMOD's own ADPCM: each channel is a run of 0x8C-byte frames, and the channels'
//!   frames are interleaved. A frame is `u32` coefficient indices and `u32` shifts (a nibble per
//!   subframe), `i16` history 1 and 2, then 8 subframes of 16 bytes. Each subframe decodes 32 nibbles
//!   (low nibble first in each little-endian `u32`) with a two-tap predictor, 256 samples a frame.
//! - **Vorbis**: a run of packets, each a `u16` size and the packet. The three Vorbis headers are left
//!   out; see `vorbis` for how they are rebuilt.

use std::fs::File;
use std::io::{self, Write};
use std::ops::Range;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

use crate::bytes::Reader;

pub mod vorbis;

/// Codec of an FSB5 bank (FMOD's `FMOD_SOUND_FORMAT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    Pcm8,
    Pcm16,
    Pcm24,
    Pcm32,
    PcmFloat,
    GcAdpcm,
    ImaAdpcm,
    Vag,
    HeVag,
    Xma,
    Mpeg,
    Celt,
    Atrac9,
    Xwma,
    Vorbis,
    Fadpcm,
    Opus,
    Unknown(u32),
}

impl Codec {
    fn from_mode(mode: u32) -> Self {
        use Codec::*;
        match mode {
            1 => Pcm8,
            2 => Pcm16,
            3 => Pcm24,
            4 => Pcm32,
            5 => PcmFloat,
            6 => GcAdpcm,
            7 => ImaAdpcm,
            8 => Vag,
            9 => HeVag,
            10 => Xma,
            11 => Mpeg,
            12 => Celt,
            13 => Atrac9,
            14 => Xwma,
            15 => Vorbis,
            16 => Fadpcm,
            17 => Opus,
            m => Unknown(m),
        }
    }
}

const RATES: [u32; 11] = [4000, 8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000];

const CHUNK_CHANNELS: u32 = 1;
const CHUNK_RATE: u32 = 2;
const CHUNK_LOOP: u32 = 3;
const CHUNK_VORBIS: u32 = 11;

/// One sample of an FSB5 bank.
#[derive(Clone, Debug)]
pub struct Sample {
    pub name: Option<String>,
    pub rate: u32,
    pub channels: u8,
    /// Length in sample frames (samples per channel).
    pub frames: u32,
    /// Loop start and end frames, when the sample loops.
    pub loop_points: Option<(u32, u32)>,
    /// For Vorbis: CRC32 of the setup header the sample was encoded with.
    pub vorbis_setup: Option<u32>,
    /// Byte range of the sample's data within the FSB5.
    pub data: Range<usize>,
}

impl Sample {
    pub fn duration(&self) -> f64 {
        f64::from(self.frames) / f64::from(self.rate.max(1))
    }
}

/// A parsed FSB5 sample bank, borrowing its bytes.
pub struct Fsb5<'a> {
    pub version: u32,
    pub codec: Codec,
    pub samples: Vec<Sample>,
    buf: &'a [u8],
}

impl<'a> Fsb5<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(buf);
        ensure!(r.bytes(4)? == b"FSB5", "not an FSB5 sample bank");
        let version = r.u32()?;
        ensure!(version <= 1, "unsupported FSB5 version {version}");
        let count = r.u32()? as usize;
        let header_bytes = r.u32()? as usize;
        let name_bytes = r.u32()? as usize;
        let data_bytes = r.u32()? as usize;
        let codec = Codec::from_mode(r.u32()?);
        let base = if version == 0 { 0x40 } else { 0x3C };
        let headers_end = base + header_bytes;
        let data_start = headers_end + name_bytes;
        let data_end = data_start + data_bytes;
        ensure!(data_end <= buf.len(), "FSB5 needs {data_end} bytes but has {}", buf.len());

        let mut r = Reader::at(buf, base)?;
        let mut samples = Vec::with_capacity(count.min(header_bytes / 8));
        let mut offsets = Vec::with_capacity(samples.capacity());
        for i in 0..count {
            let mode = r.u64()?;
            let mut more = mode & 1 != 0;
            let rate_index = ((mode >> 1) & 0xF) as usize;
            let mut rate = RATES.get(rate_index).copied().unwrap_or(0);
            let mut channels = [1, 2, 6, 8][((mode >> 5) & 3) as usize];
            offsets.push((((mode >> 7) & 0x07FF_FFFF) << 5) as usize);
            let frames = (mode >> 34) as u32;
            let (mut loop_points, mut vorbis_setup) = (None, None);
            while more {
                let word = r.u32()?;
                more = word & 1 != 0;
                let size = ((word >> 1) & 0xFF_FFFF) as usize;
                let kind = word >> 25;
                let mut c = Reader::new(r.bytes(size)?);
                match kind {
                    CHUNK_CHANNELS => channels = c.u8()?,
                    CHUNK_RATE => rate = c.u32()?,
                    CHUNK_LOOP => loop_points = Some((c.u32()?, c.u32().unwrap_or(frames))),
                    CHUNK_VORBIS => vorbis_setup = Some(c.u32()?),
                    _ => {}
                }
            }
            ensure!(rate != 0, "sample {i} has an unknown rate index {rate_index}");
            ensure!(r.pos() <= headers_end, "sample headers overrun their {header_bytes} bytes");
            samples.push(Sample { name: None, rate, channels, frames, loop_points, vorbis_setup, data: 0..0 });
        }

        for i in 0..count {
            let start = offsets[i];
            let end = offsets.get(i + 1).copied().unwrap_or(data_bytes);
            ensure!(start <= end && end <= data_bytes, "sample {i} data {start}..{end} is outside {data_bytes} bytes");
            samples[i].data = data_start + start..data_start + end;
        }

        if name_bytes > 0 {
            let table = &buf[headers_end..data_start];
            let mut r = Reader::new(table);
            for (i, s) in samples.iter_mut().enumerate() {
                let off = r.u32()? as usize;
                s.name = Some(Reader::at(table, off)?.cstr().with_context(|| format!("name of sample {i}"))?);
            }
        }
        Ok(Fsb5 { version, codec, samples, buf })
    }

    /// The encoded bytes of a sample.
    pub fn data(&self, s: &Sample) -> &'a [u8] {
        &self.buf[s.data.clone()]
    }

    /// Decodes a sample to 16-bit PCM.
    pub fn decode(&self, s: &Sample) -> Result<Pcm> {
        let data = self.data(s);
        let channels = usize::from(s.channels);
        ensure!(channels > 0, "sample has no channels");
        let want = s.frames as usize * channels;
        let mut samples: Vec<i16> = match self.codec {
            Codec::Pcm8 => data.iter().map(|&b| i16::from(b as i8) << 8).collect(),
            Codec::Pcm16 => data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect(),
            Codec::Pcm24 => data.chunks_exact(3).map(|b| i16::from_le_bytes([b[1], b[2]])).collect(),
            Codec::Pcm32 => data.chunks_exact(4).map(|b| i16::from_le_bytes([b[2], b[3]])).collect(),
            Codec::PcmFloat => data
                .chunks_exact(4)
                .map(|b| (f32::from_le_bytes(b.try_into().unwrap()) * 32768.0).clamp(-32768.0, 32767.0) as i16)
                .collect(),
            Codec::Fadpcm => fadpcm_decode(data, channels),
            Codec::Vorbis => vorbis::decode(s, data)?,
            c => bail!("decoding {c:?} is not supported"),
        };
        ensure!(samples.len() >= want, "decoded {} frames, the header says {}", samples.len() / channels, s.frames);
        samples.truncate(want);
        Ok(Pcm { channels: s.channels.into(), rate: s.rate, samples })
    }
}

/// Byte ranges of the FSB5 sample banks inside a `.bank` file. Empty for banks without sample data.
pub fn fsb5_ranges(file: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut r = Reader::new(file);
    ensure!(r.bytes(4)? == b"RIFF", "not a RIFF file");
    let riff_end = (r.u32()? as usize + 8).min(file.len());
    ensure!(r.bytes(4)? == b"FEV ", "not an FMOD bank (RIFF form is not FEV)");

    let (mut sndh, mut snd_chunks) = (None, Vec::new());
    for (id, range) in chunks(file, 12..riff_end)? {
        match &id {
            b"LIST" if file.get(range.start..range.start + 4) == Some(b"PROJ") => {
                for (id, inner) in chunks(file, range.start + 4..range.end)? {
                    if &id == b"SNDH" {
                        sndh = Some(inner);
                    }
                }
            }
            b"SND " => snd_chunks.push(range),
            _ => {}
        }
    }

    let mut out = Vec::new();
    match sndh.filter(|s| s.len() > 4) {
        Some(table) => {
            let mut r = Reader::new(&file[table.start + 4..table.end]);
            while r.pos() + 8 <= table.len() - 4 {
                let (off, size) = (r.u32()? as usize, r.u32()? as usize);
                ensure!(
                    snd_chunks.iter().any(|c| c.start <= off && off + size <= c.end),
                    "SNDH entry {off}+{size} is not inside an SND chunk"
                );
                out.push(off..off + size);
            }
        }
        // No table: the FSB5 starts at the first 32-byte aligned offset of each SND chunk
        None => {
            for c in snd_chunks {
                out.push(c.start.next_multiple_of(32).min(c.end)..c.end);
            }
        }
    }
    for range in &out {
        ensure!(file.get(range.start..range.start + 4) == Some(b"FSB5"), "no FSB5 at offset {}", range.start);
    }
    Ok(out)
}

/// The chunks in `range`: their ids and data ranges.
fn chunks(file: &[u8], range: Range<usize>) -> Result<Vec<([u8; 4], Range<usize>)>> {
    let mut r = Reader::at(&file[..range.end], range.start)?;
    let mut out = Vec::new();
    while r.pos() + 8 <= range.end {
        let id: [u8; 4] = r.bytes(4)?.try_into().unwrap();
        let size = r.u32()? as usize;
        let start = r.pos();
        r.skip(size).with_context(|| format!("chunk {:?} at {} runs past its parent", String::from_utf8_lossy(&id), start - 8))?;
        out.push((id, start..start + size));
        if size % 2 == 1 && r.pos() < range.end {
            r.skip(1)?;
        }
    }
    Ok(out)
}

/// Memory-maps a bank, so only the pages that are read are loaded.
pub fn map(path: &Path) -> Result<memmap2::Mmap> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    // Safety: the game files are not modified while we read them
    unsafe { memmap2::Mmap::map(&file) }.with_context(|| format!("mapping {}", path.display()))
}

const FADPCM_FRAME: usize = 0x8C;
const FADPCM_COEFS: [[i32; 2]; 8] = [[0, 0], [60, 0], [122, 60], [115, 52], [98, 55], [0, 0], [0, 0], [0, 0]];

fn fadpcm_decode(data: &[u8], channels: usize) -> Vec<i16> {
    let frames = data.len() / (FADPCM_FRAME * channels);
    let mut out = vec![0i16; frames * 256 * channels];
    for f in 0..frames {
        for ch in 0..channels {
            let frame = &data[(f * channels + ch) * FADPCM_FRAME..][..FADPCM_FRAME];
            let word = |o: usize| u32::from_le_bytes(frame[o..o + 4].try_into().unwrap());
            let (coefs, shifts) = (word(0), word(4));
            let mut hist1 = i32::from(i16::from_le_bytes([frame[8], frame[9]]));
            let mut hist2 = i32::from(i16::from_le_bytes([frame[10], frame[11]]));
            for sub in 0..8 {
                // Indices past the table wrap around (FMOD takes them modulo 7)
                let [c1, c2] = FADPCM_COEFS[(((coefs >> (sub * 4)) & 0xF) % 7) as usize];
                let shift = 22 - ((shifts >> (sub * 4)) & 0xF);
                for j in 0..4 {
                    let nibbles = word(12 + sub * 16 + j * 4);
                    for k in 0..8 {
                        let n = ((nibbles >> (k * 4)) & 0xF) as i32;
                        let scaled = (n << 28) >> shift;
                        let s = ((scaled - hist2 * c2 + hist1 * c1) >> 6).clamp(-32768, 32767);
                        let i = sub * 32 + j * 8 + k as usize;
                        out[(f * 256 + i) * channels + ch] = s as i16;
                        hist2 = hist1;
                        hist1 = s;
                    }
                }
            }
        }
    }
    out
}

/// Interleaved 16-bit PCM.
pub struct Pcm {
    pub channels: u16,
    pub rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    pub fn write_wav<W: Write>(&self, mut w: W) -> io::Result<()> {
        let data_bytes = u32::try_from(self.samples.len() * 2).map_err(|_| io::Error::other("too long for WAV"))?;
        let align = self.channels * 2;
        w.write_all(b"RIFF")?;
        w.write_all(&(36 + data_bytes).to_le_bytes())?;
        w.write_all(b"WAVEfmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&1u16.to_le_bytes())?; // PCM
        w.write_all(&self.channels.to_le_bytes())?;
        w.write_all(&self.rate.to_le_bytes())?;
        w.write_all(&(self.rate * u32::from(align)).to_le_bytes())?;
        w.write_all(&align.to_le_bytes())?;
        w.write_all(&16u16.to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&data_bytes.to_le_bytes())?;
        let mut bytes = Vec::with_capacity(self.samples.len() * 2);
        for s in &self.samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        w.write_all(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Name, rate index, channel index, frames, (type, body) chunks and data of a test sample.
    type TestSample<'a> = (&'a str, u64, u64, u64, Vec<(u32, Vec<u8>)>, Vec<u8>);

    /// Builds an FSB5 (version 1) from its samples.
    fn fsb5(codec: u32, samples: &[TestSample]) -> Vec<u8> {
        let (mut headers, mut names, mut data) = (Vec::new(), Vec::new(), Vec::new());
        let mut name_strings = Vec::new();
        for (name, rate, channels, frames, chunks, bytes) in samples {
            let offset = data.len() as u64 / 32;
            let mode = u64::from(!chunks.is_empty()) | rate << 1 | channels << 5 | offset << 7 | frames << 34;
            headers.extend_from_slice(&mode.to_le_bytes());
            for (i, (kind, body)) in chunks.iter().enumerate() {
                let more = u32::from(i + 1 < chunks.len());
                headers.extend_from_slice(&(more | (body.len() as u32) << 1 | kind << 25).to_le_bytes());
                headers.extend_from_slice(body);
            }
            data.extend_from_slice(bytes);
            data.resize(data.len().next_multiple_of(32), 0);
            name_strings.push(name.as_bytes().to_vec());
        }
        let mut pos = 4 * samples.len();
        for n in &name_strings {
            names.extend_from_slice(&(pos as u32).to_le_bytes());
            pos += n.len() + 1;
        }
        for n in &name_strings {
            names.extend_from_slice(n);
            names.push(0);
        }
        let mut out = b"FSB5".to_vec();
        for v in [1, samples.len() as u32, headers.len() as u32, names.len() as u32, data.len() as u32, codec, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.resize(0x3C, 0);
        out.extend(headers);
        out.extend(names);
        out.extend(data);
        out
    }

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// Wraps FSB5s in a minimal bank, with or without the SNDH table.
    fn bank(fsbs: &[Vec<u8>], with_sndh: bool) -> Vec<u8> {
        let fmt = chunk(b"FMT ", &[0x8E, 0, 0, 0, 0x8C, 0, 0, 0]);
        let sndh_len = if with_sndh { 8 + 4 + 8 * fsbs.len() } else { 0 };
        let mut pos = 12 + fmt.len() + 12 + sndh_len;
        let mut snds = Vec::new();
        let mut table = 0x0008_0001u32.wrapping_add(2 * fsbs.len() as u32).to_le_bytes().to_vec();
        for f in fsbs {
            let start = (pos + 8).next_multiple_of(32);
            let mut body = vec![0; start - pos - 8];
            body.extend_from_slice(f);
            table.extend_from_slice(&(start as u32).to_le_bytes());
            table.extend_from_slice(&(f.len() as u32).to_le_bytes());
            let c = chunk(b"SND ", &body);
            pos += c.len();
            snds.extend(c);
        }
        let mut proj = b"PROJ".to_vec();
        if with_sndh {
            proj.extend(chunk(b"SNDH", &table));
        }
        let mut body = b"FEV ".to_vec();
        body.extend(fmt);
        body.extend(chunk(b"LIST", &proj));
        body.extend(snds);
        chunk(b"RIFF", &body)
    }

    #[test]
    fn parses_sample_headers_chunks_and_names() {
        let fsb = fsb5(
            2,
            &[
                ("engine_idle", 8, 0, 4, vec![(CHUNK_LOOP, [1u32.to_le_bytes(), 3u32.to_le_bytes()].concat())], vec![1, 0, 2, 0, 3, 0, 4, 0]),
                ("odd_rate", 0, 1, 1, vec![(CHUNK_RATE, 12345u32.to_le_bytes().to_vec()), (CHUNK_CHANNELS, vec![4])], vec![9; 8]),
            ],
        );
        let f = Fsb5::parse(&fsb).unwrap();
        assert_eq!(f.codec, Codec::Pcm16);
        assert_eq!(f.samples.len(), 2);
        let a = &f.samples[0];
        assert_eq!((a.name.as_deref(), a.rate, a.channels, a.frames, a.loop_points), (Some("engine_idle"), 44100, 1, 4, Some((1, 3))));
        assert_eq!(a.data.len(), 32, "data runs to the next sample's offset");
        let b = &f.samples[1];
        assert_eq!((b.name.as_deref(), b.rate, b.channels), (Some("odd_rate"), 12345, 4));
        assert_eq!(f.decode(a).unwrap().samples, [1, 2, 3, 4]);
    }

    #[test]
    fn decodes_pcm_widths_to_16_bit() {
        for (mode, bytes, want) in [
            (1, vec![0x80u8, 0x7F], [-32768, 32512]),
            (3, vec![0, 0, 0x80, 0xFF, 0xFF, 0x7F], [-32768, 32767]),
            (4, vec![0, 0, 0, 0x80, 0, 0, 0xFF, 0x7F], [-32768, 32767]),
        ] {
            let fsb = fsb5(mode, &[("s", 9, 0, 2, vec![], bytes)]);
            let f = Fsb5::parse(&fsb).unwrap();
            assert_eq!(f.decode(&f.samples[0]).unwrap().samples, want, "mode {mode}");
        }
        let float = [-1.0f32, 0.5].iter().flat_map(|v| v.to_le_bytes()).collect();
        let fsb = fsb5(5, &[("s", 9, 0, 2, vec![], float)]);
        let f = Fsb5::parse(&fsb).unwrap();
        assert_eq!(f.decode(&f.samples[0]).unwrap().samples, [-32768, 16384]);
    }

    #[test]
    fn fadpcm_predicts_from_history_and_interleaves_channels() {
        // Channel 0: coefficient set 1 (60/64 of the last sample), shift 0, every nibble 1
        let mut left = vec![0u8; FADPCM_FRAME];
        left[0..4].copy_from_slice(&0x1111_1111u32.to_le_bytes());
        left[8..10].copy_from_slice(&1000i16.to_le_bytes());
        left[12..].fill(0x11);
        // Channel 1: no prediction, shift 22 - 6 = 16, nibble 0xF (-1) scales to -4096 / 64
        let mut right = vec![0u8; FADPCM_FRAME];
        right[4..8].copy_from_slice(&0x6666_6666u32.to_le_bytes());
        right[12..].fill(0xFF);
        let pcm = fadpcm_decode(&[left, right].concat(), 2);
        assert_eq!(pcm.len(), 512);
        // (1 << 28 >> 22) = 64, then (64 + 1000 * 60) >> 6 = 938
        assert_eq!(pcm[0], 938);
        assert_eq!(pcm[2], ((64 + 938 * 60) >> 6) as i16);
        assert!(pcm[1..].iter().step_by(2).all(|&s| s == -64));
    }

    #[test]
    fn finds_fsb5s_with_and_without_sndh() {
        let a = fsb5(2, &[("a", 8, 0, 1, vec![], vec![0, 1])]);
        let b = fsb5(16, &[("b", 8, 0, 1, vec![], vec![0; 0x8C])]);
        for with_sndh in [true, false] {
            let file = bank(&[a.clone(), b.clone()], with_sndh);
            let ranges = fsb5_ranges(&file).unwrap();
            assert_eq!(ranges.len(), 2);
            assert!(ranges.iter().all(|r| r.start % 32 == 0));
            assert_eq!(&file[ranges[0].start..][..a.len()], &a[..]);
            assert_eq!(Fsb5::parse(&file[ranges[1].clone()]).unwrap().codec, Codec::Fadpcm);
        }
        assert!(fsb5_ranges(&bank(&[], true)).unwrap().is_empty());
    }

    #[test]
    fn rejects_truncated_banks() {
        let fsb = fsb5(2, &[("a", 8, 0, 4, vec![], vec![0; 8])]);
        assert!(Fsb5::parse(&fsb[..fsb.len() - 1]).is_err());
        assert!(Fsb5::parse(b"FSB4").is_err());
        let file = bank(&[fsb], true);
        assert!(fsb5_ranges(&file[..file.len() - 40]).is_err());
    }

    #[test]
    fn wav_header_matches_the_data() {
        let mut out = Vec::new();
        Pcm { channels: 2, rate: 48000, samples: vec![1, -1, 2, -2] }.write_wav(&mut out).unwrap();
        assert_eq!(out.len(), 44 + 8);
        assert_eq!(&out[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 44);
        assert_eq!(u32::from_le_bytes(out[28..32].try_into().unwrap()), 48000 * 4);
        assert_eq!(&out[44..46], &1i16.to_le_bytes());
    }
}
