//! Reader for Dagor's read-only data block dumps (`RoDataBlock`), the form in which game resources
//! such as land classes keep their BLK.
//!
//! A dump is patchable (see `level.rs` for tables and pointers). From its start:
//!
//! ```text
//! block (48 bytes): table params, table blocks, ptr name map, i32 name id (-1 for the root), i32
//! param (8 bytes):  u32 value, u16 name id, u16 type
//! name map:         table of ptrs to the names, sorted; a name id is an index into that table
//! ```
//!
//! A parameter's value is inline for bool, int, real, colour; for strings, vectors, matrices and
//! 64-bit integers it is an offset from the name map to the data. Types are numbered as in
//! `RoDataBlock::ParamType`: 1 string, 2 int, 3 real, 4 point2, 5 point3, 6 point4, 7 ipoint2,
//! 8 ipoint3, 9 bool, 10 colour, 11 matrix, 12 int64, 13 ipoint4.

use anyhow::{bail, ensure, Context, Result};

use crate::blk::{Block, Value};
use crate::bytes::Reader;
use crate::level::{pptr, ptab};

const BLOCK_SIZE: usize = 48;
const PARAM_SIZE: usize = 8;
/// Deepest nesting accepted (real land classes go three or four deep)
const MAX_DEPTH: usize = 32;

/// What the blocks and parameters read so far may add up to. A dump stores every block and parameter once
/// and each takes room in it, so a tree can hold no more of either than the dump has room for; a pointer
/// that leads back to a block already read would otherwise multiply (two children per block, eighteen
/// levels, 1.7 KB of dump make 262,143 blocks).
struct Budget {
    blocks: usize,
    params: usize,
}

/// Parses a dump that starts with its root block
pub fn parse(dump: &[u8]) -> Result<Block> {
    let names = {
        let mut r = Reader::at(dump, 32)?;
        let map = pptr(&mut r)?.context("data block has no name map")?;
        let (ofs, count) = ptab(&mut Reader::at(dump, map)?)?;
        ensure!(count <= dump.len() / 8, "name map claims {count} names");
        let mut ptrs = Reader::at(dump, ofs)?;
        let names = (0..count)
            .map(|_| {
                let p = pptr(&mut ptrs)?.context("null name")?;
                Reader::at(dump, p)?.cstr()
            })
            .collect::<Result<Vec<_>>>()?;
        (map, names)
    };
    let mut budget = Budget { blocks: dump.len() / BLOCK_SIZE, params: dump.len() / PARAM_SIZE };
    read_block(dump, 0, &names.1, names.0, 0, &mut budget)
}

