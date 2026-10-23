//! Reader for level files (`levels/*.bin`), Dagor's binary level dump (DBLD).
//!
//! Container layout (all little-endian):
//!
//! ```text
//! "DBLD" "3x64"     magic and version
//! u32 tex_count     number of texture names (the DxP2 block lists them)
//! tagged blocks, the last one tagged END:
//!   u32 len_flags   bits 0..29: length of the rest of the block, tag included; bits 30..31: codec
//!   [4] tag         four characters, left-padded with NULs ("\0TEX", "\0SCN")
//!   [len - 4]       payload
//! ```
//!
//! Inside a payload, nested blocks use the same `len_flags` word without a tag. For a nested block
//! the two top bits name the codec of its contents: 0 LZMA (old data), 1 zstd, 2 Oodle. Strings are
//! a u32 length and the bytes, zero-padded to a multiple of 4. Relocatable structures ("patchable"
//! dumps) store pointers as offsets from the dump start: a table is `u32 ofs, u32 count, [8]`, a
//! pointer is a u64 whose low 32 bits are a signed offset.
//!
//! Blocks seen in War Thunder 2.59 levels:
//!
//! | tag    | contents                                                                  |
//! |--------|---------------------------------------------------------------------------|
//! | `eVER` | exporter version string, SHA-1 of the level and of its main sections      |
//! | `RqRL` | names of the game resources the level needs (a patchable name map)        |
//! | `DxP2` | texture packs (`~`-separated) and the texture names the scenes index      |
//! | `TEX`  | one inline texture: its name, then a nested block holding a DDSx          |
//! | `TEX.` | end of the texture list                                                   |
//! | `lmap` | landmesh (`lndm` v4): render cells, detail map, land ray tracer           |
//! | `HM2`  | block-compressed 16-bit heightmap (see [`Heightmap`])                     |
//! | `LMp2` | physics map (ground material per cell)                                    |
//! | `hspl` `spgz` `stbl` | splines, road grid (zstd), road intersections               |
//! | `WBBX` | world bounding box, six floats                                            |
//! | `SCN`  | static render scene                                                       |
//! | `RIGz` | rendinst generator layers: land classes and placed objects ([`RendInstLayer`]) |
//! | `FRT`  | static collision (fast ray tracer)                                        |
//! | `Lnav` `Lnv2` `Lnvs` | navigation meshes                                           |
//! | `Obj`  | objects to place (game objects, effects)                                  |
//! | `Wire` `OCCL` `SCol` | cables, occlusion geometry, static collision                |
//! | `wt3d` `lmct` `d2co` `WCol` | War Thunder specific, not decoded                    |

use std::panic::{self, AssertUnwindSafe};

use anyhow::{bail, ensure, Context, Result};

use crate::bytes::Reader;

/// Largest unpacked nested block accepted (the biggest real ones are around 40 MB)
const MAX_UNPACKED: usize = 1 << 30;

pub struct Level<'a> {
    pub tex_count: u32,
    pub blocks: Vec<Block<'a>>,
}

#[derive(Clone)]
pub struct Block<'a> {
    /// Tag without its NUL padding, e.g. `TEX`
    pub tag: String,
    /// File offset of the block's length word
    pub offset: usize,
    /// Top two bits of the length word (always 0 for top-level blocks in practice)
    pub codec: u8,
    /// Payload after the tag
    pub data: &'a [u8],
}

impl<'a> Block<'a> {
    /// File offset of the payload
    pub fn data_offset(&self) -> usize {
        self.offset + 8
    }
}

impl<'a> Level<'a> {
    pub fn parse(raw: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(raw);
        ensure!(r.bytes(4)? == b"DBLD", "not a level file (no DBLD magic)");
        let version = r.bytes(4)?;
        ensure!(version == b"3x64", "unsupported DBLD version {:?}", String::from_utf8_lossy(version));
        let tex_count = r.u32()?;
        let mut blocks = Vec::new();
        loop {
            let offset = r.pos();
            let len_flags = r.u32().context("level ends without an END block")?;
            let len = (len_flags & 0x3FFF_FFFF) as usize;
            ensure!(len >= 4, "block at {offset:#x} is too short for its tag");
            let body = r.bytes(len).with_context(|| format!("block at {offset:#x} runs past the end of the file"))?;
            let tag: String = body[..4].iter().skip_while(|&&b| b == 0).map(|&b| char::from(b)).collect();
            let end = tag == "END";
            blocks.push(Block { tag, offset, codec: (len_flags >> 30) as u8, data: &body[4..] });
            if end {
                break;
            }
        }
        Ok(Level { tex_count, blocks })
    }

    pub fn find(&self, tag: &str) -> Option<&Block<'a>> {
        self.blocks.iter().find(|b| b.tag == tag)
    }

    pub fn all<'s>(&'s self, tag: &'s str) -> impl Iterator<Item = &'s Block<'a>> {
        self.blocks.iter().filter(move |b| b.tag == tag)
    }

    pub fn export_info(&self) -> Result<Option<ExportInfo>> {
        self.find("eVER").map(|b| ExportInfo::parse(b.data)).transpose()
    }

    /// Game resources the level needs: rendinsts, land classes, collision, effects
    pub fn required_resources(&self) -> Result<Vec<String>> {
        let Some(b) = self.find("RqRL") else { return Ok(Vec::new()) };
        name_map(b.data).context("RqRL")
    }

    pub fn texture_refs(&self) -> Result<Option<TextureRefs>> {
        self.find("DxP2").map(|b| TextureRefs::parse(b.data).context("DxP2")).transpose()
    }

    /// Textures stored inside the level (`TEX` blocks): name and the DDSx bytes
    pub fn inline_textures(&self) -> Result<Vec<(String, &'a [u8])>> {
        self.all("TEX")
            .map(|b| {
                let mut r = Reader::new(b.data);
                let name = dstring(&mut r)?;
                let (_, ddsx) = sub_block(&mut r)?;
                Ok((name, ddsx))
            })
            .collect()
    }

    /// World bounding box as `[min x, min y, min z, max x, max y, max z]`
    pub fn world_bbox(&self) -> Result<Option<[f32; 6]>> {
        let Some(b) = self.find("WBBX") else { return Ok(None) };
        let mut r = Reader::new(b.data);
        let mut out = [0.0; 6];
        for v in &mut out {
            *v = r.f32()?;
        }
        Ok(Some(out))
    }

    pub fn landmesh(&self) -> Result<Option<LandMesh<'a>>> {
        self.find("lmap").map(|b| LandMesh::parse(b.data).context("lmap")).transpose()
    }

    pub fn heightmap(&self) -> Result<Option<Heightmap>> {
        self.find("HM2").map(|b| Heightmap::parse(b.data).context("HM2")).transpose()
    }
}

/// Reads a Dagor string: u32 length, bytes, zero padding to a multiple of 4
pub(crate) fn dstring(r: &mut Reader) -> Result<String> {
    let len = r.u32()? as usize;
    let s = String::from_utf8_lossy(r.bytes(len)?).into_owned();
    r.skip((4 - len % 4) % 4)?;
    Ok(s)
}

/// Reads a nested block: its codec and contents
pub(crate) fn sub_block<'a>(r: &mut Reader<'a>) -> Result<(u8, &'a [u8])> {
    let len_flags = r.u32()?;
    let data = r.bytes((len_flags & 0x3FFF_FFFF) as usize).context("nested block runs past its parent")?;
    Ok(((len_flags >> 30) as u8, data))
}

/// Unpacks a nested block's contents. Oodle streams do not record their size, so it must be known.
pub(crate) fn unpack(codec: u8, packed: &[u8], size: Option<usize>) -> Result<Vec<u8>> {
    if let Some(size) = size {
        ensure!(size <= MAX_UNPACKED, "claimed unpacked size {size} is too large");
    }
    let out = match codec {
        1 => {
            use std::io::Read;
            let limit = size.unwrap_or(MAX_UNPACKED);
            let mut out = Vec::with_capacity(size.unwrap_or(0));
            zstd::stream::read::Decoder::new(packed)?.take(limit as u64 + 1).read_to_end(&mut out).context("zstd")?;
            ensure!(out.len() <= limit, "zstd data unpacks to more than {limit} bytes");
            out
        }
        2 => oodle(packed, size.context("Oodle data needs its unpacked size")?)?,
        0 => bail!("LZMA-packed data is not supported"),
        other => bail!("unknown block codec {other}"),
    };
    if let Some(size) = size {
        ensure!(out.len() == size, "unpacked {} bytes, expected {size}", out.len());
    }
    Ok(out)
}

