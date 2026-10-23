//! Assembles a vehicle from the game files: meshes placed by their skeleton, materials with decoded
//! textures (camouflage and occlusion baked in, Dagor's packed normal split into PBR maps).
//!
//! The result is in a right-handed, Y-up space in metres (glTF and Bevy convention), independent of
//! any output format.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use memmap2::Mmap;

use wt_blk::dxp_and_grp::{
    dynmodel::{DynModel, Elem, Lod, Skin, VData},
    geom_node_tree::GeomNodeTree,
    grp_pack::{class_id, GrpPack},
};

use crate::{blk, bytes, camo};

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    fn view(&self) -> camo::Image<'_> {
        camo::Image { width: self.width as usize, height: self.height as usize, rgba: &self.rgba }
    }
}

/// How a material's alpha is used
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alpha {
    /// The alpha channel is not opacity (for masked tank, ship and chrome shaders it is a paint or shine mask)
    Opaque,
    /// Alpha-tested (cut-out): texels with alpha below the cutoff, in 0..=1, are discarded
    Mask(f32),
    /// Alpha-blended
    Blend,
}

/// How a material uses its alpha, from the shader class and parameters that `dynModelDesc.bin` gives it
/// (`cls`, and `atest=N` among the `par` lines, N in 0..=255):
///
/// - classes with `glass` in their name, `*alpha_blend`, `aces_weapon_fire` (muzzle flashes and tracers) and
///   `propeller_side` (the blurred disc of a spinning propeller seen from the side) blend; the textures of
///   the last three are mostly transparent, so they cannot be drawn opaque;
/// - an `atest=N` parameter with N > 0 makes the material alpha-tested with cutoff N / 255, in any other
///   class (`dynamic_masked_chrome_bump` with `atest=127` is a cut-out; without it, its alpha is a shine
///   mask, as for every `masked_*` class);
/// - the `*atest` classes (`dynamic_tank_atest`, `dynamic_tank_net_atest`, `dynamic_layered_atest`) are
///   alpha-tested even without the parameter, at the cutoff 0.5 (the shader's own default is not in the data);
/// - everything else is opaque.
///
/// The blend state itself lives in the compiled shader dumps (`compiledShaders/*.shdump.bin`), which are not
/// decoded here; the classes above are the ones whose names and textures say it.
pub fn material_alpha(class: &str, atest: Option<u32>) -> Alpha {
    if class.contains("glass") || class.ends_with("alpha_blend") || class == "aces_weapon_fire" || class == "propeller_side" {
        Alpha::Blend
    } else if let Some(n) = atest.filter(|&n| n > 0) {
        Alpha::Mask(n.min(255) as f32 / 255.0)
    } else if class.contains("atest") && atest != Some(0) {
        Alpha::Mask(0.5)
    } else {
        Alpha::Opaque
    }
}

pub struct Material {
    pub name: String,
    /// Base colour, with camouflage and occlusion baked in where the material has them
    pub color: Option<Image>,
    /// glTF-style metallic-roughness map: G roughness, B metalness
    pub metal_rough: Option<Image>,
    /// Tangent-space normal map, OpenGL convention (green up)
    pub normal: Option<Image>,
    pub double_sided: bool,
    /// How the material's alpha is used, see [`material_alpha`]
    pub alpha: Alpha,
    /// Stable names for the textures, derived from the game's texture names
    pub color_key: Option<String>,
    pub pbr_key: Option<String>,
}

