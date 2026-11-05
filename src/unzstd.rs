//! zstd decoding for the archives, BLKs and level blocks, with [ruzstd](https://crates.io/crates/ruzstd)
//! (pure Rust).
//!
//! Frames follow one after another until the input is used up, skippable frames are passed over, and a
//! dictionary can be given for the frames that were packed with the archive's. Output grows as data comes
//! out and stops at `limit`: a header's claimed size is never allocated before the stream has produced it.

use std::panic::{self, AssertUnwindSafe};

use anyhow::{anyhow, bail, ensure, Result};
use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};

/// Unpacks every zstd frame of `packed`; `dict` is the dictionary of the frames that were packed with one (a
/// trained dictionary, which starts with zstd's magic number, as the archives' are; raw-content ones are refused).
/// Fails when the data unpacks to more than `limit` bytes.
pub fn decode(packed: &[u8], dict: Option<&[u8]>, limit: usize) -> Result<Vec<u8>> {
    // A decoder must not take a hostile stream down with it; report a panic as an error
    panic::catch_unwind(AssertUnwindSafe(|| decode_frames(packed, dict, limit)))
        .map_err(|_| anyhow!("zstd decoder panicked"))?
}

fn decode_frames(mut input: &[u8], dict: Option<&[u8]>, limit: usize) -> Result<Vec<u8>> {
    let mut decoder = FrameDecoder::new();
    let mut dict_id = None;
    if let Some(raw) = dict {
        ensure!(raw.len() >= 8, "zstd dictionary is too short");
        let dictionary =
            Dictionary::decode_dict(raw).map_err(|e| anyhow!("zstd dictionary: {e}"))?;
        dict_id = Some(dictionary.id);
        decoder
            .add_dict(dictionary)
            .map_err(|e| anyhow!("zstd dictionary: {e}"))?;
    }
    let mut out = Vec::new();
    while !input.is_empty() {
        // Inspect each header before init/reset can reserve its window.
        if let Some(window) = zstd_window(input) {
            let window_limit = limit.saturating_sub(out.len()).max(128 << 10);
            ensure!(window <= window_limit as u64, "zstd window {window} exceeds the {window_limit}-byte budget");
        }
        match decoder.init(&mut input) {
            Ok(()) => {}
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                input = input
                    .get(length as usize..)
                    .ok_or_else(|| anyhow!("zstd: a skippable frame runs past the data"))?;
                continue;
            }
            Err(e) => bail!("zstd: {e}"),
        }
        // Room for what the frame says it holds, as far as the packed size makes that believable: output that
        // grows by doubling would leave a good part of a large buffer unused
        let claimed = usize::try_from(decoder.content_size()).unwrap_or(usize::MAX);
        out.reserve_exact(
            claimed
                .min(limit.saturating_sub(out.len()))
                .min(input.len().saturating_mul(64)),
        );
        // A frame that does not name its dictionary still uses the one the archive gives
        if let Some(id) = dict_id {
            decoder
                .force_dict(id)
                .map_err(|e| anyhow!("zstd dictionary: {e}"))?;
        }
        loop {
            decoder
                .decode_blocks(&mut input, BlockDecodingStrategy::UptoBlocks(1))
                .map_err(|e| anyhow!("zstd: {e}"))?;
            ensure!(decoder.can_collect() <= limit.saturating_sub(out.len()), "zstd data unpacks to more than {limit} bytes");
            // Drain what can be collected: the window stays in the decoder until more comes out
            if let Some(bytes) = decoder.collect() {
                ensure!(
                    out.len() + bytes.len() <= limit,
                    "zstd data unpacks to more than {limit} bytes"
                );
                out.extend_from_slice(&bytes);
            }
            if decoder.is_finished() && decoder.can_collect() == 0 {
                break;
            }
        }
    }
    Ok(out)
}