/// Unpacks a nested block whose Oodle form starts with its u32 unpacked size (zstd and LZMA forms do
/// not carry it)
pub(crate) fn unpack_sized(codec: u8, packed: &[u8]) -> Result<Vec<u8>> {
    if codec == 2 {
        ensure!(packed.len() >= 4, "empty Oodle block");
        let size = u32::from_le_bytes(packed[..4].try_into().unwrap()) as usize;
        unpack(codec, &packed[4..], Some(size))
    } else {
        unpack(codec, packed, None)
    }
}

/// Oodle unpacks in 256 KiB blocks and spends at least 6 bytes on each: reject sizes the stream
/// cannot reach before allocating the output
fn oodle(packed: &[u8], size: usize) -> Result<Vec<u8>> {
    ensure!(size.div_ceil(0x4_0000) <= packed.len() / 6, "{} bytes of Oodle data cannot unpack to {size}", packed.len());
    let mut out = vec![0u8; size];
    // oozextract can panic on malformed streams; report that as an error
    let written = panic::catch_unwind(AssertUnwindSafe(|| oozextract::Extractor::new().read_from_slice(packed, &mut out)))
        .map_err(|_| anyhow::anyhow!("Oodle decoder panicked"))?
        .map_err(|e| anyhow::anyhow!("Oodle: {e:?}"))?;
    ensure!(written == size, "Oodle produced {written} bytes, expected {size}");
    Ok(out)
}

/// Reads a patchable table header: offset into the dump and element count
pub(crate) fn ptab(r: &mut Reader) -> Result<(usize, usize)> {
    let ofs = r.u32()? as usize;
    let count = r.u32()? as usize;
    r.skip(8)?;
    Ok((ofs, count))
}

/// Reads a patchable pointer: an offset into the dump, or `None` for null
pub(crate) fn pptr(r: &mut Reader) -> Result<Option<usize>> {
    let v = r.u64()? as u32 as i32;
    Ok((v >= 0).then_some(v as usize))
}

/// A `RoNameMap` dump: a table of string pointers
fn name_map(dump: &[u8]) -> Result<Vec<String>> {
    let (ofs, count) = ptab(&mut Reader::new(dump))?;
    ensure!(count <= dump.len() / 8, "name map claims {count} names");
    let mut ptrs = Reader::at(dump, ofs)?;
    (0..count)
        .map(|_| {
            let p = pptr(&mut ptrs)?.context("null name")?;
            Reader::at(dump, p)?.cstr()
        })
        .collect()
}

pub struct ExportInfo {
    /// e.g. `DaEditorX ver 5.0`
    pub version: String,
    pub sha1: Option<[u8; 20]>,
    /// SHA-1 of selected sections, by tag
    pub sections: Vec<(String, [u8; 20])>,
}

impl ExportInfo {
    fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = dstring(&mut r)?;
        let (mut sha1, mut sections) = (None, Vec::new());
        if r.remaining() >= 24 {
            let count = r.u32()? as usize;
            sha1 = Some(r.bytes(20)?.try_into().unwrap());
            for _ in 0..count {
                let tag: String = r.bytes(4)?.iter().skip_while(|&&b| b == 0).map(|&b| char::from(b)).collect();
                sections.push((tag, r.bytes(20)?.try_into().unwrap()));
            }
        }
        Ok(ExportInfo { version, sha1, sections })
    }
}

pub struct TextureRefs {
    /// Texture packs to load, from the `~`-separated list (empty in War Thunder levels)
    pub packs: Vec<String>,
    /// Texture names, in the order the level's scenes index them
    pub textures: Vec<String>,
}

impl TextureRefs {
    fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let packs = dstring(&mut r)?.split('~').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        let count = r.u32()? as usize;
        ensure!(count <= data.len() / 4, "DxP2 claims {count} textures");
        let textures = (0..count).map(|_| dstring(&mut r)).collect::<Result<_>>()?;
        Ok(TextureRefs { packs, textures })
    }
}

/// The landmesh (`lmap` block, `lndm` version 4).
///
/// ```text
/// "lndm" u32 version (4)
/// f32 grid_cell_size, f32 land_cell_size    detail grid step and size of one mesh cell, metres
/// i32 cells_x, cells_y, origin_x, origin_y  mesh cells; the first cell is at origin * land_cell_size
/// u32 use_tile
/// i32 mesh_ofs, detail_ofs, tile_ofs, tracer_ofs   relative to the position of mesh_ofs
/// [mesh_ofs]   render mesh: ShaderMatVdata, then per cell LOD meshes, then cell bounding boxes
/// [detail_ofs] detail (land class weight) map
/// [tile_ofs]   if use_tile: tile texture name, f32 tile_x, tile_y
///              if tracer_ofs != 0: nested block (Oodle: u32 unpacked size first) with an LTdump
///              string vertical texture; if not empty: 6 f32, string, 3 f32
///              optional "LTS4" + nested block (Oodle: u32 unpacked size first): newer ray tracer
/// ```
///
/// Both ray tracers hold the terrain's collision mesh cell by cell; see [`parse_ltdump`] and
/// [`parse_lts4`].
pub struct LandMesh<'a> {
    pub grid_cell_size: f32,
    pub land_cell_size: f32,
    pub cells: [i32; 2],
    pub origin: [i32; 2],
    pub tile_texture: Option<(String, [f32; 2])>,
    pub vertical_texture: Option<String>,
    /// The cliff texture's placement, when the landmesh has one
    pub vertical: Option<VerticalTexture>,
    /// Packed ray tracer streams (codec, data); decode with [`LandMesh::tracer_mesh`]
    ltdump: Option<(u8, &'a [u8])>,
    lts4: Option<(u8, &'a [u8])>,
    /// The whole `lmap` block, and where the detail data starts in it
    data: &'a [u8],
    base: usize,
    detail_ofs: usize,
}

/// How the landmesh paints its steep slopes (`vertical_param*` shader variables): a texture
/// projected sideways, blended in by height
#[derive(Debug, Clone, PartialEq)]
pub struct VerticalTexture {
    /// Colour texture, and the normal and detail textures named after it
    pub texture: String,
    pub detail_texture: String,
    pub scale_xz: f32,
    pub scale_y: f32,
    pub blend_from_y: f32,
    pub blend_to_y: f32,
    pub horizontal_blend: f32,
    pub y_offset: f32,
    pub detail_tile_xz: f32,
    pub detail_tile_y: f32,
    pub detail_y_offset: f32,
}

impl<'a> LandMesh<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        ensure!(r.bytes(4)? == b"lndm", "landmesh has no lndm magic");
        let version = r.u32()?;
        ensure!(version == 4, "unsupported landmesh version {version}");
        let grid_cell_size = r.f32()?;
        let land_cell_size = r.f32()?;
        let cells = [r.i32()?, r.i32()?];
        let origin = [r.i32()?, r.i32()?];
        let use_tile = r.u32()? != 0;
        let base = r.pos();
        let _mesh_ofs = r.i32()?;
        let detail_ofs = r.i32()?;
        let tile_ofs = r.i32()?;
        let tracer_ofs = r.i32()?;
        let mut r = Reader::at(data, base + usize::try_from(tile_ofs).context("negative tile offset")?)?;
        let tile_texture = if use_tile {
            let name = dstring(&mut r)?;
            Some((name, [r.f32()?, r.f32()?]))
        } else {
            None
        };
        let ltdump = if tracer_ofs != 0 {
            ensure!(base + tracer_ofs as usize == r.pos() + 4, "ray tracer is not where its offset points");
            Some(sub_block(&mut r)?)
        } else {
            None
        };
        let vert = dstring(&mut r)?;
        let (vertical_texture, vertical) = if vert.is_empty() {
            (None, None)
        } else {
            let f = |r: &mut Reader| r.f32();
            let (scale_xz, scale_y, blend_from_y, blend_to_y, horizontal_blend, y_offset) =
                (f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?);
            let detail_texture = dstring(&mut r)?;
            let (detail_tile_xz, detail_tile_y, detail_y_offset) = (f(&mut r)?, f(&mut r)?, f(&mut r)?);
            let v = VerticalTexture {
                texture: vert.clone(),
                detail_texture,
                scale_xz,
                scale_y,
                blend_from_y,
                blend_to_y,
                horizontal_blend,
                y_offset,
                detail_tile_xz,
                detail_tile_y,
                detail_y_offset,
            };
            (Some(vert), Some(v))
        };
        let lts4 = if r.remaining() >= 8 && r.bytes(4)? == b"LTS4" { Some(sub_block(&mut r)?) } else { None };
        let detail_ofs = base + usize::try_from(detail_ofs).context("negative detail offset")?;
        Ok(LandMesh { grid_cell_size, land_cell_size, cells, origin, tile_texture, vertical_texture, vertical, ltdump, lts4, data, base, detail_ofs })
    }

    /// Which ray tracer streams the landmesh carries: `LTdump`, `LTS4`, both or `none`
    pub fn tracer_kind(&self) -> &'static str {
        match (self.ltdump.is_some(), self.lts4.is_some()) {
            (true, true) => "LTdump + LTS4",
            (true, false) => "LTdump",
            (false, true) => "LTS4",
            (false, false) => "none",
        }
    }

    /// World XZ rectangle the mesh cells cover: `[min x, min z, max x, max z]`
    pub fn extent(&self) -> [f32; 4] {
        let s = self.land_cell_size;
        let (x0, z0) = (self.origin[0] as f32 * s, self.origin[1] as f32 * s);
        [x0, z0, x0 + self.cells[0] as f32 * s, z0 + self.cells[1] as f32 * s]
    }

    /// Decodes the land ray tracer (the legacy LTdump when present, else LTS4): the terrain's
    /// collision mesh, in world space
    pub fn tracer_mesh(&self) -> Result<Option<TriMesh>> {
        if let Some((codec, packed)) = self.ltdump {
            let raw = unpack_sized(codec, packed).context("LTdump ray tracer")?;
            return parse_ltdump(&raw).map(Some);
        }
        if let Some((codec, packed)) = self.lts4 {
            let raw = unpack_sized(codec, packed).context("LTS4 ray tracer")?;
            return parse_lts4(&raw).map(Some);
        }
        Ok(None)
    }
}

