//! Builds the starting corpus: small synthetic inputs, made here from the layouts in the module docs of the
//! readers (nothing from the game). `cargo run --release --bin make_seeds -- corpus`.

use std::fs;
use std::path::Path;

fn le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn put(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&le(v));
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

fn zstd(data: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(data, 3).unwrap()
}

// ---- BLK -------------------------------------------------------------------------------------------------

/// Format 1: root { speed:r=1.5 engine{ name:t="V12" } }; without the leading format byte
fn blk_dump() -> Vec<u8> {
    let mut d = Vec::new();
    let names = b"speed\0engine\0name\0";
    d.push(3);
    d.push(names.len() as u8);
    d.extend_from_slice(names);
    d.extend_from_slice(&[2, 2]);
    let complex = b"V12\0";
    d.push(complex.len() as u8);
    d.extend_from_slice(complex);
    put(&mut d, 3 << 24);
    put(&mut d, 1.5f32.to_bits());
    put(&mut d, 2 | 1 << 24);
    put(&mut d, 0);
    d.extend_from_slice(&[0, 1, 1, 1]);
    d.extend_from_slice(&[2, 1, 0]);
    d
}

/// A dump with one parameter of every type, in the complex data
fn blk_all_types() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&[1, 2, b'p', 0]); // one own name
    let mut complex = Vec::new();
    let mut params = Vec::new();
    let add = |ty: u32, inline: Option<u32>, bytes: &[u8], params: &mut Vec<u8>, complex: &mut Vec<u8>| {
        let v = match inline {
            Some(v) => v,
            None => {
                let at = complex.len() as u32;
                complex.extend_from_slice(bytes);
                at
            }
        };
        put(params, ty << 24);
        put(params, v);
    };
    add(1, None, b"text\0", &mut params, &mut complex);
    add(2, Some(7), &[], &mut params, &mut complex);
    add(3, Some(2.5f32.to_bits()), &[], &mut params, &mut complex);
    add(4, None, &[0; 8], &mut params, &mut complex);
    add(5, None, &[0; 12], &mut params, &mut complex);
    add(6, None, &[0; 16], &mut params, &mut complex);
    add(7, None, &[1; 8], &mut params, &mut complex);
    add(8, None, &[1; 12], &mut params, &mut complex);
    add(9, Some(1), &[], &mut params, &mut complex);
    add(10, Some(0xFF80_4020), &[], &mut params, &mut complex);
    add(11, None, &[0; 48], &mut params, &mut complex);
    add(12, None, &[3; 8], &mut params, &mut complex);
    add(13, None, &[1; 16], &mut params, &mut complex);
    d.extend_from_slice(&[1, 13]); // 1 block, 13 params
    varint(complex.len() as u32, &mut d);
    d.extend_from_slice(&complex);
    d.extend_from_slice(&params);
    d.extend_from_slice(&[0, 13, 0]); // root: no name, 13 params, no blocks
    d
}

fn blk_seeds(dir: &Path) {
    let dump = blk_dump();
    let with = |first: u8, rest: &[u8]| [&[first][..], rest].concat();
    // fuzz input: one mode byte (0 none, 1 shared names, 2/3 shared names and a dictionary), then the BLK
    let put_seed = |name: &str, mode: u8, blk: Vec<u8>| fs::write(dir.join(name), [&[mode][..], &blk].concat()).unwrap();
    put_seed("fmt1", 0, with(1, &dump));
    put_seed("fmt1_all_types", 0, with(1, &blk_all_types()));
    put_seed("fmt3", 1, with(3, &dump));
    put_seed("fmt4", 1, with(4, &zstd(&dump)));
    let packed = zstd(&with(1, &dump));
    let mut f2 = vec![2, packed.len() as u8, (packed.len() >> 8) as u8, 0];
    f2.extend_from_slice(&packed);
    put_seed("fmt2", 0, f2);
    // Format 6: one name, a root and two children sharing a parameter range
    let mut f6 = vec![6u8, 1, 2, b'a', 0];
    varint(3, &mut f6);
    f6.extend([1, 0]);
    put(&mut f6, 2 << 24);
    put(&mut f6, 7);
    f6.extend([0, 0, 2, 1]);
    f6.extend([1, 1, 0, 0]);
    f6.extend([1, 1, 0, 0]);
    put_seed("fmt6", 0, f6);
}

