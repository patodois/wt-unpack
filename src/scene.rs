//! A War Thunder level as a rewrite-bridge [`Scene`]: the ground with its real colours, the placed
//! objects with their real models, and the water level. Everything comes from the level file, the game's
//! resources and the level's settings; whatever cannot be read is left out and listed in the
//! [`SceneReport`].
//!
//! # Axes
//!
//! Dagor's world is left-handed with Y up. As for vehicles ([`crate::bridge`]), Dagor's `(x, y, z)`
//! becomes `(z, y, x)`, a swap that is also a mirror, so a model that faces +X in the game faces +Z here.
//! Triangles are rewound where the mirror turned them (each model is checked against its own normals).
//!
//! # What is in the scene
//!
//! | model | contents | source |
//! |-------|----------|--------|
//! | `terrain` | one mesh per landmesh cell: the landmesh's collision mesh (whole map, coarse) and the `HM2` heightmap (the playable area, finer), textured with the land classes' colour maps blended by the level's weights | `lmap`, `HM2`, land class resources, the `.dxp.bin` packs |
//! | `water` | one quad at the level's `water_level`, over the landmesh | `levels/<name>.blk` |
//! | placed objects | one model per distinct rendinst, in the LOD the game uses at [`SceneOptions::lod_distance_m`]; one [`Instance`] per placement | `RIGz`, `RendInst` resources, `riDesc.bin` |
//!
//! # What is left out
//!
//! See [`NOT_APPLIED`]: the land shaders' detail layers, the water shader, object shaders (layers,
//! decals, wind), sun, sky and fog.
//!
//! # Leaf cards
//!
//! The leaves of trees and bushes (`rendinst_facing_leaves`) are cards that face the camera: the file
//! gives each card's centre and the corners' offsets in the camera's frame, so where the corners lie in
//! the object's space depends on the viewer. A `Scene` has no primitive for that and any fixed
//! orientation would be made up, so the cards are not in the scene's meshes. [`load_level_scene_with_leaves`]
//! returns them next to the scene, as [`SceneLeaves`]: the same data as the file, for a host that can
//! draw a camera-facing card.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use rewrite_bridge::{AlphaMode, Image, Instance, Material, Mesh, Model, Node, Scene, SceneModel, Transform};
use wt_blk::dxp_and_grp::dynmodel::Elem;
use wt_blk::dxp_and_grp::grp_pack::GrpPack;

use crate::blk::{self, Block, Value};
use crate::landclass::{decode_colormaps, GroundBaker, GroundMaterial};
use crate::level::{Heightmap, Level, RendInstLayer};
use crate::rendinst::{is_facing_leaves, model_descs_where, LeafCard, ResourceIndex, RiModel, LAND_CLASS, REND_INST};
use crate::terrain::HeightGrid;
use crate::texlib::{Rgba, TextureLibrary};
use crate::vehicle::{self, TextureIndex};

/// Parameters of the conversion that are about budget, not about the game's data
#[derive(Debug, Clone)]
pub struct SceneOptions {
    /// Distance in metres for which each object model's LOD is chosen, as the game's
    /// `getBestLodForDist` does from the model's own ranges
    pub lod_distance_m: f32,
    /// Longest side of an object texture; larger ones are shrunk by halving
    pub object_texture_px: u32,
    /// Megabytes the land classes' colour maps may take together as they are read (larger maps are
    /// shrunk by halving until they fit)
    pub colormap_mb: u32,
    /// Metres per ground colour pixel in landmesh cells the heightmap covers finer than the landmesh,
    /// and elsewhere
    pub playable_texel_m: f32,
    pub outer_texel_m: f32,
    /// Metres between terrain vertices from the heightmap, and from the landmesh
    pub fine_mesh_m: f32,
    pub coarse_mesh_m: f32,
    /// Megabytes of baked ground pictures and thousands of terrain vertices the whole level may take;
    /// the spacings above grow together until they fit
    pub ground_texture_mb: u32,
    pub terrain_vertices_k: u32,
}

impl Default for SceneOptions {
    fn default() -> Self {
        SceneOptions {
            lod_distance_m: 250.0,
            object_texture_px: 256,
            colormap_mb: 384,
            playable_texel_m: 2.0,
            outer_texel_m: 16.0,
            fine_mesh_m: 8.0,
            coarse_mesh_m: 128.0,
            ground_texture_mb: 384,
            terrain_vertices_k: 3000,
        }
    }
}

/// Data the level has and the scene does not carry, with the reason
pub const NOT_APPLIED: &[&str] = &[
    "land shader detail layers (splat masks and tiled detail textures of mega land classes, index maps of biome classes): the blend is in shaders that are not public for those classes, so the ground is the land classes' colour maps blended by the level's weights",
    "ground normals and roughness: the land classes' normal and reflectance maps are read as names only",
    "water appearance: the water shader is not public; the plane carries the level's own water_aces_color_and_density colour, flat, when the level has one (28 of the 160 levels do) and the bridge's default material otherwise",
    "sun, sky and fog: the level stores only its place and date (stars{}); the time of day and weather come from the mission, and the sky colour from the atmosphere shader",
    "object shaders: layered, perlin and decal blending, wind, interior mapping; objects use the first texture of each material",
    "tree leaves (rendinst_facing_leaves) in the Scene: each leaf card is four vertices around one centre that the vertex shader moves to face the camera, so the data holds no corners in the object's space, and Scene has no camera-facing primitive; the Scene's trees are their trunks and branch meshes, and bushes made only of leaves are not in it. The cards themselves (centre, corner offsets in the camera's frame, uv, normal, wind group, texture) come from load_level_with_leaves as SceneLeaves",
    "impostor billboards (rendinst_baked_impostor): the far LOD of a tree that has its meshes in the scene, drawn by the shader from views baked into an atlas by the camera's direction; the mesh is one template shared by every tree, so the LOD is never chosen",
    "object normals and reflectance maps",
    "collision: Scene has no collision field, so the shapes of the placed objects come separately from WarThunder::load_level_collision (crate::level_collision); the ground's collision is the terrain model itself",
    "per-instance rotation of trees without a palette (the level stores none); scatter by land class (obj_plant_generate) and the SCN, FRT, Obj, Wire and spline blocks",
];

/// What the conversion did
#[derive(Debug, Default, Clone)]
pub struct SceneReport {
    pub name: String,
    pub load_ms: u128,
    /// Distinct object models in the scene, and placements of them
    pub object_models: usize,
    pub instances: usize,
    pub object_triangles: usize,
    /// Triangles of all placements together (instances times their model's triangles)
    pub drawn_triangles: u64,
    pub object_textures: usize,
    /// Pool names no pack has, with the placements lost
    pub unresolved: Vec<(String, usize)>,
    /// Models that failed to parse
    pub failed: Vec<(String, String)>,
    /// Material classes whose meshes were left out, with the number of draw calls
    pub skipped_classes: BTreeMap<String, usize>,
    /// Material classes drawn with their first texture only
    pub approximated_classes: BTreeMap<String, usize>,
    pub terrain_tiles: usize,
    pub terrain_triangles: usize,
    pub ground_textured: bool,
    pub water_level: Option<f32>,
    /// The level's own water colour (rgb of `water_aces_color_and_density`), when it has one
    pub water_colour: Option<[f32; 3]>,
    pub land_classes: usize,
    pub warnings: Vec<String>,
    /// Bytes of mesh, image and instance data in the scene
    pub memory_bytes: usize,
    pub heightmap: bool,
    pub landmesh: bool,
    pub palette_pools: usize,
    pub trees_without_rotation: usize,
    /// With the leaf cards asked for ([`load_level_scene_with_leaves`]): the models that have leaf cards,
    /// the cards of those models (each counted once), and the cards of all their placements together
    pub leaf_models: usize,
    pub leaf_cards: usize,
    pub leaf_cards_drawn: u64,
}