/// One land class the landmesh paints with: a game resource (class `03FB59C4`) found by name
#[derive(Debug, Clone)]
pub struct LandClassRef<'a> {
    pub name: String,
    /// Data the level stores next to the name instead of the resource's own, when it does
    pub inline_data: &'a [u8],
}

/// How a cell's weights over its land classes are stored
pub enum DetailWeights<'a> {
    /// The level's own packed atlas: one `LAND_WEIGHT_CELL_STRIDE` record per cell and a DDSx
    /// texture holding the pages (see [`DetailMap::cell_classes`] and [`DetailMap::cell_blend_count`])
    Packed { pages_per_row: usize, page_count: usize, records: Vec<u32>, atlas: &'a [u8] },
    /// One record per cell: 7 land class id bytes, two lengths, then up to two DDSx weight textures
    Legacy { cells: Vec<LegacyCell<'a>> },
}

pub struct LegacyCell<'a> {
    /// Land class ids by slot; 0xFF is a free slot
    pub ids: [u8; LAND_WEIGHT_DET_NUM],
    /// First weight texture (4 channels) and second (2 channels); empty when absent
    pub tex1: &'a [u8],
    pub tex2: &'a [u8],
}

/// The landmesh's detail data: which land classes it uses, and for every cell of the detail grid
/// which of them blend there, with what weights
pub struct DetailMap<'a> {
    pub width: usize,
    pub height: usize,
    /// Side of a legacy per-cell weight texture, and the texels of it a cell owns (0 in packed levels)
    pub tex_size: u32,
    pub elem_size: u32,
    pub land_classes: Vec<LandClassRef<'a>>,
    pub weights: DetailWeights<'a>,
}

/// Land classes blended per cell (`LAND_WEIGHT_DET_NUM`)
pub const LAND_WEIGHT_DET_NUM: usize = 7;
const LAND_WEIGHT_COUNT_SHIFT: u32 = 28;
const LAND_WEIGHT_COUNT_MASK: u32 = 7;
const LAND_WEIGHT_CELL_WORDS: usize = 3;

impl<'a> DetailMap<'a> {
    /// Number of land classes a cell blends
    pub fn cell_blend_count(&self, cell: usize) -> Option<usize> {
        match &self.weights {
            DetailWeights::Packed { records, .. } => {
                let rec = *records.get(cell * LAND_WEIGHT_CELL_WORDS)?;
                Some(((rec >> LAND_WEIGHT_COUNT_SHIFT) & LAND_WEIGHT_COUNT_MASK) as usize + 1)
            }
            DetailWeights::Legacy { cells } => Some(cells.get(cell)?.ids.iter().filter(|&&i| i != 0xFF).count()),
        }
    }

    /// Land class ids (indices into [`DetailMap::land_classes`]) a cell blends, by weight channel
    pub fn cell_classes(&self, cell: usize) -> Option<Vec<u8>> {
        let n = self.cell_blend_count(cell)?;
        match &self.weights {
            DetailWeights::Packed { records, .. } => {
                let rec = records.get(cell * LAND_WEIGHT_CELL_WORDS..(cell + 1) * LAND_WEIGHT_CELL_WORDS)?;
                let bytes: Vec<u8> = rec[1..].iter().flat_map(|w| w.to_le_bytes()).collect();
                Some(bytes[..n.min(LAND_WEIGHT_DET_NUM)].to_vec())
            }
            DetailWeights::Legacy { cells } => Some(cells.get(cell)?.ids.iter().copied().filter(|&i| i != 0xFF).collect()),
        }
    }
}

impl<'a> LandMesh<'a> {
    /// Reads the detail data: the land classes by name, then the per-cell weights (see
    /// `LandMeshManager::loadDetailData` and `DetailMap::load` in Dagor's `lmeshManager.cpp`)
    pub fn detail_map(&self) -> Result<DetailMap<'a>> {
        let mut r = Reader::at(self.data, self.detail_ofs)?;
        let (_, classes) = sub_block(&mut r).context("land class list")?;
        let mut c = Reader::new(classes);
        let count = c.u32()? as usize;
        ensure!(count <= classes.len() / 8 + 1, "{count} land classes in {} bytes", classes.len());
        let mut land_classes = Vec::with_capacity(count);
        for _ in 0..count {
            let (_, one) = sub_block(&mut c)?;
            let mut o = Reader::new(one);
            let name = dstring(&mut o)?;
            land_classes.push(LandClassRef { name, inline_data: o.rest() });
        }
        let (width, height) = (r.i32()?, r.i32()?);
        let (tex_size, elem_size) = (r.i32()?, r.i32()?);
        ensure!(width > 0 && height > 0 && width <= 1 << 14 && height <= 1 << 14, "bad detail grid {width}x{height}");
        let (width, height) = (width as usize, height as usize);
        let cells = width * height;
        let weights = if tex_size == 0 {
            // Packed: the legacy offset table (every entry names an empty record) and one empty record
            // (`LEGACY_CELL_HDR`: the id bytes and two lengths) come first
            r.skip(cells * 4 + LAND_WEIGHT_DET_NUM + 8)?;
            let pages_per_row = r.i32()?;
            let page_count = r.i32()?;
            ensure!(pages_per_row > 0 && (0..=1 << 14).contains(&page_count), "bad weight atlas layout");
            ensure!(cells * LAND_WEIGHT_CELL_WORDS * 4 <= r.remaining(), "weight atlas records overrun the landmesh");
            let records = (0..cells * LAND_WEIGHT_CELL_WORDS).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
            let len = r.i32()?;
            let atlas = r.bytes(usize::try_from(len).context("negative atlas length")?)?;
            DetailWeights::Packed { pages_per_row: pages_per_row as usize, page_count: page_count as usize, records, atlas }
        } else {
            ensure!(cells * 4 <= r.remaining(), "detail offsets overrun the landmesh");
            let offsets = (0..cells).map(|_| r.i32()).collect::<Result<Vec<_>>>()?;
            let mut out = Vec::with_capacity(cells);
            for ofs in offsets {
                let mut rec = Reader::at(self.data, self.base + usize::try_from(ofs).context("negative cell offset")?)?;
                let ids: [u8; LAND_WEIGHT_DET_NUM] = rec.bytes(LAND_WEIGHT_DET_NUM)?.try_into().unwrap();
                let (len, tex2) = (rec.i32()?, rec.i32()?);
                ensure!(len >= 0 && tex2 >= 0 && tex2 <= len, "bad weight record lengths {len} / {tex2}");
                let body = rec.bytes(len as usize)?;
                let (tex1, tex2) = body.split_at(if tex2 != 0 { tex2 as usize } else { body.len() });
                out.push(LegacyCell { ids, tex1, tex2 });
            }
            DetailWeights::Legacy { cells: out }
        };
        Ok(DetailMap { width, height, tex_size: tex_size as u32, elem_size: elem_size as u32, land_classes, weights })
    }
}