fn namemap_seed(dir: &Path) {
    let names = b"alpha\0beta\0gamma\0";
    let mut raw = Vec::new();
    varint(3, &mut raw);
    varint(names.len() as u32, &mut raw);
    raw.extend_from_slice(names);
    let mut f = vec![0u8; 8];
    f.extend_from_slice(&[0x11; 32]);
    f.extend_from_slice(&zstd(&raw));
    fs::write(dir.join("namemap"), f).unwrap();
}

// ---- vromfs ----------------------------------------------------------------------------------------------

fn xor_ends(data: &mut [u8]) {
    const KEY: [u32; 4] = [0xAA55_AA55, 0xF00F_F00F, 0xAA55_AA55, 0x1248_1248];
    let xor = |block: &mut [u8], key: [u32; 4]| {
        for (c, k) in block.chunks_exact_mut(4).zip(key) {
            let w = u32::from_le_bytes(c.try_into().unwrap()) ^ k;
            c.copy_from_slice(&w.to_le_bytes());
        }
    };
    let len = data.len();
    if len < 16 {
        return;
    }
    xor(&mut data[..16], KEY);
    if len >= 32 {
        let end = len / 4 * 4;
        let mut rev = KEY;
        rev.reverse();
        xor(&mut data[end - 16..end], rev);
    }
}

/// The unpacked body of an archive: the tables, the names and the files
fn vromfs_body(files: &[(&[u8], &[u8])]) -> Vec<u8> {
    let n = files.len() as u32;
    let names_ofs = 32u32;
    let data_ofs = names_ofs + 8 * n;
    let mut strings_at = data_ofs + 16 * n;
    let mut body = Vec::new();
    for (ofs, count) in [(names_ofs, n), (data_ofs, n)] {
        put(&mut body, ofs);
        put(&mut body, count);
        body.extend_from_slice(&[0; 8]);
    }
    let mut name_ptrs = Vec::new();
    for (name, _) in files {
        name_ptrs.push(strings_at);
        strings_at += name.len() as u32 + 1;
    }
    let mut file_at = strings_at;
    let mut entries = Vec::new();
    for (_, data) in files {
        entries.push((file_at, data.len() as u32));
        file_at += data.len() as u32;
    }
    for p in name_ptrs {
        body.extend_from_slice(&(p as u64).to_le_bytes());
    }
    for (ofs, size) in entries {
        put(&mut body, ofs);
        put(&mut body, size);
        body.extend_from_slice(&[0; 8]);
    }
    for (name, _) in files {
        body.extend_from_slice(name);
        body.push(0);
    }
    for (_, data) in files {
        body.extend_from_slice(data);
    }
    body
}

fn vromfs_file(label: &[u8; 4], body: &[u8], packed: bool) -> Vec<u8> {
    let mut out = label.to_vec();
    out.extend_from_slice(b"pc\0\0");
    put(&mut out, body.len() as u32);
    let payload = if packed {
        let mut z = zstd(body);
        xor_ends(&mut z);
        put(&mut out, z.len() as u32 | 0x4000_0000);
        z
    } else {
        put(&mut out, 0);
        Vec::new()
    };
    if label == b"VRFx" {
        out.extend_from_slice(&[12, 0, 0, 0]); // ext size 12, flags
        put(&mut out, 2 << 24 | 59 << 16 | 80);
        out.extend_from_slice(&[0; 4]);
    }
    out.extend_from_slice(if packed { &payload } else { body });
    out.extend_from_slice(&[0; 16]); // md5 (wrong on purpose: only the flag changes)
    out
}

