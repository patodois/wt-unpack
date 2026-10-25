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
                .decode_blocks(&mut input, BlockDecodingStrategy::UptoBytes(1 << 20))
                .map_err(|e| anyhow!("zstd: {e}"))?;
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