/// A triangle mesh in Dagor's world space (Y up, left-handed)
#[derive(Default)]
pub struct TriMesh {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// Parses an `LTdump` stream (the unpacked legacy land ray tracer).
///
/// ```text
/// u32 dump_size
/// "LTdump"  i32 cells_x, cells_y  f32 cell_size  f32 origin[3]  f32 bbox[6]
/// u32 n, n x 64-byte cells: f32 ofs[4], f32 scale[4], f32 max_ht, u32 grid_ht_start, grid_start,
///        fistart_gridsize, faces_start, verts_start, [8]
/// u32 n, n x u32    height grid (acceleration, unused here)
/// u32 n, n x u32    grid (acceleration, unused here)
/// u32 n, n x u16    face indices, local to their cell
/// u32 n, n x 4 u16  vertices: position = q[i] * scale[i] + ofs[i] of their cell
/// ...               face index grid (unused here)
/// ```
fn parse_ltdump(raw: &[u8]) -> Result<TriMesh> {
    let mut r = Reader::new(raw);
    let _dump_size = r.u32()?;
    ensure!(r.bytes(6)? == b"LTdump", "land ray tracer has no LTdump signature");
    let (cx, cy) = (r.i32()?, r.i32()?);
    ensure!((0..=4096).contains(&cx) && (0..=4096).contains(&cy), "bad ray tracer grid {cx}x{cy}");
    r.skip(4 + 12 + 24)?;
    let count = |r: &mut Reader, elem: usize| -> Result<usize> {
        let n = r.u32()? as usize;
        ensure!(n.checked_mul(elem).is_some_and(|b| b <= r.remaining()), "section of {n} x {elem} bytes overruns the stream");
        Ok(n)
    };
    let cell_count = count(&mut r, 64)?;
    ensure!(cell_count == (cx * cy) as usize, "{cell_count} cells for a {cx}x{cy} grid");
    let mut cells = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        let mut c = Reader::new(r.bytes(64)?);
        let ofs = [c.f32()?, c.f32()?, c.f32()?, c.f32()?];
        let scale = [c.f32()?, c.f32()?, c.f32()?, c.f32()?];
        c.skip(4 + 12)?;
        let (faces_start, verts_start) = (c.u32()? as usize, c.u32()? as usize);
        cells.push((ofs, scale, faces_start, verts_start));
    }
    for _ in 0..2 {
        let n = count(&mut r, 4)?;
        r.skip(n * 4)?;
    }
    let face_count = count(&mut r, 2)?;
    let faces = r.bytes(face_count * 2)?;
    let vert_count = count(&mut r, 8)?;
    let verts = r.bytes(vert_count * 8)?;
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);

    let mut mesh = TriMesh::default();
    for (i, &(ofs, scale, f0, v0)) in cells.iter().enumerate() {
        let (f1, v1) = cells.get(i + 1).map_or((face_count, vert_count), |c| (c.2, c.3));
        ensure!(f0 <= f1 && f1 <= face_count && v0 <= v1 && v1 <= vert_count, "cell {i} spans outside the tables");
        let (nv, nf) = (v1 - v0, f1 - f0);
        // Cells without geometry carry whatever an empty box packs to
        if nv == 0 || nf < 3 {
            continue;
        }
        ensure!(nf % 3 == 0, "cell {i} has {nf} face indices");
        let base = mesh.positions.len() as u32;
        for v in v0..v1 {
            let q = |k| f32::from(u16_at(verts, v * 4 + k));
            mesh.positions.push([q(0) * scale[0] + ofs[0], q(1) * scale[1] + ofs[1], q(2) * scale[2] + ofs[2]]);
        }
        for f in f0..f1 {
            let vi = u32::from(u16_at(faces, f));
            ensure!((vi as usize) < nv, "cell {i} indexes vertex {vi} of {nv}");
            mesh.indices.push(base + vi);
        }
    }
    Ok(mesh)
}

/// Parses an `LTS4` stream (the unpacked newer land ray tracer): one quantised BVH per cell, whose
/// leaves index the cell's vertices.
///
/// ```text
/// "LTS4" u32 version (1)  i32 cells_x, cells_y  f32 cell_size  f32 origin[3]  f32 bbox[6]
/// per cell: u32 has_geometry; if 1:
///   f32 scale[4], ofs[4]    box-space value b = world * scale + ofs
///   "dBLi" u16 version, u8 vert_stride (8), u8 leaf_size (28), u32 tree_bytes, u32 vert_count,
///   f32 bbox[6]; version >= 2: u32 flags (bit 0: edge flags follow the tree)
///   vert_count x u64        x, y, z in bits 0..20, 21..41, 42..62; b = value / 32
///   tree, pre-order, until tree_bytes of runtime nodes (16 bytes per inner node, 28 per leaf):
///     u8 children (1..252), then the children; or a leaf:
///     0: u32 w0..w3   0xFE / 0xFD: u32 w0, w1 (w2 = flip flag for 0xFD, w3 = 0)   0xFF: [16] empty
///   if edge flags: u16 per leaf
/// ```
///
/// A leaf holds one or two quads of up to two triangles each, as 13-bit signed vertex offsets from
/// an apex vertex: `w1 & 0xFFFFFF` is quad A's apex index, offsets o1 = w0[0..12], o2 = w0[13..25],
/// o3 = w0[26..30] | w1[24..31] << 5. Quad B's apex is `w2 & 0xFFFF` vertices further, with
/// o1 = w2[16..28], o2 = w3[0..12], o3 = w3[13..25]; it exists when its o1 != o2. Triangles are
/// (v0, v1, v2) and, unless o3 == o2, (v1, v2, v3) — (v2, v1, v3) when the quad's flip bit
/// (w2 bit 29 for A, 30 for B) is set.
fn parse_lts4(raw: &[u8]) -> Result<TriMesh> {
    let mut r = Reader::new(raw);
    ensure!(r.bytes(4)? == b"LTS4", "ray tracer has no LTS4 signature");
    let version = r.u32()?;
    ensure!(version == 1, "unsupported LTS4 version {version}");
    let (cx, cy) = (r.i32()?, r.i32()?);
    ensure!((1..=4096).contains(&cx) && (1..=4096).contains(&cy), "bad ray tracer grid {cx}x{cy}");
    r.skip(4 + 12 + 24)?;
    let mut mesh = TriMesh::default();
    for ci in 0..(cx * cy) as usize {
        if r.u32()? == 0 {
            continue;
        }
        let scale = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        let ofs = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        ensure!(r.bytes(4)? == b"dBLi", "cell {ci}: BVH has no dBLi signature");
        let blas_version = r.u16()?;
        let (stride, leaf_size) = (r.u8()?, r.u8()?);
        ensure!((1..=2).contains(&blas_version) && stride == 8 && leaf_size == 28, "cell {ci}: unsupported BVH layout");
        let tree_bytes = r.u32()? as usize;
        let vert_count = r.u32()? as usize;
        r.skip(24)?;
        let edge_flags = blas_version >= 2 && r.u32()? & 1 != 0;
        ensure!(vert_count <= r.remaining() / 8, "cell {ci}: {vert_count} vertices overrun the stream");
        let base = mesh.positions.len() as u32;
        for _ in 0..vert_count {
            let v = r.u64()?;
            let q = [v & 0x1F_FFFF, (v >> 21) & 0x1F_FFFF, (v >> 42) & 0x1F_FFFF];
            mesh.positions.push([0, 1, 2].map(|k| (q[k] as f32 / 32.0 - ofs[k]) / scale[k]));
        }

        // Nodes come in pre-order, so the leaves can be read in a flat loop; the runtime size of the
        // nodes read so far tells where the tree ends
        let (mut done, mut leaves) = (0usize, 0usize);
        while done < tree_bytes {
            let cc = r.u8()?;
            let body = match cc {
                0 | 0xFF => [r.u32()?, r.u32()?, r.u32()?, r.u32()?],
                0xFE | 0xFD => [r.u32()?, r.u32()?, if cc == 0xFD { 1 << 29 } else { 0 }, 0],
                _ => {
                    done += 16;
                    continue;
                }
            };
            done += 28;
            leaves += 1;
            ensure!(done <= tree_bytes, "cell {ci}: BVH tree overruns its size");
            if cc == 0xFF {
                continue;
            }
            let [w0, w1, w2, w3] = body;
            let sx = |v: u32| ((v << 19) as i32 >> 19) as i64;
            let apex = i64::from(w1 & 0xFF_FFFF);
            let (o1, o2, o3) = (sx(w0), sx(w0 >> 13), sx(((w0 >> 26) & 0x1F) | (w1 >> 24) << 5));
            let (o1b, o2b, o3b) = (sx(w2 >> 16), sx(w3), sx(w3 >> 13));
            let mut quads = vec![(apex, o1, o2, o3, w2 & 1 << 29 != 0)];
            if o1b != o2b {
                quads.push((apex + i64::from(w2 & 0xFFFF), o1b, o2b, o3b, w2 & 1 << 30 != 0));
            }
            for (a, o1, o2, o3, flip) in quads {
                let mut tris = vec![[a, a + o1, a + o2]];
                if o3 != o2 {
                    tris.push(if flip { [a + o2, a + o1, a + o3] } else { [a + o1, a + o2, a + o3] });
                }
                for t in tris {
                    for v in t {
                        ensure!((0..vert_count as i64).contains(&v), "cell {ci}: leaf indexes vertex {v} of {vert_count}");
                        mesh.indices.push(base + v as u32);
                    }
                }
            }
        }
        if edge_flags {
            r.skip(leaves * 2)?;
        }
    }
    ensure!(r.remaining() == 0, "{} bytes left after the LTS4 cells", r.remaining());
    Ok(mesh)
}