fn vromfs_seeds(dir: &Path) {
    let dump = [&[1u8][..], &blk_dump()].concat();
    let nm = {
        let mut raw = Vec::new();
        varint(2, &mut raw);
        varint(8, &mut raw);
        raw.extend_from_slice(b"one\0two\0");
        let mut f = vec![0u8; 8];
        f.extend_from_slice(&[0x22; 32]);
        f.extend_from_slice(&zstd(&raw));
        f
    };
    let plain = vromfs_body(&[(b"a.blk", &dump), (b"dir/b.blk", &dump)]);
    fs::write(dir.join("stored"), vromfs_file(b"VRFs", &plain, false)).unwrap();
    fs::write(dir.join("zstd"), vromfs_file(b"VRFs", &plain, true)).unwrap();
    fs::write(dir.join("vrfx"), vromfs_file(b"VRFx", &plain, true)).unwrap();
    let with_nm = vromfs_body(&[("\u{fffd}?nm".as_bytes(), &nm), (b"a.blk", &[3u8, 0, 1, 0, 0, 0, 0][..]), (b"e", b"")]);
    fs::write(dir.join("namemap"), vromfs_file(b"VRFs", &with_nm, true)).unwrap();
}

// ---- rodb, land class, collision, rendinst ---------------------------------------------------------------

/// root { n:i=7  s:t="hi"  <child>{ } }; `child` is the name of the one child block
fn rodb_dump(child: &str) -> Vec<u8> {
    let mut d = Vec::new();
    put(&mut d, 0x88);
    put(&mut d, 2);
    d.extend_from_slice(&[0; 8]);
    put(&mut d, 0x58);
    put(&mut d, 1);
    d.extend_from_slice(&[0; 8]);
    put(&mut d, 0x30);
    put(&mut d, 0);
    put(&mut d, u32::MAX);
    put(&mut d, 0);
    put(&mut d, 0x40);
    put(&mut d, 3);
    d.extend_from_slice(&[0; 8]);
    let names_at = 0x98u32;
    let offs = [names_at, names_at + child.len() as u32 + 1, names_at + child.len() as u32 + 3];
    for o in offs {
        put(&mut d, o);
        put(&mut d, 0);
    }
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
    put(&mut d, 7);
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    put(&mut d, offs[2] + 2 - 0x30);
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(child.as_bytes());
    d.extend_from_slice(b"\0n\0s\0hi\0");
    d
}

fn nested(codec: u32, data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32 | codec << 30).to_le_bytes().to_vec();
    out.extend_from_slice(data);
    out
}

fn dstr(s: &str) -> Vec<u8> {
    let mut out = le(s.len() as u32).to_vec();
    out.extend_from_slice(s.as_bytes());
    out.resize(out.len().next_multiple_of(4), 0);
    out
}

fn small_seeds(corpus: &Path) {
    fs::write(corpus.join("rodb/small"), rodb_dump("child")).unwrap();
    let dump = rodb_dump("detail");
    let mut raw = le(dump.len() as u32).to_vec();
    raw.extend_from_slice(&dump);
    fs::write(corpus.join("landclass_resource/detail"), nested(1, &zstd(&raw))).unwrap();

    // Collision: an empty resource (the counts of the eleven arrays all zero)
    let mut body = vec![0u8; 32 + 24 + 8];
    body.extend_from_slice(&[0; 44]);
    let mut res = le(0xACE5_0003).to_vec();
    put(&mut res, body.len() as u32);
    res.extend_from_slice(&body);
    fs::write(corpus.join("collision/empty"), &res).unwrap();
    let mut res = le(0xACE5_0003).to_vec();
    let z = zstd(&body);
    put(&mut res, z.len() as u32 | 1 << 30);
    res.extend_from_slice(&z);
    fs::write(corpus.join("collision/zstd"), &res).unwrap();

    // RendInst: one vertex buffer (three float3 positions and one triangle) and one LOD with one element
    let mut vdata = vec![0u8; 32];
    for w in [3u32, 12, 6, 0x20, 64, 1, 0, 0] {
        put(&mut vdata, w);
    }
    vdata[16..20].copy_from_slice(&32u32.to_le_bytes());
    vdata[20..24].copy_from_slice(&1u32.to_le_bytes());
    for w in [2u32 << 16, 0, 0] {
        put(&mut vdata, w);
    }
    let header_len = vdata.len() as u32;
    vdata.extend_from_slice(&f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]));
    for i in [0u16, 1, 2] {
        vdata.extend_from_slice(&i.to_le_bytes());
    }
    let mut dump = vec![0u8; 56];
    dump[0..4].copy_from_slice(&40u32.to_le_bytes());
    dump[4..8].copy_from_slice(&1u32.to_le_bytes());
    dump[44..48].copy_from_slice(&50.0f32.to_bits().to_le_bytes());
    let mut mesh = vec![0u8; 96];
    mesh[0..4].copy_from_slice(&48u32.to_le_bytes());
    mesh[4..8].copy_from_slice(&1u32.to_le_bytes());
    mesh[32..36].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    mesh[48 + 36..48 + 40].copy_from_slice(&1u32.to_le_bytes()); // one triangle
    let mut res = Vec::new();
    for w in [dump.len() as u32, u32::MAX, u32::MAX, 1, header_len] {
        put(&mut res, w);
    }
    res.extend_from_slice(&vdata);
    res.extend_from_slice(&dump);
    put(&mut res, mesh.len() as u32);
    put(&mut res, 0);
    res.extend_from_slice(&mesh);
    fs::write(corpus.join("rendinst_model/one_lod"), res).unwrap();
}

