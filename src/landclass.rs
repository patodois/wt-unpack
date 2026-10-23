//! The ground's material data: the land classes a landmesh paints with, and the per-cell weights that
//! blend them. Everything here is read from the level and the game's resources; nothing is a default of
//! ours.
//!
//! The landmesh's detail data (see [`crate::level::DetailMap`]) lists the level's land classes by name
//! and, for every cell of the detail grid (a landmesh cell, 4096 m in War Thunder), up to seven of them
//! with a weight texture. A land class is a game resource (class `03FB59C4`, a [`crate::rodb`] data
//! block) or, when the level stores it, the same block inline. Its `detail` block says what paints it:
//!
//! | kind | keys | meaning |
//! |------|------|---------|
//! | mega | `texture`, `splattingmap`, `detail{Red,Green,Blue,Black}` (`detail1*` normal, `detail2*` reflectance), `detailSize*` | a colour map over the class's `size`, a splat mask and four tiled detail layers |
//! | detailed | `texture`, `detailmap`, `detailmap_r`, `detailSize` | a colour map and one tiled detail |
//! | indexed | `shader`, `texture`, `landClassTextures{indices, flowmap, ...}`, `landClassParams`, `details{...}` | a colour map and an index map choosing one of many detail sets per texel |
//! | simple | `texture` | a colour map |
//!
//! A class's textures tile every `size` metres from `origin * cell size + offset`, with the image's top
//! row at the larger Z (the shaders' V is `-(z - z0) * tile`).
//!
//! The weights follow `land_weight_inc.dshl`, `lmeshWeightAtlasLegacyCpu.cpp` and
//! `lmeshWeightAtlasInternal.h`: a legacy cell record holds two weight textures, four channels and two,
//! mapped to the cell's class slots in order; the channel at the even index just below the class count
//! is not stored but is what the others leave of 1.
//!
//! What the game's land shaders do with the layers (detail tiling blend, splat masks, indexed details)
//! lives in shaders that are public only for the indexed class and not for the mega and detailed ones, so
//! this module does not reproduce them: [`GroundBaker`] blends the classes' colour maps by the cell
//! weights, and the detail layers stay data (see [`LandClassMaterial`]).

use std::collections::HashMap;

use anyhow::{ensure, Context, Result};
use wt_blk::dxp_and_grp::ddsx::{DdsxHeader, Texture};
use wt_blk::dxp_and_grp::grp_pack::GrpPack;

use crate::blk::{Block, Value};
use crate::level::{DetailMap, DetailWeights, LegacyCell, LAND_WEIGHT_DET_NUM};
use crate::rendinst::{ResourceIndex, LAND_CLASS};
use crate::texlib::{Rgba, TextureLibrary};
use crate::vehicle;

/// What paints a land class
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandClassKind {
    Mega,
    Detailed,
    Indexed,
    Simple,
}

/// One tiled detail layer of a land class
#[derive(Debug, Clone, PartialEq)]
pub struct DetailLayer {
    /// `Red`, `Green`, `Blue`, `Black` for mega classes, the detail's name for indexed ones
    pub slot: String,
    pub albedo: Option<String>,
    pub normal: Option<String>,
    pub reflectance: Option<String>,
    /// Metres per tile of the texture
    pub tile_m: Option<f32>,
    pub phys_mat: Option<String>,
}

/// A land class as the level and the game's resources describe it
#[derive(Debug, Clone)]
pub struct LandClassMaterial {
    pub name: String,
    pub kind: LandClassKind,
    /// Colour map (`texture`)
    pub colormap: Option<String>,
    /// Metres the colour map spans, and its offset from the landmesh's corner
    pub size: [f32; 2],
    pub offset: [f32; 2],
    /// Splat mask of a mega class
    pub splat_map: Option<String>,
    pub layers: Vec<DetailLayer>,
    /// The index map and the other textures of an indexed class, by key of `landClassTextures`
    pub extra_textures: Vec<(String, String)>,
    /// The shader an indexed class names
    pub shader: Option<String>,
    /// The whole `detail` block, for whatever the fields above leave out
    pub raw: Block,
}

fn param<'a>(b: &'a Block, key: &str) -> Option<&'a Value> {
    b.params.iter().find(|(n, _)| n == key).map(|(_, v)| v)
}