/// The leaf cards of one material of a placed object, in the bridge's axes
#[derive(Debug, Clone)]
pub struct LeafSet {
    /// The material's texture (`t0`), shrunk like the object textures; `None` when no pack has it
    pub base_color: Option<Arc<Image>>,
    /// Pixels below the cutoff are discarded; `rendinst_facing_leaves` states none, so it is the 127 the
    /// tree classes use
    pub alpha: AlphaMode,
    /// The material's numeric parameters from `riDesc.bin`: `facing_leaves_translucency`,
    /// `facing_leaves_smoothness` and the cross-dissolve of the LOD change
    pub params: Vec<(String, f32)>,
    /// Centres and normals in the bridge's axes like the rest of the scene; the corners' offsets are in the
    /// camera's frame and are not turned (see [`LeafCard`])
    pub cards: Vec<LeafCard>,
}

/// A placed object's leaf cards, in the LOD the scene uses for it
#[derive(Debug, Clone)]
pub struct LeafModel {
    /// The rendinst's name, the same as the scene model's when the object has meshes
    pub name: String,
    pub sets: Vec<LeafSet>,
}

/// The leaf cards of a level's placed objects: what [`SceneOptions`] chose for the scene, left out of its
/// meshes because a card faces the camera (see the module docs)
#[derive(Debug, Clone, Default)]
pub struct SceneLeaves {
    pub models: Vec<LeafModel>,
    /// One per placement of a model that has cards (`Instance::model` indexes `models`), with the
    /// same transform as the object's instance in the scene
    pub instances: Vec<Instance>,
}

impl SceneLeaves {
    /// Cards of all models, each counted once
    pub fn card_count(&self) -> usize {
        self.models.iter().flat_map(|m| &m.sets).map(|s| s.cards.len()).sum()
    }
}

/// A card in the bridge's axes: the centre and the normal are objects' space points turned like a mesh's, the
/// corners' offsets belong to the camera's frame and stay as they are
fn card_to_bridge(c: LeafCard) -> LeafCard {
    LeafCard { centre: turn(c.centre), normal: turn(c.normal), ..c }
}

/// Dagor world (left-handed) to the bridge's right-handed axes
fn turn(p: [f32; 3]) -> [f32; 3] {
    [p[2], p[1], p[0]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn len(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// The bridge transform of a placement: `rows` are the instance's axes and position in Dagor's world
/// (x axis, y axis, z axis, position; the axes include the scale)
pub fn instance_transform(rows: &[[f32; 3]; 4]) -> Transform {
    // Bridge axis j is the turned Dagor axis 2 - j (the swap exchanges x and z)
    let mut cols = [turn(rows[2]), turn(rows[1]), turn(rows[0])];
    let mut scale = [len(cols[0]), len(cols[1]), len(cols[2])];
    for (c, s) in cols.iter_mut().zip(&mut scale) {
        if *s > 0.0 {
            *c = [c[0] / *s, c[1] / *s, c[2] / *s];
        } else {
            *s = f32::MIN_POSITIVE;
        }
    }
    if dot(cross(cols[0], cols[1]), cols[2]) < 0.0 {
        scale[0] = -scale[0];
        cols[0] = [-cols[0][0], -cols[0][1], -cols[0][2]];
    }
    Transform { translation: turn(rows[3]), rotation: quaternion(&cols), scale }
}

/// Unit quaternion (x, y, z, w) of the rotation whose columns are `c`
fn quaternion(c: &[[f32; 3]; 3]) -> [f32; 4] {
    // m[row][col]
    let m = |r: usize, k: usize| c[k][r];
    let trace = m(0, 0) + m(1, 1) + m(2, 2);
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(m(2, 1) - m(1, 2)) / s, (m(0, 2) - m(2, 0)) / s, (m(1, 0) - m(0, 1)) / s, 0.25 * s]
    } else if m(0, 0) > m(1, 1) && m(0, 0) > m(2, 2) {
        let s = (1.0 + m(0, 0) - m(1, 1) - m(2, 2)).sqrt() * 2.0;
        [0.25 * s, (m(0, 1) + m(1, 0)) / s, (m(0, 2) + m(2, 0)) / s, (m(2, 1) - m(1, 2)) / s]
    } else if m(1, 1) > m(2, 2) {
        let s = (1.0 + m(1, 1) - m(0, 0) - m(2, 2)).sqrt() * 2.0;
        [(m(0, 1) + m(1, 0)) / s, 0.25 * s, (m(1, 2) + m(2, 1)) / s, (m(0, 2) - m(2, 0)) / s]
    } else {
        let s = (1.0 + m(2, 2) - m(0, 0) - m(1, 1)).sqrt() * 2.0;
        [(m(0, 2) + m(2, 0)) / s, (m(1, 2) + m(2, 1)) / s, 0.25 * s, (m(1, 0) - m(0, 1)) / s]
    };
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if n > 0.0 {
        [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn image(i: Rgba) -> Image {
    Image { width: i.width, height: i.height, rgba: i.rgba }
}

/// A placed-object material, from the model's `riDesc.bin` entry
struct ObjMaterial {
    class: String,
    color: Option<String>,
    atest: Option<u32>,
    two_sided: bool,
    /// Every `key=number` line of the material's parameters
    params: Vec<(String, f32)>,
}

fn str_param<'a>(b: &'a Block, key: &str) -> Option<&'a str> {
    b.params.iter().find(|(n, _)| n == key).and_then(|(_, v)| if let Value::Str(s) = v { Some(s.as_str()) } else { None })
}

fn object_materials(desc: &Block) -> Vec<ObjMaterial> {
    let tex: Vec<&str> = desc
        .blocks
        .iter()
        .find(|b| b.name == "tex")
        .map(|b| b.params.iter().filter_map(|(_, v)| if let Value::Str(s) = v { Some(s.as_str()) } else { None }).collect())
        .unwrap_or_default();
    let Some(mats) = desc.blocks.iter().find(|b| b.name == "mat") else { return Vec::new() };
    mats.blocks
        .iter()
        .map(|m| {
            let class = str_param(m, "cls").unwrap_or("").to_string();
            let par = str_param(m, "par").unwrap_or("");
            let color = m.params.iter().find(|(n, _)| n == "t0").and_then(|(_, v)| if let Value::Int(i) = v { tex.get(*i as usize) } else { None }).map(|s| s.to_string());
            let par_value = |key: &str| par.lines().find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('=')?.trim().parse::<u32>().ok());
            let two_sided = matches!(m.params.iter().find(|(n, _)| n == "two_sided").map(|(_, v)| v), Some(Value::Bool(true))) || par_value("real_two_sided") == Some(1);
            let params = par.lines().filter_map(|l| l.split_once('=')).filter_map(|(k, v)| Some((k.trim().to_string(), v.trim().parse::<f32>().ok()?))).collect();
            ObjMaterial { class, color, atest: par_value("atest"), two_sided, params }
        })
        .collect()
}

/// Meshes with these classes are left out: billboards and stickers laid over other geometry, and the
/// camera-facing leaf cards of trees, whose corners exist only in the camera's frame (see
/// [`crate::rendinst::LeafVertex`])
fn skipped_class(class: &str) -> bool {
    class.contains("decal") || class.contains("baked_impostor") || is_facing_leaves(class)
}

/// How a material cuts its pixels out
fn alpha_mode(m: Option<&ObjMaterial>, class: &str) -> AlphaMode {
    match m.and_then(|m| m.atest) {
        Some(0) => AlphaMode::Opaque,
        Some(n) => AlphaMode::Mask(n as f32 / 255.0),
        // Tree classes alpha-test without a parameter; 127, the value most materials carry
        None if alpha_tested_class(class) => AlphaMode::Mask(127.0 / 255.0),
        None => AlphaMode::Opaque,
    }
}

/// Classes drawn with their first texture only: the engine blends further layers in the shader
fn approximated_class(class: &str) -> bool {
    class.contains("layered") || class.contains("perlin") || class.contains("interior_mapping") || class.contains("painted") || class.contains("colored")
}

/// Whether a class alpha-tests its leaves without saying how much in its parameters
fn alpha_tested_class(class: &str) -> bool {
    class.contains("tree") || class.contains("leaves") || class.contains("atest")
}

struct ModelBuilder<'a> {
    textures: &'a TextureLibrary,
    opts: &'a SceneOptions,
    /// Where each object texture is already held, by name: `(model, material)` in the scene, or `None`
    /// for a texture no pack has. A texture is decoded once and the later materials that use it copy it
    /// from the first, so no second copy is kept here.
    holders: HashMap<String, Option<(usize, usize)>>,
    /// Textures decoded for the model being built, with the material that holds them
    pending: Vec<(String, usize)>,
    report: &'a mut SceneReport,
    /// Whether the leaf cards are kept (they are counted in the report's skipped classes either way)
    collect_leaves: bool,
    /// The leaf textures decoded so far, by name
    leaf_textures: HashMap<String, Option<Arc<Image>>>,
    /// The leaf cards of the model `build` last built
    leaf_sets: Vec<LeafSet>,
}

