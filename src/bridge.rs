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
//! Skinned meshes (tracks, cables) become skins with the game's own bones and weights: each vertex
//! follows up to four skeleton nodes, taken from the model's bone table (a track follows the `root` node
//! and the suspension wheel nodes `bone_wheel_<l|r>_<NN>`). A skin's inverse bind matrices undo the
//! joint's place, `translate(-pivot)`, since node frames carry no axes (the game's own matrices
//! include them).

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use rewrite_bridge::{AlphaMode, AssetInfo, AssetKind, BoxError, Image, Material, Mesh, Model, Node, Provider, Skin};
use wt_blk::dxp_and_grp::grp_pack::{class_id, GrpPack};

use crate::vehicle::{self, Library, Vehicle};

/// Folders of `content/base/res` that hold vehicles
pub const FOLDERS: [&str; 3] = ["tanks", "aircrafts", "ships"];

/// Damage and x-ray variants share their vehicle's pack but are not vehicles of their own
const VARIANTS: [&str; 3] = ["_dmg", "_xray", "_dm"];

pub struct WarThunder {
    game: PathBuf,
    library: OnceLock<Library>,
}

impl WarThunder {
    /// `game` is the installed game's folder, the one holding `content` and `aces.vromfs.bin`
    pub fn new(game: impl Into<PathBuf>) -> Self {
        Self { game: game.into(), library: OnceLock::new() }
    }

    /// The texture index and material descriptions, gathered on first use
    fn library(&self) -> Result<&Library> {
        if let Some(l) = self.library.get() {
            return Ok(l);
        }
        let l = Library::open(&self.game)?;
        Ok(self.library.get_or_init(|| l))
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

    fn load_vehicle(&self, id: &str) -> Result<Model> {
        let (rel, name) = id.split_once(':').with_context(|| format!("{id}: expected pack.grp:model"))?;
        let v = self.library()?.load(&self.pack(rel)?, name, 0)?;
        let model = to_model(v);
        model.validate()?;
        Ok(model)
    }
}

impl Provider for WarThunder {
    fn game(&self) -> &str {
        "War Thunder"
    }

    fn list(&self, filter: &str) -> Result<Vec<AssetInfo>, BoxError> {
        Ok(self.list_vehicles(filter)?)
    }

    fn load(&self, id: &str) -> Result<Model, BoxError> {
        Ok(self.load_vehicle(id)?)
    }
}

/// From [`vehicle`]'s space (front +X, right +Z) to the bridge's (front +Z, left +X)
fn turn(p: [f32; 3]) -> [f32; 3] {
    [-p[2], p[1], p[0]]
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
    fn ids_name_vehicle_packs_only() {
        let wt = WarThunder::new("/nonexistent");
        assert!(wt.pack("../aces.vromfs.bin").is_err());
        assert!(wt.pack("tanks/../../x.grp").is_err());
        assert!(wt.pack("levels/x.grp").is_err());
        assert!(wt.pack("tanks/x.grp").unwrap_err().to_string().contains("no pack"));
        assert!(wt.load_vehicle("tanks/x.grp").unwrap_err().to_string().contains("expected pack.grp:model"));
    }
}