fn text(b: &Block, key: &str) -> Option<String> {
    match param(b, key) {
        Some(Value::Str(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn real(b: &Block, key: &str) -> Option<f32> {
    match param(b, key) {
        Some(Value::Real(r)) => Some(*r),
        _ => None,
    }
}

fn point2(b: &Block, key: &str) -> Option<[f32; 2]> {
    match param(b, key) {
        Some(Value::Point2(p)) => Some(*p),
        _ => None,
    }
}

impl LandClassMaterial {
    /// Reads a `detail` block (see the module docs). `size` is `[1, 1]` when the block has none, as the
    /// game's `LandDetile::load` has it.
    pub fn from_block(name: &str, detail: &Block) -> Self {
        let size = point2(detail, "size").unwrap_or([1.0, 1.0]);
        let offset = point2(detail, "offset").unwrap_or([0.0, 0.0]);
        let default_phys = text(detail, "physMat");
        let default_size = real(detail, "detailSize");
        let kind = if detail.params.iter().any(|(n, _)| n == "shader") {
            LandClassKind::Indexed
        } else if text(detail, "detailmap").is_some() {
            LandClassKind::Detailed
        } else if text(detail, "splattingmap").is_some() {
            LandClassKind::Mega
        } else {
            LandClassKind::Simple
        };
        let mut layers = Vec::new();
        let mut extra_textures = Vec::new();
        match kind {
            LandClassKind::Mega => {
                for slot in ["Red", "Green", "Blue", "Black"] {
                    let albedo = text(detail, &format!("detail{slot}"));
                    if albedo.is_none() {
                        continue;
                    }
                    layers.push(DetailLayer {
                        slot: slot.to_string(),
                        albedo,
                        normal: text(detail, &format!("detail1{slot}")),
                        reflectance: text(detail, &format!("detail2{slot}")),
                        tile_m: real(detail, &format!("detailSize{slot}")).or(default_size),
                        phys_mat: text(detail, &format!("physMat{slot}")).or_else(|| default_phys.clone()),
                    });
                }
            }
            LandClassKind::Detailed => layers.push(DetailLayer {
                slot: "detail".into(),
                albedo: text(detail, "detailmap"),
                normal: None,
                reflectance: text(detail, "detailmap_r"),
                tile_m: default_size,
                phys_mat: default_phys.clone(),
            }),
            LandClassKind::Indexed => {
                if let Some(b) = detail.blocks.iter().find(|b| b.name == "landClassTextures") {
                    for (k, v) in &b.params {
                        if let Value::Str(s) = v {
                            extra_textures.push((k.clone(), s.clone()));
                        }
                    }
                }
                if let Some(b) = detail.blocks.iter().find(|b| b.name == "details") {
                    for d in b.blocks.iter().filter(|d| !matches!(d.name.as_str(), "scheme" | "detail" | "detail_group")) {
                        layers.push(DetailLayer {
                            slot: d.name.clone(),
                            albedo: text(d, "albedo"),
                            normal: text(d, "normal"),
                            reflectance: text(d, "reflectance"),
                            tile_m: None,
                            phys_mat: None,
                        });
                    }
                }
            }
            LandClassKind::Simple => {}
        }
        LandClassMaterial {
            name: name.to_string(),
            kind,
            colormap: text(detail, "texture"),
            size,
            offset,
            splat_map: text(detail, "splattingmap"),
            layers,
            extra_textures,
            shader: text(detail, "shader"),
            raw: detail.clone(),
        }
    }
}

/// Finds the `detail` block of a land class resource (`landClsExp.cpp`): one nested block holding a
/// zstd or LZMA-packed read-only data block, then the density maps
pub fn resource_detail(res: &[u8]) -> Result<Block> {
    let mut r = crate::bytes::Reader::new(res);
    let (codec, packed) = crate::level::sub_block(&mut r)?;
    let raw = crate::level::unpack(codec, packed, None).context("land class resource")?;
    ensure!(raw.len() >= 4, "land class resource is empty");
    let size = u32::from_le_bytes(raw[..4].try_into().unwrap()) as usize;
    let root = crate::rodb::parse(raw.get(4..4 + size).context("land class data block overruns its resource")?)?;
    root.blocks.into_iter().find(|b| b.name == "detail").context("land class has no detail block")
}

/// Reads the level's land classes: the level's own copy of a class when it stores one, else the
/// game resource of that name. Classes that cannot be read are reported with their error.
pub fn load_land_classes(map: &DetailMap, index: &ResourceIndex) -> Vec<Result<LandClassMaterial>> {
    map.land_classes
        .iter()
        .map(|lc| {
            let block = if !lc.inline_data.is_empty() {
                crate::blk::parse(lc.inline_data, None).with_context(|| format!("land class {}: inline data", lc.name))?
            } else {
                let path = index.find(LAND_CLASS, &lc.name).with_context(|| format!("land class {} is in no pack", lc.name))?;
                let file = vehicle::map(path)?;
                let pack = GrpPack::parse(&file).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
                let res = pack.find(&lc.name, LAND_CLASS).with_context(|| format!("land class {} vanished from {}", lc.name, path.display()))?;
                resource_detail(pack.data(res)).with_context(|| format!("land class {}", lc.name))?
            };
            Ok(LandClassMaterial::from_block(&lc.name, &block))
        })
        .collect()
}

/// The weights of a landmesh's detail grid, decoded: per cell the land class ids of its slots and a
/// weight per slot for every texel of its texture
pub struct GroundWeights {
    /// Cells along X and Z
    pub width: usize,
    pub height: usize,
    /// Texels per cell side
    pub texels: usize,
    /// World X and Z of the first cell's corner
    pub origin: [f32; 2],
    /// Metres per cell side
    pub cell_m: f32,
    cells: Vec<CellWeights>,
}

struct CellWeights {
    classes: Vec<u8>,
    /// `texels * texels` rows of `classes.len()` bytes, rows along +Z from the cell's corner
    weights: Vec<u8>,
}

/// `land_weight_derive`: the stored channels, the derived one, nothing above the class count
fn derive(w: &mut [f32; LAND_WEIGHT_DET_NUM], count: usize) {
    let count = count.clamp(1, LAND_WEIGHT_DET_NUM);
    let derived = (count - 1) & !1;
    for v in w.iter_mut().skip(count) {
        *v = 0.0;
    }
    w[derived] = 0.0;
    let sum: f32 = w[..count].iter().sum();
    w[derived] = (1.0 - sum).clamp(0.0, 1.0);
}

/// The four channels of a weight texture: DXT and 8-bit formats as R, G, B, A; 4-bit ARGB with R at
/// bit 8, G at 4, B at 0 and A at 12 (`LEGACY_TEX1_NIBBLE`)
fn weight_channels(ddsx: &[u8], size: usize) -> Result<Vec<[u8; 4]>> {
    let header = DdsxHeader::parse(ddsx).map_err(|e| anyhow::anyhow!("weight texture: {e:?}"))?;
    let tex = Texture::from_ddsx(header, &ddsx[DdsxHeader::SIZE..]).map_err(|e| anyhow::anyhow!("weight texture: {e:?}"))?;
    ensure!(tex.width() as usize == size && tex.height() as usize == size, "weight texture is {}x{}, not {size}x{size}", tex.width(), tex.height());
    const D3DFMT_A4R4G4B4: u32 = 26;
    if tex.header.d3d_format == D3DFMT_A4R4G4B4 {
        let top = tex.data.get(..size * size * 2).context("short weight texture")?;
        return Ok(top
            .chunks_exact(2)
            .map(|p| {
                let v = u16::from_le_bytes([p[0], p[1]]);
                [8, 4, 0, 12].map(|shift| (((v >> shift) & 15) * 17) as u8)
            })
            .collect());
    }
    let rgba = tex.to_rgba().map_err(|e| anyhow::anyhow!("weight texture: {e:?}"))?;
    Ok(rgba.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect())
}

/// Most texels per side of a weight cell accepted. The game's levels use 64 to 512.
const MAX_TEXELS: usize = 1024;
/// Most texels all the cells of a level may add up to (the game's largest: 256 cells of 512 x 512 = 2^26)
const MAX_WEIGHT_TEXELS: usize = 1 << 28;

impl GroundWeights {
    /// Decodes the weights of a legacy detail map (`world_origin` is the landmesh's first cell corner,
    /// `cell_m` the metres a cell spans). Packed atlases are not decoded yet.
    pub fn from_detail_map(map: &DetailMap, world_origin: [f32; 2], cell_m: f32) -> Result<Self> {
        let texels = map.elem_size as usize;
        ensure!(texels > 0 && texels <= MAX_TEXELS, "weight cells of {} texels per side (up to {MAX_TEXELS} are accepted)", map.elem_size as i32);
        // Every cell gets a buffer of texels x texels x classes bytes whatever the file holds, so the whole grid
        // is bounded rather than trusted
        ensure!(
            map.width.checked_mul(map.height).and_then(|cells| cells.checked_mul(texels * texels)).is_some_and(|n| n <= MAX_WEIGHT_TEXELS),
            "{}x{} weight cells of {texels}x{texels} texels are more than {MAX_WEIGHT_TEXELS} texels in all",
            map.width,
            map.height
        );
        let out = match &map.weights {
            DetailWeights::Legacy { cells } => {
                ensure!(map.tex_size == map.elem_size, "weight textures of {} with {} texels per cell", map.tex_size, map.elem_size);
                cells.iter().map(|c| decode_cell(c, texels)).collect::<Result<Vec<_>>>()?
            }
            DetailWeights::Packed { pages_per_row, records, atlas, .. } => decode_packed(records, atlas, *pages_per_row, texels)?,
        };
        Ok(GroundWeights { width: map.width, height: map.height, texels, origin: world_origin, cell_m, cells: out })
    }

    /// The land class slots at a texel of the whole grid, with their weights (0..1). Coordinates outside the
    /// grid clamp to its edge.
    pub fn texel(&self, tx: i64, tz: i64) -> Vec<(u8, f32)> {
        let n = self.texels as i64;
        let (tx, tz) = (tx.clamp(0, self.width as i64 * n - 1), tz.clamp(0, self.height as i64 * n - 1));
        let cell = &self.cells[(tz / n) as usize * self.width + (tx / n) as usize];
        let k = cell.classes.len();
        let at = ((tz % n) * n + tx % n) as usize * k;
        cell.classes.iter().zip(&cell.weights[at..at + k]).map(|(&c, &w)| (c, f32::from(w) / 255.0)).collect()
    }

    /// Bilinear weights per land class at a world position, as `(class, weight)` for the classes that
    /// have any. Texel centres sit half a texel in from the cell corner.
    pub fn at(&self, x: f32, z: f32, out: &mut Vec<(u8, f32)>) {
        out.clear();
        let texel_m = self.cell_m / self.texels as f32;
        let fx = (x - self.origin[0]) / texel_m - 0.5;
        let fz = (z - self.origin[1]) / texel_m - 0.5;
        let (x0, z0) = (fx.floor(), fz.floor());
        let (ax, az) = (fx - x0, fz - z0);
        let (x0, z0) = (x0 as i64, z0 as i64);
        for (dx, dz, w) in [(0, 0, (1.0 - ax) * (1.0 - az)), (1, 0, ax * (1.0 - az)), (0, 1, (1.0 - ax) * az), (1, 1, ax * az)] {
            if w <= 0.0 {
                continue;
            }
            for (class, cw) in self.texel(x0.saturating_add(dx), z0.saturating_add(dz)) {
                match out.iter_mut().find(|(c, _)| *c == class) {
                    Some((_, acc)) => *acc += w * cw,
                    None => out.push((class, w * cw)),
                }
            }
        }
    }

    /// Land class ids that have any weight anywhere, with the number of cells that use them
    pub fn class_usage(&self) -> Vec<(u8, usize)> {
        let mut counts: HashMap<u8, usize> = HashMap::new();
        for c in &self.cells {
            for &id in &c.classes {
                *counts.entry(id).or_default() += 1;
            }
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// Bytes held
    pub fn memory(&self) -> usize {
        self.cells.iter().map(|c| c.weights.len() + c.classes.len()).sum()
    }
}

/// The weights a packed cell stores (`get_weights` in `land_weight_inc.dshl`): page 0 holds channels 0 to
/// 2 in its colour channels, page 1 channels 3 to 5; the last channel, `count - 1`, is what the others
/// leave of 1 and nothing is stored above it
fn packed_weights(count: usize, page0: [u8; 3], page1: [u8; 3]) -> [f32; LAND_WEIGHT_DET_NUM] {
    let mut w = [0.0f32; LAND_WEIGHT_DET_NUM];
    if count <= 1 {
        w[0] = 1.0;
        return w;
    }
    let f = |v: u8| f32::from(v) / 255.0;
    for (c, v) in page0.iter().enumerate() {
        w[c] = f(*v);
    }
    if count > 4 {
        for (c, v) in page1.iter().enumerate() {
            w[3 + c] = f(*v);
        }
    }
    let last = count.min(LAND_WEIGHT_DET_NUM) - 1;
    for v in w.iter_mut().skip(last) {
        *v = 0.0;
    }
    let sum: f32 = w.iter().sum();
    w[last] = (1.0 - sum).clamp(0.0, 1.0);
    w
}

/// Decodes a packed atlas: the pages hold each cell's texels with a border of its neighbours' weights
/// (`LAND_WEIGHT_BORDER`), page `p` at column `p % pages_per_row` and row `p / pages_per_row`
fn decode_packed(records: &[u32], atlas: &[u8], pages_per_row: usize, texels: usize) -> Result<Vec<CellWeights>> {
    const WORDS: usize = 3;
    let header = DdsxHeader::parse(atlas).map_err(|e| anyhow::anyhow!("weight atlas: {e:?}"))?;
    let tex = Texture::from_ddsx(header, &atlas[DdsxHeader::SIZE..]).map_err(|e| anyhow::anyhow!("weight atlas: {e:?}"))?;
    let (aw, ah) = (tex.width() as usize, tex.height() as usize);
    ensure!(pages_per_row > 0 && aw % pages_per_row == 0, "{pages_per_row} pages do not tile an atlas {aw} wide");
    let page_w = aw / pages_per_row;
    ensure!(page_w >= texels && (page_w - texels) % 2 == 0, "pages of {page_w} texels cannot hold cells of {texels}");
    let border = (page_w - texels) / 2;
    let rgba = tex.to_rgba().map_err(|e| anyhow::anyhow!("weight atlas: {e:?}"))?;
    let pixel = |page: usize, tx: usize, tz: usize| -> Result<[u8; 3]> {
        let (px, py) = ((page % pages_per_row) * page_w + border + tx, (page / pages_per_row) * page_w + border + tz);
        ensure!(py < ah, "weight page {page} is outside the atlas");
        let i = (py * aw + px) * 4;
        Ok([rgba[i], rgba[i + 1], rgba[i + 2]])
    };
    let mut out = Vec::with_capacity(records.len() / WORDS);
    for rec in records.chunks_exact(WORDS) {
        let count = ((rec[0] >> 28) & 7) as usize + 1;
        let (page0, page1) = ((rec[0] & 0x3FFF) as usize, ((rec[0] >> 14) & 0x3FFF) as usize);
        let ids: Vec<u8> = rec[1..].iter().flat_map(|w| w.to_le_bytes()).take(count.min(LAND_WEIGHT_DET_NUM)).collect();
        let k = ids.len();
        let mut weights = vec![0u8; texels * texels * k];
        for tz in 0..texels {
            for tx in 0..texels {
                let p0 = if count > 1 { pixel(page0, tx, tz)? } else { [0; 3] };
                let p1 = if count > 4 { pixel(page1, tx, tz)? } else { [0; 3] };
                let w = packed_weights(count, p0, p1);
                let at = (tz * texels + tx) * k;
                for c in 0..k {
                    weights[at + c] = (w[c] * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                }
            }
        }
        out.push(CellWeights { classes: ids, weights });
    }
    Ok(out)
}

fn decode_cell(cell: &LegacyCell, texels: usize) -> Result<CellWeights> {
    let classes: Vec<u8> = cell.ids.iter().copied().filter(|&i| i != 0xFF).collect();
    let k = classes.len();
    if k == 0 {
        return Ok(CellWeights { classes, weights: Vec::new() });
    }
    let tex1 = if cell.tex1.is_empty() { None } else { Some(weight_channels(cell.tex1, texels)?) };
    let tex2 = if cell.tex2.is_empty() { None } else { Some(weight_channels(cell.tex2, texels)?) };
    let mut weights = vec![0u8; texels * texels * k];
    for t in 0..texels * texels {
        let mut w = [0.0f32; LAND_WEIGHT_DET_NUM];
        if let Some(a) = &tex1 {
            for c in 0..4 {
                w[c] = f32::from(a[t][c]) / 255.0;
            }
        }
        if let Some(b) = &tex2 {
            for c in 0..2 {
                w[4 + c] = f32::from(b[t][c]) / 255.0;
            }
        }
        derive(&mut w, k);
        for c in 0..k {
            weights[t * k + c] = (w[c] * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
    Ok(CellWeights { classes, weights })
}

/// Blends land classes' colour maps by their weights into ground colour pictures
pub struct GroundBaker {
    weights: GroundWeights,
    /// Per land class: its colour map, its size in metres and the world position of its image corner
    classes: Vec<Option<(Rgba, [f32; 2], [f32; 2])>>,
    /// sRGB to linear
    to_linear: Vec<f32>,
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

impl GroundBaker {
    /// `colormaps` holds each land class's decoded colour map (the class's own `size` and `offset` place
    /// it); `world_origin` is the landmesh's first cell corner
    pub fn new(weights: GroundWeights, classes: &[Option<LandClassMaterial>], colormaps: Vec<Option<Rgba>>, world_origin: [f32; 2]) -> Self {
        let classes = classes
            .iter()
            .zip(colormaps)
            .map(|(c, img)| {
                let (c, img) = (c.as_ref()?, img?);
                Some((img, c.size, [world_origin[0] + c.offset[0], world_origin[1] + c.offset[1]]))
            })
            .collect();
        GroundBaker { weights, classes, to_linear: (0..256).map(|i| srgb_to_linear(i as f32 / 255.0)).collect() }
    }

    pub fn weights(&self) -> &GroundWeights {
        &self.weights
    }

    /// Bilinear sample of a wrapped colour map at world (x, z), linear RGB
    fn sample(&self, class: usize, x: f32, z: f32) -> Option<[f32; 3]> {
        let (img, size, corner) = self.classes.get(class)?.as_ref()?;
        let (w, h) = (img.width as f32, img.height as f32);
        let u = (x - corner[0]) / size[0];
        // The image's top row is at the larger Z
        let v = -(z - corner[1]) / size[1];
        let px = (u - u.floor()) * w - 0.5;
        let py = (v - v.floor()) * h - 0.5;
        let (x0, y0) = (px.floor(), py.floor());
        let (ax, ay) = (px - x0, py - y0);
        let wrap = |i: f32, n: u32| (i as i64).rem_euclid(i64::from(n)) as usize;
        let mut out = [0.0f32; 3];
        for (dx, dy, wt) in [(0.0, 0.0, (1.0 - ax) * (1.0 - ay)), (1.0, 0.0, ax * (1.0 - ay)), (0.0, 1.0, (1.0 - ax) * ay), (1.0, 1.0, ax * ay)] {
            let i = (wrap(y0 + dy, img.height) * img.width as usize + wrap(x0 + dx, img.width)) * 4;
            for c in 0..3 {
                out[c] += wt * self.to_linear[usize::from(img.rgba[i + c])];
            }
        }
        Some(out)
    }

    /// Ground colour (sRGB, 8 bit) over the world rectangle `[x0, z0, x1, z1]` at `texel_m` metres per
    /// pixel: row 0 at `z0`, column 0 at `x0`. Pixels no class paints stay transparent black.
    pub fn bake(&self, rect: [f32; 4], texel_m: f32) -> Rgba {
        let width = (((rect[2] - rect[0]) / texel_m).round() as u32).max(1);
        let height = (((rect[3] - rect[1]) / texel_m).round() as u32).max(1);
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let mut ws = Vec::with_capacity(8);
        for py in 0..height {
            let z = rect[1] + (py as f32 + 0.5) * texel_m;
            for px in 0..width {
                let x = rect[0] + (px as f32 + 0.5) * texel_m;
                self.weights.at(x, z, &mut ws);
                let (mut acc, mut total) = ([0.0f32; 3], 0.0f32);
                for &(class, w) in &ws {
                    if let Some(c) = self.sample(usize::from(class), x, z) {
                        for k in 0..3 {
                            acc[k] += w * c[k];
                        }
                        total += w;
                    }
                }
                if total > 0.0 {
                    let i = (py as usize * width as usize + px as usize) * 4;
                    for k in 0..3 {
                        rgba[i + k] = (linear_to_srgb((acc[k] / total).clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
                    }
                    rgba[i + 3] = 255;
                }
            }
        }
        Rgba { width, height, rgba }
    }
}

/// Decodes the colour maps of land classes. Each keeps its own size unless all of them together would
/// pass `budget_mb`; then the largest side allowed halves until they fit.
pub fn decode_colormaps(classes: &[Option<LandClassMaterial>], textures: &TextureLibrary, budget_mb: u32, warn: &mut dyn FnMut(String)) -> Vec<Option<Rgba>> {
    let names: Vec<Option<&str>> = classes.iter().map(|c| c.as_ref().and_then(|c| c.colormap.as_deref())).collect();
    let sizes: Vec<Option<(u32, u32)>> = names.iter().map(|n| n.and_then(|n| textures.size(n).ok().flatten())).collect();
    let bytes = |cap: u32| -> u64 { sizes.iter().flatten().map(|&(w, h)| u64::from(w.min(cap)) * u64::from(h.min(cap)) * 4).sum() };
    let mut cap = 16384u32;
    while cap > 64 && bytes(cap) > u64::from(budget_mb) << 20 {
        cap /= 2;
    }
    classes
        .iter()
        .zip(&names)
        .map(|(c, name)| {
            let (c, name) = (c.as_ref()?, (*name)?);
            match textures.decode(name, cap) {
                Ok(Some(img)) => Some(img),
                Ok(None) => {
                    warn(format!("land class {}: colour map {name} is in no texture pack", c.name));
                    None
                }
                Err(e) => {
                    warn(format!("land class {}: colour map {name}: {e:#}", c.name));
                    None
                }
            }
        })
        .collect()
}

/// A level's ground material data, gathered for the CLI and the scene builder
pub struct GroundMaterial {
    pub classes: Vec<Option<LandClassMaterial>>,
    pub errors: Vec<String>,
    pub weights: Option<GroundWeights>,
}

impl GroundMaterial {
    pub fn load(level: &crate::level::Level, index: &ResourceIndex) -> Result<Option<Self>> {
        let Some(lm) = level.landmesh()? else { return Ok(None) };
        let map = lm.detail_map()?;
        let mut classes = Vec::new();
        let mut errors = Vec::new();
        for c in load_land_classes(&map, index) {
            match c {
                Ok(c) => classes.push(Some(c)),
                Err(e) => {
                    errors.push(format!("{e:#}"));
                    classes.push(None);
                }
            }
        }
        let corner = [lm.origin[0] as f32 * lm.land_cell_size, lm.origin[1] as f32 * lm.land_cell_size];
        let weights = match GroundWeights::from_detail_map(&map, corner, lm.land_cell_size * lm.cells[0] as f32 / map.width as f32) {
            Ok(w) => Some(w),
            Err(e) => {
                errors.push(format!("ground weights: {e:#}"));
                None
            }
        };
        Ok(Some(GroundMaterial { classes, errors, weights }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &[(&str, Value)]) -> Block {
        Block { name: "detail".into(), params: text.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), blocks: Vec::new() }
    }

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    #[test]
    fn mega_classes_list_their_layers() {
        let b = block(&[
            ("texture", s("city*")),
            ("splattingmap", s("city_m")),
            ("detailRed", s("grass_d")),
            ("detail2Red", s("grass_r")),
            ("detailSizeRed", Value::Real(23.3)),
            ("detailBlack", s("asphalt_d")),
            ("detailSize", Value::Real(33.9)),
            ("size", Value::Point2([2048.0, 2048.0])),
            ("physMat", s("soil")),
        ]);
        let c = LandClassMaterial::from_block("city", &b);
        assert_eq!(c.kind, LandClassKind::Mega);
        assert_eq!((c.size, c.colormap.as_deref(), c.splat_map.as_deref()), ([2048.0, 2048.0], Some("city*"), Some("city_m")));
        assert_eq!(c.layers.len(), 2);
        assert_eq!((c.layers[0].slot.as_str(), c.layers[0].tile_m, c.layers[0].reflectance.as_deref()), ("Red", Some(23.3), Some("grass_r")));
        // The default detail size and physical material apply where a layer has none of its own
        assert_eq!((c.layers[1].tile_m, c.layers[1].phys_mat.as_deref()), (Some(33.9), Some("soil")));
    }

    #[test]
    fn kinds_follow_the_keys() {
        let kind = |p: &[(&str, Value)]| LandClassMaterial::from_block("x", &block(p)).kind;
        assert_eq!(kind(&[("texture", s("a*")), ("detailmap", s("d"))]), LandClassKind::Detailed);
        assert_eq!(kind(&[("texture", s("a*")), ("shader", s("landmesh_indexed_landclass"))]), LandClassKind::Indexed);
        assert_eq!(kind(&[("texture", s("a*"))]), LandClassKind::Simple);
        assert_eq!(LandClassMaterial::from_block("x", &block(&[])).size, [1.0, 1.0]);
    }

    #[test]
    fn the_derived_weight_is_what_the_others_leave() {
        // Three classes: channel 2 is derived
        let mut w = [0.25, 0.5, 0.9, 0.9, 0.9, 0.9, 0.9];
        derive(&mut w, 3);
        assert_eq!(w, [0.25, 0.5, 0.25, 0.0, 0.0, 0.0, 0.0]);
        // Two classes: channel 0 is derived
        let mut w = [0.9, 0.3, 0.0, 0.0, 0.0, 0.0, 0.0];
        derive(&mut w, 2);
        assert!((w[0] - 0.7).abs() < 1e-6 && (w[1] - 0.3).abs() < 1e-6);
        // Five classes: channel 4
        let mut w = [0.1, 0.1, 0.1, 0.1, 0.0, 0.0, 0.0];
        derive(&mut w, 5);
        assert!((w[4] - 0.6).abs() < 1e-6);
        // One class: all of it
        let mut w = [0.0; 7];
        derive(&mut w, 1);
        assert_eq!(w[0], 1.0);
    }

    #[test]
    fn packed_weights_derive_the_last_channel() {
        // One class: all of it, whatever the pages hold
        assert_eq!(packed_weights(1, [9, 9, 9], [9, 9, 9])[0], 1.0);
        // Four classes: page 0 holds channels 0 to 2, channel 3 is what they leave
        let w = packed_weights(4, [51, 51, 51], [255, 255, 255]);
        assert!((w[0] - 0.2).abs() < 1e-6 && (w[2] - 0.2).abs() < 1e-6 && (w[3] - 0.4).abs() < 1e-6);
        assert_eq!(&w[4..], &[0.0; 3]);
        // Two classes: channel 0 is stored, channel 1 is the rest; the other page colours are not read
        let w = packed_weights(2, [102, 255, 255], [0; 3]);
        assert!((w[0] - 0.4).abs() < 1e-6 && (w[1] - 0.6).abs() < 1e-6 && w[2] == 0.0);
        // Seven classes: page 1 holds channels 3 to 5, channel 6 is derived
        let w = packed_weights(7, [25, 25, 25], [25, 25, 25]);
        assert!((w[6] - (1.0 - 6.0 * 25.0 / 255.0)).abs() < 1e-5);
    }

    fn grid(texels: usize) -> GroundWeights {
        // Two cells side by side: cell 0 blends class 3 (weight 1), cell 1 class 5
        let cell = |class: u8| CellWeights { classes: vec![class], weights: vec![255; texels * texels] };
        GroundWeights { width: 2, height: 1, texels, origin: [100.0, 200.0], cell_m: 8.0, cells: vec![cell(3), cell(5)] }
    }

    #[test]
    fn weights_for_positions_off_the_map_do_not_overflow() {
        let g = grid(2);
        let mut out = Vec::new();
        // floor() of an infinite coordinate saturates at i64::MAX, and `+ 1` on it overflowed
        for (x, z) in [(f32::INFINITY, 0.0), (f32::NEG_INFINITY, 1e30), (f32::NAN, 5.0), (1e30, f32::INFINITY)] {
            g.at(x, z, &mut out);
        }
        // Far outside the map the edge cell's weights stand in
        g.at(1e30, 201.0, &mut out);
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [5]);
    }

    fn legacy_map<'a>(width: usize, height: usize, elem_size: u32, ids: [u8; LAND_WEIGHT_DET_NUM]) -> DetailMap<'a> {
        let cell = || LegacyCell { ids, tex1: &[], tex2: &[] };
        DetailMap {
            width,
            height,
            tex_size: elem_size,
            elem_size,
            land_classes: Vec::new(),
            weights: DetailWeights::Legacy { cells: (0..width * height).map(|_| cell()).collect() },
        }
    }

    #[test]
    fn weight_cells_are_bounded_by_what_a_level_can_hold() {
        let err = |map: &DetailMap| GroundWeights::from_detail_map(map, [0.0; 2], 100.0).err().map(|e| e.to_string()).unwrap_or_default();
        let one_class = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        // The game's levels use 64 to 512 texels per cell
        let g = GroundWeights::from_detail_map(&legacy_map(2, 2, 128, one_class), [0.0; 2], 100.0).unwrap();
        assert_eq!((g.texels, g.texel(0, 0)), (128, vec![(0, 1.0)]));
        // A cell of 4,000 x 4,000 texels (7 classes: 112 MB for the one cell, 17.5 GB at 50,000), a count
        // the file negated (elem_size read as u32), and a grid whose cells add up to more than 2^28 texels
        for (map, what) in [
            (legacy_map(1, 1, 4000, [0, 1, 2, 3, 4, 5, 6]), "4000 texels"),
            (legacy_map(1, 1, -1i32 as u32, one_class), "-1 texels"),
            (legacy_map(17, 17, 1024, [0xFF; 7]), "17x17 cells of 1024"),
        ] {
            let e = err(&map);
            assert!(e.contains("texels"), "{what}: {e:?}");
        }
    }

    #[test]
    fn weights_blend_across_cells_bilinearly() {
        let g = grid(2);
        let mut out = Vec::new();
        // Inside the first cell, away from the edge
        g.at(101.0, 201.0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 3);
        assert!((out[0].1 - 1.0).abs() < 1e-5);
        // Straight on the border between the cells: half of each
        g.at(108.0, 202.0, &mut out);
        out.sort_by_key(|o| o.0);
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [3, 5]);
        assert!((out[0].1 - 0.5).abs() < 1e-5 && (out[1].1 - 0.5).abs() < 1e-5);
    }

    #[test]
    fn colour_maps_tile_with_the_top_row_at_larger_z() {
        // A 1 x 2 map: red on the top row, blue on the bottom; 10 m across, from the origin
        let img = Rgba { width: 1, height: 2, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        let class = LandClassMaterial::from_block("c", &block(&[("texture", s("c*")), ("size", Value::Point2([10.0, 10.0]))]));
        let baker = GroundBaker::new(grid(2), &[None, None, None, Some(class)], vec![None, None, None, Some(img)], [0.0, 0.0]);
        // Class 3 at z just above the corner: v = -0.1 wraps to 0.9: the bottom row (blue)
        let low = baker.sample(3, 5.0, 1.0).unwrap();
        let high = baker.sample(3, 5.0, 9.0).unwrap();
        assert!(low[2] > low[0], "{low:?}");
        assert!(high[0] > high[2], "{high:?}");
        // A whole tile later it repeats
        let again = baker.sample(3, 5.0, 19.0).unwrap();
        assert!((again[0] - high[0]).abs() < 1e-5);
    }

    #[test]
    fn baking_leaves_unpainted_pixels_clear() {
        let class = LandClassMaterial::from_block("c", &block(&[("texture", s("c*")), ("size", Value::Point2([10.0, 10.0]))]));
        let img = Rgba { width: 1, height: 1, rgba: vec![128, 128, 128, 255] };
        // Only class 3 has a colour map; cell 1 (x from 108 on) uses class 5, which has none
        let baker = GroundBaker::new(grid(2), &[None, None, None, Some(class)], vec![None, None, None, Some(img)], [0.0, 0.0]);
        let out = baker.bake([100.0, 200.0, 116.0, 208.0], 4.0);
        assert_eq!((out.width, out.height), (4, 2));
        assert_eq!(out.rgba[3], 255);
        assert_eq!(out.rgba[(3) * 4 + 3], 0);
        assert!((i32::from(out.rgba[0]) - 128).abs() <= 1);
    }
}
