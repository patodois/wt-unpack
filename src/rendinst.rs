//! Placed-object models: Dagor's `RendInst` game resource (class `77F8232F`,
//! `RenderableInstanceLodsResource`), which is not a `DynModel`: it has no skeleton and no skins, only
//! LODs of rigid meshes sharing one vertex pool.
//!
//! Stream, as read by `RenderableInstanceLodsResource::loadResource` and
//! `RenderableInstanceResource::loadResourceInternal` (`prog/engine/shaders/rendInstRes.cpp`):
//!
//! ```text
//! u32 size word        bits 0..23: size of the lods dump, bit 24: has an impostor LOD
//! u32 x4               texture count, material count, vdata count, header size. Both counts
//!                      0xFFFFFFFF: materials are described in riDesc.bin (all of War Thunder's)
//! vertex data          ShaderMatVdata, as in a DynModel
//! lods dump            u32 ofs, u32 count, [8] LOD table; BBox3 (6 f32); bounding sphere centre (3 f32),
//!                      radius, radius about the origin; u16 impostor data offset, u16 flags;
//!                      LOD record: ptr to its scene, f32 range, f32 texture scale (16 bytes)
//! per LOD:             i32 mesh size, i32 voxel data offset, ShaderMesh dump (as in a DynModel's rigid
//!                      part); if the voxel offset is not 0: i32 size, voxel surface data
//! ```
//!
//! Not every element's vertices follow the vertex declaration stored with the buffer: the declaration
//! describes the buffer's first meshes, but the layout of an element's vertices is the one its shader
//! asks for, and a buffer can hold several. `rendinst_facing_leaves` elements (camera-facing leaf cards of
//! trees) are the case this reader knows: see [`LeafVertex`] and [`is_facing_leaves`].
//!
//! A LOD's `range` is the distance up to which it is used. A model with an impostor has one more LOD
//! at the end: a camera-facing billboard (material class `rendinst_baked_impostor`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use wt_blk::dxp_and_grp::dynmodel::{Channel, Elem, VData};
use wt_blk::dxp_and_grp::grp_pack::{class_id, GrpPack};
use wt_blk::dxp_and_grp::meshopt::decode_index_sequence;

use crate::blk;
use crate::bytes::Reader;
use crate::level::{pptr, ptab, unpack};
use crate::vehicle;

// Vertex channel types and usages (`VSDT_*`, `SCUSAGE_*`), as in wt_blk's `dynmodel::channel`
const FLOAT2: u32 = 1;
const FLOAT3: u32 = 2;
const FLOAT4: u32 = 3;
const E3DCOLOR: u32 = 4;
const UBYTE4: u32 = 5;
const SHORT2: u32 = 6;
const SHORT4: u32 = 7;
const SHORT2N: u32 = 9;
const SHORT4N: u32 = 10;
const HALF2: u32 = 15;
const HALF4: u32 = 16;
const USAGE_POSITION: u32 = 0;
const USAGE_NORMAL: u32 = 1;
const USAGE_TEXCOORD: u32 = 3;
const VDATA_I16: u32 = 0x20;
const VDATA_I32: u32 = 0x40;
const VDATA_PACKED_IB: u32 = 0x200;
/// Texture coordinates are stored as SHORT2 in 1/4096 units
const TEXCOORD_SCALE: f32 = 1.0 / 4096.0;

/// One LOD: the distance it is used up to, and its draw calls
#[derive(Debug, Clone)]
pub struct RiLod {
    pub range: f32,
    pub elems: Vec<Elem>,
}

/// A placed-object model, in the object's own space (Dagor: Y up, left-handed, metres)
#[derive(Debug, Clone)]
pub struct RiModel {
    pub vdata: Vec<VData>,
    pub lods: Vec<RiLod>,
    pub bbox: [[f32; 3]; 2],
    pub has_impostor: bool,
    /// What `RenderableInstanceLodsResource::ImpostorData` records about the model, for models that have it
    pub impostor_data: Option<ImpostorData>,
}