#[derive(Default)]
pub struct Mesh {
    pub material: usize,
    /// Skeleton node the mesh is attached to (an index into `Vehicle::nodes`); positions are still in
    /// model space
    pub node: usize,
    /// A skinned mesh (tracks, cables), in its bind pose
    pub skinned: bool,
    /// Skinned meshes: the skeleton nodes (indices into `Vehicle::nodes`) that move the vertices, from
    /// the model's bone table. `joints` refer to this list.
    pub bones: Vec<usize>,
    /// Skinned meshes, per vertex: the `bones` that move it (unused entries are 0 with weight 0) ...
    pub joints: Vec<[u16; 4]>,
    /// ... and their weights, which sum to 1
    pub weights: Vec<[f32; 4]>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// A rigid motion in model space as a column-major 3x4 matrix: three axis columns, then the translation
pub type Affine = [f32; 12];

pub const IDENTITY: Affine = [1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.];

impl Mesh {
    /// Vertex positions of a skinned mesh once its bones have moved, blended by the vertex weights
    /// (linear blend skinning, as the game does it). `moves[i]` is how bone `self.bones[i]` has moved
    /// from its bind pose, in model space: the node's transform now, times the inverse of its bind
    /// transform. All identities give back `positions`; a bone without an entry stays put.
    pub fn skinned_positions(&self, moves: &[Affine]) -> Vec<[f32; 3]> {
        let apply = |m: &Affine, p: [f32; 3]| [0, 1, 2].map(|r| m[r] * p[0] + m[3 + r] * p[1] + m[6 + r] * p[2] + m[9 + r]);
        self.positions
            .iter()
            .enumerate()
            .map(|(v, &p)| {
                let (joints, weights) = (self.joints.get(v), self.weights.get(v));
                let (Some(joints), Some(weights)) = (joints, weights) else { return p };
                let mut out = [0.0; 3];
                for (&j, &w) in joints.iter().zip(weights) {
                    if w > 0.0 {
                        let q = apply(moves.get(usize::from(j)).unwrap_or(&IDENTITY), p);
                        (0..3).for_each(|k| out[k] += w * q[k]);
                    }
                }
                out
            })
            .collect()
    }
}

/// A skeleton node. Only its origin is kept: the axes of Dagor's nodes vary (many are mirrored), and
/// animating a part only needs the point it turns about.
pub struct Node {
    pub name: String,
    /// Parent node; parents come before their children
    pub parent: Option<usize>,
    /// Origin in model space
    pub pivot: [f32; 3],
}

pub struct Vehicle {
    pub name: String,
    /// One mesh per node and material
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    /// The skeleton; node 0 is the model origin
    pub nodes: Vec<Node>,
}

/// Texture name (without anything from the `*` on) -> pack holding a non-empty copy of it. The paths are
/// shared, since a pack holds thousands of textures.
pub type TextureIndex = HashMap<String, Arc<Path>>;

/// Indexes the textures of every `.dxp.bin` pack under the game folder (patched packs win)
pub fn texture_index(game: &Path) -> Result<TextureIndex> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.to_string_lossy().ends_with(".dxp.bin") {
                out.push(p);
            }
        }
    }
    let mut packs = Vec::new();
    walk(game, &mut packs);
    // Patched packs override the originals
    packs.sort_by_key(|p| p.to_string_lossy().contains("/patch/"));
    let mut index = HashMap::new();
    for path in packs {
        let mut f = fs::File::open(&path)?;
        let shared: Arc<Path> = Arc::from(path.as_path());
        let mut head = [0u8; 16];
        if f.read_exact(&mut head).is_err() || &head[..4] != b"DxP2" {
            continue;
        }
        let dump_size = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
        let mut dump = vec![0u8; dump_size];
        f.read_exact(&mut dump)?;
        let mut r = bytes::Reader::new(&dump);
        let (ofs, count) = (r.u32()? as usize, r.u32()? as usize);
        r.skip(8)?;
        let headers = r.u32()? as usize;
        for i in 0..count {
            // Skip placeholder entries, whose DDSx header is empty apart from the label
            let mut h = bytes::Reader::at(&dump, headers + i * 32 + 4)?;
            if h.u32()? == 0 && h.u32()? == 0 && h.u32()? == 0 {
                continue;
            }
            let name_ofs = bytes::Reader::at(&dump, ofs + i * 8)?.u64()? as usize;
            let name = bytes::Reader::at(&dump, name_ofs)?.cstr()?;
            // Names look like "tex_c*", "tex_c$hq*" or "tex_c*?q0-1-1u01"
            let key = name.split('*').next().unwrap_or(&name).to_string();
            index.insert(key, shared.clone());
        }
    }
    Ok(index)
}

/// Maps a game file into memory. Packs reach 1.2 GB; mapped, only the pages actually read are loaded,
/// and the system can drop them again under memory pressure.
pub fn map(path: &Path) -> Result<Mmap> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    // Safety: the game's files are not modified while we read them
    unsafe { Mmap::map(&file) }.with_context(|| format!("mapping {}", path.display()))
}

/// Decodes textures by name, preferring the high-quality copy, keeping opened packs mapped
struct TextureSource<'a> {
    index: &'a TextureIndex,
    packs: HashMap<PathBuf, Mmap>,
}

impl TextureSource<'_> {
    fn decode(&mut self, tex: &str) -> Result<Option<Image>> {
        let Some((full, path)) = [format!("{tex}$hq"), tex.to_string()]
            .into_iter()
            .find_map(|n| self.index.get(&n).map(|p| (n, PathBuf::from(&**p))))
        else {
            return Ok(None);
        };
        if !self.packs.contains_key(&path) {
            self.packs.insert(path.clone(), map(&path)?);
        }
        let pack = wt_blk::dxp_and_grp::dxp::DxpPack::parse(&self.packs[&path])?;
        let i = pack
            .entries
            .iter()
            .position(|e| e.name.split('*').next() == Some(full.as_str()) && !e.header.is_empty())
            .context("texture vanished")?;
        let texture = pack.texture(i)?;
        Ok(Some(Image { width: texture.width(), height: texture.height(), rgba: texture.to_rgba()? }))
    }

    /// Like `decode`, reporting failures instead of returning them
    fn decode_or_warn(&mut self, tex: &Option<String>) -> Option<Image> {
        let t = tex.as_deref()?;
        self.decode(t).unwrap_or_else(|e| {
            eprintln!("texture {t}: {e:#}");
            None
        })
    }
}