/// The `HM2` heightmap.
///
/// ```text
/// f32 cell_size, height_min, height_scale, origin_x, origin_z
/// u32 width | version << 24
/// u32 height | mirrored << 31     mirrored: the map repeats mirrored beyond its edges
/// i32 exclude[4]                  cells (in land mesh cells) the heightmap leaves to the landmesh
/// version 2: u32 chunk_info       bits 0..7 block shift, 8..11 log2 of the height-range grid's
///                                 finest block (0 = none), 12..31 bytes of variance per chunk
///            nested blocks: if chunk size is 0, one block with everything; otherwise one block of
///            block headers + height-range grid, then one per chunk of variance data
/// version 0/1: one nested block of u16 heights (version 1: deltas along each row)
/// ```
///
/// Version 2 is compressed like BC4: the map is cut into square blocks (8x8 in War Thunder) with a
/// u16 minimum and u16 range each, and one byte per pixel interpolating in that range. The bytes of
/// a block are stored as deltas from the previous one. A pixel's height in metres is
/// `height_min + height_scale * raw / 65535`; pixel (x, y) sits at
/// `origin + (x, y) * cell_size` in world X and Z.
pub struct Heightmap {
    pub width: u32,
    pub height: u32,
    pub cell_size: f32,
    pub height_min: f32,
    pub height_scale: f32,
    /// World X and Z of pixel (0, 0)
    pub origin: [f32; 2],
    pub mirrored: bool,
    pub version: u32,
    /// Row-major raw heights, rows along increasing world Z
    pub data: Vec<u16>,
}

impl Heightmap {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let cell_size = r.f32()?;
        let height_min = r.f32()?;
        let height_scale = r.f32()?;
        let origin = [r.f32()?, r.f32()?];
        let wv = r.u32()?;
        let (width, version) = (wv & 0xFF_FFFF, wv >> 24);
        let hm = r.u32()?;
        let (height, mirrored) = (hm & 0x7FFF_FFFF, hm >> 31 != 0);
        r.skip(16)?;
        ensure!((1..=16384).contains(&width) && (1..=16384).contains(&height), "bad heightmap size {width}x{height}");
        let (w, h) = (width as usize, height as usize);
        let data = match version {
            2 => {
                let chunk_info = r.u32()?;
                decode_cblock(&mut r, w, h, chunk_info)?
            }
            0 | 1 => {
                let (codec, packed) = sub_block(&mut r)?;
                let raw = unpack(codec, packed, Some(w * h * 2))?;
                let mut out: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                if version == 1 {
                    for row in out.chunks_exact_mut(w) {
                        for x in 1..w {
                            row[x] = row[x].wrapping_add(row[x - 1]);
                        }
                    }
                }
                out
            }
            v => bail!("unknown heightmap version {v}"),
        };
        Ok(Heightmap { width, height, cell_size, height_min, height_scale, origin, mirrored, version, data })
    }

    pub fn metres(&self, raw: u16) -> f32 {
        self.height_min + self.height_scale * f32::from(raw) / 65535.0
    }
}

/// Decodes a version 2 (block-compressed, delta-coded) heightmap
fn decode_cblock(r: &mut Reader, w: usize, h: usize, chunk_info: u32) -> Result<Vec<u16>> {
    let shift = (chunk_info & 0xFF).min(5) as usize;
    let hrb_sub = if chunk_info & 0xF00 != 0 { 1usize << ((chunk_info >> 8) & 0xF) } else { 0 };
    let chunk_bytes = (chunk_info & !0xFFF) as usize;
    let bwidth = 1usize << shift;
    ensure!(w % bwidth == 0 && h % bwidth == 0, "{w}x{h} heightmap is not a whole number of {bwidth}-pixel blocks");
    ensure!(hrb_sub == 0 || (w >= hrb_sub * 2 && (w / hrb_sub).is_power_of_two()), "bad height-range grid");
    let (bw, bh) = (w >> shift, h >> shift);
    let blocks = bw * bh;
    let block_size = 1usize << (2 * shift);
    let header_bytes = blocks * 4;
    // Height-range grid: (4^levels - 1) / 3 records of 16 bytes, levels = log2(w / finest block)
    let levels = if hrb_sub > 0 { (w / hrb_sub).trailing_zeros() } else { 0 };
    let hrb_bytes = ((1usize << (2 * levels)) - 1) / 3 * 16;

    let mut variance = vec![0u8; blocks * block_size];
    let headers = if chunk_bytes == 0 {
        let (codec, packed) = sub_block(r)?;
        let vlen = variance.len();
        let raw = unpack(codec, packed, Some(header_bytes + vlen + hrb_bytes))?;
        variance.copy_from_slice(&raw[header_bytes..header_bytes + vlen]);
        raw[..header_bytes].to_vec()
    } else {
        let per_chunk = chunk_bytes / block_size;
        ensure!(per_chunk > 0, "heightmap chunk smaller than a block");
        let (codec, packed) = sub_block(r)?;
        let raw = unpack(codec, packed, Some(header_bytes + hrb_bytes))?;
        for chunk in variance.chunks_mut(per_chunk * block_size) {
            let (codec, packed) = sub_block(r)?;
            chunk.copy_from_slice(&unpack(codec, packed, Some(chunk.len()))?);
        }
        raw[..header_bytes].to_vec()
    };

    let mut out = vec![0u16; w * h];
    for (b, vi) in variance.chunks_exact_mut(block_size).enumerate() {
        for i in 1..block_size {
            vi[i] = vi[i].wrapping_add(vi[i - 1]);
        }
        let mn = u16::from_le_bytes([headers[4 * b], headers[4 * b + 1]]);
        let delta = u32::from(u16::from_le_bytes([headers[4 * b + 2], headers[4 * b + 3]]));
        let (x0, y0) = ((b % bw) << shift, (b / bw) << shift);
        for (i, &v) in vi.iter().enumerate() {
            let ht = if delta == 0 { mn } else { mn.wrapping_add(((u32::from(v) * delta + 127) / 255) as u16) };
            out[(y0 + (i >> shift)) * w + x0 + (i & (bwidth - 1))] = ht;
        }
    }
    Ok(out)
}