/// `RenderableInstanceLodsResource::ImpostorData`, written by `RenderableInstanceLodsResSrc::save`
/// (`rendInstResSrc.cpp`): the points the impostor is baked from, and the extent of the facing leaves
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImpostorData {
    /// Vertices of the mesh LODs, as the builder saw them (Y clamped to 0 or above)
    pub points: Vec<[f32; 3]>,
    /// One box per distinct (vertex, leaf corner) of a `facing_leaves` material: the leaf's centre (Y
    /// clamped to 0 or above, like [`ImpostorData::points`]) and the corner delta from its extra channel 0
    pub shadow_points: Vec<[[f32; 3]; 2]>,
    pub min_y: f32,
    pub max_y: f32,
    pub cyl_rad: f32,
    /// The largest |x| or |y| of any facing-leaf corner delta, in metres
    pub max_facing_leaves_delta: f32,
    pub bbox_aspect_ratio: f32,
}

/// One decoded vertex
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RiVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// Whether a material class is a camera-facing leaf card (`rendinst_facing_leaves`). The vertices of its
/// elements are [`LeafVertex`] records, not what the buffer's declaration says (see [`RiModel::leaf_vertex`]).
pub fn is_facing_leaves(class: &str) -> bool {
    class.contains("facing_leaves")
}

/// One vertex of a `rendinst_facing_leaves` element. A leaf card is four vertices that share one centre;
/// the vertex shader moves each by `corner` (x right and y up in the camera's frame, so the card always
/// faces the viewer). Nothing in the file gives a card a fixed place in the object's space beyond its
/// centre.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LeafVertex {
    /// The card's centre in the object's space, metres
    pub centre: [f32; 3],
    /// Offset of this corner from the centre (the third component is the depth offset), metres
    pub corner: [f32; 3],
    pub uv: [f32; 2],
    pub normal: [f32; 3],
}

/// Bytes per vertex of a facing-leaves element
const LEAF_STRIDE: usize = 24;

/// IEEE half to single
fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (exp, frac) = (i32::from((h >> 10) & 0x1F), f32::from(h & 0x3FF));
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 if frac == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

fn read_block<'a>(r: &mut Reader<'a>) -> Result<&'a [u8]> {
    let head = r.u32()?;
    r.bytes((head & 0x3FFF_FFFF) as usize)
}

/// A compressed vertex block: a zstd frame, or the u32 unpacked size followed by an Oodle stream
fn unpack_block(block: &[u8]) -> Result<Vec<u8>> {
    if block.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Ok(zstd::stream::decode_all(block)?);
    }
    ensure!(block.len() >= 4, "empty vertex block");
    let size = u32::from_le_bytes(block[..4].try_into().unwrap()) as usize;
    unpack(2, &block[4..], Some(size))
}

fn packed_index_size(w1: u32, w2: u32, flags: u32) -> usize {
    if flags & VDATA_PACKED_IB != 0 {
        ((w1 >> 8) | ((w2 >> 28) << 24)) as usize
    } else {
        (w2 & 0x0FFF_FFFF) as usize
    }
}

fn check_table(buf: &[u8], ofs: usize, count: usize, elem: usize) -> Result<()> {
    ensure!(count.checked_mul(elem).and_then(|n| n.checked_add(ofs)).is_some_and(|end| end <= buf.len()), "table of {count} x {elem} bytes at {ofs} overruns {}", buf.len());
    Ok(())
}

/// `ShaderMatVdata::loadMatVdata`: the header word also selects the compression
fn read_mat_vdata(r: &mut Reader, hdr_word: u32) -> Result<Vec<VData>> {
    if hdr_word & 0xE000_0000 == 0xC000_0000 {
        let vd = unpack_block(read_block(r)?)?;
        return read_vdata(&vd, (hdr_word & !0xC000_0000) as usize);
    }
    ensure!(hdr_word & 0xC000_0000 == 0, "zlib or lzma packed vertex data is not supported");
    let hdr_size = hdr_word as usize;
    let header = r.bytes(hdr_size)?;
    let mut h = Reader::at(header, 16)?;
    let (tab_ofs, count) = (h.u32()? as usize, h.u32()? as usize);
    check_table(header, tab_ofs, count, 32)?;
    let mut payload = 0usize;
    for i in 0..count {
        let mut e = Reader::at(header, tab_ofs + i * 32)?;
        let (vert_num, w1, w2, flags) = (e.u32()? as usize, e.u32()?, e.u32()?, e.u32()?);
        payload = payload
            .saturating_add(vert_num.saturating_mul((w1 & 0xFF) as usize))
            .saturating_add(packed_index_size(w1, w2, flags));
    }
    let mut all = header.to_vec();
    all.extend_from_slice(r.bytes(payload)?);
    read_vdata(&all, hdr_size)
}

