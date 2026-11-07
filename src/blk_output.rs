use super::{Block, Value};
use std::collections::HashMap;
use std::io::{self, Write};

impl Block {
    /// Write Dagor text without retaining the complete encoded document.
    pub fn write_text_to(&self, out: &mut impl Write) -> io::Result<()> {
        text_block(self, out, 0)
    }

    /// Write pretty JSON without building a JSON tree or retaining the encoded document.
    /// Repeated names require an index of borrowed entries for each open block.
    pub fn write_json_to(&self, out: &mut impl Write) -> io::Result<()> {
        json_block(self, out, 0)
    }
}

fn indent(out: &mut impl Write, depth: usize) -> io::Result<()> {
    for _ in 0..depth { out.write_all(b"  ")?; }
    Ok(())
}

fn quoted(out: &mut impl Write, s: &str) -> io::Result<()> {
    let has_ln = s.contains(['\n', '\r']);
    let has_quot = s.contains('"');
    let has_tick = s.contains('\'');
    let (open, close) = if has_ln && (!has_quot || has_tick) {
        ("\"\"\"\n", "\n\"\"\"")
    } else if has_ln { ("'''\n", "\n'''") }
    else if has_quot && !has_tick { ("'", "'") }
    else { ("\"", "\"") };
    out.write_all(open.as_bytes())?;
    let single = open.len() == 1;
    let q = open.chars().next().unwrap();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' => out.write_all(b"~~")?,
            c if c == q && (single || chars.peek() == Some(&c)) => write!(out, "~{c}")?,
            '\r' if single => out.write_all(b"~r")?,
            '\n' if single => out.write_all(b"~n")?,
            '\t' => out.write_all(b"~t")?,
            c => write!(out, "{c}")?,
        }
    }
    out.write_all(close.as_bytes())
}

fn name(out: &mut impl Write, s: &str) -> io::Result<()> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.~".contains(c)) {
        out.write_all(s.as_bytes())
    } else { quoted(out, s) }
}

fn text_block(block: &Block, out: &mut impl Write, depth: usize) -> io::Result<()> {
    for (key, value) in &block.params {
        indent(out, depth)?;
        name(out, key)?;
        write!(out, ":{}=", value.type_tag())?;
        // Only strings can make a single value arbitrarily large; numeric values have at most 12 elements.
        if let Value::Str(s) = value { quoted(out, s)?; }
        else { out.write_all(value.text().as_bytes())?; }
        out.write_all(b"\n")?;
    }
    for child in &block.blocks {
        indent(out, depth)?;
        name(out, &child.name)?;
        out.write_all(b"{\n")?;
        text_block(child, out, depth + 1)?;
        indent(out, depth)?;
        out.write_all(b"}\n")?;
    }
    Ok(())
}

// serde's pretty printer indents a value relative to zero; add its containing block's indentation.
struct Indented<'a, W> { out: &'a mut W, depth: usize }
impl<W: Write> Write for Indented<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            self.out.write_all(part)?;
            if part.last() == Some(&b'\n') { indent(self.out, self.depth)?; }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { self.out.flush() }
}