/// One layer of the rendinst generator (`RIGz` block): land classes that scatter objects at load
/// time, and the pregenerated (placed) objects.
///
/// A `RIGz` block holds layers until fewer bytes than one dump header are left. Each layer is:
///
/// ```text
/// nested block (Oodle: u32 unpacked size first): u32 dump_size, dump, then sweep and land class
///   bitmaps (not decoded)
/// nested block: pregenerated object data, per cell at the cell's data offset; each is a nested
///   zstd (or LZMA) block of instances, subcell by subcell, pool by pool
/// ```
///
/// The dump is a patchable `RendInstGenData` (64-bit layout):
///
/// ```text
///   0 [8] runtime pointer        8 table cells         24 i32 cells_w, cells_h   32 i16 cell_size
///  34 u8 flags (bit 0: 32-bit counters)  35 u8 per-instance dwords  36 f32 sweep mask scale
///  40 [8] sweep mask pointer    48 table land classes 64 f32 world0 x, z, [8]   80 [32]
/// 112 f32 grid2world  116 f32 density pivot x, z  124 i32 data base  128 table pools  144 [8]
/// cell (552 bytes): table land class coverage, [8], i16 ht_min, i16 ht_delta, i32 data offset
///   (-1 = none), 65 pointers to instance counters: subcell s owns counters [ptr[s], ptr[s + 1])
/// land class (32 bytes): name pointer, [24]
/// pool (32 bytes): [8], name pointer, [8], u32 bits (0 position-only, 1 palette rotation,
///   2 no per-instance seeds), i32 palette rotation count
/// counter: u32 pool, u32 count (flag bit 0), else u32 pool low 10 bits | count << 10 | pool high << 30
/// ```
///
/// An instance is 12 i16 (a 3x4 matrix, rows of x, y, z: axis components / 256 and the position) or,
/// for position-only pools, 4 i16 (position and scale / 256), followed by the per-instance dwords
/// unless the pool has no seeds. In pools with palette rotation the scale's low 3 bits instead pick
/// one of the pool's preset rotations about Y: id `i` of `n` (the pool's palette count, which the level
/// stores) is a turn of `i / n` of a circle. Positions are fractions of 32767 of the cell: x and z of
/// `cell_size * grid2world` from the cell corner, y of `ht_delta` (8192 if 0) above `ht_min`.
pub struct RendInstLayer<'a> {
    pub cells_w: usize,
    pub cells_h: usize,
    /// Grid steps per cell
    pub cell_size: usize,
    /// Metres per grid step
    pub grid2world: f32,
    /// World X and Z of the first cell's corner
    pub world0: [f32; 2],
    pub per_inst_dwords: usize,
    pub land_classes: Vec<String>,
    pub pools: Vec<RendInstPool>,
    cells: Vec<RendInstCell>,
    pregen: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct RendInstPool {
    pub name: String,
    pub pos_only: bool,
    pub palette_rotation: bool,
    pub zero_inst_seeds: bool,
    /// Number of preset rotations about Y the pool's instances choose from (palette pools only). The
    /// instance's palette id `i` means a turn of `i / count` of a full circle (`PaletteEntry` in
    /// Dagor's `rendInstRotationPalette.cpp`).
    pub palette_count: usize,
}

struct RendInstCell {
    ht_min: i16,
    ht_delta: i16,
    data_ofs: Option<usize>,
    /// Per subcell: (pool, count) in data order
    counters: Vec<Vec<(usize, usize)>>,
}

/// A placed object: its pool and a Dagor transform (rows: x axis, y axis, z axis, position)
#[derive(Debug, Clone, PartialEq)]
pub struct RendInstance {
    pub pool: usize,
    pub tm: [[f32; 3]; 4],
}

const RIGEN_DUMP_HEADER: usize = 160;
const RIGEN_CELL: usize = 552;
const SUBCELLS: usize = 64;
const PALETTE_ID_BITS: u32 = 3;
const PALETTE_ID_MASK: u16 = (1 << PALETTE_ID_BITS) - 1;

impl<'a> RendInstLayer<'a> {
    /// Parses every layer of a `RIGz` block
    pub fn parse_all(data: &'a [u8]) -> Result<Vec<Self>> {
        let mut r = Reader::new(data);
        let mut layers = Vec::new();
        while r.remaining() > RIGEN_DUMP_HEADER {
            let (codec, packed) = sub_block(&mut r)?;
            let raw = unpack_sized(codec, packed).with_context(|| format!("RIGz layer {}", layers.len()))?;
            let mut d = Reader::new(&raw);
            let dump_size = d.u32()? as usize;
            let dump = d.bytes(dump_size).context("RIGz dump is larger than its stream")?;
            let (_, pregen) = sub_block(&mut r)?;
            layers.push(Self::parse_dump(dump, pregen).with_context(|| format!("RIGz layer {}", layers.len()))?);
        }
        Ok(layers)
    }

    fn parse_dump(dump: &[u8], pregen: &'a [u8]) -> Result<Self> {
        ensure!(dump.len() >= RIGEN_DUMP_HEADER, "RIGz dump of {} bytes is too small", dump.len());
        let mut r = Reader::at(dump, 8)?;
        let (cells_ofs, cell_count) = ptab(&mut r)?;
        let (cells_w, cells_h) = (r.i32()?, r.i32()?);
        let cell_size = r.i16()?;
        let flags = r.u8()?;
        let per_inst_dwords = r.u8()? as usize;
        ensure!(cells_w >= 0 && cells_h >= 0 && cell_count == (cells_w * cells_h) as usize, "{cell_count} cells for {cells_w}x{cells_h}");
        ensure!(cell_size > 0, "bad cell size {cell_size}");
        let mut r = Reader::at(dump, 48)?;
        let (lc_ofs, lc_count) = ptab(&mut r)?;
        let world0 = [r.f32()?, r.f32()?];
        let mut r = Reader::at(dump, 112)?;
        let grid2world = r.f32()?;
        let mut r = Reader::at(dump, 128)?;
        let (pool_ofs, pool_count) = ptab(&mut r)?;
        ensure!(lc_count <= dump.len() / 32 && pool_count <= dump.len() / 32, "RIGz tables overrun the dump");

        let name_at = |ofs: Option<usize>| -> Result<String> {
            match ofs {
                Some(o) => Reader::at(dump, o)?.cstr(),
                None => Ok(String::new()),
            }
        };
        let land_classes = (0..lc_count)
            .map(|i| name_at(pptr(&mut Reader::at(dump, lc_ofs + i * 32)?)?))
            .collect::<Result<Vec<_>>>()?;
        let pools = (0..pool_count)
            .map(|i| {
                let mut p = Reader::at(dump, pool_ofs + i * 32 + 8)?;
                let name = name_at(pptr(&mut p)?)?;
                p.skip(8)?;
                let bits = p.u32()?;
                let palette_count = p.i32()?.clamp(0, 1 << 16) as usize;
                Ok(RendInstPool { name, pos_only: bits & 1 != 0, palette_rotation: bits & 2 != 0, zero_inst_seeds: bits & 4 != 0, palette_count })
            })
            .collect::<Result<Vec<_>>>()?;

        let counter_size = if flags & 1 != 0 { 8 } else { 4 };
        let mut cells = Vec::with_capacity(cell_count);
        for i in 0..cell_count {
            let mut c = Reader::at(dump, cells_ofs + i * RIGEN_CELL + 24)?;
            let ht_min = c.i16()?;
            let ht_delta = c.i16()?;
            let data_ofs = usize::try_from(c.i32()?).ok();
            let ptrs = (0..=SUBCELLS).map(|_| pptr(&mut c)).collect::<Result<Vec<_>>>()?;
            let mut counters = Vec::with_capacity(SUBCELLS);
            for s in 0..SUBCELLS {
                let mut list = Vec::new();
                if let (Some(p0), Some(p1)) = (ptrs[s], ptrs[s + 1]) {
                    ensure!(p0 <= p1 && p1 <= dump.len(), "cell {i} counters out of range");
                    let mut cr = Reader::new(&dump[p0..p1]);
                    while cr.remaining() >= counter_size {
                        let (pool, count) = if counter_size == 8 {
                            (cr.u32()? as usize, cr.u32()? as usize)
                        } else {
                            let v = cr.u32()?;
                            (((v & 0x3FF) | (v >> 30) << 10) as usize, ((v >> 10) & 0xF_FFFF) as usize)
                        };
                        ensure!(pool < pools.len(), "cell {i} counts instances of pool {pool} of {}", pools.len());
                        list.push((pool, count));
                    }
                }
                counters.push(list);
            }
            cells.push(RendInstCell { ht_min, ht_delta, data_ofs, counters });
        }
        Ok(RendInstLayer {
            cells_w: cells_w as usize,
            cells_h: cells_h as usize,
            cell_size: cell_size as usize,
            grid2world,
            world0,
            per_inst_dwords,
            land_classes,
            pools,
            cells,
            pregen,
        })
    }

    /// Number of placed objects per pool, from the counters (without unpacking the instances)
    pub fn counts(&self) -> Vec<usize> {
        let mut out = vec![0; self.pools.len()];
        for (pool, n) in self.cells.iter().filter(|c| c.data_ofs.is_some()).flat_map(|c| c.counters.iter().flatten()) {
            out[*pool] += n;
        }
        out
    }