fn read_vdata(vd: &[u8], payload_start: usize) -> Result<Vec<VData>> {
    let mut h = Reader::at(vd, 16)?; // material table, empty when materials live in riDesc.bin
    let (tab_ofs, count) = (h.u32()? as usize, h.u32()? as usize);
    let mut payload = Reader::at(vd, payload_start)?;
    check_table(vd, tab_ofs, count, 32)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut e = Reader::at(vd, tab_ofs + i * 32)?;
        let vert_num = e.u32()? as usize;
        let (w1, w2, flags) = (e.u32()?, e.u32()?, e.u32()?);
        let (decl_ofs, decl_n) = (e.u32()? as usize, e.u32()? as usize);
        let stride = (w1 & 0xFF) as usize;
        let idx_size = (w2 & 0x0FFF_FFFF) as usize;
        // GlobalVertexData::initGvd: more than 64k vertices means 32-bit indices unless forced to 16
        let wide = flags & VDATA_I32 != 0 || (vert_num > 65536 && flags & VDATA_I16 == 0);
        let idx_count = idx_size / if wide { 4 } else { 2 };

        check_table(vd, decl_ofs, decl_n, 12)?;
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
                E3DCOLOR | UBYTE4 | SHORT2 | SHORT2N | HALF2 => 4,
                FLOAT2 | SHORT4 | SHORT4N | HALF4 => 8,
                FLOAT3 => 12,
                FLOAT4 => 16,
                other => bail!("vertex channel type {other}"),
            };
        }
        ensure!(offset == stride, "vertex declaration covers {offset} of {stride} bytes");

        let vertices = payload.bytes(vert_num.saturating_mul(stride))?.to_vec();
        let raw = payload.bytes(packed_index_size(w1, w2, flags))?;
        let indices = if flags & VDATA_PACKED_IB != 0 {
            decode_index_sequence(raw, idx_count, wide).map_err(|e| anyhow::anyhow!("index buffer: {e:?}"))?
        } else if wide {
            raw.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
        } else {
            raw.chunks_exact(2).map(|c| u32::from(u16::from_le_bytes([c[0], c[1]]))).collect()
        };
        out.push(VData { stride, vertices, indices, channels });
    }
    ensure!(payload.remaining() == 0, "vertex data has trailing bytes");
    Ok(out)
}

/// The elements of a `ShaderMesh` dump: an element table, then either the end index of each render
/// stage's elements (bit 31 set in the word at +0x20) or a second element table at +0x10 with the
/// see-through parts
fn read_mesh(dump: &[u8], vdata: &[VData]) -> Result<Vec<Elem>> {
    let mut r = Reader::new(dump);
    let main = (r.u32()? as usize, r.u32()? as usize);
    r.skip(8)?;
    let second = (r.u32()? as usize, r.u32()? as usize);
    r.skip(8)?;
    let tables = if r.u32()? & 0x8000_0000 != 0 { vec![main] } else { vec![main, second] };
    let mut elems = Vec::new();
    for (ofs, count) in tables {
        check_table(dump, ofs, count, 48)?;
        for i in 0..count {
            let mut e = Reader::at(dump, ofs + i * 48)?;
            e.skip(8)?; // shader element, set at runtime
            let material = e.u64()? as usize;
            let vd = e.u64()? as usize;
            e.skip(12)?; // vdata order index, vertex range
            let elem = Elem { material, vdata: vd, first_index: e.u32()? as usize, triangles: e.u32()? as usize, base_vertex: e.u32()? as usize };
            let buffer = vdata.get(elem.vdata).with_context(|| format!("element uses missing vertex buffer {}", elem.vdata))?;
            let range = elem
                .triangles
                .checked_mul(3)
                .and_then(|n| n.checked_add(elem.first_index))
                .and_then(|end| buffer.indices.get(elem.first_index..end))
                .context("element index range is out of bounds")?;
            let vcount = buffer.vertex_count();
            ensure!(range.iter().all(|&i| (i as usize).checked_add(elem.base_vertex).is_some_and(|v| v < vcount)), "element references a vertex out of bounds");
            elems.push(elem);
        }
    }
    Ok(elems)
}