struct MatInfo {
    name: String,
    class: String,
    color: Option<String>,
    camo: Option<String>,
    occlusion: Option<String>,
    packed_normal: Option<String>,
    double_sided: bool,
    skinned: bool,
    index: usize,
    /// `atest=N` among the material's `par` lines
    atest: Option<u32>,
}

impl MatInfo {
    fn alpha(&self) -> Alpha {
        material_alpha(&self.class, self.atest)
    }

    /// Name of the baked colour texture, when the material needs baking
    fn bake_key(&self) -> Option<String> {
        let c = self.color.as_ref()?;
        (self.camo.is_some() || self.occlusion.is_some()).then(|| {
            format!("{c}__{}__{}", self.camo.as_deref().unwrap_or("nocamo"), self.occlusion.as_deref().unwrap_or("noao"))
        })
    }
}

/// Material descriptions of every model, from dynModelDesc.bin (patched copy first)
fn model_descs(game: &Path) -> Result<HashMap<String, blk::Block>> {
    let mut descs = HashMap::new();
    for rel in ["patch/content/base/res/dynModelDesc.bin", "content/base/res/dynModelDesc.bin"] {
        let Ok(raw) = fs::read(game.join(rel)) else { continue };
        let root = blk::parse(&raw, None).with_context(|| format!("parsing {rel}"))?;
        for b in root.blocks {
            descs.entry(b.name.clone()).or_insert(b);
        }
    }
    Ok(descs)
}

/// Materials and texture names of a model
fn material_infos(descs: &HashMap<String, blk::Block>, name: &str) -> Result<Vec<MatInfo>> {
    let desc = descs.get(name).with_context(|| format!("{name} not found in dynModelDesc.bin"))?;
    let child = |n: &str| desc.blocks.iter().find(|b| b.name == n);
    let tex_names: Vec<String> = child("tex")
        .map(|b| {
            b.params
                .iter()
                .filter_map(|(_, v)| match v {
                    blk::Value::Str(s) => Some(s.trim_end_matches('*').to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    // matR describes the rigid parts' materials, matS the skinned meshes'
    let mut infos = Vec::new();
    for block in ["matR", "matS"] {
        let Some(list) = child(block) else { continue };
        for (i, m) in list.blocks.iter().enumerate() {
            let get = |k: &str| m.params.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let tex = |k: &str| match get(k) {
                Some(blk::Value::Int(t)) => tex_names.get(*t as usize).cloned(),
                _ => None,
            };
            let class = match get("cls") {
                Some(blk::Value::Str(s)) => s.clone(),
                _ => String::new(),
            };
            // Masked tank shaders take the camouflage in t1; other masked shaders use t1 for something else
            let camo = tex("t1").filter(|t| class.contains("masked") && t.contains("_camo"));
            let par = match get("par") {
                Some(blk::Value::Str(s)) => s.as_str(),
                _ => "",
            };
            let atest = par.lines().find_map(|l| l.trim().strip_prefix("atest")?.trim_start().strip_prefix('=')?.trim().parse::<u32>().ok());
            infos.push(MatInfo {
                atest,
                name: format!("{block}{i}_{class}"),
                color: tex("t0"),
                camo,
                occlusion: tex("t3").filter(|t| t.ends_with("_ao")),
                packed_normal: tex("t2").filter(|t| t.ends_with("_n")),
                double_sided: matches!(get("two_sided"), Some(blk::Value::Bool(true))),
                skinned: block == "matS",
                index: i,
                class,
            });
        }
    }
    Ok(infos)
}

/// Column-major 4x4 node transform
fn transform(m: &[f32; 16], p: [f32; 3], w: f32) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r] * w)
}

/// Makes a mesh's triangles wind counter-clockwise around its vertex normals, as the bridge requires.
///
/// The stored triangles of most parts come out right once the z mirror reverses them, whatever the sign of
/// the determinant of their node's matrix, because the exporter writes each part's winding for its own
/// matrix. A few parts do not: `germ_he_115`'s `radiator2_25` (768 triangles, a node with determinant +2.36)
/// and `prop_side_2` (2, determinant +0.08), the Ontos' `bone_suspension_l_02` and `_03` (886 each, proper
/// rotations among nodes whose right-hand twins are mirrored). Neither the sign of the determinant nor the
/// material tells them apart (the same materials and matrices wind correctly on other parts), so the stored
/// data is read the way the shader reads it: the vertex normals say which side is the front, and a mesh
/// whose triangles face the other way is turned over as a whole. The normals are the part lighting uses, so
/// they are kept as they are; meshes that are only partly against them (smoothed edges) are left alone.
fn face_normals_out(mesh: &mut Mesh) {
    let (mut with, mut against) = (0usize, 0usize);
    for t in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[t[k] as usize]);
        let (e1, e2) = ([0, 1, 2].map(|k| b[k] - a[k]), [0, 1, 2].map(|k| c[k] - a[k]));
        let g = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let n: f32 = (0..3).map(|k| g[k] * (mesh.normals[t[0] as usize][k] + mesh.normals[t[1] as usize][k] + mesh.normals[t[2] as usize][k])).sum();
        if n > 0.0 {
            with += 1;
        } else if n < 0.0 {
            against += 1;
        }
    }
    if against > with {
        for t in mesh.indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 { v.map(|x| x / len) } else { [0.0, 1.0, 0.0] }
}

/// Dagor is left-handed with Y up; mirror Z to get a right-handed space (winding is flipped too)
fn right_handed(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[1], -p[2]]
}

