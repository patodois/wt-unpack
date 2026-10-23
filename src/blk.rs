//! Reader for Dagor binary BLK files.
//!
//! A binary BLK starts with a format byte:
//!
//! | byte | meaning                                                     |
//! |------|-------------------------------------------------------------|
//! | 1    | full BLK with its own name map                              |
//! | 2    | u24 packed size, then zstd-compressed full BLK              |
//! | 3    | BLK using the archive's shared name map                     |
//! | 4    | zstd-compressed BLK using the shared name map               |
//! | 5    | zstd-compressed (with dictionary) BLK using shared name map |
//! | 6    | like 1, but blocks carry explicit param offsets (dedup)     |
//!
//! Files that do not start with one of these bytes are plain-text BLK.
//!
//! The dump body is: name map, block/param counts, a "complex data" area
//! (strings and values wider than 4 bytes), a table of 8-byte params and
//! finally varint-packed block descriptors in breadth-first order.

use std::fmt::Write as _;
use std::io::Read as _;

use anyhow::{bail, ensure, Context, Result};
use serde_json::{Map, Value as Json};

use crate::bytes::Reader;

const IS_NAMEMAP_ID: u32 = 0x8000_0000;

/// Deepest block nesting `parse` accepts. The deepest of the game's 31,651 binary BLKs nests 23 levels
/// (`gamedata/ai/inf/ai_common_attack.blk`), so this leaves plenty. Every `Block` this crate builds is
/// at most this deep, which is what keeps `Drop`, `to_text` and `to_json` (all recursive) off the stack limit.
pub const MAX_DEPTH: usize = 128;

/// Parameters the blocks of a dump may hold in total, counting the copies that format 6 makes when blocks
/// share a range, unless 16 times the stored parameters is more. Every real file stores each parameter once.
const MAX_COPIED_PARAMS: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i32),
    Real(f32),
    Point2([f32; 2]),
    Point3([f32; 3]),
    Point4([f32; 4]),
    IPoint2([i32; 2]),
    IPoint3([i32; 3]),
    IPoint4([i32; 4]),
    Bool(bool),
    /// Stored as BGRA bytes; kept here as `[r, g, b, a]`.
    Color([u8; 4]),
    /// Four rows of three floats (`TMatrix`).
    Matrix([[f32; 3]; 4]),
    Int64(i64),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Block {
    pub name: String,
    pub params: Vec<(String, Value)>,
    pub blocks: Vec<Block>,
}

/// What kind of content a BLK file holds, judged from its first byte.
pub fn is_binary(data: &[u8]) -> bool {
    matches!(data.first(), Some(1..=6))
}

/// Shared names and zstd dictionary needed by slim BLK files (formats 3-5).
#[derive(Default)]
pub struct SharedContext {
    pub names: Vec<String>,
    pub dict: Option<Vec<u8>>,
}

impl SharedContext {
    /// Parses the `\xff?nm` file: u64 names hash, 32-byte dictionary hash,
    /// then a zstd stream holding a name map.
    pub fn from_namemap(nm: &[u8]) -> Result<(Self, String)> {
        ensure!(nm.len() >= 40, "shared name map is too short");
        let dict_name = format!("{}.dict", hex(&nm[8..40]));
        let raw = zstd::stream::decode_all(&nm[40..]).context("shared name map is not valid zstd")?;
        let names = read_names(&mut Reader::new(&raw))?;
        Ok((SharedContext { names, dict: None }, dict_name))
    }
}

