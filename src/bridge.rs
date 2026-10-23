//! War Thunder as a [`rewrite_bridge::Provider`]: lists the vehicles in the installed game's .grp packs
//! and loads them as bridge models, so other game rewrites can put them in their scenes.
//!
//! Ids look like `tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman`: a pack under `content/base/res`
//! (the patched copy wins) and a model in it.
//!
//! Axes: Dagor is left-handed with Y up, and its vehicles face +X with their left side at +Z (the
//! Sherman's muzzle node `emtr_gun_flame` is at x = 3.9 and its `wheel_l_*` nodes at z = +1.08). The
//! bridge follows glTF: right-handed, Y up, the front at +Z and so the left side at +X. Dagor's
//! `(x, y, z)` therefore becomes `(z, y, x)`, a swap that is also a mirror. [`vehicle`] already
//! mirrors Z (front +X, right +Z, triangles rewound counter-clockwise), and this module turns that a
//! quarter turn about Y: `(x, y, z)` to `(-z, y, x)`.
//!
//! Nodes keep their pivots but not their axes: every node frame is aligned with the model's, so
//! turning a node (the turret about Y, the gun about X) turns its subtree in the vehicle's own axes.
//! Dagor's node axes vary from node to node and many are mirrored, which a glTF node cannot hold.
//!
//! Ground vehicles come with their rig: [`Provider::load_animated`] returns the model plus a socket for
//! every data-named node worth attaching to or turning (the turret, gun, barrel and muzzle named by the
//! unit file, the wheels, track belts, crew cameras, hatches and effect emitters), and
//! [`WarThunder::vehicle_rig`] gives the turret limits and speeds, the drive train, the wheel radii and track
//! texture rate as plain structs. See [`crate::rig`] for the socket names and what each field means.
//!
//! Levels are scenes: `levels/avg_berlin.bin` is listed as [`AssetKind::Level`] and loaded with
//! `load_scene` (see [`crate::scene`]); the collision of their objects comes from
//! [`WarThunder::load_level_collision`] (see [`crate::level_collision`]).
//!
//! The provider owns the indexes it builds (textures, packs, units), nothing is global, and
//! [`WarThunder::clear_caches`] drops them and hands freed memory back.
//!
//! Skinned meshes (tracks, cables) become skins with the game's own bones and weights: each vertex
//! follows up to four skeleton nodes, taken from the model's bone table (a track follows the `root` node
//! and the suspension wheel nodes `bone_wheel_<l|r>_<NN>`). A skin's inverse bind matrices undo the
//! joint's place, `translate(-pivot)`, since node frames carry no axes (the game's own matrices
//! include them).

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{bail, Context, Result};
use rewrite_bridge::{AlphaMode, AnimatedModel, AssetInfo, AssetKind, BoxError, Image, Material, Mesh, Model, Node, Provider, Scene, Skin, Socket, Transform};
use wt_blk::dxp_and_grp::grp_pack::{class_id, GrpPack};

use crate::level_collision::{load_level_collision, load_object, LevelCollision, ObjectCollision};
use crate::rig::{to_bridge_axes, VehicleRig};
use crate::scene::{load_level_scene_with, LevelSources, SceneOptions, SceneReport};
use crate::tank::{self, UnitIndex};
use crate::vehicle::{self, Library, TextureIndex, Vehicle};

/// Folders of `content/base/res` that hold vehicles
pub const FOLDERS: [&str; 3] = ["tanks", "aircrafts", "ships"];

/// Damage and x-ray variants share their vehicle's pack but are not vehicles of their own
const VARIANTS: [&str; 3] = ["_dmg", "_xray", "_dm"];

/// What the provider keeps between loads, all of it rebuilt on demand: indexes of the game's packs and units.
/// A few tens of megabytes together; [`WarThunder::clear_caches`] drops them.
#[derive(Default)]
struct Caches {
    textures: Option<Arc<TextureIndex>>,
    library: Option<Arc<Library>>,
    levels: Option<LevelSources>,
    units: Option<Arc<UnitIndex>>,
}

/// War Thunder as a bridge provider. It owns every cache it uses (nothing is global), so dropping it, or
/// calling [`WarThunder::clear_caches`], gives the memory back.
pub struct WarThunder {
    game: PathBuf,
    caches: Mutex<Caches>,
    scene_options: SceneOptions,
}

impl WarThunder {
    /// `game` is the installed game's folder, the one holding `content` and `aces.vromfs.bin`
    pub fn new(game: impl Into<PathBuf>) -> Self {
        Self { game: game.into(), caches: Mutex::new(Caches::default()), scene_options: SceneOptions::default() }
    }

