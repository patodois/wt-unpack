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

use anyhow::{bail, ensure, Context, Result};
use serde_json::{Map, Value as Json};

use crate::bytes::Reader;

const IS_NAMEMAP_ID: u32 = 0x8000_0000;

/// Deepest block nesting `parse` accepts. The deepest of the game's 31,651 binary BLKs nests 23 levels
/// (`gamedata/ai/inf/ai_common_attack.blk`), so this leaves plenty. Every `Block` this crate builds is
/// at most this deep, which is what keeps `Drop`, `to_text` and `to_json` (all recursive) off the stack limit.
pub const MAX_DEPTH: usize = 128;

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

/// The most a zstd-packed BLK may unpack to (the largest in the game's files is far below this)
const MAX_BLK: usize = 1 << 30;

/// What kind of content a BLK file holds, judged from its first byte.
pub fn is_binary(data: &[u8]) -> bool {
    matches!(data.first(), Some(1..=6))
}

/// Shared names and zstd dictionary needed by slim BLK files (formats 3-5).
#[derive(Default)]
pub struct SharedContext {
    names: Vec<String>,
    name_bytes: usize,
    pub dict: Option<Vec<u8>>,
}

impl SharedContext {
    /// Prepares immutable shared names and their byte budget once for a batch of BLKs.
    pub fn new(names: Vec<String>, dict: Option<Vec<u8>>) -> Self {
        let name_bytes=names.iter().fold(0usize,|n,s|n.saturating_add(s.len()));
        Self {names,name_bytes,dict}
    }

    /// The archive's immutable shared name map.
    pub fn names(&self)->&[String] {&self.names}

    /// Parses the `\xff?nm` file: u64 names hash, 32-byte dictionary hash,
    /// then a zstd stream holding a name map.
    pub fn from_namemap(nm: &[u8]) -> Result<(Self, String)> {
        ensure!(nm.len() >= 40, "shared name map is too short");
        let dict_name = format!("{}.dict", hex(&nm[8..40]));
        let raw = crate::unzstd::decode(&nm[40..], None, MAX_BLK).context("shared name map is not valid zstd")?;
        let names = read_names(&mut Reader::new(&raw))?;
        Ok((SharedContext::new(names,None), dict_name))
    }
}