impl ModelBuilder<'_> {
    /// The picture of a texture for the material about to be added at index `current.len()` of the model
    /// being built (`current` holds its materials so far, `done` the models already in the scene)
    fn texture(&mut self, name: &str, done: &[SceneModel], current: &[Material]) -> Option<Image> {
        let key = crate::texlib::texture_key(name).to_string();
        let copy = |m: &Material| m.base_color.as_ref().map(|i| Image { width: i.width, height: i.height, rgba: i.rgba.clone() });
        match self.holders.get(&key) {
            Some(None) => return None,
            Some(Some((model, material))) => {
                return match done.get(*model) {
                    Some(SceneModel::Inline(m)) => m.materials.get(*material).and_then(copy),
                    _ => None,
                }
            }
            None => {}
        }
        if let Some((_, material)) = self.pending.iter().find(|(k, _)| *k == key) {
            return current.get(*material).and_then(copy);
        }
        match self.textures.decode(name, self.opts.object_texture_px) {
            Ok(Some(i)) => {
                self.pending.push((key, current.len()));
                Some(image(i))
            }
            Ok(None) => {
                self.holders.insert(key, None);
                None
            }
            Err(e) => {
                self.report.warnings.push(format!("texture {name}: {e:#}"));
                self.holders.insert(key, None);
                None
            }
        }
    }

    /// A leaf texture, decoded once for all the models that use it
    fn leaf_texture(&mut self, name: &str) -> Option<Arc<Image>> {
        let key = crate::texlib::texture_key(name).to_string();
        if let Some(t) = self.leaf_textures.get(&key) {
            return t.clone();
        }
        let t = match self.textures.decode(name, self.opts.object_texture_px) {
            Ok(t) => t.map(|i| Arc::new(image(i))),
            Err(e) => {
                self.report.warnings.push(format!("texture {name}: {e:#}"));
                None
            }
        };
        self.leaf_textures.insert(key, t.clone());
        t
    }

    /// The leaf cards of the elements of a model's `rendinst_facing_leaves` materials. Elements that are
    /// not cards in pairs of triangles are reported and left out, not guessed at.
    fn leaf_sets_of(&mut self, name: &str, model: &RiModel, mats: &[ObjMaterial], elems: BTreeMap<usize, Vec<&Elem>>) -> Vec<LeafSet> {
        let mut sets = Vec::new();
        for (mi, elems) in elems {
            let m = mats.get(mi);
            let mut cards = Vec::new();
            for e in elems {
                match model.leaf_cards(e) {
                    Ok(c) => cards.extend(c.into_iter().map(card_to_bridge)),
                    Err(err) => self.report.warnings.push(format!("{name}: an element of leaf material {mi} is left out: {err:#}")),
                }
            }
            if cards.is_empty() {
                continue;
            }
            cards.shrink_to_fit();
            let class = m.map_or("", |m| m.class.as_str());
            let base_color = m.and_then(|m| m.color.as_deref()).and_then(|t| self.leaf_texture(t));
            sets.push(LeafSet { base_color, alpha: alpha_mode(m, class), params: m.map(|m| m.params.clone()).unwrap_or_default(), cards });
        }
        sets
    }

    /// Converts one object model in its chosen LOD. `None` when nothing of it can be drawn. The leaf
    /// cards, when they are collected, are left in `self.leaf_sets`.
    fn build(&mut self, name: &str, model: &RiModel, desc: Option<&Block>, done: &[SceneModel]) -> Option<(Model, usize)> {
        self.pending.clear();
        let lod = model.lod_for_distance(self.opts.lod_distance_m);
        let mats = desc.map(object_materials).unwrap_or_default();
        // Elements by material, in material order
        let mut by_material: BTreeMap<usize, Vec<&Elem>> = BTreeMap::new();
        let mut leaf_elems: BTreeMap<usize, Vec<&Elem>> = BTreeMap::new();
        self.leaf_sets.clear();
        for e in &model.lods.get(lod)?.elems {
            let class = mats.get(e.material).map_or("", |m| m.class.as_str());
            if skipped_class(class) {
                *self.report.skipped_classes.entry(class.to_string()).or_default() += 1;
                if self.collect_leaves && is_facing_leaves(class) {
                    leaf_elems.entry(e.material).or_default().push(e);
                }
                continue;
            }
            if approximated_class(class) {
                *self.report.approximated_classes.entry(class.to_string()).or_default() += 1;
            }
            by_material.entry(e.material).or_default().push(e);
        }
        if !leaf_elems.is_empty() {
            self.leaf_sets = self.leaf_sets_of(name, model, &mats, leaf_elems);
        }
        let mut meshes = Vec::new();
        let mut materials = Vec::new();
        let mut triangles = 0;
        for (mi, elems) in by_material {
            let m = mats.get(mi);
            let mut mesh = Mesh { name: format!("{name}_{mi}"), node: 0, material: materials.len(), ..Mesh::default() };
            let mut remap: HashMap<(usize, u32), u32> = HashMap::new();
            for e in elems {
                let vd = &model.vdata[e.vdata];
                let range = &vd.indices[e.first_index..e.first_index + e.triangles * 3];
                for &raw in range {
                    let v = raw as usize + e.base_vertex;
                    let idx = match remap.get(&(e.vdata, v as u32)) {
                        Some(&i) => i,
                        None => {
                            let Ok(vx) = model.vertex(vd, v) else { continue };
                            let i = mesh.positions.len() as u32;
                            mesh.positions.push(turn(vx.pos));
                            mesh.normals.push(turn(vx.normal));
                            mesh.uvs.push(vx.uv);
                            remap.insert((e.vdata, v as u32), i);
                            i
                        }
                    };
                    mesh.indices.push(idx);
                }
            }
            mesh.indices.truncate(mesh.indices.len() / 3 * 3);
            fix_winding(&mut mesh);
            triangles += mesh.indices.len() / 3;
            let class = m.map_or("", |m| m.class.as_str());
            let base_color = m.and_then(|m| m.color.as_deref()).and_then(|t| self.texture(t, done, &materials));
            let alpha = alpha_mode(m, class);
            materials.push(Material {
                name: format!("{class}_{mi}"),
                base_color,
                metallic: 0.0,
                roughness: 1.0,
                alpha,
                double_sided: m.is_some_and(|m| m.two_sided) || !matches!(alpha, AlphaMode::Opaque),
                ..Material::default()
            });
            trim(&mut mesh);
            meshes.push(mesh);
        }
        if meshes.is_empty() || triangles == 0 {
            self.pending.clear();
            return None;
        }
        // The model is accepted: it is the next one in the scene, and holds what it decoded
        for (key, material) in self.pending.drain(..) {
            self.holders.insert(key, Some((done.len(), material)));
        }
        self.report.object_textures += materials.iter().filter(|m| m.base_color.is_some()).count();
        Some((Model { name: name.to_string(), nodes: vec![Node::new(name, None)], meshes, materials, skins: Vec::new() }, triangles))
    }
}