/// Builds one mesh per node and material. `skin_material_base` is where matS materials start.
fn build_meshes(model: &DynModel, lod: &Lod, tree: &GeomNodeTree, skin_material_base: usize) -> Vec<Mesh> {
    const IDENTITY: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    let index: HashMap<&str, usize> = tree.nodes.iter().enumerate().map(|(i, n)| (n.name.as_str(), i)).collect();
    let mut meshes: HashMap<(usize, usize), Mesh> = HashMap::new();
    let mut missing_nodes = Vec::new();
    // Rigid parts are placed by their skeleton node; skinned meshes are stored in bind pose, in model space
    let parts = lod
        .rigids
        .iter()
        .map(|r| (r.node.as_str(), &r.elems, &model.vdata, 0, None))
        .chain(lod.skins.iter().map(|s| (s.node.as_str(), &s.elems, &model.skin_vdata, skin_material_base, Some(s))));
    for (node, elems, vdata, material_base, skin) in parts {
        let placed = skin.is_none();
        // "@root" is the model origin; parts without a node go there too
        let (node_index, wtm) = match index.get(node) {
            Some(&i) => (i, &tree.nodes[i].wtm),
            None => {
                if node != "@root" {
                    missing_nodes.push(node);
                }
                (0, &IDENTITY)
            }
        };
        let tm = if placed { wtm } else { &IDENTITY };
        for e in elems {
            let mesh = meshes.entry((node_index, material_base + e.material)).or_default();
            mesh.material = material_base + e.material;
            mesh.node = node_index;
            mesh.skinned = !placed;
            let vd = &vdata[e.vdata];
            let mut remap: HashMap<u32, u32> = HashMap::new();
            for tri in vd.indices[e.first_index..e.first_index + e.triangles * 3].chunks_exact(3) {
                // Reverse the winding to match the mirrored axis
                for &i in &[tri[0], tri[2], tri[1]] {
                    let src = i + e.base_vertex as u32;
                    let next = mesh.positions.len() as u32;
                    let dst = *remap.entry(src).or_insert_with(|| {
                        // Elements are validated by DynModel::parse, so indices are in range
                        let v = model.vertex(vd, src as usize).unwrap_or_default();
                        mesh.positions.push(right_handed(transform(tm, v.pos, 1.0)));
                        mesh.normals.push(right_handed(normalize(transform(tm, v.normal, 0.0))));
                        mesh.uvs.push(v.uv);
                        if let Some(skin) = skin {
                            let (joints, weights) = vertex_bones(model, skin, e, vd, src as usize, &index, node_index, &mut mesh.bones);
                            mesh.joints.push(joints);
                            mesh.weights.push(weights);
                        }
                        next
                    });
                    mesh.indices.push(dst);
                }
            }
        }
    }
    if !missing_nodes.is_empty() {
        eprintln!("warning: parts without a skeleton node, left untransformed: {missing_nodes:?}");
    }
    let mut out: Vec<Mesh> = meshes.into_values().collect();
    for m in &mut out {
        face_normals_out(m);
    }
    out.sort_by_key(|m| (m.material, m.node));
    out
}

/// The bones and weights of one skinned vertex, as indices into the mesh's own `bones` list (which
/// grows as new skeleton nodes turn up). Bones are named by the skin's bone table, through the
/// material's palette. A vertex the data leaves without a weight follows `fallback`, the skin's node.
#[allow(clippy::too_many_arguments)]
fn vertex_bones(
    model: &DynModel,
    skin: &Skin,
    elem: &Elem,
    vd: &VData,
    vertex: usize,
    nodes: &HashMap<&str, usize>,
    fallback: usize,
    bones: &mut Vec<usize>,
) -> ([u16; 4], [f32; 4]) {
    let sw = model.skin_weights(vd, vertex).unwrap_or_default();
    // (skeleton node, weight) of each influence; two slots can name the same node
    let mut influences: Vec<(usize, f32)> = Vec::with_capacity(4);
    for k in 0..4 {
        if sw.weights[k] <= 0.0 {
            continue;
        }
        // A slot the bone table cannot resolve, or a bone whose node the skeleton lacks, follows the skin's node
        let node = skin.bone(elem.material, usize::from(sw.joints[k])).and_then(|b| nodes.get(b.node.as_str()).copied()).unwrap_or(fallback);
        match influences.iter_mut().find(|i| i.0 == node) {
            Some(i) => i.1 += sw.weights[k],
            None => influences.push((node, sw.weights[k])),
        }
    }
    let sum: f32 = influences.iter().map(|i| i.1).sum();
    if sum > 0.0 {
        influences.iter_mut().for_each(|i| i.1 /= sum);
    } else {
        influences = vec![(fallback, 1.0)];
    }
    let mut joints = [0u16; 4];
    let mut weights = [0f32; 4];
    for (k, (node, weight)) in influences.into_iter().enumerate() {
        joints[k] = match bones.iter().position(|&b| b == node) {
            Some(i) => i as u16,
            None => {
                bones.push(node);
                (bones.len() - 1) as u16
            }
        };
        weights[k] = weight;
    }
    (joints, weights)
}