fn json_value(value: &Value, out: &mut impl Write, depth: usize) -> io::Result<()> {
    let mut out = Indented { out, depth };
    let result = match value {
        Value::Str(s) => serde_json::to_writer_pretty(&mut out, s),
        Value::Int(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::Int64(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::Bool(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::Real(v) => serde_json::to_writer_pretty(&mut out, &f64::from(*v)),
        Value::Point2(v) => serde_json::to_writer_pretty(&mut out, &v.map(f64::from)),
        Value::Point3(v) => serde_json::to_writer_pretty(&mut out, &v.map(f64::from)),
        Value::Point4(v) => serde_json::to_writer_pretty(&mut out, &v.map(f64::from)),
        Value::IPoint2(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::IPoint3(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::IPoint4(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::Color(v) => serde_json::to_writer_pretty(&mut out, v),
        Value::Matrix(v) => serde_json::to_writer_pretty(&mut out, &v.map(|row| row.map(f64::from))),
    };
    result.map_err(io::Error::other)
}

enum Entry<'a> { Value(&'a Value), Block(&'a Block) }
impl Entry<'_> {
    fn write(&self, out: &mut impl Write, depth: usize) -> io::Result<()> {
        match self { Self::Value(v) => json_value(v, out, depth), Self::Block(b) => json_block(b, out, depth) }
    }
}

fn json_block(block: &Block, out: &mut impl Write, depth: usize) -> io::Result<()> {
    let mut groups: Vec<(&str, Vec<Entry<'_>>)> = Vec::new();
    let mut index = HashMap::new();
    for (key, value) in block.params.iter().map(|(k,v)| (k.as_str(), Entry::Value(v)))
        .chain(block.blocks.iter().map(|b| (b.name.as_str(), Entry::Block(b)))) {
        let i = *index.entry(key).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(value);
    }
    if groups.is_empty() { return out.write_all(b"{}"); }
    out.write_all(b"{\n")?;
    for (i, (key, values)) in groups.iter().enumerate() {
        if i > 0 { out.write_all(b",\n")?; }
        indent(out, depth + 1)?;
        serde_json::to_writer(&mut *out, key).map_err(io::Error::other)?;
        out.write_all(b": ")?;
        if values.len() == 1 { values[0].write(out, depth + 1)?; }
        else {
            out.write_all(b"[\n")?;
            for (j, value) in values.iter().enumerate() {
                if j > 0 { out.write_all(b",\n")?; }
                indent(out, depth + 2)?;
                value.write(out, depth + 2)?;
            }
            out.write_all(b"\n")?;
            indent(out, depth + 1)?;
            out.write_all(b"]")?;
        }
    }
    out.write_all(b"\n")?;
    indent(out, depth)?;
    out.write_all(b"}")
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::blk::Value;
    #[test]
    fn streaming_matches_the_existing_formats(){
        let mut root=Block::default();
        for s in ["","plain","a b","quote\"","a'b\"","a\n\r\t\"'~","~\"\"\"","ç"] {root.params.push((s.into(),Value::Str(s.into())));root.params.push((s.into(),Value::Real(-0.0)));root.blocks.push(Block{name:s.into(),..Block::default()});}
        root.params.extend([(String::from("matrix"),Value::Matrix([[f32::MAX,f32::NAN,f32::INFINITY];4])),(String::from("small"),Value::Real(f32::MIN_POSITIVE)),(String::from("ints"),Value::IPoint4([1,-2,3,4])),(String::from("color"),Value::Color([0,1,2,255])),(String::from("flag"),Value::Bool(true)),(String::from("i64"),Value::Int64(i64::MIN))]);
        let mut text=Vec::new();root.write_text_to(&mut text).unwrap();assert_eq!(text,root.to_text().as_bytes());
        let mut json=Vec::new();root.write_json_to(&mut json).unwrap();assert_eq!(json,serde_json::to_vec_pretty(&root.to_json()).unwrap());
    }
    #[test]
    fn streaming_does_not_allocate_an_encoded_document(){
        let root=Block{name:String::new(),params:vec![("x".into(),Value::Matrix([[f32::MAX;3];4]));4096],blocks:vec![]};
        for json in [false,true] {
            let (result,largest)=crate::alloc_probe::largest_allocation(||{let mut sink=io::sink();if json{root.write_json_to(&mut sink)}else{root.write_text_to(&mut sink)}});
            result.unwrap();assert!(largest<128<<10,"json={json} retained a {largest}-byte output allocation");
        }
    }
    #[test]
    fn writer_failures_are_returned(){struct Broken;impl Write for Broken {fn write(&mut self,_:&[u8])->io::Result<usize>{Err(io::Error::other("synthetic writer failure"))}fn flush(&mut self)->io::Result<()>{Ok(())}}
        let root=Block{params:vec![("x".into(),Value::Int(1))],..Block::default()};assert!(root.write_text_to(&mut Broken).is_err());assert!(root.write_json_to(&mut Broken).is_err());
    }
}
