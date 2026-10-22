//! Reader for DynModel game resources (Dagor's `DynamicRenderableSceneLodsResource`) and the
//! GeomNodeTree skeletons that place their parts.
//!
//! DynModel stream, as read by `DynamicRenderableSceneLodsResource::loadResource` (dynSceneRes.cpp):
//!
//! ```text
//! u32 lods_dump_size
//! u32 tex_count, mat_count     both 0xFFFFFFFF: materials live in dynModelDesc.bin
//! u32 vdata_count, hdr_size    hdr_size top bits 0b110: compressed block follows
//! block: u32 (tag << 30 | size), then a zstd frame or u32 unpacked size + Oodle stream.
//!   Unpacked: header (mat tab, vdata tab of 32-byte entries with vertex declarations),
//!   then per vdata: vertices (count * stride), indices (meshopt-encoded when flag 0x200)
//! lods dump: lods tab, bbox, position unpack offset (+0x28) and scale (+0x38), LOD records at
//!   the tab offset: {u64 scene dump size, f32 range, f32 tex scale}
//! u32 size + node name map dump
//! per LOD: scene dump (rigids tab of 32-byte {u64 mesh size, sphere, i32 node, u32}, skins tab),
//!   then per rigid a ShaderMesh dump whose 48-byte elements give material, vdata and index ranges
//! ```

use std::collections::HashMap;
use std::io::Read;

use anyhow::{bail, ensure, Context, Result};

use crate::bytes::Reader;

// Vertex channel types (VSDT_*) and usages (SCUSAGE_*) from Dagor's dag_consts_base.h / dag_shaderCommon.h
const T_FLOAT2: u32 = 1;
const T_FLOAT3: u32 = 2;
const T_E3DCOLOR: u32 = 4;
const T_SHORT2: u32 = 6;
const T_SHORT4N: u32 = 10;
const T_HALF2: u32 = 15;
const U_POS: u32 = 0;
const U_NORM: u32 = 1;
const U_TC: u32 = 3;

const VDATA_I32: u32 = 0x40;
const VDATA_PACKED_IB: u32 = 0x200;

#[derive(Debug, Clone, Copy)]
pub struct Channel {
    pub ty: u32,
    pub usage: u32,
    pub index: u16,
    pub offset: usize,
}

pub struct VData {
    pub stride: usize,
    pub vertices: Vec<u8>,
    pub indices: Vec<u32>,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Copy)]
pub struct Elem {
    pub material: usize,
    pub vdata: usize,
    pub first_index: usize,
    pub triangles: usize,
    pub base_vertex: usize,
}

pub struct Rigid {
    pub node: String,
    pub elems: Vec<Elem>,
}

/// A skinned mesh (tracks, cables): elements index into `DynModel::skin_vdata`
pub struct Skin {
    pub node: String,
    pub elems: Vec<Elem>,
}

pub struct Lod {
    pub range: f32,
    pub rigids: Vec<Rigid>,
    pub skins: Vec<Skin>,
}

pub struct DynModel {
    pub vdata: Vec<VData>,
    pub skin_vdata: Vec<VData>,
    pub pos_offset: [f32; 3],
    pub pos_scale: [f32; 3],
    pub lods: Vec<Lod>,
}

/// One decoded vertex in model space before node transforms
#[derive(Debug, Clone, Copy, Default)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

fn read_block<'a>(r: &mut Reader<'a>) -> Result<&'a [u8]> {
    let head = r.u32()?;
    r.bytes((head & 0x3FFF_FFFF) as usize)
}

/// A compressed block body: zstd frame, or u32 unpacked size followed by an Oodle stream
fn unpack_block(block: &[u8]) -> Result<Vec<u8>> {
    if block.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Ok(zstd::stream::decode_all(block)?);
    }
    let mut r = Reader::new(block);
    let size = r.u32()? as usize;
    let mut out = vec![0u8; size];
    let n = oozextract::Extractor::new()
        .read_from_slice(r.rest(), &mut out)
        .map_err(|e| anyhow::anyhow!("Oodle: {e:?}"))?;
    ensure!(n == size, "Oodle produced {n} of {size} bytes");
    Ok(out)
}