/// Gives back the room the vectors grew into: a mesh built by pushing holds up to twice what it needs,
/// and the scene keeps it for as long as it lives
fn trim(mesh: &mut Mesh) {
    mesh.positions.shrink_to_fit();
    mesh.normals.shrink_to_fit();
    mesh.uvs.shrink_to_fit();
    mesh.indices.shrink_to_fit();
}

/// Turns the triangles of a mesh that faces against its own normals (the axis swap is a mirror, and
/// whether a pack's triangles wind with or against the normals is the pack's)
fn fix_winding(mesh: &mut Mesh) {
    let mut agree = 0i64;
    for t in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[t[k] as usize]);
        let g = cross(sub(b, a), sub(c, a));
        let n = [0, 1, 2].map(|k| mesh.normals[t[k] as usize]);
        let s = dot(g, [n[0][0] + n[1][0] + n[2][0], n[0][1] + n[1][1] + n[2][1], n[0][2] + n[1][2] + n[2][2]]);
        agree += if s > 0.0 { 1 } else if s < 0.0 { -1 } else { 0 };
    }
    if agree < 0 {
        for t in mesh.indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
}

/// A patch of terrain: positions in Dagor's world and triangles
struct Patch {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

/// Samples `height` on a grid over `rect` (`[x0, z0, x1, z1]`) about `step` metres apart; quads with an
/// uncovered corner, or whose centre lies inside `skip`, are left out
fn grid_patch(height: &dyn Fn(f32, f32) -> f32, rect: [f32; 4], step: f32, skip: Option<[f32; 4]>) -> Patch {
    let nx = (((rect[2] - rect[0]) / step).round() as usize).max(1);
    let nz = (((rect[3] - rect[1]) / step).round() as usize).max(1);
    let (dx, dz) = ((rect[2] - rect[0]) / nx as f32, (rect[3] - rect[1]) / nz as f32);
    let mut index = vec![u32::MAX; (nx + 1) * (nz + 1)];
    let mut positions = Vec::new();
    for j in 0..=nz {
        for i in 0..=nx {
            let (x, z) = (rect[0] + i as f32 * dx, rect[1] + j as f32 * dz);
            let h = height(x, z);
            if !h.is_nan() {
                index[j * (nx + 1) + i] = positions.len() as u32;
                positions.push([x, h, z]);
            }
        }
    }
    let mut indices = Vec::new();
    for j in 0..nz {
        for i in 0..nx {
            let q = |a: usize, b: usize| index[(j + b) * (nx + 1) + i + a];
            let c = [q(0, 0), q(1, 0), q(0, 1), q(1, 1)];
            if c.contains(&u32::MAX) {
                continue;
            }
            if let Some(s) = skip {
                let (cx, cz) = (rect[0] + (i as f32 + 0.5) * dx, rect[1] + (j as f32 + 0.5) * dz);
                if cx > s[0] && cx < s[2] && cz > s[1] && cz < s[3] {
                    continue;
                }
            }
            indices.extend_from_slice(&[c[0], c[2], c[1], c[1], c[2], c[3]]);
        }
    }
    Patch { positions, indices }
}

/// Appends a patch to a mesh in the bridge's axes, with upward normals and the tile's UVs
fn add_patch(mesh: &mut Mesh, patch: &Patch, tile: [f32; 4]) {
    let base = mesh.positions.len() as u32;
    let (w, h) = ((tile[2] - tile[0]).max(1e-3), (tile[3] - tile[1]).max(1e-3));
    for p in &patch.positions {
        mesh.positions.push(turn(*p));
        mesh.uvs.push([(p[0] - tile[0]) / w, (p[2] - tile[1]) / h]);
        mesh.normals.push([0.0; 3]);
    }
    for t in patch.indices.chunks_exact(3) {
        let mut t = [t[0] + base, t[1] + base, t[2] + base];
        let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[t[k] as usize]);
        let n = cross(sub(b, a), sub(c, a));
        if n[1] < 0.0 {
            t.swap(1, 2);
        }
        let n = if n[1] < 0.0 { [-n[0], -n[1], -n[2]] } else { n };
        for &i in &t {
            let v = &mut mesh.normals[i as usize];
            *v = [v[0] + n[0], v[1] + n[1], v[2] + n[2]];
        }
        mesh.indices.extend_from_slice(&t);
    }
}

fn normalize_normals(mesh: &mut Mesh) {
    for n in &mut mesh.normals {
        let l = len(*n);
        *n = if l > 0.0 { [n[0] / l, n[1] / l, n[2] / l] } else { [0.0, 1.0, 0.0] };
    }
}