pub fn parse(data: &[u8], shared: Option<&SharedContext>) -> Result<Block> {
    let mut r = Reader::new(data);
    let format = r.u8()?;
    let need_shared = || shared.context("this BLK uses a shared name map; read it from inside its vromfs");
    match format {
        1 => parse_dump(&mut r, &[], false),
        6 => parse_dump(&mut r, &[], true),
        2 => {
            let size = r.bytes(3)?;
            let size = usize::from(size[0]) | usize::from(size[1]) << 8 | usize::from(size[2]) << 16;
            let raw = zstd::stream::decode_all(r.bytes(size)?)?;
            // The unpacked stream repeats the format byte of a plain full BLK
            ensure!(raw.first() == Some(&1), "packed BLK does not unpack to format 1");
            parse_dump(&mut Reader::new(&raw[1..]), &[], false)
        }
        3 => parse_dump(&mut r, &need_shared()?.names, false),
        4 => {
            let raw = zstd::stream::decode_all(r.rest())?;
            parse_dump(&mut Reader::new(&raw), &need_shared()?.names, false)
        }
        5 => {
            let ctx = need_shared()?;
            let dict = ctx.dict.as_deref().context("this BLK needs the archive's zstd dictionary")?;
            // Stream it: the unpacked size is not stored and the ratio has no upper bound
            let mut raw = Vec::new();
            zstd::stream::read::Decoder::with_dictionary(r.rest(), dict)?.read_to_end(&mut raw)?;
            parse_dump(&mut Reader::new(&raw), &ctx.names, false)
        }
        other => bail!("not a binary BLK (format byte {other:#04x})"),
    }
}

fn read_names(r: &mut Reader) -> Result<Vec<String>> {
    let count = r.varint()? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let size = r.varint()? as usize;
    let mut area = Reader::new(r.bytes(size)?);
    (0..count).map(|_| area.cstr()).collect()
}

fn type_size(ty: u8) -> Option<usize> {
    Some(match ty {
        1 | 2 | 3 | 10 => 4,
        9 => 1,
        4 | 7 | 12 => 8,
        5 | 8 => 12,
        6 | 13 => 16,
        11 => 48,
        _ => return None,
    })
}

struct BlockInfo {
    name_id: u32,
    params: Vec<(String, Value)>,
    blocks_count: usize,
    first_block: usize,
}

fn parse_dump(r: &mut Reader, shared_names: &[String], dedup: bool) -> Result<Block> {
    let own_names = read_names(r)?;
    let name = |id: u32| -> Result<&str> {
        let id = id as usize;
        shared_names
            .get(id)
            .or_else(|| own_names.get(id - shared_names.len().min(id)))
            .map(String::as_str)
            .with_context(|| format!("name id {id} out of range"))
    };

    let blocks_count = r.varint()? as usize;
    let params_count = r.varint()? as usize;
    let complex_size = r.varint()? as usize;
    ensure!(blocks_count > 0, "BLK has no root block");
    let complex = r.bytes(complex_size)?;
    let param_table = r.bytes(params_count.checked_mul(8).context("too many params")?)?;
    // A block descriptor is at least three bytes (name, param count, block count), so a count the rest of
    // the file cannot hold is refused before anything is sized by it
    ensure!(
        blocks_count <= r.remaining() / 3,
        "BLK claims {blocks_count} blocks but only {} bytes follow its parameters",
        r.remaining()
    );

    let mut params = Vec::with_capacity(params_count);
    for raw in param_table.chunks_exact(8) {
        let head = u32::from_le_bytes(raw[..4].try_into().unwrap());
        let v = u32::from_le_bytes(raw[4..].try_into().unwrap());
        let (name_id, ty) = (head & 0x00FF_FFFF, (head >> 24) as u8);
        params.push((name(name_id)?.to_string(), decode_value(ty, v, complex, shared_names, &own_names)?));
    }

    let mut infos = Vec::with_capacity(blocks_count);
    let mut next_param = 0usize;
    let copy_budget = MAX_COPIED_PARAMS.max(params_count.saturating_mul(16));
    let mut copied = 0usize;
    for _ in 0..blocks_count {
        let name_id = r.varint()?;
        let pc = r.varint()? as usize;
        let bc = r.varint()? as usize;
        let first_block = if bc > 0 { r.varint()? as usize } else { 0 };
        let start = if dedup {
            if pc > 0 { r.varint()? as usize } else { 0 }
        } else {
            let s = next_param;
            next_param = next_param.saturating_add(pc);
            s
        };
        let slice = start
            .checked_add(pc)
            .and_then(|end| params.get(start..end))
            .context("block params out of range")?;
        copied += slice.len();
        ensure!(copied <= copy_budget, "the blocks' shared parameter ranges expand to more than {copy_budget} parameters");
        ensure!(bc == 0 || first_block.checked_add(bc).is_some_and(|end| end <= blocks_count), "block children out of range");
        infos.push(BlockInfo { name_id, params: slice.to_vec(), blocks_count: bc, first_block });
    }

    // Children always come after their parent, so build the tree back to front.
    let mut built: Vec<Option<Block>> = vec![None; blocks_count];
    let mut depth = vec![1usize; blocks_count];
    for i in (0..blocks_count).rev() {
        let info = &infos[i];
        let block_name = if info.name_id == 0 { String::new() } else { name(info.name_id - 1)?.to_string() };
        let mut children = Vec::with_capacity(info.blocks_count);
        for c in info.first_block..info.first_block + info.blocks_count {
            ensure!(c > i, "block {i} references earlier block {c}");
            depth[i] = depth[i].max(depth[c] + 1);
            ensure!(depth[i] <= MAX_DEPTH, "blocks nest deeper than {MAX_DEPTH} levels");
            children.push(built[c].take().context("block used twice")?);
        }
        built[i] = Some(Block { name: block_name, params: info.params.clone(), blocks: children });
    }
    Ok(built[0].take().unwrap())
}

