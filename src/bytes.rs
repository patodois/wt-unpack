//! Minimal little-endian cursor over a byte slice.

use anyhow::{bail, Context, Result};

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Result<Self> {
        if pos > buf.len() {
            bail!("offset {pos} is past the end of a {}-byte buffer", buf.len());
        }
        Ok(Reader { buf, pos })
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).context("length overflow")?;
        let out = self
            .buf
            .get(self.pos..end)
            .with_context(|| format!("unexpected end of data reading {n} bytes at {}", self.pos))?;
        self.pos = end;
        Ok(out)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let out = &self.buf[self.pos..];
        self.pos = self.buf.len();
        out
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    /// LEB128-style unsigned varint, as written by Dagor's `writeCompressedUnsigned`.
    pub fn varint(&mut self) -> Result<u32> {
        let mut v: u64 = 0;
        for i in 0..5 {
            let byte = self.u8()?;
            v |= u64::from(byte & 0x7F) << (i * 7);
            if byte & 0x80 == 0 {
                break;
            }
        }
        u32::try_from(v).context("varint does not fit in 32 bits")
    }

    /// NUL-terminated string.
    pub fn cstr(&mut self) -> Result<String> {
        let rest = &self.buf[self.pos..];
        let len = rest.iter().position(|&b| b == 0).context("unterminated string")?;
        let s = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += len + 1;
        Ok(s)
    }
}