/// The `ImpostorData` block, found through the `u16` at +60 of the lods dump (0: none): two pointers,
/// two `u16` counts and five floats, then the points (12 bytes each) and the shadow boxes (24 bytes each)
fn read_impostor_data(dump: &[u8]) -> Result<Option<ImpostorData>> {
    let ofs = usize::from(Reader::at(dump, 60)?.u16()?);
    if ofs == 0 {
        return Ok(None);
    }
    let mut r = Reader::at(dump, ofs)?;
    let (points_ofs, shadow_ofs) = (pptr(&mut r)?, pptr(&mut r)?);
    let (point_count, shadow_count) = (usize::from(r.u16()?), usize::from(r.u16()?));
    let (min_y, max_y, cyl_rad, max_facing_leaves_delta, bbox_aspect_ratio) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?);
    let mut points = Vec::with_capacity(point_count);
    if point_count > 0 {
        let at = points_ofs.context("impostor points pointer is null")?;
        check_table(dump, at, point_count, 12)?;
        let mut p = Reader::at(dump, at)?;
        for _ in 0..point_count {
            points.push([p.f32()?, p.f32()?, p.f32()?]);
        }
    }
    let mut shadow_points = Vec::with_capacity(shadow_count);
    if shadow_count > 0 {
        let at = shadow_ofs.context("impostor shadow pointer is null")?;
        check_table(dump, at, shadow_count, 24)?;
        let mut p = Reader::at(dump, at)?;
        for _ in 0..shadow_count {
            let mut b = [[0.0f32; 3]; 2];
            for c in b.iter_mut().flatten() {
                *c = p.f32()?;
            }
            shadow_points.push(b);
        }
    }
    Ok(Some(ImpostorData { points, shadow_points, min_y, max_y, cyl_rad, max_facing_leaves_delta, bbox_aspect_ratio }))
}

impl RiModel {
    /// Parses the data of a `RendInst` resource (see the module docs)
    pub fn parse(res: &[u8]) -> Result<Self> {
        let mut r = Reader::new(res);
        let size_word = r.u32()?;
        let dump_size = (size_word & 0x00FF_FFFF) as usize;
        let has_impostor = (size_word >> 24) & 1 != 0;
        let (tex_count, mat_count, _vdata_count, hdr_word) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        ensure!(tex_count == 0xFFFF_FFFF && mat_count == 0xFFFF_FFFF, "models with embedded materials are not supported");
        let vdata = read_mat_vdata(&mut r, hdr_word)?;

        let dump = r.bytes(dump_size)?;
        let mut d = Reader::new(dump);
        let (lod_ofs, lod_count) = ptab(&mut d)?;
        let bbox = {
            let mut v = [[0.0f32; 3]; 2];
            for p in &mut v {
                for c in p.iter_mut() {
                    *c = d.f32()?;
                }
            }
            v
        };
        ensure!((1..=16).contains(&lod_count), "{lod_count} LODs: a model has at least one");
        check_table(dump, lod_ofs, lod_count, 16)?;
        let mut ranges = Vec::with_capacity(lod_count);
        for l in 0..lod_count {
            let mut rec = Reader::at(dump, lod_ofs + l * 16)?;
            let _scene = pptr(&mut rec)?;
            ranges.push(rec.f32()?);
        }

        let mut lods = Vec::with_capacity(lod_count);
        for range in ranges {
            let mesh_size = r.i32()?;
            let voxel_ofs = r.i32()?;
            ensure!(mesh_size >= 0, "negative mesh size");
            let elems = read_mesh(r.bytes(mesh_size as usize)?, &vdata)?;
            if voxel_ofs != 0 {
                let vox = r.i32()?;
                r.skip(usize::try_from(vox).context("negative voxel size")?)?;
            }
            lods.push(RiLod { range, elems });
        }
        let impostor_data = if has_impostor { read_impostor_data(dump)? } else { None };
        Ok(RiModel { vdata, lods, bbox, has_impostor, impostor_data })
    }