fn decode_value(ty: u8, v: u32, complex: &[u8], shared: &[String], own: &[String]) -> Result<Value> {
    let size = type_size(ty).with_context(|| format!("unknown param type {ty}"))?;
    let at = |n: usize| -> Result<&[u8]> {
        complex.get(v as usize..v as usize + n).context("param value outside complex data")
    };
    let inline = v.to_le_bytes();
    let bytes: &[u8] = if size <= 4 || ty == 1 { &inline } else { at(size)? };
    let f = |i: usize| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    let n = |i: usize| i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());

    Ok(match ty {
        1 => {
            if v & IS_NAMEMAP_ID != 0 {
                let id = (v & !IS_NAMEMAP_ID) as usize;
                let s = shared.get(id).or_else(|| own.get(id.wrapping_sub(shared.len())));
                Value::Str(s.context("string name id out of range")?.clone())
            } else {
                Value::Str(Reader::at(complex, v as usize)?.cstr()?)
            }
        }
        2 => Value::Int(v as i32),
        3 => Value::Real(f32::from_bits(v)),
        4 => Value::Point2([f(0), f(1)]),
        5 => Value::Point3([f(0), f(1), f(2)]),
        6 => Value::Point4([f(0), f(1), f(2), f(3)]),
        7 => Value::IPoint2([n(0), n(1)]),
        8 => Value::IPoint3([n(0), n(1), n(2)]),
        13 => Value::IPoint4([n(0), n(1), n(2), n(3)]),
        9 => Value::Bool(v & 0xFF != 0),
        10 => Value::Color([inline[2], inline[1], inline[0], inline[3]]),
        11 => Value::Matrix([
            [f(0), f(1), f(2)],
            [f(3), f(4), f(5)],
            [f(6), f(7), f(8)],
            [f(9), f(10), f(11)],
        ]),
        12 => Value::Int64(i64::from_le_bytes(bytes.try_into().unwrap())),
        _ => unreachable!(),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Output

fn fmt_f(x: f32) -> String {
    let s = format!("{x}");
    if s.contains(['.', 'e', 'i', 'N']) { s } else { format!("{s}.0") }
}

fn join<T>(items: &[T], f: impl Fn(&T) -> String) -> String {
    items.iter().map(f).collect::<Vec<_>>().join(", ")
}

impl Value {
    fn type_tag(&self) -> &'static str {
        match self {
            Value::Str(_) => "t",
            Value::Int(_) => "i",
            Value::Real(_) => "r",
            Value::Point2(_) => "p2",
            Value::Point3(_) => "p3",
            Value::Point4(_) => "p4",
            Value::IPoint2(_) => "ip2",
            Value::IPoint3(_) => "ip3",
            Value::IPoint4(_) => "ip4",
            Value::Bool(_) => "b",
            Value::Color(_) => "c",
            Value::Matrix(_) => "m",
            Value::Int64(_) => "i64",
        }
    }

    fn text(&self) -> String {
        match self {
            Value::Str(s) => quote_string(s),
            Value::Int(x) => x.to_string(),
            Value::Real(x) => fmt_f(*x),
            Value::Point2(p) => join(p, |x| fmt_f(*x)),
            Value::Point3(p) => join(p, |x| fmt_f(*x)),
            Value::Point4(p) => join(p, |x| fmt_f(*x)),
            Value::IPoint2(p) => join(p, |x| x.to_string()),
            Value::IPoint3(p) => join(p, |x| x.to_string()),
            Value::IPoint4(p) => join(p, |x| x.to_string()),
            Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
            Value::Color(c) => join(c, |x| x.to_string()),
            Value::Matrix(m) => {
                let rows: Vec<String> = m.iter().map(|row| format!("[{}]", join(row, |x| fmt_f(*x)))).collect();
                format!("[{}]", rows.join(" "))
            }
            Value::Int64(x) => x.to_string(),
        }
    }

    fn json(&self) -> Json {
        let floats = |xs: &[f32]| Json::from(xs.iter().map(|x| f64::from(*x)).collect::<Vec<_>>());
        match self {
            Value::Str(s) => Json::from(s.as_str()),
            Value::Int(x) => Json::from(*x),
            Value::Real(x) => Json::from(f64::from(*x)),
            Value::Point2(p) => floats(p),
            Value::Point3(p) => floats(p),
            Value::Point4(p) => floats(p),
            Value::IPoint2(p) => Json::from(p.to_vec()),
            Value::IPoint3(p) => Json::from(p.to_vec()),
            Value::IPoint4(p) => Json::from(p.to_vec()),
            Value::Bool(b) => Json::from(*b),
            Value::Color(c) => Json::from(c.to_vec()),
            Value::Matrix(m) => Json::from(m.iter().map(|row| floats(row)).collect::<Vec<_>>()),
            Value::Int64(x) => Json::from(*x),
        }
    }
}

impl Block {
    /// Dagor text BLK syntax (`name:type=value`, `name{ ... }`).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write_text(&mut out, 0);
        out
    }

    fn write_text(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        for (name, value) in &self.params {
            let _ = writeln!(out, "{pad}{}:{}={}", quote_name(name), value.type_tag(), value.text());
        }
        for child in &self.blocks {
            let _ = writeln!(out, "{pad}{}{{", quote_name(&child.name));
            child.write_text(out, depth + 1);
            let _ = writeln!(out, "{pad}}}");
        }
    }

    /// JSON object; repeated names become arrays so nothing is lost.
    pub fn to_json(&self) -> Json {
        let mut groups: Vec<(&str, Vec<Json>)> = Vec::new();
        let mut index = std::collections::HashMap::new();
        let entries = self
            .params
            .iter()
            .map(|(k, v)| (k.as_str(), v.json()))
            .chain(self.blocks.iter().map(|b| (b.name.as_str(), b.to_json())));
        for (key, value) in entries {
            let i = *index.entry(key).or_insert_with(|| {
                groups.push((key, Vec::new()));
                groups.len() - 1
            });
            groups[i].1.push(value);
        }
        let mut map = Map::new();
        for (key, mut values) in groups {
            let v = if values.len() == 1 { values.pop().unwrap() } else { Json::Array(values) };
            map.insert(key.to_string(), v);
        }
        Json::Object(map)
    }
}