/// How fast a track's texture runs along the track: the change in V per metre on the bottom run, which
/// lies flat on the ground. None when the mesh has no bottom run to measure.
///
/// Track meshes are mapped with U across the track and V along it; V wraps once or more around the
/// loop, so edges that cross a texture seam give outliers, which the median ignores.
pub fn track_v_per_metre(mesh: &Mesh) -> Option<f32> {
    let low = mesh.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
    let on_bottom = |i: u32| mesh.positions[i as usize][1] < low + 0.05;
    let mut slopes = Vec::new();
    for tri in mesh.indices.chunks_exact(3) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            if !on_bottom(a) || !on_bottom(b) {
                continue;
            }
            let (a, b) = (a as usize, b as usize);
            let dx = mesh.positions[b][0] - mesh.positions[a][0];
            // Edges across the track say nothing about V along it
            if dx.abs() > 0.02 {
                slopes.push((mesh.uvs[b][1] - mesh.uvs[a][1]) / dx);
            }
        }
    }
    if slopes.is_empty() {
        return None;
    }
    slopes.sort_by(f32::total_cmp);
    Some(slopes[slopes.len() / 2])
}

/// Dagor packs the normal in A/G, smoothness in R and metalness in B (see `unpack_ag_normal`)
fn split_packed_normal(img: &Image) -> (Image, Image) {
    let mut mr = Vec::with_capacity(img.rgba.len());
    let mut nrm = Vec::with_capacity(img.rgba.len());
    for p in img.rgba.chunks_exact(4) {
        mr.extend_from_slice(&[0, 255 - p[0], p[2], 255]);
        let x = f32::from(p[3]) / 127.5 - 1.0;
        // DirectX-style green: flip for the OpenGL convention
        let y = -(f32::from(p[1]) / 127.5 - 1.0);
        let z = (1.0 - x * x - y * y).max(0.0).sqrt();
        nrm.extend_from_slice(&[x, y, z].map(|c| ((c * 0.5 + 0.5) * 255.0).round() as u8));
        nrm.push(255);
    }
    let size = |rgba| Image { width: img.width, height: img.height, rgba };
    (size(mr), size(nrm))
}

/// What loading vehicles needs from the game folder, gathered once: where each texture is, and every
/// model's material description
pub struct Library {
    textures: Arc<TextureIndex>,
    descs: HashMap<String, blk::Block>,
}

/// Opens the game folder and loads one vehicle; use [`Library`] to load several
pub fn load(game: &Path, pack: &Path, name: &str, lod: usize) -> Result<Vehicle> {
    Library::open(game)?.load(pack, name, lod)
}

impl Library {
    pub fn open(game: &Path) -> Result<Self> {
        Self::with_index(game, Arc::new(texture_index(game)?))
    }

    /// Like [`Library::open`], with a texture index that is already built (it can be shared with
    /// [`crate::texlib::TextureLibrary::from_index`])
    pub fn with_index(game: &Path, textures: Arc<TextureIndex>) -> Result<Self> {
        Ok(Self { textures, descs: model_descs(game)? })
    }

    pub fn load(&self, pack: &Path, name: &str, lod: usize) -> Result<Vehicle> {
        load_from(self, pack, name, lod, true)
    }

    /// Like [`Library::load`] without decoding any texture: the materials are there, their images are not.
    /// For the skeleton and meshes alone (the rig of a vehicle), this is orders of magnitude cheaper.
    pub fn load_geometry(&self, pack: &Path, name: &str, lod: usize) -> Result<Vehicle> {
        load_from(self, pack, name, lod, false)
    }
}