    /// The index of the LOD the game uses at `dist` metres (`getBestLodForDist`): the first whose range
    /// is beyond it, and the last real mesh when none is. The impostor billboard is not a mesh LOD.
    pub fn lod_for_distance(&self, dist: f32) -> usize {
        let last = if self.has_impostor { self.lods.len().saturating_sub(2) } else { self.lods.len().saturating_sub(1) };
        if dist <= 0.0 {
            return 0;
        }
        (0..=last).find(|&i| self.lods.get(i).is_some_and(|l| dist < l.range)).unwrap_or(last)
    }

    /// Triangles in a LOD
    pub fn triangles(&self, lod: usize) -> usize {
        self.lods.get(lod).map_or(0, |l| l.elems.iter().map(|e| e.triangles).sum())
    }

    /// Decodes vertex `index` of vertex buffer `vdata`. Packed positions (SHORT4N) mean fractions of the
    /// model's bounding box.
    pub fn vertex(&self, vdata: &VData, index: usize) -> Result<RiVertex> {
        let v = index
            .checked_mul(vdata.stride)
            .and_then(|start| vdata.vertices.get(start..start.checked_add(vdata.stride)?))
            .with_context(|| format!("vertex {index} out of range"))?;
        let i16_at = |o: usize| f32::from(i16::from_le_bytes([v[o], v[o + 1]]));
        let f32_at = |o: usize| f32::from_le_bytes([v[o], v[o + 1], v[o + 2], v[o + 3]]);
        let mut out = RiVertex::default();
        for c in &vdata.channels {
            let o = c.offset;
            match (c.usage, c.index, c.ty) {
                (USAGE_POSITION, _, FLOAT3 | FLOAT4) => out.pos = [f32_at(o), f32_at(o + 4), f32_at(o + 8)],
                (USAGE_POSITION, _, SHORT4N) => {
                    // Fractions of the box: -1..1 spans it from centre to edge
                    for k in 0..3 {
                        let (lo, hi) = (self.bbox[0][k], self.bbox[1][k]);
                        out.pos[k] = i16_at(o + k * 2) / 32767.0 * 0.5 * (hi - lo) + 0.5 * (hi + lo);
                    }
                }
                // E3DCOLOR is stored as B, G, R, A
                (USAGE_NORMAL, _, E3DCOLOR) => out.normal = [v[o + 2], v[o + 1], v[o]].map(|x| f32::from(x) / 127.5 - 1.0),
                (USAGE_TEXCOORD, 0, SHORT2) => out.uv = [i16_at(o), i16_at(o + 2)].map(|x| x * TEXCOORD_SCALE),
                (USAGE_TEXCOORD, 0, FLOAT2) => out.uv = [f32_at(o), f32_at(o + 4)],
                _ => {}
            }
        }
        Ok(out)
    }
}

impl RiModel {
    /// Decodes vertex `index` of vertex buffer `vdata` as the vertex of a `rendinst_facing_leaves`
    /// element. The layout is the shader's (the buffer's declaration describes the tree's other meshes),
    /// 24 bytes: `HALF4` centre (w is 1), `SHORT2` texture coordinates in 1/4096, `HALF4` corner offset
    /// (z, then 0), `E3DCOLOR` normal. The file checks it: the model's `ImpostorData` lists the same
    /// centres and corner offsets in single precision ([`ImpostorData::shadow_points`]).
    pub fn leaf_vertex(&self, vdata: &VData, index: usize) -> Result<LeafVertex> {
        ensure!(vdata.stride == LEAF_STRIDE, "facing leaves have {LEAF_STRIDE}-byte vertices, the buffer has {}", vdata.stride);
        let v = index
            .checked_mul(LEAF_STRIDE)
            .and_then(|start| vdata.vertices.get(start..start + LEAF_STRIDE))
            .with_context(|| format!("vertex {index} out of range"))?;
        let half = |o: usize| half_to_f32(u16::from_le_bytes([v[o], v[o + 1]]));
        let short = |o: usize| f32::from(i16::from_le_bytes([v[o], v[o + 1]]));
        Ok(LeafVertex {
            centre: [half(0), half(2), half(4)],
            corner: [half(12), half(14), half(16)],
            uv: [short(8), short(10)].map(|x| x * TEXCOORD_SCALE),
            normal: [v[22], v[21], v[20]].map(|x| f32::from(x) / 127.5 - 1.0),
        })
    }
}