    /// Unpacks every placed object
    pub fn instances(&self) -> Result<Vec<RendInstance>> {
        let mut out = Vec::new();
        let cell_xz = self.cell_size as f32 * self.grid2world;
        for (i, cell) in self.cells.iter().enumerate() {
            let Some(ofs) = cell.data_ofs else { continue };
            let mut r = Reader::at(self.pregen, ofs)?;
            let (codec, packed) = sub_block(&mut r).with_context(|| format!("cell {i}"))?;
            if packed.is_empty() {
                continue;
            }
            let raw = unpack(codec, packed, None).with_context(|| format!("cell {i}"))?;
            let mut d = Reader::new(&raw);
            let (cx, cz) = ((i % self.cells_w) as f32, (i / self.cells_w) as f32);
            let add = [self.world0[0] + cx * cell_xz, f32::from(cell.ht_min), self.world0[1] + cz * cell_xz];
            let dy = if cell.ht_delta != 0 { f32::from(cell.ht_delta) } else { 8192.0 };
            let mul = [cell_xz / 32767.0, dy / 32767.0, cell_xz / 32767.0];
            for &(pool, count) in cell.counters.iter().flatten() {
                let p = &self.pools[pool];
                let seeds = if p.zero_inst_seeds { 0 } else { 4 * self.per_inst_dwords };
                for _ in 0..count {
                    let mut q = [0i16; 12];
                    let n = if p.pos_only { 4 } else { 12 };
                    for v in &mut q[..n] {
                        *v = d.i16().with_context(|| format!("cell {i}: instance data ends early"))?;
                    }
                    d.skip(seeds)?;
                    let f = |v: i16| f32::from(v);
                    let tm = if p.pos_only {
                        // With palette rotation the low bits pick one of the pool's preset rotations:
                        // a turn about Y of id / count of a circle, as `rotyTM`
                        // (x axis (cos, 0, sin), z axis (-sin, 0, cos))
                        let (s, (sin, cos)) = if p.palette_rotation {
                            let id = q[3] as u16 & PALETTE_ID_MASK;
                            let count = p.palette_count.clamp(1, 1 << PALETTE_ID_BITS);
                            (f32::from(q[3] as u16 & !PALETTE_ID_MASK) / 256.0, (std::f32::consts::TAU * f32::from(id) / count as f32).sin_cos())
                        } else {
                            (f(q[3]) / 256.0, (0.0, 1.0))
                        };
                        let pos = [0, 1, 2].map(|k| f(q[k]) * mul[k] + add[k]);
                        [[s * cos, 0.0, s * sin], [0.0, s, 0.0], [-s * sin, 0.0, s * cos], pos]
                    } else {
                        let axis = |c: usize| [f(q[c]) / 256.0, f(q[4 + c]) / 256.0, f(q[8 + c]) / 256.0];
                        [axis(0), axis(1), axis(2), [0, 1, 2].map(|k| f(q[4 * k + 3]) * mul[k] + add[k])]
                    };
                    out.push(RendInstance { pool, tm });
                }
            }
            ensure!(d.remaining() == 0, "cell {i}: {} bytes left after its instances", d.remaining());
        }
        Ok(out)
    }
}