fn load_from(library: &Library, pack: &Path, name: &str, lod: usize, with_textures: bool) -> Result<Vehicle> {
    let file = map(pack)?;
    let grp = GrpPack::parse(&file)?;
    let find = |res: &str, class: u32| {
        grp.find(res, class).map(|r| grp.data(r)).with_context(|| format!("{res} not found in {}", pack.display()))
    };
    let model = DynModel::parse(find(name, class_id::DYN_MODEL)?)?;
    let tree = GeomNodeTree::parse(find(&format!("{name}_skeleton"), class_id::GEOM_NODE_TREE)?)?;
    let lod = model.lods.get(lod).context("no such LOD")?;
    let infos = material_infos(&library.descs, name)?;
    let skin_material_base = infos.iter().position(|m| m.skinned).unwrap_or(infos.len());

    let meshes = build_meshes(&model, lod, &tree, skin_material_base);
    // Materials the description lists but no mesh of this LOD uses are neither decoded nor baked
    let used: std::collections::HashSet<usize> = meshes.iter().map(|m| m.material).collect();
    let is_used = |i: usize| used.contains(&i);

    // UV triangles of every material that needs baking, grouped by texture combination
    let mut bake_tris: HashMap<String, Vec<camo::UvTriangle>> = HashMap::new();
    for (_, info) in infos.iter().enumerate().filter(|(i, _)| with_textures && is_used(*i)) {
        let Some(key) = info.bake_key() else { continue };
        let tris = bake_tris.entry(key).or_default();
        let (elems, vdata): (Vec<&Elem>, _) = if info.skinned {
            (lod.skins.iter().flat_map(|s| &s.elems).collect(), &model.skin_vdata)
        } else {
            (lod.rigids.iter().flat_map(|r| &r.elems).collect(), &model.vdata)
        };
        for e in elems.into_iter().filter(|e| e.material == info.index) {
            let vd = &vdata[e.vdata];
            // Camouflage and occlusion are looked up through the second UV set
            if !vd.has_uv1() {
                continue;
            }
            for t in vd.indices[e.first_index..e.first_index + e.triangles * 3].chunks_exact(3) {
                tris.push([0, 1, 2].map(|k| {
                    let v = model.vertex(vd, t[k] as usize + e.base_vertex).unwrap_or_default();
                    [v.uv[0], v.uv[1], v.uv1[0], v.uv1[1]]
                }));
            }
        }
    }

    let mut source = TextureSource { index: &library.textures, packs: HashMap::new() };
    let mut materials = Vec::with_capacity(infos.len());
    for (i, info) in infos.iter().enumerate() {
        if !with_textures || !is_used(i) {
            materials.push(Material {
                name: info.name.clone(),
                color: None,
                metal_rough: None,
                normal: None,
                double_sided: info.double_sided,
                alpha: info.alpha(),
                color_key: None,
                pbr_key: None,
            });
            continue;
        }
        let bake_key = info.bake_key();
        let mut color = source.decode_or_warn(&info.color);
        if let (Some(img), Some(key)) = (color.as_mut(), &bake_key) {
            let camo_img = source.decode_or_warn(&info.camo);
            let ao_img = source.decode_or_warn(&info.occlusion);
            // Alpha-tested materials keep their alpha; masked ones use it as the paint mask
            let opaque = !matches!(info.alpha(), Alpha::Mask(_));
            let (camo_view, ao_view) = (camo_img.as_ref().map(Image::view), ao_img.as_ref().map(Image::view));
            camo::bake(&mut img.rgba, img.width as usize, img.height as usize, camo_view.as_ref(), ao_view.as_ref(), &bake_tris[key], opaque);
        }
        let (metal_rough, normal) = match source.decode_or_warn(&info.packed_normal) {
            Some(packed) => {
                let (mr, n) = split_packed_normal(&packed);
                (Some(mr), Some(n))
            }
            None => (None, None),
        };
        materials.push(Material {
            name: info.name.clone(),
            color_key: color.as_ref().and(bake_key.or_else(|| info.color.clone())),
            pbr_key: metal_rough.as_ref().and(info.packed_normal.clone()),
            color,
            metal_rough,
            normal,
            double_sided: info.double_sided,
            alpha: info.alpha(),
        });
    }

    let mut nodes: Vec<Node> = tree
        .nodes
        .into_iter()
        .map(|n| Node { pivot: right_handed([n.wtm[12], n.wtm[13], n.wtm[14]]), name: n.name, parent: n.parent })
        .collect();
    // Meshes refer to node 0 at least
    if nodes.is_empty() {
        nodes.push(Node { name: String::new(), parent: None, pivot: [0.0; 3] });
    }
    Ok(Vehicle { name: name.to_string(), meshes, materials, nodes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wt_blk::dxp_and_grp::dynmodel::{channel, Bone, Channel, SkinMaterial};

    /// A skin vertex buffer of 8-byte vertices holding bone slots and weights as the E3DCOLORs of texture
    /// coordinates 4 and 5. Each vertex is `(slots, weights)` in the order x, y, z, w; E3DCOLOR is stored
    /// B, G, R, A, so z comes first in memory.
    fn bone_buffer(vertices: &[([u8; 4], [u8; 4])]) -> VData {
        let color = |c: [u8; 4]| [c[2], c[1], c[0], c[3]];
        let mut bytes = Vec::new();
        for (slots, weights) in vertices {
            bytes.extend_from_slice(&color(*slots));
            bytes.extend_from_slice(&color(*weights));
        }
        let channel = |index, offset| Channel { ty: channel::E3DCOLOR, usage: channel::TEXCOORD, index, offset };
        VData { stride: 8, vertices: bytes, indices: Vec::new(), channels: vec![channel(4, 0), channel(5, 4)] }
    }

    fn skin_model() -> (DynModel, Skin) {
        let model = DynModel { vdata: vec![], skin_vdata: vec![], pos_offset: [0.0; 3], pos_scale: [1.0; 3], lods: vec![] };
        let bone = |node: &str| Bone { node: node.into(), inverse_bind: [0.0; 16] };
        // Material 0 uses the bone table as it is; material 1 has a palette of bones 2 and 0
        let skin = Skin {
            node: "track_l".into(),
            elems: vec![],
            bones: vec![bone("root"), bone("bone_wheel_l_01"), bone("bone_wheel_l_02"), bone("not_in_the_skeleton")],
            materials: vec![SkinMaterial { material: 0, palette: vec![] }, SkinMaterial { material: 1, palette: vec![2, 0] }],
        };
        (model, skin)
    }

    #[test]
    fn vertices_get_the_skeleton_nodes_of_their_bones() {
        let (model, skin) = skin_model();
        // Skeleton nodes by name; "not_in_the_skeleton" is deliberately absent
        let nodes: HashMap<&str, usize> = [("root", 1), ("bone_wheel_l_01", 5), ("bone_wheel_l_02", 6), ("track_l", 9)].into_iter().collect();
        let elem = |material| Elem { material, vdata: 0, first_index: 0, triangles: 0, base_vertex: 0 };
        let vd = bone_buffer(&[
            // 0: slot 1 alone: bone_wheel_l_01
            ([1, 0, 0, 0], [255, 0, 0, 0]),
            // 1: root a quarter, bone_wheel_l_02 three quarters
            ([0, 2, 0, 0], [64, 191, 0, 0]),
            // 2: two slots naming the root merge into one influence
            ([0, 0, 0, 0], [128, 127, 0, 0]),
            // 3: a bone whose node the skeleton lacks follows the skin's own node
            ([3, 0, 0, 0], [255, 0, 0, 0]),
            // 4: a slot the bone table lacks does too
            ([9, 0, 0, 0], [255, 0, 0, 0]),
            // 5: and so does a vertex without any weight
            ([0, 0, 0, 0], [0, 0, 0, 0]),
            // 6: a slot with weight 0 is skipped, whatever it names
            ([1, 3, 0, 0], [255, 0, 0, 0]),
        ]);
        let mut bones = Vec::new();
        let mut get = |v| vertex_bones(&model, &skin, &elem(0), &vd, v, &nodes, 9, &mut bones);

        assert_eq!(get(0), ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]));
        // Bones are added to the mesh's list as they turn up: [5], then root and bone_wheel_l_02
        let (j, w) = get(1);
        assert_eq!(&j[..2], &[1, 2]);
        assert!((w[0] - 0.25).abs() < 0.01 && (w[1] - 0.75).abs() < 0.01 && w[2..] == [0.0, 0.0], "{w:?}");
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(get(2), ([1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]));
        assert_eq!(get(3), ([3, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]));
        assert_eq!(get(4), ([3, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]));
        assert_eq!(get(5), ([3, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]));
        assert_eq!(get(6), ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]));
        assert_eq!(bones, [5, 1, 6, 9]);

        // Material 1 goes through its palette: slot 0 is bone 2 (node 6), slot 1 is bone 0 (the root)
        let vd1 = bone_buffer(&[([0, 1, 0, 0], [128, 127, 0, 0])]);
        let mut bones1 = Vec::new();
        let (j, w) = vertex_bones(&model, &skin, &elem(1), &vd1, 0, &nodes, 9, &mut bones1);
        assert_eq!((bones1, &j[..2]), (vec![6, 1], &[0u16, 1][..]));
        assert!((w[0] - 128.0 / 255.0).abs() < 1e-6 && (w[1] - 127.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn moving_a_bone_moves_the_vertices_it_weighs_on() {
        let mut m = Mesh { skinned: true, bones: vec![10, 11], ..Mesh::default() };
        m.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 1.0, 0.0]];
        // Vertex 0 follows bone 0, vertex 1 follows bone 1, vertex 2 is half and half
        m.joints = vec![[0, 0, 0, 0], [1, 0, 0, 0], [0, 1, 0, 0]];
        m.weights = vec![[1., 0., 0., 0.], [1., 0., 0., 0.], [0.5, 0.5, 0., 0.]];
        assert_eq!(m.skinned_positions(&[IDENTITY, IDENTITY]), m.positions);
        // Bone 1 moves up 0.4 m; bone 0 turns a quarter turn about Z: (x, y) -> (-y, x)
        let up: Affine = [1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0.4, 0.];
        let quarter: Affine = [0., 1., 0., -1., 0., 0., 0., 0., 1., 0., 0., 0.];
        let moved = m.skinned_positions(&[quarter, up]);
        assert_eq!(moved[0], [0.0, 0.0, 0.0]);
        assert_eq!(moved[1], [1.0, 0.4, 0.0]);
        // Halfway between (2, 1) turned to (-1, 2) and (2, 1.4)
        assert!((moved[2][0] - 0.5).abs() < 1e-6 && (moved[2][1] - 1.7).abs() < 1e-6);
        // A bone without an entry stays put; a rigid mesh is returned as it is
        assert_eq!(m.skinned_positions(&[quarter])[1], [1.0, 0.0, 0.0]);
        let rigid = Mesh { positions: m.positions.clone(), ..Mesh::default() };
        assert_eq!(rigid.skinned_positions(&[up]), m.positions);
    }


    #[test]
    fn track_texture_rate_comes_from_the_bottom_run() {
        // A 2 m bottom run at y = 0 in 0.5 m strips across a 0.4 m wide track, V = 0.6 per metre, with
        // one strip crossing a texture seam (V jumps back by 1), and a top run 0.8 m up mapped the other way
        let mut m = Mesh::default();
        for i in 0..5 {
            let x = i as f32 * 0.5;
            let v = 0.6 * x - if i >= 3 { 1.0 } else { 0.0 };
            m.positions.extend([[x, 0.0, 0.0], [x, 0.0, 0.4], [x, 0.8, 0.0], [x, 0.8, 0.4]]);
            m.uvs.extend([[0.0, v], [1.0, v], [0.0, -v], [1.0, -v]]);
        }
        for i in 0..4u32 {
            let (a, b) = (i * 4, i * 4 + 4);
            m.indices.extend([a, b, a + 1, a + 1, b, b + 1, a + 2, b + 2, a + 3]);
        }
        let rate = track_v_per_metre(&m).unwrap();
        assert!((rate - 0.6).abs() < 1e-5, "{rate}");
        assert_eq!(track_v_per_metre(&Mesh::default()), None);
    }

    #[test]
    fn alpha_follows_the_shader_class_and_its_atest_parameter() {
        // The glass, blend and flash classes blend, whatever else they carry
        for class in ["dynamic_glass_chrome", "dynamic_pbr_glass", "dynamic_masked_glass_chrome", "dynamic_alpha_blend", "aces_weapon_fire", "propeller_side"] {
            assert_eq!(material_alpha(class, None), Alpha::Blend, "{class}");
        }
        assert_eq!(material_alpha("dynamic_alpha_blend", Some(127)), Alpha::Blend);
        // `atest=N` makes any other class a cut-out at N / 255; without it the masked classes' alpha is a mask
        assert_eq!(material_alpha("dynamic_masked_chrome_bump", Some(127)), Alpha::Mask(127.0 / 255.0));
        assert_eq!(material_alpha("dynamic_simple", Some(255)), Alpha::Mask(1.0));
        for class in ["dynamic_masked_chrome_bump", "dynamic_masked_ship", "dynamic_masked_tank", "dynamic_simple", "propeller_front", "dynamic_perlin_layered"] {
            assert_eq!(material_alpha(class, None), Alpha::Opaque, "{class}");
        }
        // The `atest` classes cut out even without the parameter, unless it says 0
        assert_eq!(material_alpha("dynamic_tank_atest", None), Alpha::Mask(0.5));
        assert_eq!(material_alpha("dynamic_tank_atest", Some(0)), Alpha::Opaque);
        assert_eq!(material_alpha("dynamic_tank_atest", Some(100)), Alpha::Mask(100.0 / 255.0));
    }

    #[test]
    fn meshes_that_face_away_from_their_normals_are_turned_over() {
        let tri = |indices: Vec<u32>| Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            indices,
            ..Mesh::default()
        };
        // Counter-clockwise seen from +Z, the normals' side: kept
        let mut ok = tri(vec![0, 1, 2, 1, 3, 2]);
        face_normals_out(&mut ok);
        assert_eq!(ok.indices, [0, 1, 2, 1, 3, 2]);
        // Clockwise: every triangle turned
        let mut flipped = tri(vec![0, 2, 1, 1, 2, 3]);
        face_normals_out(&mut flipped);
        assert_eq!(flipped.indices, [0, 1, 2, 1, 3, 2]);
        // Mostly right with one against (smoothing): left as it is
        let mut mixed = tri(vec![0, 1, 2, 1, 3, 2, 0, 2, 1]);
        face_normals_out(&mut mixed);
        assert_eq!(mixed.indices, [0, 1, 2, 1, 3, 2, 0, 2, 1]);
        // Nothing to say about an empty mesh
        face_normals_out(&mut Mesh::default());
    }
}