/// Where each game resource of the wanted classes lives: `(class, name)` to the `.grp` pack holding it.
/// Patched packs replace the originals.
pub struct ResourceIndex {
    by_name: HashMap<(u32, String), PathBuf>,
}

impl ResourceIndex {
    /// Scans the headers of the `.grp` packs under `content/base/res` and `patch/content/base/res`
    /// (vehicle folders aside) for resources of `classes`
    pub fn build(game: &Path, classes: &[u32]) -> Result<Self> {
        let mut packs = Vec::new();
        for root in ["content/base/res", "patch/content/base/res"] {
            let Ok(rd) = fs::read_dir(game.join(root)) else { continue };
            let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "grp")).collect();
            found.sort();
            packs.extend(found);
        }
        let mut by_name = HashMap::new();
        for path in packs {
            let file = vehicle::map(&path)?;
            let Ok(grp) = GrpPack::parse(&file) else { continue };
            for r in grp.resources.iter().filter(|r| classes.contains(&r.class_id)) {
                by_name.insert((r.class_id, r.name.clone()), path.clone());
            }
        }
        Ok(ResourceIndex { by_name })
    }

    pub fn find(&self, class: u32, name: &str) -> Option<&Path> {
        self.by_name.get(&(class, name.to_owned())).map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// Class ids of the resources levels need
pub const REND_INST: u32 = class_id::REND_INST;
/// `LandClass` game resources, whose data is a read-only data block (see [`crate::landclass`])
pub const LAND_CLASS: u32 = 0x03FB_59C4;

/// Material descriptions of every placed-object model, from riDesc.bin (patched copy first)
pub fn model_descs(game: &Path) -> Result<HashMap<String, blk::Block>> {
    model_descs_where(game, |_| true)
}

/// Like [`model_descs`], keeping only the models `wanted` accepts. The whole file is a big tree (hundreds
/// of megabytes for the game's 20,000 models), so the rest is dropped as soon as it is parsed rather than
/// held for the whole load.
pub fn model_descs_where(game: &Path, wanted: impl Fn(&str) -> bool) -> Result<HashMap<String, blk::Block>> {
    let mut descs = HashMap::new();
    for rel in ["patch/content/base/res/riDesc.bin", "content/base/res/riDesc.bin"] {
        let Ok(raw) = fs::read(game.join(rel)) else { continue };
        let root = blk::parse(&raw, None).with_context(|| format!("parsing {rel}"))?;
        drop(raw);
        for b in root.blocks {
            if wanted(&b.name) {
                descs.entry(b.name.clone()).or_insert(b);
            }
        }
    }
    Ok(descs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(ranges: &[f32], impostor: bool) -> RiModel {
        RiModel {
            vdata: Vec::new(),
            lods: ranges.iter().map(|&range| RiLod { range, elems: Vec::new() }).collect(),
            bbox: [[0.0; 3]; 2],
            has_impostor: impostor,
            impostor_data: None,
        }
    }

    #[test]
    fn lod_follows_the_ranges() {
        let m = model(&[50.0, 150.0, 400.0], false);
        assert_eq!([0.0, 10.0, 50.0, 149.0, 150.0, 399.0, 5000.0].map(|d| m.lod_for_distance(d)), [0, 0, 1, 1, 2, 2, 2]);
    }

    /// A resource with embedded nothing: an empty vertex-data header and a dump of `lod_count` LODs (none
    /// are listed, so only a count of 0 is valid)
    fn resource(lod_count: u32) -> Vec<u8> {
        let mut dump = vec![0u8; 40];
        dump[4..8].copy_from_slice(&lod_count.to_le_bytes());
        let mut res = Vec::new();
        for word in [dump.len() as u32, u32::MAX, u32::MAX, 0, 32] {
            res.extend_from_slice(&word.to_le_bytes());
        }
        res.extend_from_slice(&[0; 32]); // vertex data header: no buffers
        res.extend_from_slice(&dump);
        res
    }

    #[test]
    fn a_model_without_lods_is_refused_and_cannot_panic() {
        let err = RiModel::parse(&resource(0)).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("0 LODs"), "{err}");
        // Built by hand, with or without an impostor, it still has an answer for every distance
        for impostor in [false, true] {
            let m = model(&[], impostor);
            assert_eq!([0.0, 250.0, f32::INFINITY].map(|d| m.lod_for_distance(d)), [0, 0, 0]);
        }
        // One LOD and an impostor billboard stays valid
        assert_eq!(model(&[50.0], true).lod_for_distance(250.0), 0);
    }

    #[test]
    fn the_impostor_billboard_is_never_a_mesh_lod() {
        let m = model(&[50.0, 150.0, 400.0], true);
        assert_eq!(m.lod_for_distance(10_000.0), 1);
        assert_eq!(m.lod_for_distance(60.0), 1);
    }

    #[test]
    fn packed_positions_span_the_box() {
        let mut m = model(&[10.0], false);
        m.bbox = [[-1.0, 0.0, 2.0], [3.0, 4.0, 6.0]];
        let vd = VData {
            stride: 8,
            vertices: [32767i16, -32767, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect(),
            indices: Vec::new(),
            channels: vec![Channel { ty: SHORT4N, usage: USAGE_POSITION, index: 0, offset: 0 }],
        };
        assert_eq!(m.vertex(&vd, 0).unwrap().pos, [3.0, 0.0, 4.0]);
        assert!(m.vertex(&vd, 1).is_err());
    }

    #[test]
    fn half_floats_convert() {
        assert_eq!([0x3C00, 0xC000, 0x0000, 0x8000, 0x4333, 0x0001, 0x7BFF].map(half_to_f32), [1.0, -2.0, 0.0, -0.0, 3.599_609_4, 2f32.powi(-24), 65504.0]);
        assert!(half_to_f32(0x7C00).is_infinite() && half_to_f32(0x7E00).is_nan());
    }

    /// The bytes of the first leaf vertex of liveoak_01_rendinst
    const LEAF: [u8; 24] = [0x46, 0xC8, 0xEE, 0x4D, 0x8D, 0xC4, 0x00, 0x3C, 0x00, 0x08, 0x00, 0x08, 0x33, 0x43, 0x63, 0x4B, 0xA7, 0x3C, 0x00, 0x00, 0x69, 0xF5, 0x55, 0x80];

    fn leaf_buffer(stride: usize) -> VData {
        VData { stride, vertices: LEAF.to_vec(), indices: Vec::new(), channels: Vec::new() }
    }

    #[test]
    fn leaf_vertices_are_the_shaders_layout_not_the_trunks() {
        let m = model(&[10.0], false);
        let lv = m.leaf_vertex(&leaf_buffer(24), 0).unwrap();
        // The file's own record for this vertex: centre (-8.547323, 23.721893, -4.5499086), corner (3.5996037, 14.773094, 1.1629229)
        assert!(lv.centre.iter().zip([-8.547_323, 23.721_893, -4.549_908_6]).all(|(a, b)| (a - b).abs() < 0.02), "{:?}", lv.centre);
        assert!(lv.corner.iter().zip([3.599_603_7, 14.773_094, 1.162_922_9]).all(|(a, b)| (a - b).abs() < 0.03), "{:?}", lv.corner);
        assert_eq!(lv.uv, [0.5, 0.5]);
        let len = lv.normal.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((len - 1.0).abs() < 0.02, "{len}");
        // The trunk's declaration (float3 position) reads the same bytes as a point half a billion metres out
        let trunk = VData { channels: vec![Channel { ty: FLOAT3, usage: USAGE_POSITION, index: 0, offset: 0 }], ..leaf_buffer(24) };
        assert!(m.vertex(&trunk, 0).unwrap().pos[0] > 1.0e8);
        // Other strides are not leaves, and neither is a vertex that is not there
        assert!(m.leaf_vertex(&leaf_buffer(12), 0).is_err());
        assert!(m.leaf_vertex(&leaf_buffer(24), 1).is_err());
    }

    #[test]
    fn only_the_leaf_class_is_facing_leaves() {
        assert!(is_facing_leaves("rendinst_facing_leaves"));
        assert!(!is_facing_leaves("rendinst_tree") && !is_facing_leaves("rendinst_baked_impostor") && !is_facing_leaves(""));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(RiModel::parse(&[]).is_err());
        assert!(RiModel::parse(&[0xFF; 64]).is_err());
    }
}
