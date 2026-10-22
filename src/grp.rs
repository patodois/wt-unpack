//! Reader for `.grp` game resource packs.
//!
//! Layout (see `gamerespackbin` in Dagor's `prog/engine/gameRes/grpData.h`):
//!
//! ```text
//! u32 label            "GRP2" or "GRP3"
//! u32 desc_only_size
//! u32 full_data_size   size of the table dump that follows the header
//! u32 rest_file_size   file size minus this 16-byte header
//! dump (offsets are relative to the start of the file):
//!   +0x10 name table:     u32 offset, u32 count, 8 bytes padding -> i32 name offsets (-1 = none)
//!   +0x20 resource table: u32 offset, u32 count, padding -> {u32 class_id, u32 file_offset, u16 name_id, u16}
//!   +0x30 resource data:  u32 offset, u32 count, padding (dependency lists, unused here)
//! ```
//!
//! A resource's bytes run from its offset to the next resource's offset (or the end of the file).

use anyhow::{ensure, Context, Result};

use crate::bytes::Reader;

pub const DYN_MODEL: u32 = 0xB4B7_D9C4;
pub const GEOM_NODE_TREE: u32 = 0x56F8_1B6D;

pub struct GrpEntry {
    pub name: String,
    pub class_id: u32,
    pub offset: usize,
    pub size: usize,
}

pub fn parse(file: &[u8]) -> Result<Vec<GrpEntry>> {
    let mut r = Reader::new(file);
    let label = r.bytes(4)?;
    ensure!(label == b"GRP2" || label == b"GRP3", "not a GRP file");
    r.skip(4)?;
    let full = r.u32()? as usize;
    ensure!(file.len() >= 16 + full, "GRP table dump is truncated");

    let tab = |at: usize| -> Result<(usize, usize)> {
        let mut t = Reader::at(file, at)?;
        Ok((t.u32()? as usize, t.u32()? as usize))
    };
    let (names_ofs, names_count) = tab(0x10)?;
    let (res_ofs, res_count) = tab(0x20)?;

    let mut names = Vec::with_capacity(names_count);
    let mut nr = Reader::at(file, names_ofs)?;
    for _ in 0..names_count {
        let ofs = nr.u32()? as i32;
        names.push(if ofs < 0 { String::new() } else { Reader::at(file, ofs as usize)?.cstr()? });
    }

    let mut raw = Vec::with_capacity(res_count);
    let mut rr = Reader::at(file, res_ofs)?;
    for _ in 0..res_count {
        let class_id = rr.u32()?;
        let offset = rr.u32()? as usize;
        let name_id = rr.u16()? as usize;
        rr.skip(2)?;
        let name = names.get(name_id).cloned().context("resource name id out of range")?;
        raw.push((name, class_id, offset));
    }
    let mut entries = Vec::with_capacity(raw.len());
    for (i, (name, class_id, offset)) in raw.iter().enumerate() {
        let end = raw.get(i + 1).map_or(file.len(), |next| next.2);
        ensure!(*offset <= end && end <= file.len(), "resource {name} has a bad offset");
        entries.push(GrpEntry { name: name.clone(), class_id: *class_id, offset: *offset, size: end - offset });
    }
    Ok(entries)
}