// ---- levels -----------------------------------------------------------------------------------------------

fn block(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = le(payload.len() as u32 + 4).to_vec();
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    out
}

fn level(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"DBLD3x64".to_vec();
    put(&mut out, 0);
    for b in blocks {
        out.extend_from_slice(b);
    }
    out.extend(block(b"\0END", &[]));
    out
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn heightmap(version: u32, w: u32, h: u32) -> Vec<u8> {
    let mut d = f32s(&[2.0, -10.0, 100.0, 0.0, 0.0]);
    put(&mut d, w | version << 24);
    put(&mut d, h);
    d.extend_from_slice(&[0; 16]);
    match version {
        2 => {
            put(&mut d, 3); // blocks of 8 x 8
            let blocks = (w / 8 * (h / 8)) as usize;
            let mut raw = Vec::new();
            for b in 0..blocks {
                raw.extend_from_slice(&((b as u16) * 100).to_le_bytes());
                raw.extend_from_slice(&200u16.to_le_bytes());
            }
            raw.extend((0..blocks * 64).map(|i| (i % 7) as u8));
            d.extend(nested(1, &zstd(&raw)));
        }
        _ => {
            let raw: Vec<u8> = (0..w * h).flat_map(|i| ((i * 37) as u16).to_le_bytes()).collect();
            d.extend(nested(1, &zstd(&raw)));
        }
    }
    d
}

fn ltdump() -> Vec<u8> {
    let mut raw = Vec::new();
    put(&mut raw, 0);
    raw.extend_from_slice(b"LTdump");
    put(&mut raw, 1);
    put(&mut raw, 1);
    raw.extend_from_slice(&[0; 40]);
    put(&mut raw, 1); // one cell
    raw.extend_from_slice(&f32s(&[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0]));
    raw.extend_from_slice(&[0; 3 * 4 + 4 + 4 + 4 + 8 + 4 + 4]);
    raw.truncate(raw.len().min(4 + 6 + 8 + 40 + 4 + 64));
    while raw.len() < 4 + 6 + 8 + 40 + 4 + 64 {
        raw.push(0);
    }
    put(&mut raw, 0); // height grid
    put(&mut raw, 0); // grid
    put(&mut raw, 3); // face indices
    for i in 0..3u16 {
        raw.extend_from_slice(&i.to_le_bytes());
    }
    put(&mut raw, 3); // vertices
    for v in [[0u16, 0, 0, 0], [10, 0, 0, 0], [0, 0, 10, 0]] {
        for c in v {
            raw.extend_from_slice(&c.to_le_bytes());
        }
    }
    raw
}

fn landmesh(with_tracer: bool) -> Vec<u8> {
    let mut d = b"lndm".to_vec();
    put(&mut d, 4);
    d.extend_from_slice(&f32s(&[32.0, 64.0]));
    for v in [2i32, 2, 0, 0] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    put(&mut d, 0); // no tile texture
    let base = d.len();
    let mut tail = Vec::new();
    if with_tracer {
        tail.extend(nested(1, &zstd(&ltdump())));
    }
    tail.extend(dstr("")); // no vertical texture
    let detail_ofs = 16 + tail.len();
    for v in [0i32, detail_ofs as i32, 16, if with_tracer { 20 } else { 0 }] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.extend(tail);
    assert_eq!(d.len() - base, detail_ofs);
    // detail data: one land class, a 2 x 2 grid of 4-texel cells
    let mut class = dstr("grass");
    class.extend_from_slice(b"");
    let mut classes = le(1).to_vec();
    classes.extend(nested(0, &class));
    d.extend(nested(0, &classes));
    for v in [2i32, 2, 4, 4] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    let offsets_at = d.len();
    let rec_at = offsets_at + 16;
    for _ in 0..4 {
        d.extend_from_slice(&((rec_at - base) as i32).to_le_bytes());
    }
    d.extend_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    put(&mut d, 0);
    put(&mut d, 0);
    d
}

/// A RIGz layer: one cell, one land class, one position-only pool and two placed objects
fn rigz() -> Vec<u8> {
    let mut dump = vec![0u8; 160];
    let put_at = |dump: &mut Vec<u8>, at: usize, bytes: &[u8]| dump[at..at + bytes.len()].copy_from_slice(bytes);
    let (cell_at, lc_at, pool_at, str_at, counters_at) = (160u32, 712u32, 744u32, 776u32, 800u32);
    put_at(&mut dump, 8, &le(cell_at));
    put_at(&mut dump, 12, &le(1));
    put_at(&mut dump, 24, &1i32.to_le_bytes());
    put_at(&mut dump, 28, &1i32.to_le_bytes());
    put_at(&mut dump, 32, &64i16.to_le_bytes());
    put_at(&mut dump, 48, &le(lc_at));
    put_at(&mut dump, 52, &le(1));
    put_at(&mut dump, 112, &1.0f32.to_le_bytes());
    put_at(&mut dump, 128, &le(pool_at));
    put_at(&mut dump, 132, &le(1));
    let mut cell = vec![0u8; 552];
    cell[24..26].copy_from_slice(&0i16.to_le_bytes());
    cell[28..32].copy_from_slice(&0i32.to_le_bytes()); // data offset 0
    for s in 0..=64usize {
        let p = if s == 0 { counters_at } else { counters_at + 4 };
        cell[32 + s * 8..36 + s * 8].copy_from_slice(&le(p));
    }
    dump.extend_from_slice(&cell);
    let mut lc = vec![0u8; 32];
    lc[0..4].copy_from_slice(&le(str_at));
    dump.extend_from_slice(&lc);
    let mut pool = vec![0u8; 32];
    pool[8..12].copy_from_slice(&le(str_at + 3));
    pool[24..28].copy_from_slice(&le(1)); // position-only
    dump.extend_from_slice(&pool);
    dump.extend_from_slice(b"lc\0pool\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
    dump.truncate(counters_at as usize);
    dump.resize(counters_at as usize, 0);
    put(&mut dump, 2 << 10); // pool 0, two objects
    let mut raw = le(dump.len() as u32).to_vec();
    raw.extend_from_slice(&dump);
    let inst: Vec<u8> = [100i16, 200, 300, 256, 400, 500, 600, 256].iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut pregen = nested(1, &zstd(&inst));
    pregen.resize(pregen.len() + 200, 0); // the loop reads layers while more than a dump header is left
    let mut out = nested(1, &zstd(&raw));
    out.extend(nested(0, &pregen));
    out
}

fn level_seeds(corpus: &Path) {
    let mut ever = dstr("DaEditorX ver 5.0");
    put(&mut ever, 1);
    ever.extend_from_slice(&[7; 20]);
    ever.extend_from_slice(b"lmap");
    ever.extend_from_slice(&[9; 20]);
    let mut dxp = dstr("~");
    put(&mut dxp, 2);
    dxp.extend(dstr("a*"));
    dxp.extend(dstr("tex_b*"));
    let mut lmp = le(1).to_vec();
    put(&mut lmp, 2);
    lmp.extend(dstr("default"));
    lmp.extend(dstr("road"));
    put(&mut lmp, 2048);
    put(&mut lmp, 2048);
    lmp.extend_from_slice(&f32s(&[-100.0, 50.0, 2.0]));
    let bbox = f32s(&[-1.0, -2.0, -3.0, 1.0, 2.0, 3.0]);
    // RqRL: a name map of two names (table, then pointers, then the strings)
    let mut rq = Vec::new();
    put(&mut rq, 16);
    put(&mut rq, 2);
    rq.extend_from_slice(&[0; 8]);
    rq.extend_from_slice(&32u64.to_le_bytes());
    rq.extend_from_slice(&36u64.to_le_bytes());
    rq.extend_from_slice(b"one\0two\0");
    let mut tex = dstr("tex_a");
    tex.extend(nested(1, &zstd(b"DDSx")));

    let write = |dir: &str, name: &str, data: &[u8]| fs::write(corpus.join(dir).join(name), data).unwrap();
    let lm = landmesh(true);
    let hm = heightmap(2, 16, 16);
    let rg = rigz();
    write(
        "level",
        "container",
        &level(&[block(b"eVER", &ever), block(b"RqRL", &rq), block(b"DxP2", &dxp), block(b"\0TEX", &tex), block(b"WBBX", &bbox), block(b"LMp2", &lmp)]),
    );
    write("level", "landmesh", &level(&[block(b"lmap", &lm), block(b"\0HM2", &hm)]));
    write("level", "heightmap_v2", &level(&[block(b"\0HM2", &hm)]));
    write("level", "rigz", &level(&[block(b"RIGz", &rg)]));
    for (i, (tag, payload)) in [(b"eVER", &ever), (b"RqRL", &rq), (b"DxP2", &dxp), (b"\0TEX", &tex), (b"WBBX", &bbox), (b"LMp2", &lmp), (b"RIGz", &rg)].into_iter().enumerate() {
        write("level_block", &format!("block{i}_{}", String::from_utf8_lossy(tag).trim_matches('\0')), &[&[i as u8][..], payload].concat());
    }
    write("level_landmesh", "tracer", &lm);
    write("level_landmesh", "plain", &landmesh(false));
    write("level_heightmap", "v0", &heightmap(0, 8, 8));
    write("level_heightmap", "v1", &heightmap(1, 8, 8));
    write("level_heightmap", "v2", &hm);
    write("level_rendinst_layers", "one_layer", &rg);
}

// ---- sound ------------------------------------------------------------------------------------------------

/// An FSB5 with one sample: codec mode, the sample's header chunks, `frames` and the sample data
fn fsb5(mode: u32, rate_index: u64, channels_index: u64, frames: u64, chunks: &[u8], data: &[u8]) -> Vec<u8> {
    let sample_mode = (rate_index << 1) | (channels_index << 5) | (frames << 34) | u64::from(!chunks.is_empty());
    let header_bytes = 8 + chunks.len() as u32;
    let mut f = b"FSB5".to_vec();
    put(&mut f, 1);
    put(&mut f, 1);
    put(&mut f, header_bytes);
    put(&mut f, 0);
    put(&mut f, data.len() as u32);
    put(&mut f, mode);
    f.resize(0x3C, 0);
    f.extend_from_slice(&sample_mode.to_le_bytes());
    f.extend_from_slice(chunks);
    f.extend_from_slice(data);
    f
}

fn sound_seeds(corpus: &Path) {
    let pcm: Vec<u8> = (0..64u8).flat_map(|i| (i16::from(i) * 300).to_le_bytes()).collect();
    let crcs = wt_unpack_fuzz::setup_crcs();
    let write = |dir: &str, name: &str, data: &[u8]| fs::write(corpus.join(dir).join(name), data).unwrap();
    write("fsb5", "pcm16", &fsb5(2, 8, 0, 64, &[], &pcm));
    write("fsb5", "pcm8_stereo", &fsb5(1, 9, 1, 32, &[], &pcm[..64]));
    write("fsb5", "fadpcm", &fsb5(16, 8, 0, 256, &[], &[0x55; 140 * 2]));
    // Vorbis chunk: more bit 0, size 4 in bits 1..24, kind 11 in the top bits
    let chunk = 1u32 << 25 | 11 << 25 | 4 << 1;
    let chunk = (chunk & !(0x7F << 25)) | 11 << 25;
    let mut chunks = le(chunk).to_vec();
    chunks.extend_from_slice(&crcs[0].to_le_bytes());
    let mut packets = Vec::new();
    for len in [40u16, 17, 90] {
        packets.extend_from_slice(&len.to_le_bytes());
        packets.extend((0..len).map(|i| (i * 7) as u8));
    }
    packets.extend_from_slice(&[0, 0]);
    write("fsb5", "vorbis", &fsb5(15, 8, 1, 2048, &chunks, &packets));

    let bank = fsb5(2, 8, 0, 64, &[], &pcm);
    let mut chunk_data = vec![0u8; 12];
    chunk_data.extend_from_slice(&bank);
    let mut riff = b"RIFF".to_vec();
    put(&mut riff, 4 + 8 + chunk_data.len() as u32);
    riff.extend_from_slice(b"FEV ");
    riff.extend_from_slice(b"SND ");
    put(&mut riff, chunk_data.len() as u32);
    riff.extend_from_slice(&chunk_data);
    write("fsb5_bank", "snd", &riff);

    // The same bank with an SNDH table: LIST PROJ { SNDH }, then the SND chunk the table points into
    let mut sndh = le(0).to_vec();
    let snd_data_at = 12 + 8 + 4 + 8 + 12 + 8;
    put(&mut sndh, snd_data_at as u32);
    put(&mut sndh, bank.len() as u32);
    let mut list = b"PROJ".to_vec();
    list.extend_from_slice(b"SNDH");
    put(&mut list, sndh.len() as u32);
    list.extend_from_slice(&sndh);
    let mut riff = b"RIFF".to_vec();
    put(&mut riff, 0);
    riff.extend_from_slice(b"FEV ");
    riff.extend_from_slice(b"LIST");
    put(&mut riff, list.len() as u32);
    riff.extend_from_slice(&list);
    riff.extend_from_slice(b"SND ");
    put(&mut riff, bank.len() as u32);
    riff.extend_from_slice(&bank);
    let total = riff.len() as u32 - 8;
    riff[4..8].copy_from_slice(&total.to_le_bytes());
    write("fsb5_bank", "sndh", &riff);

    for (i, frames) in [2048u32, 0, 100_000].into_iter().enumerate() {
        let mut s = vec![i as u8, 0, 2, 2];
        s.extend_from_slice(&frames.to_le_bytes());
        s.extend_from_slice(&packets);
        write("vorbis_sample", &format!("packets{i}"), &s);
    }
}

fn main() {
    let corpus = std::env::args().nth(1).expect("usage: make_seeds <corpus dir>");
    let corpus = Path::new(&corpus);
    for t in [
        "vromfs", "blk", "blk_namemap", "rodb", "landclass_resource", "collision", "rendinst_model", "level", "level_block", "level_landmesh",
        "level_heightmap", "level_rendinst_layers", "fsb5", "fsb5_bank", "vorbis_sample", "live_json", "drive_script",
    ] {
        fs::create_dir_all(corpus.join(t)).unwrap();
    }
    blk_seeds(&corpus.join("blk"));
    namemap_seed(&corpus.join("blk_namemap"));
    vromfs_seeds(&corpus.join("vromfs"));
    small_seeds(corpus);
    level_seeds(corpus);
    sound_seeds(corpus);
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/live");
    for e in fs::read_dir(samples).unwrap() {
        let e = e.unwrap();
        fs::copy(e.path(), corpus.join("live_json").join(e.file_name())).unwrap();
    }
    for (i, s) in ["W:20,WA:3,S:2", "-:1", "W:0.5, D:2 ,A:1e3", "WASD:NaN"].iter().enumerate() {
        fs::write(corpus.join("drive_script").join(format!("s{i}")), s).unwrap();
    }
}