fn rect_overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn rect_clip(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// The level's settings (`levels/<name>.blk` in `aces.vromfs.bin`)
pub fn level_settings(game: &Path, level_name: &str) -> Result<Option<Block>> {
    let path = game.join("aces.vromfs.bin");
    let raw = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let vfs = crate::vromfs::Vromfs::parse(&raw)?;
    drop(raw);
    let Some(file) = vfs.get(&format!("levels/{level_name}.blk")) else { return Ok(None) };
    let shared = crate::vromfs::shared_context(&vfs)?;
    Ok(Some(blk::parse(&file.data, shared.as_ref())?))
}

fn level_real(settings: &Block, key: &str) -> Option<f32> {
    settings.params.iter().find(|(n, _)| n == key).and_then(|(_, v)| if let Value::Real(r) = v { Some(*r) } else { None })
}

/// The ground: one mesh per landmesh cell, textured when the ground material can be read
fn build_terrain(
    level: &Level,
    index: &ResourceIndex,
    textures: &TextureLibrary,
    opts: &SceneOptions,
    report: &mut SceneReport,
) -> Result<Option<(Model, [f32; 4])>> {
    let landmesh = level.landmesh()?;
    let hm: Option<Heightmap> = level.heightmap()?;
    report.landmesh = landmesh.is_some();
    report.heightmap = hm.is_some();
    let heightmap = hm.as_ref().map(HeightGrid::from_heightmap);
    let heightmap_rect = heightmap.as_ref().map(|g| [g.origin[0], g.origin[1], g.origin[0] + (g.width - 1) as f32 * g.cell_size, g.origin[1] + (g.height - 1) as f32 * g.cell_size]);
    let coarse = match &landmesh {
        Some(lm) => match lm.tracer_mesh()? {
            Some(mesh) => Some(HeightGrid::rasterize(&mesh, lm.grid_cell_size)?),
            None => None,
        },
        None => None,
    };
    if coarse.is_none() && heightmap.is_none() {
        return Ok(None);
    }
    // The heightmap earns a finer mesh and finer colours only where it is finer than the landmesh: on
    // the maps where it covers everything at the landmesh's own spacing it adds nothing
    let fine = heightmap.as_ref().filter(|h| coarse.as_ref().is_none_or(|c| h.cell_size < c.cell_size));
    let fine_rect = fine.and(heightmap_rect);

    // Tiles: the landmesh's cells, or the heightmap alone
    let tiles: Vec<[f32; 4]> = match &landmesh {
        Some(lm) => {
            let s = lm.land_cell_size;
            let mut t = Vec::new();
            for cz in 0..lm.cells[1] {
                for cx in 0..lm.cells[0] {
                    let (x0, z0) = ((lm.origin[0] + cx) as f32 * s, (lm.origin[1] + cz) as f32 * s);
                    t.push([x0, z0, x0 + s, z0 + s]);
                }
            }
            t
        }
        None => vec![heightmap_rect.context("no terrain source")?],
    };
    let extent = tiles.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |e, t| [e[0].min(t[0]), e[1].min(t[1]), e[2].max(t[2]), e[3].max(t[3])]);
    let area = |t: &[f32; 4]| f64::from(t[2] - t[0]) * f64::from(t[3] - t[1]);
    let (mut fine_area, mut all_area) = (0.0f64, 0.0f64);
    for t in &tiles {
        all_area += area(t);
        if fine_rect.is_some_and(|r| rect_overlaps(r, *t)) {
            fine_area += area(&rect_clip(fine_rect.unwrap(), *t));
        }
    }
    let outer_area = (all_area - fine_area).max(0.0);
    // Budgets: scale the spacing of both kinds of tile together until the totals fit
    let pixels = |f: f32| fine_area / f64::from(opts.playable_texel_m * f).powi(2) + outer_area / f64::from(opts.outer_texel_m * f).powi(2);
    let max_pixels = f64::from(opts.ground_texture_mb) * 1048576.0 / 4.0;
    let texel_scale = if pixels(1.0) > max_pixels { (pixels(1.0) / max_pixels).sqrt() as f32 } else { 1.0 };
    let vertices = |f: f32| fine_area / f64::from(opts.fine_mesh_m * f).powi(2) + all_area / f64::from(opts.coarse_mesh_m * f).powi(2);
    let max_vertices = f64::from(opts.terrain_vertices_k) * 1000.0;
    let mesh_scale = if vertices(1.0) > max_vertices { (vertices(1.0) / max_vertices).sqrt() as f32 } else { 1.0 };
    if texel_scale > 1.0 || mesh_scale > 1.0 {
        report.warnings.push(format!("terrain budget: ground picture spacing x{texel_scale:.2}, mesh spacing x{mesh_scale:.2}"));
    }

    // Ground colours
    let baker = match GroundMaterial::load(level, index) {
        Ok(Some(gm)) => {
            report.land_classes = gm.classes.len();
            report.warnings.extend(gm.errors.iter().cloned());
            match gm.weights {
                Some(weights) => {
                    let corner = weights.origin;
                    let maps = decode_colormaps(&gm.classes, textures, opts.colormap_mb, &mut |w| report.warnings.push(w));
                    Some(GroundBaker::new(weights, &gm.classes, maps, corner))
                }
                None => None,
            }
        }
        Ok(None) => None,
        Err(e) => {
            report.warnings.push(format!("ground material: {e:#}"));
            None
        }
    };
    report.ground_textured = baker.is_some();

    let mut meshes = Vec::new();
    let mut materials = Vec::new();
    for tile in &tiles {
        let mut mesh = Mesh { name: format!("terrain_{:.0}_{:.0}", tile[0], tile[1]), node: 0, material: materials.len(), ..Mesh::default() };
        let under_fine = fine_rect.filter(|r| rect_overlaps(*r, *tile));
        // The landmesh where it has ground, else the heightmap (it covers what the landmesh leaves out)
        let base = |x: f32, z: f32| {
            let h = coarse.as_ref().map_or(f32::NAN, |c| c.height_near(x, z));
            if h.is_nan() {
                heightmap.as_ref().map_or(f32::NAN, |f| f.height_at(x, z))
            } else {
                h
            }
        };
        let step = (opts.coarse_mesh_m * mesh_scale).max(if coarse.is_none() { heightmap.as_ref().map_or(0.0, |h| h.cell_size) } else { 0.0 });
        let patch = grid_patch(&base, *tile, step, under_fine);
        add_patch(&mut mesh, &patch, *tile);
        if let (Some(f), Some(r)) = (fine, under_fine) {
            let clipped = rect_clip(r, *tile);
            let patch = grid_patch(&|x, z| f.height_at(x, z), clipped, (opts.fine_mesh_m * mesh_scale).max(f.cell_size), None);
            add_patch(&mut mesh, &patch, *tile);
        }
        if mesh.indices.is_empty() {
            continue;
        }
        normalize_normals(&mut mesh);
        report.terrain_triangles += mesh.indices.len() / 3;
        let base_color = baker.as_ref().map(|b| {
            let texel = if under_fine.is_some() { opts.playable_texel_m } else { opts.outer_texel_m } * texel_scale;
            image(b.bake(*tile, texel))
        });
        materials.push(Material { name: mesh.name.clone(), base_color, metallic: 0.0, roughness: 1.0, ..Material::default() });
        trim(&mut mesh);
        meshes.push(mesh);
    }
    report.terrain_tiles = meshes.len();
    if meshes.is_empty() {
        return Ok(None);
    }
    Ok(Some((Model { name: "terrain".into(), nodes: vec![Node::new("terrain", None)], meshes, materials, skins: Vec::new() }, extent)))
}

/// A flat quad over `rect` at `level` metres
fn water_model(rect: [f32; 4], level: f32, color: Option<[f32; 3]>) -> Model {
    let corners = [[rect[0], rect[1]], [rect[2], rect[1]], [rect[0], rect[3]], [rect[2], rect[3]]];
    let mut mesh = Mesh { name: "water".into(), node: 0, material: 0, ..Mesh::default() };
    for c in corners {
        mesh.positions.push(turn([c[0], level, c[1]]));
        mesh.normals.push([0.0, 1.0, 0.0]);
        mesh.uvs.push([0.0, 0.0]);
    }
    // Up-facing in the bridge's axes
    mesh.indices = vec![0, 1, 2, 2, 1, 3];
    let g = cross(sub(mesh.positions[1], mesh.positions[0]), sub(mesh.positions[2], mesh.positions[0]));
    if g[1] < 0.0 {
        mesh.indices = vec![0, 2, 1, 1, 2, 3];
    }
    let factor = color.map_or([1.0; 4], |c| [c[0], c[1], c[2], 1.0]);
    let material = Material { name: "water".into(), base_color_factor: factor, metallic: 0.0, roughness: 1.0, double_sided: true, ..Material::default() };
    Model { name: "water".into(), nodes: vec![Node::new("water", None)], meshes: vec![mesh], materials: vec![material], skins: Vec::new() }
}

/// Bytes a model holds
fn model_bytes(m: &Model) -> usize {
    let meshes: usize = m
        .meshes
        .iter()
        .map(|x| x.positions.len() * 12 + x.normals.len() * 12 + x.uvs.len() * 8 + x.indices.len() * 4)
        .sum();
    let images: usize = m.materials.iter().filter_map(|x| x.base_color.as_ref()).map(|i| i.rgba.len()).sum();
    meshes + images
}

/// What loading levels needs from the game folder, gathered once and shareable between loads: where each
/// resource pack holds what, and where each texture is. About 20 MB, kept until dropped; there is no
/// other state, so a program that has loaded its levels can drop this (the provider does, in
/// `clear_caches`) to give the memory back.
#[derive(Clone)]
pub struct LevelSources {
    pub resources: Arc<ResourceIndex>,
    pub textures: Arc<TextureLibrary>,
}

/// The resource classes a level's packs are scanned for: placed-object models, land classes, and collision
const LEVEL_CLASSES: &[u32] = &[REND_INST, LAND_CLASS, crate::collision::COLLISION];

impl LevelSources {
    /// Scans the game's `.grp` and `.dxp.bin` packs
    pub fn open(game: &Path) -> Result<Self> {
        Ok(Self { resources: Arc::new(ResourceIndex::build(game, LEVEL_CLASSES)?), textures: Arc::new(TextureLibrary::open(game)?) })
    }

    /// Like [`LevelSources::open`] over a texture index that is already built
    pub fn with_index(game: &Path, textures: Arc<TextureIndex>) -> Result<Self> {
        Ok(Self { resources: Arc::new(ResourceIndex::build(game, LEVEL_CLASSES)?), textures: Arc::new(TextureLibrary::from_index(textures)) })
    }
}

/// Loads a level as a scene. `level_file` is a `levels/*.bin` file; `game` the game folder.
pub fn load_level_scene(game: &Path, level_file: &Path, opts: &SceneOptions) -> Result<(Scene, SceneReport)> {
    load_level_scene_with(game, level_file, opts, &LevelSources::open(game)?)
}

/// Like [`load_level_scene`], with the pack indexes already built (see [`LevelSources`]). When it
/// returns, the allocator is asked to hand back the memory the load used and freed
/// ([`crate::memory::release_freed_memory`]); the scene itself is the only thing it keeps.
pub fn load_level_scene_with(game: &Path, level_file: &Path, opts: &SceneOptions, sources: &LevelSources) -> Result<(Scene, SceneReport)> {
    let out = load_scene_inner(game, level_file, opts, sources, false);
    crate::memory::release_freed_memory();
    out.map(|(scene, report, _)| (scene, report))
}