    /// Sets the budget parameters of [`Provider::load_scene`]
    pub fn with_scene_options(mut self, options: SceneOptions) -> Self {
        self.scene_options = options;
        self
    }

    /// Drops everything the provider has cached (the texture and pack indexes, the model descriptions, the
    /// unit index) and asks the allocator to hand its free pages back to the operating system. Scenes and
    /// models already returned are the caller's and are not touched; drop them first to get their memory
    /// back too. The caches are rebuilt by the next load (a second or two for the first vehicle or level).
    ///
    /// A program that loads a level and then drops the scene still shows the level's memory as resident
    /// until the allocator releases it (see [`crate::memory`]); `drop(scene); wt.clear_caches();` does.
    pub fn clear_caches(&self) {
        *self.lock() = Caches::default();
        crate::memory::release_freed_memory();
    }

    fn lock(&self) -> MutexGuard<'_, Caches> {
        // A panic while holding the lock leaves only caches behind, which are safe to use or rebuild
        self.caches.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn texture_index(&self) -> Result<Arc<TextureIndex>> {
        if let Some(t) = self.lock().textures.clone() {
            return Ok(t);
        }
        let built = Arc::new(vehicle::texture_index(&self.game)?);
        Ok(self.lock().textures.get_or_insert(built).clone())
    }

    /// The texture index and material descriptions of the vehicles, gathered on first use
    fn library(&self) -> Result<Arc<Library>> {
        if let Some(l) = self.lock().library.clone() {
            return Ok(l);
        }
        let built = Arc::new(Library::with_index(&self.game, self.texture_index()?)?);
        Ok(self.lock().library.get_or_insert(built).clone())
    }

    fn level_sources(&self) -> Result<LevelSources> {
        if let Some(l) = self.lock().levels.clone() {
            return Ok(l);
        }
        let built = LevelSources::with_index(&self.game, self.texture_index()?)?;
        Ok(self.lock().levels.get_or_insert(built).clone())
    }

    fn units(&self) -> Result<Arc<UnitIndex>> {
        if let Some(u) = self.lock().units.clone() {
            return Ok(u);
        }
        let built = Arc::new(UnitIndex::build(&self.game)?);
        Ok(self.lock().units.get_or_insert(built).clone())
    }