fn read_block(dump: &[u8], at: usize, names: &[String], map: usize, depth: usize, budget: &mut Budget) -> Result<Block> {
    ensure!(depth < MAX_DEPTH, "data blocks nest too deeply");
    budget.blocks = budget.blocks.checked_sub(1).context("the dump's blocks point at more blocks than it stores")?;
    let mut r = Reader::at(dump, at)?;
    let (params_ofs, params_count) = ptab(&mut r)?;
    let (blocks_ofs, blocks_count) = ptab(&mut r)?;
    r.skip(8)?;
    let name_id = r.i32()?;
    let name = usize::try_from(name_id).ok().and_then(|i| names.get(i)).cloned().unwrap_or_default();
    ensure!(params_count <= dump.len() / PARAM_SIZE && blocks_count <= dump.len() / BLOCK_SIZE, "block claims too many entries");
    budget.params = budget.params.checked_sub(params_count).context("the dump's blocks list more parameters than it stores")?;

    let name_of = |id: u16| names.get(usize::from(id)).cloned().with_context(|| format!("name id {id} out of range"));
    let mut params = Vec::with_capacity(params_count);
    for i in 0..params_count {
        let mut p = Reader::at(dump, params_ofs + i * PARAM_SIZE)?;
        let raw = p.u32()?;
        let (id, ty) = (p.u16()?, p.u16()?);
        let at = || map + raw as usize;
        let reals = |n: usize| -> Result<Vec<f32>> {
            let mut d = Reader::at(dump, at())?;
            (0..n).map(|_| d.f32()).collect()
        };
        let ints = |n: usize| -> Result<Vec<i32>> {
            let mut d = Reader::at(dump, at())?;
            (0..n).map(|_| d.i32()).collect()
        };
        let value = match ty {
            1 => Value::Str(Reader::at(dump, at())?.cstr()?),
            2 => Value::Int(raw as i32),
            3 => Value::Real(f32::from_bits(raw)),
            4 => Value::Point2(reals(2)?.try_into().unwrap()),
            5 => Value::Point3(reals(3)?.try_into().unwrap()),
            6 => Value::Point4(reals(4)?.try_into().unwrap()),
            7 => Value::IPoint2(ints(2)?.try_into().unwrap()),
            8 => Value::IPoint3(ints(3)?.try_into().unwrap()),
            9 => Value::Bool(raw & 0xFF != 0),
            // Stored as B, G, R, A bytes
            10 => Value::Color([(raw >> 16) as u8, (raw >> 8) as u8, raw as u8, (raw >> 24) as u8]),
            11 => {
                let m = reals(12)?;
                Value::Matrix([0, 1, 2, 3].map(|row| [m[row * 3], m[row * 3 + 1], m[row * 3 + 2]]))
            }
            12 => {
                let v = ints(2)?;
                Value::Int64(i64::from(v[0] as u32) | i64::from(v[1]) << 32)
            }
            13 => Value::IPoint4(ints(4)?.try_into().unwrap()),
            other => bail!("data block parameter of unknown type {other}"),
        };
        params.push((name_of(id)?, value));
    }
    let blocks = (0..blocks_count)
        .map(|i| read_block(dump, blocks_ofs + i * BLOCK_SIZE, names, map, depth + 1, budget))
        .collect::<Result<Vec<_>>>()?;
    Ok(Block { name, params, blocks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// root { n:i=7  s:t="hi"  child{ } }, laid out like the game's builder writes it
    #[test]
    fn reads_a_small_dump() {
        // names sorted: child, n, s
        let mut d = Vec::new();
        // root block at 0: params at 0x68, blocks at 0x58
        put(&mut d, 0x68);
        put(&mut d, 2);
        d.extend_from_slice(&[0; 8]);
        put(&mut d, 0x58);
        put(&mut d, 1);
        d.extend_from_slice(&[0; 8]);
        put(&mut d, 0x30); // name map
        put(&mut d, 0);
        put(&mut d, u32::MAX); // name id -1
        put(&mut d, 0);
        assert_eq!(d.len(), 0x30);
        // name map at 0x30: table of 3 name pointers at 0x40
        put(&mut d, 0x40);
        put(&mut d, 3);
        d.extend_from_slice(&[0; 8]);
        assert_eq!(d.len(), 0x40);
        // pointers to the names, stored after the table (3 x 8 bytes) at 0x58? keep them at 0x70..
        // layout: 0x40 table (24 bytes) ends at 0x58, where the child block starts
        for ofs in [0x98u32, 0x9e, 0xa0] {
            put(&mut d, ofs);
            put(&mut d, 0);
        }
        assert_eq!(d.len(), 0x58);
        // child block at 0x58: no params, no blocks, name id 0
        put(&mut d, 0x68);
        put(&mut d, 0);
        d.extend_from_slice(&[0; 8]);
        put(&mut d, 0);
        put(&mut d, 0);
        d.extend_from_slice(&[0; 8]);
        put(&mut d, 0x30);
        put(&mut d, 0);
        put(&mut d, 0);
        put(&mut d, 0);
        // the child block is 48 bytes: 0x58..0x88; params of the root at 0x88
        assert_eq!(d.len(), 0x88);
        // fix root's params offset to 0x88
        d[0..4].copy_from_slice(&0x88u32.to_le_bytes());
        // params: n:i=7 (name 1), s:t (name 2) pointing at the string, which sits at map + value
        put(&mut d, 7);
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        put(&mut d, 0xa2 - 0x30);
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        assert_eq!(d.len(), 0x98);
        d.extend_from_slice(b"child\0n\0s\0hi\0");
        let b = parse(&d).unwrap();
        assert_eq!(b.params, vec![("n".to_string(), Value::Int(7)), ("s".to_string(), Value::Str("hi".into()))]);
        assert_eq!(b.blocks.len(), 1);
        assert_eq!(b.blocks[0].name, "child");
    }

    /// `levels` pairs of blocks where both blocks of a level list the same two blocks of the next one as
    /// children: 1,744 bytes at 18 levels, and a reader that follows every pointer builds 2^18 blocks
    fn shared_children(levels: usize) -> Vec<u8> {
        let map = levels * 96;
        let mut d = Vec::new();
        for l in 0..levels {
            for _ in 0..2 {
                let (ofs, count) = if l + 1 < levels { (((l + 1) * 96) as u32, 2u32) } else { (0, 0) };
                put(&mut d, 0);
                put(&mut d, 0);
                d.extend_from_slice(&[0; 8]);
                put(&mut d, ofs);
                put(&mut d, count);
                d.extend_from_slice(&[0; 8]);
                d.extend_from_slice(&(map as u64).to_le_bytes());
                d.extend_from_slice(&(-1i32).to_le_bytes());
                put(&mut d, 0);
            }
        }
        // the name map: an empty table
        put(&mut d, 0);
        put(&mut d, 0);
        d.extend_from_slice(&[0; 8]);
        d
    }

    #[test]
    fn children_shared_between_blocks_cannot_blow_up_the_tree() {
        fn count(b: &Block) -> usize {
            1 + b.blocks.iter().map(count).sum::<usize>()
        }
        // Two levels read: three blocks out of the four the dump stores
        let d = shared_children(2);
        assert_eq!(count(&parse(&d).unwrap()), 3);
        let d = shared_children(18);
        assert!(d.len() < 2000);
        let err = parse(&d).unwrap_err().to_string();
        assert!(err.contains("more blocks"), "{err}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0; 8]).is_err());
        assert!(parse(&[0xFF; 100]).is_err());
    }
}
