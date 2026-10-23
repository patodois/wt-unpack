//! Reader for `.vromfs.bin` archives.
//!
//! Container layout (all little-endian):
//!
//! ```text
//! u32 label        "VRFs" or "VRFx"
//! u32 target       platform tag, e.g. "\0\0PC"
//! u32 full_size    size of the unpacked body
//! u32 hw32         bits 0..30 packed size (0 = stored), bit 30 zstd, bit 31 signed
//! [VRFx only] u16 ext_size, u16 flags, u32 version, ...   (ext_size bytes total)
//! body             packed_size (or full_size when stored) bytes
//! [16] md5         digest of the unpacked body
//! [..] signature   present when bit 31 of hw32 is set
//! ```
//!
//! Zstd bodies are lightly obfuscated: the first and last 16 bytes are XORed
//! with a fixed key. The unpacked body is a table of file names followed by a
//! table of (offset, size) pairs pointing at the file data.

use anyhow::{bail, ensure, Context, Result};
use md5::{Digest, Md5};

use crate::bytes::Reader;

const LABEL_VRFS: u32 = u32::from_le_bytes(*b"VRFs");
const LABEL_VRFX: u32 = u32::from_le_bytes(*b"VRFx");

const XOR_KEY: [u32; 4] = [0xAA55_AA55, 0xF00F_F00F, 0xAA55_AA55, 0x1248_1248];

/// Name of the shared name map that slim binary BLK files depend on.
/// On disk it is `\xff?nm`; the invalid UTF-8 byte reads back as U+FFFD.
pub const SHARED_NAMEMAP_NAME: &str = "\u{fffd}?nm";

pub struct Vromfs {
    pub target: String,
    /// Game version from the VRFx header, e.g. `2.59.0.80`.
    pub version: Option<String>,
    pub md5_ok: bool,
    pub files: Vec<VromFile>,
}

pub struct VromFile {
    pub name: String,
    pub data: Vec<u8>,
}

impl Vromfs {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut r = Reader::new(raw);
        let label = r.u32()?;
        ensure!(label == LABEL_VRFS || label == LABEL_VRFX, "not a vromfs file (bad label)");
        let target_raw = r.bytes(4)?;
        let full_size = r.u32()? as usize;
        let hw32 = r.u32()?;
        let packed_size = (hw32 & 0x3FFF_FFFF) as usize;
        let zstd_packed = hw32 & 0x4000_0000 != 0;

        let mut version = None;
        if label == LABEL_VRFX {
            let ext_size = r.u16()? as usize;
            let _flags = r.u16()?;
            ensure!(ext_size >= 8, "VRFx extension header too small");
            let v = r.u32()?;
            version = Some(format!("{}.{}.{}.{}", v >> 24, (v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF));
            r.skip(ext_size - 8)?;
        }

        let body_size = if packed_size > 0 { packed_size } else { full_size };
        let body = r.bytes(body_size)?;
        let body = if packed_size == 0 {
            body.to_vec()
        } else if zstd_packed {
            let mut packed = body.to_vec();
            deobfuscate(&mut packed);
            zstd::bulk::decompress(&packed, full_size).context("zstd decompression of vromfs body failed")?
        } else {
            bail!("zlib-packed vromfs bodies are not supported yet");
        };
        ensure!(body.len() == full_size, "unpacked size {} != header size {}", body.len(), full_size);

        let md5_ok = match r.bytes(16) {
            Ok(expected) => Md5::digest(&body).as_slice() == expected,
            Err(_) => false,
        };

        Ok(Vromfs {
            target: String::from_utf8_lossy(&target_raw).trim_matches('\0').to_string(),
            version,
            md5_ok,
            files: read_file_table(&body)?,
        })
    }

    pub fn get(&self, name: &str) -> Option<&VromFile> {
        self.files.iter().find(|f| f.name == name)
    }
}

/// Loads the shared name map and zstd dictionary that slim BLK files in an archive depend on
pub fn shared_context(vfs: &Vromfs) -> Result<Option<crate::blk::SharedContext>> {
    let Some(nm) = vfs.get(SHARED_NAMEMAP_NAME) else { return Ok(None) };
    let (mut ctx, dict_name) = crate::blk::SharedContext::from_namemap(&nm.data)?;
    ctx.dict = vfs.get(&dict_name).map(|f| f.data.clone());
    Ok(Some(ctx))
}

fn deobfuscate(data: &mut [u8]) {
    let len = data.len();
    if len < 16 {
        return;
    }
    xor_words(&mut data[..16], &XOR_KEY);
    if len >= 32 {
        // The tail block is aligned to whole 32-bit words.
        let end = (len / 4) * 4;
        let mut rev = XOR_KEY;
        rev.reverse();
        xor_words(&mut data[end - 16..end], &rev);
    }
}

fn xor_words(block: &mut [u8], key: &[u32; 4]) {
    for (chunk, k) in block.chunks_exact_mut(4).zip(key) {
        let w = u32::from_le_bytes(chunk.try_into().unwrap()) ^ k;
        chunk.copy_from_slice(&w.to_le_bytes());
    }
}

/// Each table is a 16-byte "patchable tab": u32 offset into the body, u32 count, 8 bytes padding.
fn read_tab(r: &mut Reader) -> Result<(usize, usize)> {
    let ofs = r.u32()? as usize;
    let count = r.u32()? as usize;
    r.skip(8)?;
    Ok((ofs, count))
}

fn read_file_table(body: &[u8]) -> Result<Vec<VromFile>> {
    let mut r = Reader::new(body);
    let (names_ofs, names_count) = read_tab(&mut r)?;
    let (data_ofs, data_count) = read_tab(&mut r)?;
    ensure!(names_count == data_count, "name/data table size mismatch");

    let mut names = Reader::at(body, names_ofs)?;
    let mut entries = Reader::at(body, data_ofs)?;
    let mut files = Vec::with_capacity(names_count);
    for _ in 0..names_count {
        let name_ofs = names.u64()? as usize;
        let name = Reader::at(body, name_ofs)?.cstr()?;
        let (ofs, size) = read_tab(&mut entries)?;
        let data = body
            .get(ofs..ofs + size)
            .with_context(|| format!("file {name:?} points outside the archive"))?
            .to_vec();
        files.push(VromFile { name, data });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deobfuscation_restores_zstd_magic() {
        // First word of an obfuscated zstd frame as found in game files.
        let mut data = vec![0x7d, 0x1f, 0x7a, 0x57];
        data.resize(16, 0);
        deobfuscate(&mut data);
        assert_eq!(&data[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
    }

    #[test]
    fn deobfuscation_is_an_involution() {
        let original: Vec<u8> = (0..=70u8).collect();
        let mut data = original.clone();
        deobfuscate(&mut data);
        assert_ne!(data, original);
        deobfuscate(&mut data);
        assert_eq!(data, original);
    }
}