    /// Levels (`levels/*.bin`), as `levels/<name>.bin`
    fn list_levels(&self, filter: &str) -> Vec<AssetInfo> {
        let Ok(rd) = std::fs::read_dir(self.game.join("levels")) else { return Vec::new() };
        let mut out: Vec<AssetInfo> = rd
            .flatten()
            .filter_map(|e| {
                let file = e.file_name().to_string_lossy().into_owned();
                let stem = file.strip_suffix(".bin")?;
                let id = format!("levels/{file}");
                id.contains(filter).then(|| AssetInfo { id, name: stem.to_string(), kind: AssetKind::Level })
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// The file of a level named like `levels/avg_berlin.bin`
    fn level_file(&self, id: &str) -> Result<PathBuf> {
        let path = Path::new(id);
        let ok = path.components().all(|c| matches!(c, Component::Normal(_))) && id.starts_with("levels/") && id.ends_with(".bin") && id.matches('/').count() == 1;
        if !ok {
            bail!("{id} is not a level");
        }
        let file = self.game.join(path);
        if !file.is_file() {
            bail!("no level {id}");
        }
        Ok(file)
    }

    /// The collision of the objects a level places (`levels/avg_berlin.bin`), which `Scene` has no field for:
    /// see [`crate::level_collision`]. The ground is not in it; the scene's `terrain` model is the ground's
    /// collision mesh.
    pub fn load_level_collision(&self, id: &str) -> Result<LevelCollision> {
        load_level_collision(&self.level_file(id)?, &self.level_sources()?.resources)
    }

    /// The collision of one placed object by its model name in the scene (e.g. `berlin_city_build_1`);
    /// `None` when the game has no collision resource for it
    pub fn load_object_collision(&self, name: &str) -> Result<Option<ObjectCollision>> {
        load_object(&self.level_sources()?.resources, name)
    }

    /// Loads a level with the report of what went into it
    pub fn load_level(&self, id: &str) -> Result<(Scene, SceneReport)> {
        load_level_scene_with(&self.game, &self.level_file(id)?, &self.scene_options, &self.level_sources()?)
    }

    /// Every vehicle pack, as `folder/pack.grp` -> its file (patched copies replace the originals)
    fn packs(&self) -> BTreeMap<String, PathBuf> {
        let mut packs = BTreeMap::new();
        for root in ["content/base/res", "patch/content/base/res"] {
            for folder in FOLDERS {
                let Ok(dir) = std::fs::read_dir(self.game.join(root).join(folder)) else { continue };
                for entry in dir.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".grp") {
                        packs.insert(format!("{folder}/{name}"), entry.path());
                    }
                }
            }
        }
        packs
    }

    /// The file of a pack named like `tanks/usa_m4a1_76w_sherman.grp`
    fn pack(&self, rel: &str) -> Result<PathBuf> {
        let path = Path::new(rel);
        let ok = path.components().all(|c| matches!(c, Component::Normal(_)))
            && FOLDERS.iter().any(|f| rel.starts_with(&format!("{f}/")))
            && rel.ends_with(".grp");
        if !ok {
            bail!("{rel} is not a vehicle pack");
        }
        ["patch/content/base/res", "content/base/res"]
            .iter()
            .map(|root| self.game.join(root).join(path))
            .find(|p| p.is_file())
            .with_context(|| format!("no pack {rel}"))
    }

    fn list_vehicles(&self, filter: &str) -> Result<Vec<AssetInfo>> {
        let mut out = Vec::new();
        for (rel, path) in self.packs() {
            let file = vehicle::map(&path)?;
            let grp = GrpPack::parse(&file).with_context(|| format!("parsing {rel}"))?;
            let skeletons: Vec<&str> = grp.resources.iter().filter(|r| r.class_id == class_id::GEOM_NODE_TREE).map(|r| r.name.as_str()).collect();
            for r in grp.resources.iter().filter(|r| r.class_id == class_id::DYN_MODEL) {
                let has_skeleton = skeletons.iter().any(|s| s.strip_suffix("_skeleton") == Some(r.name.as_str()));
                if !has_skeleton || VARIANTS.iter().any(|v| r.name.ends_with(v)) {
                    continue;
                }
                let id = format!("{rel}:{}", r.name);
                if id.contains(filter) {
                    out.push(AssetInfo { id, name: r.name.clone(), kind: AssetKind::Vehicle });
                }
            }
        }
        Ok(out)
    }

    /// Splits `tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman` into its pack file and model name
    fn vehicle_parts<'a>(&self, id: &'a str) -> Result<(&'a str, &'a str, PathBuf)> {
        let (rel, name) = id.split_once(':').with_context(|| format!("{id}: expected pack.grp:model"))?;
        Ok((rel, name, self.pack(rel)?))
    }

    /// The unit data of a tank model (`None` for aircraft and ships, and for tank models no unit uses)
    fn tank_of(&self, rel: &str, model: &str) -> Result<Option<tank::Tank>> {
        if !rel.starts_with("tanks/") {
            return Ok(None);
        }
        match self.units()?.unit_of_model(model) {
            Some(unit) => Ok(Some(tank::load_unit(&self.game, unit)?)),
            None => Ok(None),
        }
    }

    fn load_vehicle(&self, id: &str) -> Result<AnimatedModel> {
        let (rel, name, pack) = self.vehicle_parts(id)?;
        let v = self.library()?.load(&pack, name, 0)?;
        // Sockets are for ground vehicles, whose unit files and node names the rig knows
        let rig = if rel.starts_with("tanks/") { Some(VehicleRig::new(&v, self.tank_of(rel, name)?.as_ref())) } else { None };
        let mut animated = AnimatedModel::new(to_model(v));
        if let Some(rig) = rig {
            animated.sockets = rig.sockets.iter().map(|s| Socket::new(s.name.clone(), s.node, Transform::IDENTITY)).collect();
        }
        animated.validate()?;
        Ok(animated)
    }

    /// The rig of a vehicle by the id [`Provider::list`] gave for it: the turret's nodes and limits, the drive
    /// train, the wheels and track belts, and the sockets `load_animated` returns, as plain structs (see
    /// [`crate::rig`]). Only the skeleton and meshes are read, not the textures, so it is much quicker than
    /// loading the model. Aircraft and ships have no unit file, so their rigs hold only what the model gives.
    pub fn vehicle_rig(&self, id: &str) -> Result<VehicleRig> {
        let (rel, name, pack) = self.vehicle_parts(id)?;
        let v = self.library()?.load_geometry(&pack, name, 0)?;
        let tank = self.tank_of(rel, name)?;
        Ok(VehicleRig::new(&v, tank.as_ref()))
    }

    /// The unit file's data for a tank model by name (e.g. `m4a1_76w_sherman`), without loading the model
    pub fn unit_of_model(&self, model: &str) -> Result<Option<tank::Tank>> {
        self.tank_of("tanks/", model)
    }
}

impl Provider for WarThunder {
    fn game(&self) -> &str {
        "War Thunder"
    }