// The frame header layout is also used by this project's wt-formats decoder.
fn zstd_window(packed: &[u8]) -> Option<u64> {
    if packed.get(..4)? != [0x28, 0xB5, 0x2F, 0xFD] {
        return None;
    }
    let descriptor = *packed.get(4)?;
    let single_segment = descriptor & 0x20 != 0;
    if !single_segment {
        // A window descriptor: a power of two from 1 KiB and an eighth of it times 0 to 7 more
        let w = *packed.get(5)?;
        let base = 1u64 << (10 + u32::from(w >> 3));
        return Some(base + base / 8 * u64::from(w & 7));
    }
    // A single segment: the window is the content size, which follows the (absent) window descriptor and the
    // dictionary id
    let dictionary = [0usize, 1, 2, 4][usize::from(descriptor & 3)];
    let at = 5 + dictionary;
    let size = match descriptor >> 6 {
        0 => u64::from(*packed.get(at)?),
        1 => u64::from(u16::from_le_bytes(packed.get(at..at + 2)?.try_into().ok()?)) + 256,
        2 => u64::from(u32::from_le_bytes(packed.get(at..at + 4)?.try_into().ok()?)),
        _ => u64::from_le_bytes(packed.get(at..at + 8)?.try_into().ok()?),
    };
    Some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_concatenated_and_limited() {
        let a = zstd::bulk::compress(&[1u8; 5000], 3).unwrap();
        let b = zstd::bulk::compress(b"tail", 3).unwrap();
        let both = [a.clone(), b].concat();
        let out = decode(&both, None, 6000).unwrap();
        assert_eq!(&out[..5000], &[1u8; 5000][..]);
        assert_eq!(&out[5000..], b"tail");
        assert!(decode(&both, None, 5003).is_err());
        assert!(decode(&a, None, 4999).is_err());
        assert_eq!(decode(&[], None, 0).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn broken_input_is_an_error() {
        let packed = zstd::bulk::compress(&[3u8; 100_000], 3).unwrap();
        assert!(decode(&packed[..packed.len() / 2], None, 1 << 20).is_err());
        assert!(decode(b"not zstd at all", None, 1 << 20).is_err());
        assert!(decode(&[0x28, 0xB5, 0x2F, 0xFD], None, 1 << 20).is_err());
        assert!(decode(&packed, Some(&[1, 2, 3]), 1 << 20).is_err());
    }

    #[test]
    fn a_dictionary_frame_unpacks_with_its_dictionary() {
        let samples: Vec<Vec<u8>> = (0..400)
            .map(|i| {
                format!(
                    "{{\"name\":\"item{i}\",\"kind\":\"sample\",\"values\":[{i},{},{}]}}",
                    i * 3,
                    i * 7
                )
                .into_bytes()
            })
            .collect();
        let dict = zstd::dict::from_samples(&samples, 4096).unwrap();
        let data = br#"{"name":"item9","kind":"sample","values":[9,27,63]}"#.repeat(20);
        let packed = zstd::bulk::Compressor::with_dictionary(3, &dict)
            .unwrap()
            .compress(&data)
            .unwrap();
        assert_eq!(decode(&packed, Some(&dict), 1 << 20).unwrap(), data);
        assert!(decode(&packed, None, 1 << 20).is_err());
    }
}

#[cfg(test)]
mod window_budget_regression {
    use super::*;
    #[test]
    fn oversized_empty_windows_are_rejected_before_allocation() {
        let frame = [0x28, 0xb5, 0x2f, 0xfd, 0, 0x70, 1, 0, 0];
        let small = zstd::bulk::compress(b"", 1).unwrap();
        let packed = [small, frame.to_vec()].concat();
        let (result, largest) = crate::alloc_probe::largest_allocation(|| decode(&packed, None, 0));
        assert!(
            largest < 1 << 20,
            "reserved {largest} bytes before checking the window"
        );
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod output_window_regression {
    use super::*;
    #[test]
    fn small_output_budget_does_not_fill_a_large_window() {
        use std::io::Write;
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
        encoder.window_log(20).unwrap();
        encoder.include_contentsize(false).unwrap();
        for _ in 0..1024 { encoder.write_all(&[0;1024]).unwrap(); }
        let packed=encoder.finish().unwrap();
        let (result, largest)=crate::alloc_probe::largest_allocation(||decode(&packed,None,32));
        assert!(result.is_err());
        assert!(largest<1<<18,"reserved {largest} bytes for a 32-byte output limit");
    }
}