pub fn parse(data: &[u8], shared: Option<&SharedContext>) -> Result<Block> {
    let mut r = Reader::new(data);
    let format = r.u8()?;
    let need_shared = || shared.context("this BLK uses a shared name map; read it from inside its vromfs");
    match format {
        1 => parse_dump(&mut r, &[], false,0),
        6 => parse_dump(&mut r, &[], true,0),
        2 => {
            let size = r.bytes(3)?;
            let size = usize::from(size[0]) | usize::from(size[1]) << 8 | usize::from(size[2]) << 16;
            let raw = crate::unzstd::decode(r.bytes(size)?, None, MAX_BLK)?;
            // The unpacked stream repeats the format byte of a plain full BLK
            ensure!(raw.first() == Some(&1), "packed BLK does not unpack to format 1");
            parse_dump(&mut Reader::new(&raw[1..]), &[], false,0)
        }
        3 => parse_dump(&mut r, &need_shared()?.names, false,need_shared()?.name_bytes),
        4 => {
            let raw = crate::unzstd::decode(r.rest(), None, MAX_BLK)?;
            parse_dump(&mut Reader::new(&raw), &need_shared()?.names, false,need_shared()?.name_bytes)
        }
        5 => {
            let ctx = need_shared()?;
            let dict = ctx.dict.as_deref().context("this BLK needs the archive's zstd dictionary")?;
            // The unpacked size is not stored and the ratio has no upper bound: the output grows with the stream
            let raw = crate::unzstd::decode(r.rest(), Some(dict), MAX_BLK)?;
            parse_dump(&mut Reader::new(&raw), &ctx.names, false,ctx.name_bytes)
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
    params: std::ops::Range<usize>,
    blocks_count: usize,
    first_block: usize,
}

fn text_copy_budget(bytes:usize)->usize {bytes.saturating_mul(16).clamp(64 << 10,256 << 20)}
fn parameter_storage_budget(bytes:usize)->usize {bytes.saturating_mul(16).clamp(64 << 10,1 << 30)}
fn charge_storage(bytes:usize,remaining:&mut usize)->Result<()> {
    *remaining=remaining.checked_sub(bytes).context("parameter copies expand beyond the storage byte budget")?;Ok(())
}

fn parse_dump(r: &mut Reader, shared_names: &[String], dedup: bool, shared_text:usize) -> Result<Block> {
    let input_bytes=r.remaining();
    let mut text_budget = text_copy_budget(input_bytes.saturating_add(shared_text));
    let mut storage_budget=parameter_storage_budget(input_bytes);
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

    let parameter_bytes=params_count.checked_mul(std::mem::size_of::<(String,Value)>()).context("parameter storage overflow")?;
    charge_storage(parameter_bytes,&mut storage_budget)?;
    let mut params = Vec::with_capacity(params_count);
    for raw in param_table.chunks_exact(8) {
        let head = u32::from_le_bytes(raw[..4].try_into().unwrap());
        let v = u32::from_le_bytes(raw[4..].try_into().unwrap());
        let (name_id, ty) = (head & 0x00FF_FFFF, (head >> 24) as u8);
        params.push(Some((copy_text(name(name_id)?, &mut text_budget)?, decode_value(ty, v, complex, shared_names, &own_names, &mut text_budget)?)));
    }

    let mut uses=vec![0usize;params_count];
    let mut infos = Vec::with_capacity(blocks_count);
    let mut next_param = 0usize;
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
        let end=start.checked_add(pc).filter(|&end|end<=params_count).context("block params out of range")?;
        ensure!(bc == 0 || first_block.checked_add(bc).is_some_and(|end| end <= blocks_count), "block children out of range");
        for count in &mut uses[start..end] { *count+=1; }
        infos.push(BlockInfo { name_id, params: start..end, blocks_count: bc, first_block });
    }

    // Children always come after their parent, so build the tree back to front.
    let mut built: Vec<Option<Block>> = vec![None; blocks_count];
    let mut depth = vec![1usize; blocks_count];
    for i in (0..blocks_count).rev() {
        let info = &mut infos[i];
        let block_name = if info.name_id == 0 { String::new() } else { copy_text(name(info.name_id - 1)?, &mut text_budget)? };
        let extra_copies=info.params.clone().filter(|&index|uses[index]>1).count();
        charge_storage(extra_copies*std::mem::size_of::<(String,Value)>(),&mut storage_budget)?;
        let mut fields=Vec::with_capacity(info.params.len());
        for index in info.params.clone() {
            uses[index]-=1;
            let field=if uses[index]==0 {
                // Move the original on its last use; only additional copies consume more budget.
                params[index].take().context("parameter already moved")?
            } else {
                let (key,value)=params[index].as_ref().context("parameter already moved")?;
                charge_text(key.len()+if let Value::Str(s)=value{s.len()}else{0},&mut text_budget)?;
                (key.clone(),value.clone())
            };
            fields.push(field);
        }
        let mut children = Vec::with_capacity(info.blocks_count);
        for c in info.first_block..info.first_block + info.blocks_count {
            ensure!(c > i, "block {i} references earlier block {c}");
            depth[i] = depth[i].max(depth[c] + 1);
            ensure!(depth[i] <= MAX_DEPTH, "blocks nest deeper than {MAX_DEPTH} levels");
            children.push(built[c].take().context("block used twice")?);
        }
        built[i] = Some(Block { name: block_name, params: fields, blocks: children });
    }
    Ok(built[0].take().unwrap())
}

fn decode_value(ty: u8, v: u32, complex: &[u8], shared: &[String], own: &[String], text_budget: &mut usize) -> Result<Value> {
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
                Value::Str(copy_text(s.context("string name id out of range")?, text_budget)?)
            } else {
                Value::Str(copy_cstr(Reader::at(complex, v as usize)?.rest(), text_budget)?)
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


fn charge_text(size: usize, remaining: &mut usize) -> Result<()> {
    *remaining = remaining.checked_sub(size).context("copied text exceeds the byte budget")?;
    Ok(())
}
pub(crate) fn copy_text(s: &str, remaining: &mut usize) -> Result<String> {
    charge_text(s.len(), remaining)?;
    Ok(s.to_string())
}
pub(crate) fn copy_cstr(bytes: &[u8], remaining: &mut usize) -> Result<String> {
    let end = bytes.iter().position(|&b| b == 0).context("unterminated string")?;
    let raw = &bytes[..end];
    if let Ok(text) = std::str::from_utf8(raw) { return copy_text(text, remaining); }
    let size = raw.utf8_chunks().fold(0usize, |n, c| n.saturating_add(c.valid().len()).saturating_add(if c.invalid().is_empty() { 0 } else { 3 }));
    charge_text(size, remaining)?;
    Ok(String::from_utf8_lossy(raw).into_owned())
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
    fn invalid_utf8_is_budgeted_before_materialization() {
        let mut raw=[0xff;65]; raw[64]=0;
        let mut remaining=128;
        let (result,largest)=crate::alloc_probe::largest_allocation(||copy_cstr(&raw,&mut remaining));
        assert!(result.is_err());
        assert!(largest<192,"requested {largest} bytes before rejecting 192 bytes of text");
    }

    #[test]
    fn shared_name_text_is_part_of_the_copy_budget_input() {
        let mut data=vec![3,0,1,1,0];
        data.extend_from_slice(&(1u32<<24).to_le_bytes());
        data.extend_from_slice(&IS_NAMEMAP_ID.to_le_bytes());
        data.extend_from_slice(&[0,1,0]);
        let ctx=SharedContext::new(vec!["x".repeat(8192)],None);
        let block=parse(&data,Some(&ctx)).unwrap();
        assert_eq!(block.params[0].0.len(),8192);
        assert_eq!(block.params[0].1,Value::Str("x".repeat(8192)));
    }

    #[test]
    fn repeated_string_parameters_have_a_byte_budget() {
        let mut b = vec![1, 1, 2, b'x', 0];
        varint(1, &mut b);
        varint(128, &mut b);
        varint(8193, &mut b);
        b.extend_from_slice(&[b'A'; 8192]);
        b.push(0);
        for _ in 0..128 {
            b.extend_from_slice(&(1u32 << 24).to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
        }
        b.push(0);
        varint(128, &mut b);
        b.push(0);
        assert!(parse(&b, None).is_err());
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

        // A trained dictionary, like the archives' (the decoder does not read raw-content ones)
        let samples: Vec<Vec<u8>> = (0..300).map(|i| format!("{{\"name\":\"entry{i}\",\"kind\":\"sample\",\"v\":[{i},{}]}}", i * 5).into_bytes()).collect();
        let dict = zstd::dict::from_samples(&samples, 4096).unwrap();
        let packed = zstd::bulk::Compressor::with_dictionary(19, &dict).unwrap().compress(&dump).unwrap();
        assert!(packed.len() * 64 < dump.len());
        let mut file = vec![5u8];
        file.extend_from_slice(&packed);

        let ctx = SharedContext::new(Vec::new(),Some(dict));
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

#[cfg(test)]
mod text_copy_contract {
    use super::*;
    fn var(mut n:usize,out:&mut Vec<u8>) {loop{let mut b=(n&127) as u8;n>>=7;if n>0{b|=128;}out.push(b);if n==0{break;}}}
    fn repeated_plain_string(length:usize,count:usize)->Vec<u8> {
        let mut file=vec![1,1,2,b'x',0];var(1,&mut file);var(count,&mut file);var(length+1,&mut file);
        file.resize(file.len()+length,b'A');file.push(0);
        for _ in 0..count {file.extend_from_slice(&(1u32<<24).to_le_bytes());file.extend_from_slice(&0u32.to_le_bytes());}
        file.push(0);var(count,&mut file);file.push(0);file
    }
    #[test]
    fn ordinary_aliased_strings_are_copied_once_with_a_small_input_floor() {
        for (length,count,expected_len) in [(1000,15,1133),(130,20,303),(1000,40,1333)] {
            let file=repeated_plain_string(length,count);assert_eq!(file.len(),expected_len);
            let root=parse(&file,None).unwrap();assert_eq!(root.params.len(),count);
            assert!(root.params.iter().all(|(name,value)|name=="x" && *value==Value::Str("A".repeat(length))));
        }
    }
    #[test]
    fn copied_text_obeys_the_multiplier_above_the_floor() {
        assert!(parse(&repeated_plain_string(5000,15),None).is_ok());
        assert!(parse(&repeated_plain_string(5000,18),None).is_err());
    }
    #[test]
    fn copied_text_stops_at_the_small_input_floor() {
        let cost=1001;let count=(64<<10)/cost;
        assert!(parse(&repeated_plain_string(1000,count),None).is_ok());
        assert!(parse(&repeated_plain_string(1000,count+1),None).is_err());
    }
    #[test]
    fn repeated_parameter_slices_charge_each_additional_text_copy() {
        let mut file=vec![6,1,2,b'x',0];var(129,&mut file);var(1,&mut file);var(8193,&mut file);
        file.resize(file.len()+8192,b'A');file.push(0);file.extend_from_slice(&(1u32<<24).to_le_bytes());file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&[0,0,128,1,1]);
        for _ in 0..128 {file.extend_from_slice(&[1,1,0,0]);}
        assert!(parse(&file,None).is_err());
    }

}

#[cfg(test)]
mod numeric_copy_budget {
    use super::*;
    fn var(mut n:usize,out:&mut Vec<u8>) {loop {let mut b=(n&127) as u8;n>>=7;if n>0{b|=128;}out.push(b);if n==0{break;}}}
    fn numeric(params:usize,blocks:usize)->Vec<u8> {
        let mut file=vec![6,1,1,0];var(blocks+1,&mut file);var(params,&mut file);var(0,&mut file);
        for _ in 0..params {file.extend_from_slice(&(2u32<<24).to_le_bytes());file.extend_from_slice(&0u32.to_le_bytes());}
        file.extend_from_slice(&[0,0]);var(blocks,&mut file);var(1,&mut file);
        for _ in 0..blocks {file.push(0);var(params,&mut file);file.extend_from_slice(&[0,0]);}
        file
    }
    #[test]
    fn empty_numeric_keys_still_pay_for_parameter_storage() {
        let file=numeric(64,64);
        let (result,total)=crate::alloc_probe::total_allocation(||parse(&file,None));
        assert!(result.is_err(),"accepted {} bytes with {total} allocated bytes",file.len());
        assert!(total<file.len()*128,"allocated {total} bytes before rejecting copies");
        let small=numeric(2,8);let block=parse(&small,None).unwrap();assert_eq!(block.blocks.len(),8);
    }
    #[test]
    fn stored_parameter_tables_above_the_text_ceiling_remain_valid() {
        let file=numeric(4_000_000,1);let root=parse(&file,None).unwrap();assert_eq!(root.blocks[0].params.len(),4_000_000);
    }

}

#[cfg(test)]
mod dense_parameter_storage {
    use super::*;
    fn var(mut n:usize,out:&mut Vec<u8>) {loop {let mut b=(n&127) as u8;n>>=7;if n>0{b|=128;}out.push(b);if n==0{break;}}}
    // Format 1: `params` stored once each, one own name of name_len bytes, optional one string of str_len bytes aliased by all.
    fn file(params:usize,name_len:usize,str_len:usize)->Vec<u8>{
        let mut f=vec![1u8];
        var(1,&mut f);var(name_len+1,&mut f);f.extend(std::iter::repeat_n(b'n',name_len));f.push(0);
        var(1,&mut f);var(params,&mut f);
        let complex=if str_len>0 {str_len+1} else {0};var(complex,&mut f);
        if str_len>0 {f.extend(std::iter::repeat_n(b'A',str_len));f.push(0);}
        let ty:u32=if str_len>0 {1} else {2};
        for _ in 0..params {f.extend_from_slice(&(ty<<24).to_le_bytes());f.extend_from_slice(&0u32.to_le_bytes());}
        f.push(0);var(params,&mut f);f.push(0);f
    }

    #[test]
    fn dense_valid_tables_keep_their_independent_text_allowance() {
        for (params,name,string) in [(500,64,0),(1000,10,60),(100000,30,20),(4_000_000,1,0)] {
            let f=file(params,name,string);let root=parse(&f,None).unwrap();assert_eq!(root.params.len(),params);
        }
    }

}