fn f32_at(b: &[u8], ofs: usize) -> f32 {
    f32::from_le_bytes(b[ofs..ofs + 4].try_into().unwrap())
}

fn read_vdata(vd: &[u8], payload_start: usize) -> Result<Vec<VData>> {
    let mut h = Reader::new(vd);
    h.skip(16)?; // material table, empty when materials come from dynModelDesc.bin
    let (tab_ofs, count) = (h.u32()? as usize, h.u32()? as usize);
    let mut payload = Reader::at(vd, payload_start)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut e = Reader::at(vd, tab_ofs + i * 32)?;
        let vert_num = e.u32()? as usize;
        let w1 = e.u32()?;
        let w2 = e.u32()?;
        let flags = e.u32()?;
        let (decl_ofs, decl_n) = (e.u32()? as usize, e.u32()? as usize);
        let stride = (w1 & 0xFF) as usize;
        let idx_size = (w2 & 0x0FFF_FFFF) as usize;
        let packed = if flags & VDATA_PACKED_IB != 0 { ((w1 >> 8) | ((w2 >> 28) << 24)) as usize } else { idx_size };
        let wide = flags & VDATA_I32 != 0 || (vert_num > 65536 && flags & 0x20 == 0);
        let idx_count = idx_size / if wide { 4 } else { 2 };

        let mut channels = Vec::with_capacity(decl_n);
        let mut d = Reader::at(vd, decl_ofs)?;
        let mut offset = 0;
        for _ in 0..decl_n {
            let ty = d.u32()? >> 16;
            let usage = d.u32()?;
            let index = d.u16()?;
            d.skip(2)?;
            channels.push(Channel { ty, usage, index, offset });
            offset += match ty {
                T_FLOAT2 | T_SHORT4N => 8,
                T_FLOAT3 => 12,
                T_E3DCOLOR | T_SHORT2 | T_HALF2 => 4,
                other => bail!("vertex channel type {other} not handled"),
            };
        }
        ensure!(offset == stride, "vertex declaration covers {offset} of {stride} bytes");

        let vertices = payload.bytes(vert_num * stride)?.to_vec();
        let raw = payload.bytes(packed)?;
        let indices = if flags & VDATA_PACKED_IB != 0 {
            if wide {
                meshopt::decode_index_sequence::<u32>(raw, idx_count)?
            } else {
                meshopt::decode_index_sequence::<u16>(raw, idx_count)?.into_iter().map(u32::from).collect()
            }
        } else if wide {
            raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
        } else {
            raw.chunks_exact(2).map(|c| u32::from(u16::from_le_bytes([c[0], c[1]]))).collect()
        };
        out.push(VData { stride, vertices, indices, channels });
    }
    ensure!(payload.rest().is_empty(), "vertex data has trailing bytes");
    Ok(out)
}

/// ShaderMesh at `at` inside `dump`; its pointers are relative to the start of `dump`
fn read_mesh(dump: &[u8], at: usize) -> Result<Vec<Elem>> {
    let mut r = Reader::at(dump, at)?;
    let (elems_ofs, count) = (r.u32()? as usize, r.u32()? as usize);
    let mut elems = Vec::with_capacity(count);
    for i in 0..count {
        let mut e = Reader::at(dump, elems_ofs + i * 48)?;
        e.skip(8)?; // shader element
        let material = e.u64()? as usize;
        let vdata = e.u64()? as usize;
        let _order = e.u32()?;
        let (_sv, _numv) = (e.u32()?, e.u32()?);
        let first_index = e.u32()? as usize;
        let triangles = e.u32()? as usize;
        let base_vertex = e.u32()? as usize;
        elems.push(Elem { material, vdata, first_index, triangles, base_vertex });
    }
    Ok(elems)
}