impl Level<'_> {
    pub fn rendinst_layers(&self) -> Result<Vec<RendInstLayer<'_>>> {
        match self.find("RIGz") {
            Some(b) => RendInstLayer::parse_all(b.data),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 4) as u32).to_le_bytes().to_vec();
        out.extend_from_slice(tag);
        out.extend_from_slice(payload);
        out
    }

    fn dstr(s: &str) -> Vec<u8> {
        let mut out = (s.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    fn nested(codec: u32, data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32 | codec << 30).to_le_bytes().to_vec();
        out.extend_from_slice(data);
        out
    }

    fn level(blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"DBLD3x64".to_vec();
        out.extend_from_slice(&0u32.to_le_bytes());
        for b in blocks {
            out.extend_from_slice(b);
        }
        out.extend(block(b"\0END", &[]));
        out
    }

    #[test]
    fn container_lists_blocks_and_reads_strings() {
        let mut ever = dstr("DaEditorX ver 5.0");
        ever.extend_from_slice(&1u32.to_le_bytes());
        ever.extend_from_slice(&[7; 20]);
        ever.extend_from_slice(b"lmap");
        ever.extend_from_slice(&[9; 20]);
        let mut dxp = dstr("~");
        dxp.extend_from_slice(&2u32.to_le_bytes());
        dxp.extend(dstr("a*"));
        dxp.extend(dstr("tex_b*"));
        let bbox: Vec<u8> = [-1.0f32, -2.0, -3.0, 1.0, 2.0, 3.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        let raw = level(&[block(b"eVER", &ever), block(b"DxP2", &dxp), block(b"WBBX", &bbox)]);

        let lvl = Level::parse(&raw).unwrap();
        let tags: Vec<&str> = lvl.blocks.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["eVER", "DxP2", "WBBX", "END"]);
        let info = lvl.export_info().unwrap().unwrap();
        assert_eq!(info.version, "DaEditorX ver 5.0");
        assert_eq!(info.sections, vec![("lmap".to_string(), [9; 20])]);
        let refs = lvl.texture_refs().unwrap().unwrap();
        assert!(refs.packs.is_empty());
        assert_eq!(refs.textures, ["a*", "tex_b*"]);
        assert_eq!(lvl.world_bbox().unwrap(), Some([-1.0, -2.0, -3.0, 1.0, 2.0, 3.0]));
    }

    #[test]
    fn truncated_level_is_an_error() {
        let mut raw = level(&[block(b"WBBX", &[0; 24])]);
        raw.truncate(raw.len() - 8);
        assert!(Level::parse(&raw).is_err());
        raw.truncate(20);
        assert!(Level::parse(&raw).is_err());
    }

    #[test]
    fn name_map_resolves_offsets() {
        // Table at 16 pointing at two string pointers at 32; strings after them
        let mut dump = vec![0u8; 48];
        dump[..4].copy_from_slice(&32u32.to_le_bytes());
        dump[4..8].copy_from_slice(&2u32.to_le_bytes());
        dump[32..40].copy_from_slice(&48u64.to_le_bytes());
        dump[40..48].copy_from_slice(&52u64.to_le_bytes());
        dump.extend_from_slice(b"abc\0de\0");
        let raw = level(&[block(b"RqRL", &dump)]);
        assert_eq!(Level::parse(&raw).unwrap().required_resources().unwrap(), ["abc", "de"]);
    }

    /// Encodes a heightmap the way the exporter does: blocks of min/range plus delta-coded bytes
    fn encode_cblock(data: &[u16], w: usize, h: usize, shift: usize) -> (Vec<u8>, Vec<u8>) {
        let bwidth = 1 << shift;
        let (mut headers, mut variance) = (Vec::new(), Vec::new());
        for by in (0..h).step_by(bwidth) {
            for bx in (0..w).step_by(bwidth) {
                let px: Vec<u16> = (0..bwidth * bwidth).map(|i| data[(by + i / bwidth) * w + bx + i % bwidth]).collect();
                let (mn, mx) = (*px.iter().min().unwrap(), *px.iter().max().unwrap());
                let delta = mx - mn;
                headers.extend_from_slice(&mn.to_le_bytes());
                headers.extend_from_slice(&delta.to_le_bytes());
                let enc: Vec<u8> = px
                    .iter()
                    .map(|&v| if delta == 0 { 0 } else { ((u32::from(v - mn) * 255 + u32::from(delta >> 1)) / u32::from(delta)) as u8 })
                    .collect();
                let mut last = 0u8;
                for (i, &e) in enc.iter().enumerate() {
                    variance.push(if i == 0 { e } else { e.wrapping_sub(last) });
                    last = e;
                }
            }
        }
        (headers, variance)
    }

    fn hm2_header(w: u32, h: u32, chunk_info: u32) -> Vec<u8> {
        let mut out: Vec<u8> = [2.0f32, -10.0, 100.0, -16.0, -32.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        out.extend_from_slice(&(w | 2 << 24).to_le_bytes());
        out.extend_from_slice(&(h | 1 << 31).to_le_bytes());
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&chunk_info.to_le_bytes());
        out
    }

    #[test]
    fn block_compressed_heightmap_round_trips() {
        let (w, h) = (128usize, 64usize);
        // A ramp with flat areas: decoding is exact where each block's range is at most 255
        let data: Vec<u16> = (0..w * h).map(|i| if i % w < 8 { 1000 } else { (i % w * 7 + i / w * 3) as u16 }).collect();
        let (headers, variance) = encode_cblock(&data, w, h, 3);

        // One zstd block holding headers and variance, no height-range grid
        let mut single = hm2_header(w as u32, h as u32, 3);
        single.extend(nested(1, &zstd::bulk::compress(&[headers.clone(), variance.clone()].concat(), 3).unwrap()));
        let hm = Heightmap::parse(&single).unwrap();
        assert_eq!((hm.width, hm.height, hm.version, hm.mirrored), (128, 64, 2, true));
        assert_eq!(hm.origin, [-16.0, -32.0]);
        assert_eq!(hm.data, data);
        assert_eq!(hm.metres(65535), 90.0);

        // Chunked: headers plus a height-range grid with 8-pixel cells (4 levels, 85 records), then
        // the variance of 128 blocks in two 4 KiB chunks
        let chunk_info = 3 | 3 << 8 | 4096;
        let mut chunked = hm2_header(w as u32, h as u32, chunk_info);
        chunked.extend(nested(1, &zstd::bulk::compress(&[headers, vec![0; 85 * 16]].concat(), 3).unwrap()));
        assert_eq!(variance.len(), 2 * 4096);
        for chunk in variance.chunks(4096) {
            chunked.extend(nested(1, &zstd::bulk::compress(chunk, 3).unwrap()));
        }
        assert_eq!(Heightmap::parse(&chunked).unwrap().data, data);
    }

    #[test]
    fn heightmap_rejects_bad_sizes() {
        let mut bad = hm2_header(30, 16, 3);
        bad.extend(nested(1, &zstd::bulk::compress(&[0u8; 8], 3).unwrap()));
        assert!(Heightmap::parse(&bad).is_err());
    }

    #[test]
    fn lts4_leaves_decode_to_triangles() {
        let mut s = b"LTS4".to_vec();
        for v in [1u32, 1, 1] {
            s.extend_from_slice(&v.to_le_bytes());
        }
        s.extend_from_slice(&[0; 40]);
        s.extend_from_slice(&1u32.to_le_bytes());
        for f in [1.0f32, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0] {
            s.extend_from_slice(&f.to_le_bytes());
        }
        s.extend_from_slice(b"dBLi");
        s.extend_from_slice(&2u16.to_le_bytes());
        s.extend_from_slice(&[8, 28]);
        s.extend_from_slice(&72u32.to_le_bytes()); // one inner node and two leaves
        s.extend_from_slice(&4u32.to_le_bytes());
        s.extend_from_slice(&[0; 24]);
        s.extend_from_slice(&1u32.to_le_bytes()); // edge flags follow the tree
        for i in 0..4u64 {
            // Box space x = 10 i, y = i, z = 5, stored times 32
            s.extend_from_slice(&((i * 320) | (i * 32) << 21 | 160 << 42).to_le_bytes());
        }
        let leaf = 1u32 << 31;
        s.push(2);
        // Full leaf: quad A (0, 1, 2) single, quad B with apex 1: (1, 2, 3) single
        s.push(0);
        for w in [1 | 2 << 13 | 2 << 26 | leaf, 0, 1 | 1 << 16, 2 | 2 << 13] {
            s.extend_from_slice(&w.to_le_bytes());
        }
        // Short leaf with the flip bit: (0, 1, 2) and (2, 1, 3)
        s.push(0xFD);
        for w in [1 | 2 << 13 | 3 << 26 | leaf, 0] {
            s.extend_from_slice(&w.to_le_bytes());
        }
        s.extend_from_slice(&[0; 4]);
        let mesh = parse_lts4(&s).unwrap();
        assert_eq!(mesh.positions[3], [30.0, 3.0, 5.0]);
        assert_eq!(mesh.indices, [0, 1, 2, 1, 2, 3, 0, 1, 2, 2, 1, 3]);

        // A tree that claims more bytes than the stream holds is an error, not a panic
        let mut short = s.clone();
        short.truncate(s.len() - 10);
        assert!(parse_lts4(&short).is_err());
    }

    #[test]
    fn rendinst_layer_places_instances_in_their_cell() {
        let mut dump = vec![0u8; 760];
        let put = |d: &mut Vec<u8>, at: usize, bytes: &[u8]| d[at..at + bytes.len()].copy_from_slice(bytes);
        put(&mut dump, 8, &160u32.to_le_bytes()); // cells
        put(&mut dump, 12, &1u32.to_le_bytes());
        put(&mut dump, 24, &1i32.to_le_bytes());
        put(&mut dump, 28, &1i32.to_le_bytes());
        put(&mut dump, 32, &8i16.to_le_bytes()); // cell size in grid steps
        put(&mut dump, 34, &[1, 1]); // 32-bit counters, one per-instance dword
        put(&mut dump, 64, &100f32.to_le_bytes());
        put(&mut dump, 68, &200f32.to_le_bytes());
        put(&mut dump, 112, &4f32.to_le_bytes()); // 32 m cells
        put(&mut dump, 128, &712u32.to_le_bytes()); // pools
        put(&mut dump, 132, &1u32.to_le_bytes());
        // The cell: heights 10 + 0..100, data at 0, subcell 0 owns the one counter
        put(&mut dump, 160 + 24, &10i16.to_le_bytes());
        put(&mut dump, 160 + 26, &100i16.to_le_bytes());
        put(&mut dump, 160 + 32, &744u64.to_le_bytes());
        for s in 1..=64 {
            put(&mut dump, 160 + 32 + 8 * s, &752u64.to_le_bytes());
        }
        put(&mut dump, 712 + 8, &752u64.to_le_bytes()); // pool name
        put(&mut dump, 744, &0u32.to_le_bytes());
        put(&mut dump, 748, &2u32.to_le_bytes());
        put(&mut dump, 752, b"tree\0");

        let mut inst = Vec::new();
        for (rot, pos) in [([256i16, 0, 0, 0, 256, 0, 0, 0, 256], [32767i16, 32767, 0]), ([0, 0, 256, 0, 256, 0, -256, 0, 0], [0, 0, 16384])] {
            for row in 0..3 {
                for c in 0..3 {
                    inst.extend_from_slice(&rot[row * 3 + c].to_le_bytes());
                }
                inst.extend_from_slice(&pos[row].to_le_bytes());
            }
            inst.extend_from_slice(&[0; 4]); // seed
        }
        let mut pregen = nested(1, &zstd::bulk::compress(&inst, 3).unwrap());
        pregen.resize(pregen.len() + 200, 0);
        let stream = [(dump.len() as u32).to_le_bytes().to_vec(), dump].concat();
        let rigz = [nested(1, &zstd::bulk::compress(&stream, 3).unwrap()), nested(0, &pregen)].concat();

        let layers = RendInstLayer::parse_all(&rigz).unwrap();
        assert_eq!(layers.len(), 1);
        let layer = &layers[0];
        assert_eq!(layer.pools[0].name, "tree");
        assert_eq!(layer.counts(), [2]);
        let found = layer.instances().unwrap();
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3);
        assert_eq!(found[0].tm[..3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert!(near(found[0].tm[3], [132.0, 110.0, 200.0]), "{:?}", found[0].tm[3]);
        // Rotated 90 degrees about Y, halfway along the cell in Z
        assert_eq!(found[1].tm[0], [0.0, 0.0, -1.0]);
        assert!(near(found[1].tm[3], [100.0, 10.0, 216.0]), "{:?}", found[1].tm[3]);
    }

    #[test]
    fn ltdump_cells_decode_to_world_positions() {
        let mut s = 0u32.to_le_bytes().to_vec();
        s.extend_from_slice(b"LTdump");
        s.extend_from_slice(&1i32.to_le_bytes());
        s.extend_from_slice(&2i32.to_le_bytes());
        s.extend_from_slice(&[0; 40]);
        s.extend_from_slice(&2u32.to_le_bytes());
        for (ofs, faces, verts) in [(100.0f32, 0u32, 0u32), (200.0, 3, 3)] {
            let mut cell: Vec<u8> = [ofs, 0.0, 0.0, 0.0, 1.0, 0.5, 1.0, 0.0, 0.0].iter().flat_map(|f| f.to_le_bytes()).collect();
            cell.extend_from_slice(&[0; 12]);
            cell.extend_from_slice(&faces.to_le_bytes());
            cell.extend_from_slice(&verts.to_le_bytes());
            cell.extend_from_slice(&[0; 8]);
            s.extend(cell);
        }
        s.extend_from_slice(&[0; 8]); // two empty acceleration grids
        s.extend_from_slice(&6u32.to_le_bytes());
        for i in [0u16, 1, 2, 2, 1, 0] {
            s.extend_from_slice(&i.to_le_bytes());
        }
        s.extend_from_slice(&6u32.to_le_bytes());
        for v in 0..6u16 {
            for q in [v, v * 2, 10, 0] {
                s.extend_from_slice(&q.to_le_bytes());
            }
        }
        let mesh = parse_ltdump(&s).unwrap();
        assert_eq!(mesh.positions.len(), 6);
        assert_eq!(mesh.positions[1], [101.0, 1.0, 10.0]);
        assert_eq!(mesh.positions[4], [204.0, 4.0, 10.0]);
        assert_eq!(mesh.indices, [0, 1, 2, 5, 4, 3]);
    }
}