/// Like [`load_level_scene_with`], and also returns the leaf cards of the placed objects, which the scene
/// leaves out (see the module docs). The scene and its report are the same as without them, except
/// that a model made only of leaf cards (a bush) is not "nothing to draw" in the report.
pub fn load_level_scene_with_leaves(game: &Path, level_file: &Path, opts: &SceneOptions, sources: &LevelSources) -> Result<(Scene, SceneReport, SceneLeaves)> {
    let out = load_scene_inner(game, level_file, opts, sources, true);
    crate::memory::release_freed_memory();
    out
}

fn load_scene_inner(game: &Path, level_file: &Path, opts: &SceneOptions, sources: &LevelSources, collect_leaves: bool) -> Result<(Scene, SceneReport, SceneLeaves)> {
    let started = Instant::now();
    let name = level_file.file_stem().map_or("level".to_string(), |s| s.to_string_lossy().into_owned());
    let mut report = SceneReport { name: name.clone(), ..SceneReport::default() };
    let file = vehicle::map(level_file)?;
    let level = Level::parse(&file).with_context(|| format!("parsing {}", level_file.display()))?;
    let (index, textures) = (&*sources.resources, &*sources.textures);

    let mut scene = Scene { name: name.clone(), ..Scene::default() };

    // Ground
    let mut extent = None;
    if let Some((terrain, e)) = build_terrain(&level, index, textures, opts, &mut report)? {
        report.memory_bytes += model_bytes(&terrain);
        scene.models.push(SceneModel::Inline(terrain));
        scene.instances.push(Instance::new(scene.models.len() - 1, Transform::IDENTITY));
        extent = Some(e);
    }

    // Water: the plane at the level's own water level. Its colour is the level's water_aces_color_and_density
    // when it has one; otherwise the plane keeps the bridge's default material (the shader's own default is
    // not in the level)
    match level_settings(game, &name) {
        Ok(Some(settings)) => {
            if let (Some(l), Some(e)) = (level_real(&settings, "water_level"), extent) {
                report.water_level = Some(l);
                let shader_vars = settings.blocks.iter().find(|b| b.name == "shader_vars");
                let colour = shader_vars.and_then(|b| b.params.iter().find(|(n, _)| n == "water_aces_color_and_density")).and_then(|(_, v)| if let Value::Point4(p) = v { Some([p[0], p[1], p[2]]) } else { None });
                report.water_colour = colour;
                scene.models.push(SceneModel::Inline(water_model(e, l, colour)));
                scene.instances.push(Instance::new(scene.models.len() - 1, Transform::IDENTITY));
            }
        }
        Ok(None) => report.warnings.push(format!("no levels/{name}.blk in aces.vromfs.bin; no water level")),
        Err(e) => report.warnings.push(format!("level settings: {e:#}")),
    }

    // Placed objects
    let layers: Vec<RendInstLayer> = level.rendinst_layers()?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for layer in &layers {
        for (pool, n) in layer.pools.iter().zip(layer.counts()) {
            if n > 0 {
                *counts.entry(pool.name.clone()).or_default() += n;
            }
            if pool.palette_rotation {
                report.palette_pools += 1;
            }
        }
    }
    let mut by_pack: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for name in counts.keys() {
        match index.find(REND_INST, name) {
            Some(p) => by_pack.entry(p.to_path_buf()).or_default().push(name.clone()),
            None => report.unresolved.push((name.clone(), counts[name])),
        }
    }
    report.unresolved.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let mut descs = model_descs_where(game, |n| counts.contains_key(n))?;
    let mut model_of: HashMap<String, usize> = HashMap::new();
    let mut leaves = SceneLeaves::default();
    let mut leaf_of: HashMap<String, usize> = HashMap::new();
    {
        let mut builder = ModelBuilder { textures, opts, holders: HashMap::new(), pending: Vec::new(), report: &mut report, collect_leaves, leaf_textures: HashMap::new(), leaf_sets: Vec::new() };
        for (path, mut names) in by_pack {
            names.sort();
            let map = vehicle::map(&path)?;
            let pack = GrpPack::parse(&map).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
            for n in names {
                let Some(res) = pack.find(&n, REND_INST) else { continue };
                let ri = match RiModel::parse(pack.data(res)) {
                    Ok(m) => m,
                    Err(e) => {
                        builder.report.failed.push((n.clone(), format!("{e:#}")));
                        continue;
                    }
                };
                let desc = descs.remove(&n);
                let built = builder.build(&n, &ri, desc.as_ref(), &scene.models);
                let sets = std::mem::take(&mut builder.leaf_sets);
                if !sets.is_empty() {
                    let cards: usize = sets.iter().map(|s| s.cards.len()).sum();
                    builder.report.leaf_models += 1;
                    builder.report.leaf_cards += cards;
                    builder.report.leaf_cards_drawn += cards as u64 * counts[&n] as u64;
                    builder.report.memory_bytes += cards * std::mem::size_of::<LeafCard>() + sets.iter().filter_map(|s| s.base_color.as_ref()).map(|i| i.rgba.len()).sum::<usize>();
                    leaf_of.insert(n.clone(), leaves.models.len());
                    leaves.models.push(LeafModel { name: n.clone(), sets });
                }
                let Some((model, tris)) = built else {
                    if !leaf_of.contains_key(&n) {
                        builder.report.failed.push((n.clone(), "nothing to draw in its LOD".into()));
                    }
                    continue;
                };
                builder.report.object_triangles += tris;
                builder.report.drawn_triangles += tris as u64 * counts[&n] as u64;
                builder.report.memory_bytes += model_bytes(&model);
                model_of.insert(n, scene.models.len());
                scene.models.push(SceneModel::Inline(model));
            }
        }
    }
    drop(descs);
    report.object_models = model_of.len();

    let mut placed = 0usize;
    for layer in &layers {
        let table: Vec<Option<usize>> = layer.pools.iter().map(|p| model_of.get(&p.name).copied()).collect();
        let leaf_table: Vec<Option<usize>> = layer.pools.iter().map(|p| leaf_of.get(&p.name).copied()).collect();
        let unrotated: Vec<bool> = layer.pools.iter().map(|p| p.pos_only && !p.palette_rotation).collect();
        for inst in layer.instances()? {
            let (model, leaf) = (table[inst.pool], leaf_table[inst.pool]);
            if model.is_none() && leaf.is_none() {
                continue;
            }
            if unrotated[inst.pool] {
                report.trees_without_rotation += 1;
            }
            let transform = instance_transform(&inst.tm);
            if let Some(model) = model {
                scene.instances.push(Instance::new(model, transform));
                placed += 1;
            }
            if let Some(leaf) = leaf {
                leaves.instances.push(Instance::new(leaf, transform));
            }
        }
    }
    scene.instances.shrink_to_fit();
    leaves.instances.shrink_to_fit();
    report.instances = placed;
    report.memory_bytes += (scene.instances.len() + leaves.instances.len()) * std::mem::size_of::<Instance>();
    scene.validate().map_err(|e| anyhow::anyhow!("{}", e.0))?;
    report.load_ms = started.elapsed().as_millis();
    if scene.instances.is_empty() {
        bail!("{name}: nothing to put in a scene");
    }
    Ok((scene, report, leaves))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5)
    }

    /// Applies a bridge transform to a point
    fn apply(t: &Transform, p: [f32; 3]) -> [f32; 3] {
        let p = [p[0] * t.scale[0], p[1] * t.scale[1], p[2] * t.scale[2]];
        let [x, y, z, w] = t.rotation;
        let u = [x, y, z];
        let uv = cross(u, p);
        let uuv = cross(u, uv);
        let r = [0, 1, 2].map(|k| p[k] + 2.0 * (w * uv[k] + uuv[k]));
        [r[0] + t.translation[0], r[1] + t.translation[1], r[2] + t.translation[2]]
    }

    #[test]
    fn identity_placements_only_swap_axes() {
        let t = instance_transform(&[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [10.0, 20.0, 30.0]]);
        assert_eq!(t.translation, [30.0, 20.0, 10.0]);
        assert!(t.scale.iter().all(|&s| (s - 1.0).abs() < 1e-6));
        // The model's point (1, 2, 3) in Dagor's axes is (3, 2, 1) in the bridge's local axes, and ends up
        // at the turned Dagor world point (1+10, 2+20, 3+30)
        assert!(close(apply(&t, turn([1.0, 2.0, 3.0])), turn([11.0, 22.0, 33.0])));
    }

    #[test]
    fn placements_agree_with_dagor_matrices() {
        // A quarter turn about Y with scale 2: Dagor x axis -> (0, 0, 2) (towards +z), z axis -> (-2, 0, 0)
        let rows = [[0.0, 0.0, 2.0], [0.0, 2.0, 0.0], [-2.0, 0.0, 0.0], [5.0, 6.0, 7.0]];
        let t = instance_transform(&rows);
        assert!(t.scale.iter().all(|&s| (s - 2.0).abs() < 1e-5), "{:?}", t.scale);
        for local in [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, -0.7, 1.9]] {
            // Dagor: world = position + local.x * x axis + local.y * y axis + local.z * z axis
            let world: [f32; 3] = [0, 1, 2].map(|k| rows[3][k] + local[0] * rows[0][k] + local[1] * rows[1][k] + local[2] * rows[2][k]);
            assert!(close(apply(&t, turn(local)), turn(world)), "{local:?}");
        }
    }

    #[test]
    fn mirrored_placements_use_a_negative_scale() {
        let rows = [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0]];
        let t = instance_transform(&rows);
        assert!(t.scale.iter().filter(|&&s| s < 0.0).count() == 1);
        assert!(close(apply(&t, turn([1.0, 0.0, 0.0])), turn([-1.0, 0.0, 0.0])));
        assert!(close(apply(&t, turn([0.0, 0.0, 1.0])), turn([0.0, 0.0, 1.0])));
    }

    #[test]
    fn palette_rotations_turn_about_y() {
        // 90 degrees: x axis (0, 0, 1), z axis (-1, 0, 0), as the level reader writes them
        let t = instance_transform(&[[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
        let q = t.rotation;
        let n = (q.iter().map(|c| c * c).sum::<f32>()).sqrt();
        assert!((n - 1.0).abs() < 1e-5);
        assert!(close(apply(&t, turn([1.0, 0.0, 0.0])), turn([0.0, 0.0, 1.0])));
    }

    #[test]
    fn meshes_wind_with_their_normals() {
        let mut m = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, -1.0]; 3],
            indices: vec![0, 1, 2],
            ..Mesh::default()
        };
        fix_winding(&mut m);
        assert_eq!(m.indices, [0, 2, 1]);
        fix_winding(&mut m);
        assert_eq!(m.indices, [0, 2, 1]);
    }

    #[test]
    fn grid_patches_skip_holes_and_covered_quads() {
        let flat = |_: f32, _: f32| 3.0;
        let p = grid_patch(&flat, [0.0, 0.0, 8.0, 8.0], 4.0, None);
        assert_eq!((p.positions.len(), p.indices.len()), (9, 4 * 6));
        let p = grid_patch(&flat, [0.0, 0.0, 8.0, 8.0], 4.0, Some([0.0, 0.0, 4.0, 4.0]));
        assert_eq!(p.indices.len(), 3 * 6);
        let hole = |x: f32, _: f32| if x == 4.0 { f32::NAN } else { 1.0 };
        let p = grid_patch(&hole, [0.0, 0.0, 8.0, 8.0], 4.0, None);
        assert_eq!(p.indices.len(), 0);
    }

    #[test]
    fn terrain_patches_face_up_with_tile_uvs() {
        let mut mesh = Mesh::default();
        let patch = grid_patch(&|x, _| x * 0.5, [100.0, 200.0, 108.0, 208.0], 8.0, None);
        add_patch(&mut mesh, &patch, [100.0, 200.0, 116.0, 216.0]);
        normalize_normals(&mut mesh);
        assert!(mesh.normals.iter().all(|n| n[1] > 0.0));
        for t in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[t[k] as usize]);
            assert!(cross(sub(b, a), sub(c, a))[1] > 0.0);
        }
        // x runs to the tile's middle, z too
        assert_eq!(mesh.uvs.iter().map(|u| (u[0], u[1])).fold((0.0f32, 0.0f32), |m, u| (m.0.max(u.0), m.1.max(u.1))), (0.5, 0.5));
        // Dagor's x becomes the bridge's z
        assert!((mesh.positions.iter().map(|p| p[2]).fold(0.0f32, f32::max) - 108.0).abs() < 1e-4);
    }

    #[test]
    fn water_faces_up_at_its_level() {
        let m = water_model([0.0, 0.0, 10.0, 20.0], 35.0, Some([0.5, 0.5, 0.5]));
        let mesh = &m.meshes[0];
        assert!(mesh.positions.iter().all(|p| p[1] == 35.0));
        let t = &mesh.indices[..3];
        let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[t[k] as usize]);
        assert!(cross(sub(b, a), sub(c, a))[1] > 0.0);
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn materials_come_from_the_description() {
        let text = |k: &str, v: &str| (k.to_string(), Value::Str(v.to_string()));
        let mat = Block {
            name: "mat".into(),
            params: vec![text("cls", "rendinst_simple"), text("par", "atest=127\nreal_two_sided=1"), ("t0".into(), Value::Int(1))],
            blocks: Vec::new(),
        };
        let desc = Block {
            name: "x".into(),
            params: Vec::new(),
            blocks: vec![
                Block { name: "tex".into(), params: vec![text("tex", "a*"), text("tex", "b*")], blocks: Vec::new() },
                Block { name: "mat".into(), params: Vec::new(), blocks: vec![mat] },
            ],
        };
        let m = object_materials(&desc);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].class.as_str(), m[0].color.as_deref(), m[0].atest, m[0].two_sided), ("rendinst_simple", Some("b*"), Some(127), true));
        assert!(skipped_class("rendinst_deferred_decal") && skipped_class("rendinst_baked_impostor") && skipped_class("rendinst_facing_leaves") && !skipped_class("rendinst_simple") && !skipped_class("rendinst_tree"));
    }

    #[test]
    fn material_numbers_are_read_as_stated() {
        let text = |k: &str, v: &str| (k.to_string(), Value::Str(v.to_string()));
        let mat = Block {
            name: "mat".into(),
            params: vec![text("cls", "rendinst_facing_leaves"), text("par", "use_cross_dissolve=1\nfacing_leaves_translucency=0.3\nname=oak\n cross_dissolve_add = -66.6667 \nnot a pair")],
            blocks: Vec::new(),
        };
        let desc = Block { name: "x".into(), params: Vec::new(), blocks: vec![Block { name: "mat".into(), params: Vec::new(), blocks: vec![mat] }] };
        let m = object_materials(&desc);
        // Only the lines that are a key and a number, in order; no atest, so the tree classes' cutoff applies
        assert_eq!(m[0].params, [("use_cross_dissolve".to_string(), 1.0), ("facing_leaves_translucency".to_string(), 0.3), ("cross_dissolve_add".to_string(), -66.6667)]);
        assert_eq!(alpha_mode(m.first(), "rendinst_facing_leaves"), AlphaMode::Mask(127.0 / 255.0));
        assert_eq!(alpha_mode(None, "rendinst_simple"), AlphaMode::Opaque);
        assert_eq!(alpha_mode(Some(&ObjMaterial { class: String::new(), color: None, atest: Some(0), two_sided: false, params: Vec::new() }), "rendinst_tree"), AlphaMode::Opaque);
        assert_eq!(alpha_mode(Some(&ObjMaterial { class: String::new(), color: None, atest: Some(51), two_sided: false, params: Vec::new() }), "rendinst_simple"), AlphaMode::Mask(0.2));
    }

    #[test]
    fn leaf_cards_turn_their_centre_and_normal_but_not_their_camera_offsets() {
        use crate::rendinst::LeafCorner;
        let card =
            LeafCard { centre: [1.0, 2.0, 3.0], corners: [LeafCorner { offset: [0.5, -0.25, 0.125], uv: [1.0, 0.0] }; 4], normal: [0.0, 0.5, 1.0], wind_group: 2, triangles: [[0, 1, 2], [0, 2, 3]] };
        let b = card_to_bridge(card);
        // Dagor's (x, y, z) is the bridge's (z, y, x), like a mesh's positions and normals
        assert_eq!((b.centre, b.normal), ([3.0, 2.0, 1.0], [1.0, 0.5, 0.0]));
        // The offsets belong to the camera's frame, which the swap does not touch; nor do uv, indices or wind
        assert_eq!((b.corners, b.wind_group, b.triangles), (card.corners, 2, card.triangles));
    }

    #[test]
    fn leaves_are_counted_once_per_model() {
        use crate::rendinst::LeafCorner;
        let set = |n: usize| LeafSet { base_color: None, alpha: AlphaMode::Opaque, params: Vec::new(), cards: vec![LeafCard { corners: [LeafCorner::default(); 4], ..LeafCard::default() }; n] };
        let leaves = SceneLeaves { models: vec![LeafModel { name: "a".into(), sets: vec![set(3), set(2)] }, LeafModel { name: "b".into(), sets: vec![set(4)] }], instances: Vec::new() };
        assert_eq!(leaves.card_count(), 9);
        assert_eq!(SceneLeaves::default().card_count(), 0);
    }

    /// A tree made by hand: a trunk triangle (float3 positions, in a 12-byte buffer) and, in a 24-byte
    /// buffer, one leaf card of two triangles around (1, 2, 3)
    fn tree_with_leaves(leaf_triangles: usize) -> RiModel {
        use wt_blk::dxp_and_grp::dynmodel::{Channel, VData};
        let floats = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let trunk = VData { stride: 12, vertices: floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]), indices: vec![0, 1, 2], channels: vec![Channel { ty: 2, usage: 0, index: 0, offset: 0 }] };
        // Halves: 1 = 0x3C00, 2 = 0x4000, 3 = 0x4200, 0.5 = 0x3800, 0.25 = 0x3400, 0.125 = 0x3000; the
        // corner's last half is the wind index (0 here)
        let leaf = |ox: u16, oy: u16| {
            let mut v = Vec::new();
            for h in [0x3C00u16, 0x4000, 0x4200, 0x3C00, 0, 0, ox, oy, 0x3000, 0] {
                v.extend(h.to_le_bytes());
            }
            v.extend([128u8, 255, 128, 0]);
            v
        };
        let leaves =
            VData { stride: 24, vertices: [leaf(0x3800, 0x3400), leaf(0xB800, 0x3400), leaf(0xB800, 0xB400), leaf(0x3800, 0xB400)].concat(), indices: vec![0, 1, 2, 0, 2, 3], channels: Vec::new() };
        let elem = |vdata, material, triangles| Elem { vdata, material, first_index: 0, triangles, base_vertex: 0 };
        RiModel {
            vdata: vec![trunk, leaves],
            lods: vec![crate::rendinst::RiLod { range: 100.0, elems: vec![elem(0, 0, 1), elem(1, 1, leaf_triangles)] }],
            bbox: [[-1.0; 3], [1.0; 3]],
            has_impostor: false,
            impostor_data: None,
        }
    }

    fn tree_description() -> Block {
        let text = |k: &str, v: &str| (k.to_string(), Value::Str(v.to_string()));
        let mat = |cls: &str, par: &str| Block { name: "mat".into(), params: vec![text("cls", cls), text("par", par), ("t0".into(), Value::Int(0))], blocks: Vec::new() };
        Block {
            name: "tree".into(),
            params: Vec::new(),
            blocks: vec![
                Block { name: "tex".into(), params: vec![text("tex", "leaf*")], blocks: Vec::new() },
                Block { name: "mat".into(), params: Vec::new(), blocks: vec![mat("rendinst_tree", ""), mat("rendinst_facing_leaves", "facing_leaves_translucency=0.3")] },
            ],
        }
    }

    #[test]
    fn leaf_cards_stay_out_of_the_meshes_and_come_beside_them_when_asked() {
        // No textures anywhere: an empty library
        let library = TextureLibrary::from_index(Arc::new(TextureIndex::new()));
        let opts = SceneOptions::default();
        for collect in [false, true] {
            let mut report = SceneReport::default();
            let mut b = ModelBuilder {
                textures: &library,
                opts: &opts,
                holders: HashMap::new(),
                pending: Vec::new(),
                report: &mut report,
                collect_leaves: collect,
                leaf_textures: HashMap::new(),
                leaf_sets: Vec::new(),
            };
            let (model, triangles) = b.build("tree", &tree_with_leaves(2), Some(&tree_description()), &[]).unwrap();
            // The scene model is the trunk, with or without the cards
            assert_eq!((triangles, model.meshes.len(), model.meshes[0].positions.len()), (1, 1, 3));
            let sets = std::mem::take(&mut b.leaf_sets);
            assert_eq!(b.report.skipped_classes.get("rendinst_facing_leaves"), Some(&1), "the draw call is left out of the scene either way");
            if !collect {
                assert!(sets.is_empty());
                continue;
            }
            assert_eq!(sets.len(), 1);
            let set = &sets[0];
            assert_eq!(set.params, [("facing_leaves_translucency".to_string(), 0.3)]);
            assert_eq!((set.alpha, set.base_color.is_none()), (AlphaMode::Mask(127.0 / 255.0), true));
            let card = &set.cards[0];
            // (1, 2, 3) turned to the bridge's axes; the corners keep their camera-frame offsets, which
            // are the stored ones: (+-0.5, +-0.25, 0.125)
            assert_eq!((set.cards.len(), card.centre, card.triangles), (1, [3.0, 2.0, 1.0], [[0, 1, 2], [0, 2, 3]]));
            assert_eq!(card.corners.map(|c| c.offset), [[0.5, 0.25, 0.125], [-0.5, 0.25, 0.125], [-0.5, -0.25, 0.125], [0.5, -0.25, 0.125]]);
        }
    }

    #[test]
    fn a_model_of_leaves_alone_has_cards_and_no_mesh() {
        let library = TextureLibrary::from_index(Arc::new(TextureIndex::new()));
        let opts = SceneOptions::default();
        let mut model = tree_with_leaves(2);
        model.lods[0].elems.remove(0);
        let mut report = SceneReport::default();
        let mut b = ModelBuilder {
            textures: &library,
            opts: &opts,
            holders: HashMap::new(),
            pending: Vec::new(),
            report: &mut report,
            collect_leaves: true,
            leaf_textures: HashMap::new(),
            leaf_sets: Vec::new(),
        };
        assert!(b.build("bush", &model, Some(&tree_description()), &[]).is_none());
        assert_eq!(b.leaf_sets.len(), 1);
    }

    #[test]
    fn elements_that_are_not_cards_are_reported_not_guessed() {
        let library = TextureLibrary::from_index(Arc::new(TextureIndex::new()));
        let opts = SceneOptions::default();
        let mut report = SceneReport::default();
        let mut b = ModelBuilder {
            textures: &library,
            opts: &opts,
            holders: HashMap::new(),
            pending: Vec::new(),
            report: &mut report,
            collect_leaves: true,
            leaf_textures: HashMap::new(),
            leaf_sets: Vec::new(),
        };
        // One triangle, not a pair
        assert!(b.build("tree", &tree_with_leaves(1), Some(&tree_description()), &[]).is_some());
        assert!(b.leaf_sets.is_empty());
        assert!(b.report.warnings.iter().any(|w| w.contains("tree") && w.contains("of two")), "{:?}", b.report.warnings);
    }
}