    fn list(&self, filter: &str) -> Result<Vec<AssetInfo>, BoxError> {
        let mut out = self.list_vehicles(filter)?;
        out.extend(self.list_levels(filter));
        Ok(out)
    }

    fn load(&self, id: &str) -> Result<Model, BoxError> {
        if id.starts_with("levels/") {
            return Err(format!("{id} is a level: load it with load_scene").into());
        }
        Ok(self.load_vehicle(id)?.model)
    }

    fn load_animated(&self, id: &str) -> Result<AnimatedModel, BoxError> {
        if id.starts_with("levels/") {
            return Err(format!("{id} is a level: load it with load_scene").into());
        }
        Ok(self.load_vehicle(id)?)
    }

    fn load_scene(&self, id: &str) -> Result<Scene, BoxError> {
        Ok(self.load_level(id)?.0)
    }
}

/// From [`vehicle`]'s space (front +X, right +Z) to the bridge's (front +Z, left +X)
fn turn(p: [f32; 3]) -> [f32; 3] {
    to_bridge_axes(p)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn image(img: vehicle::Image) -> Image {
    Image { width: img.width, height: img.height, rgba: img.rgba }
}

fn material(m: vehicle::Material) -> Material {
    let textured = m.metal_rough.is_some();
    let mut out = Material {
        name: m.name,
        base_color: m.color.map(image),
        metallic_roughness: m.metal_rough.map(image),
        // Untextured parts get the viewer's plain painted-steel look
        metallic: if textured { 1.0 } else { 0.0 },
        roughness: if textured { 1.0 } else { 0.85 },
        normal: m.normal.map(image),
        alpha: if m.alpha_mask { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
        double_sided: m.double_sided,
        ..Material::default()
    };
    // Glass (vision blocks, lights): see-through and smooth, keeping its tint and normal map
    if out.name.contains("glass") {
        out.alpha = AlphaMode::Blend;
        out.base_color_factor = [1.0, 1.0, 1.0, 0.3];
        out.metallic_roughness = None;
        out.metallic = 0.0;
        out.roughness = 0.05;
    }
    out
}

/// Converts an assembled vehicle into a bridge model
pub fn to_model(v: Vehicle) -> Model {
    let pivot = |i: usize| turn(v.nodes[i].pivot);
    let nodes: Vec<Node> = v
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let name = if n.name.is_empty() { "@root".to_string() } else { n.name.clone() };
            let mut node = Node::new(name, n.parent);
            node.translation = n.parent.map_or(pivot(i), |p| sub(pivot(i), pivot(p)));
            node
        })
        .collect();

    // One skin per distinct set of bones; a skinned mesh keeps the game's per-vertex bones and weights
    let mut skins: Vec<Skin> = Vec::new();
    let mut skin_of: HashMap<Vec<usize>, usize> = HashMap::new();
    let mut meshes = Vec::with_capacity(v.meshes.len());
    for m in &v.meshes {
        let name = format!("{}_{}", nodes[m.node].name, v.materials[m.material].name);
        let mut mesh = Mesh {
            name,
            node: m.node,
            material: m.material,
            normals: m.normals.iter().map(|&n| turn(n)).collect(),
            uvs: m.uvs.clone(),
            indices: m.indices.clone(),
            ..Mesh::default()
        };
        if m.skinned {
            // Skinned vertices stay in model space; each joint's inverse bind matrix undoes its place
            let skin = *skin_of.entry(m.bones.clone()).or_insert_with(|| {
                let inverse_bind = m
                    .bones
                    .iter()
                    .map(|&b| {
                        let [x, y, z] = pivot(b);
                        [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., -x, -y, -z, 1.]
                    })
                    .collect();
                skins.push(Skin { joints: m.bones.clone(), inverse_bind });
                skins.len() - 1
            });
            mesh.positions = m.positions.iter().map(|&p| turn(p)).collect();
            mesh.joints = m.joints.clone();
            mesh.weights = m.weights.clone();
            mesh.skin = Some(skin);
        } else {
            // Rigid vertices are relative to their node
            let at = pivot(m.node);
            mesh.positions = m.positions.iter().map(|&p| sub(turn(p), at)).collect();
        }
        meshes.push(mesh);
    }
    drop(v.meshes);
    Model { name: v.name, nodes, meshes, materials: v.materials.into_iter().map(material).collect(), skins }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-triangle vehicle: a hull node at the origin and a turret node above it, plus a skinned
    /// strip on a track node
    fn vehicle() -> Vehicle {
        let tri = |node, skinned, material| vehicle::Mesh {
            material,
            node,
            skinned,
            positions: vec![[1.0, 2.0, 0.0], [2.0, 2.0, 0.0], [1.0, 3.0, 0.5]],
            normals: vec![[1.0, 0.0, 0.0]; 3],
            uvs: vec![[0.0; 2]; 3],
            indices: vec![0, 1, 2],
            ..vehicle::Mesh::default()
        };
        let material = |name: &str| vehicle::Material {
            name: name.into(),
            color: Some(vehicle::Image { width: 1, height: 1, rgba: vec![9; 4] }),
            metal_rough: None,
            normal: None,
            double_sided: false,
            alpha_mask: false,
            color_key: None,
            pbr_key: None,
        };
        let node = |name: &str, parent, pivot| vehicle::Node { name: name.into(), parent, pivot };
        // The strip follows its track node and the turret, the last vertex half and half
        let strip = vehicle::Mesh {
            bones: vec![2, 1],
            joints: vec![[0, 0, 0, 0], [1, 0, 0, 0], [0, 1, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.0, 0.0]],
            ..tri(2, true, 1)
        };
        Vehicle {
            name: "tank".into(),
            meshes: vec![tri(1, false, 0), strip],
            materials: vec![material("matR0_tank"), material("matR1_dynamic_pbr_glass")],
            nodes: vec![node("", None, [0.0; 3]), node("bone_turret", Some(0), [0.5, 2.0, 0.0]), node("track_l", Some(0), [0.0, 0.0, -1.0])],
        }
    }

    #[test]
    fn converts_axes_nodes_and_skins() {
        let m = to_model(vehicle());
        assert_eq!(m.validate(), Ok(()));
        assert_eq!(m.nodes[0].name, "@root");
        // Front (+X) becomes +Z, right (+Z) becomes -X
        assert_eq!(turn([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]);
        assert_eq!(turn([0.0, 0.0, 1.0]), [-1.0, 0.0, 0.0]);
        assert_eq!(m.nodes[1].translation, [0.0, 2.0, 0.5]);
        // Rigid vertices are relative to their node: (1, 2, 0) -> (0, 2, 1), minus the pivot (0, 2, 0.5)
        assert_eq!(m.meshes[0].positions[0], [0.0, 0.0, 0.5]);
        assert_eq!(m.meshes[0].normals[0], [0.0, 0.0, 1.0]);
        // Skinned vertices stay in model space and keep the game's bones and weights; each joint's
        // inverse bind undoes its place
        let skinned = &m.meshes[1];
        assert_eq!((skinned.skin, skinned.positions[0]), (Some(0), [0.0, 2.0, 1.0]));
        assert_eq!(skinned.joints[2], [0, 1, 0, 0]);
        assert_eq!(skinned.weights[2], [0.5, 0.5, 0.0, 0.0]);
        assert_eq!(m.skins[0].joints, vec![2, 1]);
        assert_eq!(m.skins[0].inverse_bind[0][12..15], [-1.0, 0.0, 0.0]);
        // The turret's pivot (0.5, 2, 0) is (0, 2, 0.5) in the bridge's axes
        assert_eq!(m.skins[0].inverse_bind[1][12..15], [0.0, -2.0, -0.5]);
        assert_eq!(m.nodes[2].translation, [1.0, 0.0, 0.0]);
        // Glass blends
        assert_eq!((m.materials[0].alpha, m.materials[1].alpha), (AlphaMode::Opaque, AlphaMode::Blend));
    }

    #[test]
    fn level_ids_name_level_files_only() {
        let wt = WarThunder::new("/nonexistent");
        assert!(wt.level_file("levels/../x.bin").is_err());
        assert!(wt.level_file("levels/sub/x.bin").is_err());
        assert!(wt.level_file("tanks/x.bin").is_err());
        assert!(wt.level_file("levels/x.grp").is_err());
        assert!(wt.level_file("levels/x.bin").unwrap_err().to_string().contains("no level"));
        assert!(wt.load("levels/avg_berlin.bin").unwrap_err().to_string().contains("load_scene"));
        assert!(wt.list_levels("").is_empty());
    }

    #[test]
    fn ids_name_vehicle_packs_only() {
        let wt = WarThunder::new("/nonexistent");
        assert!(wt.pack("../aces.vromfs.bin").is_err());
        assert!(wt.pack("tanks/../../x.grp").is_err());
        assert!(wt.pack("levels/x.grp").is_err());
        assert!(wt.pack("tanks/x.grp").unwrap_err().to_string().contains("no pack"));
        assert!(wt.load_vehicle("tanks/x.grp").unwrap_err().to_string().contains("expected pack.grp:model"));
    }
}