impl DynModel {
    pub fn parse(res: &[u8]) -> Result<Self> {
        let mut r = Reader::new(res);
        let lods_dump_size = r.u32()? as usize;
        let (tex_count, _mat_count, _vdata_count, hdr_size) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        ensure!(tex_count == 0xFFFF_FFFF, "models with embedded materials are not handled yet");
        ensure!(hdr_size & 0xE000_0000 == 0xC000_0000, "uncompressed vertex data is not handled yet");
        let vd = unpack_block(read_block(&mut r)?)?;
        let vdata = read_vdata(&vd, (hdr_size & !0xC000_0000) as usize)?;

        let lods_dump = r.bytes(lods_dump_size)?;
        let pos_offset = [f32_at(lods_dump, 0x28), f32_at(lods_dump, 0x2C), f32_at(lods_dump, 0x30)];
        let pos_scale = [f32_at(lods_dump, 0x38), f32_at(lods_dump, 0x3C), f32_at(lods_dump, 0x40)];

        let names_size = r.u32()? as usize;
        let names_dump = r.bytes(names_size)?;
        // RoNameMapEx: names sorted for binary search, plus a table mapping each sorted slot to its node id
        let mut nr = Reader::new(names_dump);
        let (names_ofs, names_count) = (nr.u32()? as usize, nr.u32()? as usize);
        nr.skip(8)?;
        let (ids_ofs, ids_count) = (nr.u32()? as usize, nr.u32()? as usize);
        ensure!(ids_count == names_count, "node name map and id table differ in size");
        let mut node_names = vec![String::new(); names_count];
        for i in 0..names_count {
            let name = Reader::at(names_dump, Reader::at(names_dump, names_ofs + i * 8)?.u64()? as usize)?.cstr()?;
            let id = Reader::at(names_dump, ids_ofs + i * 2)?.u16()? as usize;
            *node_names.get_mut(id).context("node id out of range")? = name;
        }

        let mut lt = Reader::new(lods_dump);
        let (lod_ofs, lod_count) = (lt.u32()? as usize, lt.u32()? as usize);
        let mut lods = Vec::with_capacity(lod_count);
        let mut skin_counts = Vec::with_capacity(lod_count);
        for l in 0..lod_count {
            let mut rec = Reader::at(lods_dump, lod_ofs + l * 16)?;
            let scene_size = rec.u64()? as usize;
            let range = f32::from_bits(rec.u32()?);
            let scene = r.bytes(scene_size)?;
            let mut sr = Reader::new(scene);
            let (rig_ofs, rig_n) = (sr.u32()? as usize, sr.u32()? as usize);
            sr.skip(8)?;
            let _skins_ofs = sr.u32()?;
            let skin_count = sr.u32()? as usize;
            let mut rigids = Vec::with_capacity(rig_n);
            for i in 0..rig_n {
                let mut ro = Reader::at(scene, rig_ofs + i * 32)?;
                let mesh_size = ro.u64()? as usize;
                ro.skip(16)?; // bounding sphere
                let node = ro.u32()? as usize;
                let node = node_names.get(node).cloned().context("rigid node id out of range")?;
                rigids.push(Rigid { node, elems: read_mesh(r.bytes(mesh_size)?, 0)? });
            }
            lods.push(Lod { range, rigids, skins: Vec::with_capacity(skin_count) });
            skin_counts.push(skin_count);
        }

        let mut skin_vdata = Vec::new();
        if skin_counts.iter().any(|&n| n > 0) {
            skin_vdata = read_skins(&mut r, &mut lods, &skin_counts, &node_names)?;
        }
        Ok(DynModel { vdata, skin_vdata, pos_offset, pos_scale, lods })
    }

    pub fn vertex(&self, vdata: usize, index: usize) -> Vertex {
        self.vertex_in(&self.vdata[vdata], index)
    }

    /// Decodes vertex `index` of any vertex data of this model (rigid or skinned)
    pub fn vertex_in(&self, vd: &VData, index: usize) -> Vertex {
        let v = &vd.vertices[index * vd.stride..(index + 1) * vd.stride];
        let i16_at = |o: usize| i16::from_le_bytes([v[o], v[o + 1]]);
        let mut out = Vertex::default();
        for c in &vd.channels {
            match (c.usage, c.index, c.ty) {
                (U_POS, _, T_SHORT4N) => {
                    for k in 0..3 {
                        let n = f32::from(i16_at(c.offset + k * 2)) / 32767.0;
                        out.pos[k] = n * self.pos_scale[k] + self.pos_offset[k];
                    }
                }
                (U_POS, _, T_FLOAT3) => {
                    for k in 0..3 {
                        out.pos[k] = f32_at(v, c.offset + k * 4);
                    }
                }
                // E3DCOLOR is stored as B, G, R, A bytes
                (U_NORM, _, T_E3DCOLOR) => {
                    let b = &v[c.offset..c.offset + 4];
                    out.normal = [b[2], b[1], b[0]].map(|x| f32::from(x) / 127.5 - 1.0);
                }
                (U_TC, 0, T_SHORT2) => {
                    out.uv = [f32::from(i16_at(c.offset)), f32::from(i16_at(c.offset + 2))];
                }
                (U_TC, 0, T_FLOAT2) => out.uv = [f32_at(v, c.offset), f32_at(v, c.offset + 4)],
                _ => {}
            }
        }
        out
    }
}