/// Names made only of Dagor identifier characters are written bare, anything else is quoted.
fn quote_name(name: &str) -> String {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || "_-.~".contains(c);
    if !name.is_empty() && name.chars().all(is_ident) {
        name.to_string()
    } else {
        quote_string(name)
    }
}

/// Quotes a string the way Dagor's `writeStringValue` does, so its text parser reads it back
/// unchanged: `~` is the escape character (`~~`, `~"`, `~'`, `~t`, `~n`, `~r`), backslashes are
/// literal, a string holding `"` but no `'` is put in single quotes, and strings with line breaks
/// use triple quotes.
fn quote_string(s: &str) -> String {
    let has_ln = s.contains(['\n', '\r']);
    let has_quot = s.contains('"');
    let has_tick = s.contains('\'');
    let (open, close) = if has_ln && (!has_quot || has_tick) {
        ("\"\"\"\n", "\n\"\"\"")
    } else if has_ln {
        ("'''\n", "\n'''")
    } else if has_quot && !has_tick {
        ("'", "'")
    } else {
        ("\"", "\"")
    };
    let single = open.len() == 1;
    let q = open.chars().next().unwrap();
    let mut out = String::with_capacity(s.len() + 2);
    out.push_str(open);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' => out.push_str("~~"),
            c if c == q && (single || chars.peek() == Some(&c)) => {
                out.push('~');
                out.push(c);
            }
            '\r' if single => out.push_str("~r"),
            '\n' if single => out.push_str("~n"),
            '\t' => out.push_str("~t"),
            c => out.push(c),
        }
    }
    out.push_str(close);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built format-1 dump: root { speed:r=1.5 engine{ name:t="V12" } }
    fn sample() -> Vec<u8> {
        let mut d = vec![1u8];
        // names: "speed", "engine", "name"
        let names = b"speed\0engine\0name\0";
        d.push(3);
        d.push(names.len() as u8);
        d.extend_from_slice(names);
        d.extend_from_slice(&[2, 2]); // 2 blocks, 2 params
        let complex = b"V12\0";
        d.push(complex.len() as u8);
        d.extend_from_slice(complex);
        // param speed (name 0, type 3)
        d.extend_from_slice(&(0u32 | 3 << 24).to_le_bytes());
        d.extend_from_slice(&1.5f32.to_bits().to_le_bytes());
        // param name (name 2, type 1) -> offset 0 in complex data
        d.extend_from_slice(&(2u32 | 1 << 24).to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        // root: name 0 (none), 1 param, 1 block starting at 1
        d.extend_from_slice(&[0, 1, 1, 1]);
        // engine: name id 1+1, 1 param, 0 blocks
        d.extend_from_slice(&[2, 1, 0]);
        d
    }

    fn varint(mut v: u32, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }

    /// Format 1, no names: `n` blocks, each the only child of the one before
    fn deep_chain(n: u32) -> Vec<u8> {
        let mut d = vec![1u8, 0];
        varint(n, &mut d);
        d.extend([0, 0]); // no params, no complex data
        for i in 0..n - 1 {
            d.extend([0, 0, 1]); // name id, params, one child
            varint(i + 1, &mut d);
        }
        d.extend([0, 0, 0]);
        d
    }

    #[test]
    fn a_deep_chain_is_an_error_instead_of_a_stack_overflow() {
        // 20,000 nested blocks overflowed an 8 MB stack in Drop and to_json
        let err = parse(&deep_chain(20_000), None).unwrap_err().to_string();
        assert!(err.contains("nest"), "{err}");
    }

    #[test]
    fn nesting_up_to_the_limit_parses_and_prints() {
        let root = parse(&deep_chain(MAX_DEPTH as u32), None).unwrap();
        assert_eq!(root.to_text().lines().count(), 2 * (MAX_DEPTH - 1));
        let mut json = root.to_json();
        let mut depth = 1;
        while let Some(child) = json.as_object_mut().and_then(|o| o.remove("")) {
            json = child;
            depth += 1;
        }
        assert_eq!(depth, MAX_DEPTH);
        assert!(parse(&deep_chain(MAX_DEPTH as u32 + 1), None).is_err());
    }

    #[test]
    fn a_huge_block_count_is_refused_before_anything_is_reserved() {
        // 9 bytes that ask for 0xFFFFFFFF blocks
        let mut d = vec![1u8, 0];
        varint(0xFFFF_FFFF, &mut d);
        d.extend([0, 0]);
        let err = parse(&d, None).unwrap_err().to_string();
        assert!(err.contains("claims"), "{err}");
    }

    /// Format 6: two params shared by `n` blocks that all point at the same range
    fn shared_params(n: u32) -> Vec<u8> {
        let mut d = vec![6u8, 1, 2, b'a', 0]; // one name, "a"
        varint(n + 1, &mut d); // root + n children
        d.extend([2, 0]); // two params, no complex data
        for _ in 0..2 {
            d.extend_from_slice(&(2u32 << 24).to_le_bytes()); // name 0, type int
            d.extend_from_slice(&7u32.to_le_bytes());
        }
        d.extend([0, 0]); // root: name, no params
        varint(n, &mut d); // n children
        d.push(1); // starting at block 1
        for _ in 0..n {
            d.extend([1, 2, 0, 0]); // name, two params, no blocks, params start at 0
        }
        d
    }

    #[test]
    fn shared_param_ranges_expand_within_a_budget() {
        let root = parse(&shared_params(100), None).unwrap();
        assert_eq!(root.blocks.len(), 100);
        assert!(root.blocks.iter().all(|b| b.params.len() == 2));
        // 1.2 million copies out of 2 stored params, in a 2.4 MB file
        let err = parse(&shared_params(600_000), None).unwrap_err().to_string();
        assert!(err.contains("expand"), "{err}");
    }

    #[test]
    fn parses_hand_built_dump() {
        let root = parse(&sample(), None).unwrap();
        assert_eq!(root.params, vec![("speed".into(), Value::Real(1.5))]);
        assert_eq!(root.blocks[0].name, "engine");
        assert_eq!(root.blocks[0].params, vec![("name".into(), Value::Str("V12".into()))]);
        assert_eq!(root.to_text(), "speed:r=1.5\nengine{\n  name:t=\"V12\"\n}\n");
    }

    /// Format 5 (shared name map, zstd with dictionary) whose dump unpacks to far more than 64x
    /// its packed size: one string param holding 5 MB of the same byte.
    #[test]
    fn parses_highly_compressed_dict_blk() {
        let value = vec![b'a'; 5 << 20];
        let mut dump = vec![1u8, 2, b'v', 0]; // own names: "v"
        dump.extend_from_slice(&[1, 1]); // 1 block, 1 param
        let mut size = value.len() as u32 + 1; // complex data: the string and its NUL
        while size >= 0x80 {
            dump.push(size as u8 | 0x80);
            size >>= 7;
        }
        dump.push(size as u8);
        dump.extend_from_slice(&value);
        dump.push(0);
        dump.extend_from_slice(&(1u32 << 24).to_le_bytes()); // name 0, type string
        dump.extend_from_slice(&0u32.to_le_bytes()); // offset 0 in complex data
        dump.extend_from_slice(&[0, 1, 0]); // root: 1 param, no blocks

        let dict = b"a dictionary made of plain bytes, used as raw content".repeat(4);
        let packed = zstd::bulk::Compressor::with_dictionary(19, &dict).unwrap().compress(&dump).unwrap();
        assert!(packed.len() * 64 < dump.len());
        let mut file = vec![5u8];
        file.extend_from_slice(&packed);

        let ctx = SharedContext { names: Vec::new(), dict: Some(dict) };
        let root = parse(&file, Some(&ctx)).unwrap();
        assert_eq!(root.params, vec![("v".into(), Value::Str("a".repeat(5 << 20)))]);
    }

    #[test]
    fn text_strings_use_dagor_escapes() {
        let s = |v: &str| Value::Str(v.into());
        let blk = Block {
            name: String::new(),
            params: vec![
                ("path".into(), s(r"..\..\prog\scripts")),
                ("anim".into(), s("~hatch_hide")),
                ("quoted".into(), s(r#"say "hi""#)),
                ("both".into(), s(r#"it's "x""#)),
                ("tab".into(), s("a\tb")),
                ("lines".into(), s("one\ntwo")),
                ("@override".into(), s("")),
                ("rendinst::tm".into(), Value::Bool(true)),
                ("".into(), Value::Int(1)),
            ],
            blocks: vec![Block { name: "a b".into(), ..Block::default() }],
        };
        let expected = [
            r#"path:t="..\..\prog\scripts""#,
            r#"anim:t="~~hatch_hide""#,
            r#"quoted:t='say "hi"'"#,
            r#"both:t="it's ~"x~"""#,
            r#"tab:t="a~tb""#,
            "lines:t=\"\"\"\none\ntwo\n\"\"\"",
            r#""@override":t="""#,
            r#""rendinst::tm":b=yes"#,
            r#""":i=1"#,
            r#""a b"{"#,
            "}",
            "",
        ];
        assert_eq!(blk.to_text(), expected.join("\n"));
    }

    #[test]
    fn repeated_names_become_arrays() {
        let blk = Block {
            name: String::new(),
            params: vec![
                ("gun".into(), Value::Str("a".into())),
                ("gun".into(), Value::Str("b".into())),
                ("pos".into(), Value::Point2([1.0, 2.0])),
            ],
            blocks: vec![],
        };
        assert_eq!(blk.to_json().to_string(), r#"{"gun":["a","b"],"pos":[1.0,2.0]}"#);
    }
}