/// World transforms of a GeomNodeTree, by node name (column-major 4x4)
pub fn parse_skeleton(res: &[u8]) -> Result<HashMap<String, [f32; 16]>> {
    let mut r = Reader::new(res);
    let ofs = r.u32()? as usize;
    let count_raw = r.u32()?;
    let count = (count_raw & 0x7FFF_FFFF) as usize;
    let data = if count_raw & 0x8000_0000 != 0 { unpack_block(read_block(&mut r)?)? } else { r.rest().to_vec() };
    let size = ofs & 0xF_FFFF;
    ensure!(data.len() >= size && size >= count * 160, "skeleton data is truncated");

    let mut out = HashMap::with_capacity(count);
    for i in 0..count {
        let node = &data[i * 160..(i + 1) * 160];
        let mut wtm = [0f32; 16];
        for (k, w) in wtm.iter_mut().enumerate() {
            *w = f32_at(node, 64 + k * 4);
        }
        let name_ofs = Reader::at(node, 152)?.u64()? as usize;
        out.insert(Reader::at(&data, name_ofs)?.cstr()?, wtm);
    }
    Ok(out)
}

/// Skinned meshes follow the LODs: a table of variants per GPU constant budget, a vertex data block
/// like the rigid one, then a zlib stream with, per LOD, the skin node ids, the mesh sizes and the
/// ShaderSkinnedMesh dumps (bone tables followed by a ShaderMesh at +0x38)
fn read_skins(r: &mut Reader, lods: &mut [Lod], counts: &[usize], node_names: &[String]) -> Result<Vec<VData>> {
    const MAX_VS_CONSTS: u32 = 4096;
    let section = r.pos();
    let (variants, total) = (r.u32()?, r.u32()? as usize);
    let mut chosen = false;
    for _ in 0..variants {
        let (consts, skip) = (r.u32()?, r.u32()? as usize);
        if consts <= MAX_VS_CONSTS {
            r.skip(skip)?;
            chosen = true;
            break;
        }
    }
    ensure!(chosen, "no skinned mesh variant fits");

    let (tex_count, _mat_count, _vdata_count, hdr_size) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
    ensure!(tex_count == 0xFFFF_FFFF, "skins with embedded materials are not handled yet");
    ensure!(hdr_size & 0xE000_0000 == 0xC000_0000, "uncompressed skin vertex data is not handled yet");
    let vd = unpack_block(read_block(r)?)?;
    let vdata = read_vdata(&vd, (hdr_size & !0xC000_0000) as usize)?;

    let end = section + 8 + total;
    let zipped = r.bytes(end.checked_sub(r.pos()).context("skin section overrun")?)?;
    let mut unzipped = Vec::new();
    flate2::read::ZlibDecoder::new(zipped).read_to_end(&mut unzipped)?;
    let mut z = Reader::new(&unzipped);
    for (lod, &count) in lods.iter_mut().zip(counts) {
        let nodes = (0..count).map(|_| z.u32()).collect::<Result<Vec<_>>>()?;
        let sizes = (0..count).map(|_| z.u64()).collect::<Result<Vec<_>>>()?;
        for (node, size) in nodes.into_iter().zip(sizes) {
            ensure!(size & 0x4000_0000 != 0, "obsolete skinned mesh format");
            let dump = z.bytes((size & !0x4000_0000) as usize)?;
            let node = node_names.get(node as usize).cloned().context("skin node id out of range")?;
            lod.skins.push(Skin { node, elems: read_mesh(dump, 0x38)? });
        }
    }
    Ok(vdata)
}
