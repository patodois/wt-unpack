//! The collision of a level's placed objects as bridge models.
//!
//! Every placed object (rendinst) has a collision resource in the game's packs, `<name>_collision` (see
//! [`crate::collision`]). [`LevelCollision`] gathers those of a level's objects, in the same space as the
//! objects' render models in the level's [`rewrite_bridge::Scene`]: the instance's transform that places a
//! scene model places its collision too, so a consumer collides with `collision.object(&model.name)` at each
//! `Instance` of that model.
//!
//! `Scene` has no collision field (at the bridge revision this crate uses), so the collision comes
//! separately, from `WarThunder::load_level_collision`. The ground has no entry here: the scene's `terrain`
//! model already is the landmesh's collision mesh (and the heightmap's grid where it is finer).
//!
//! What is in an [`ObjectCollision`]:
//!
//! - `model`: all triangle shapes in one model: the triangle meshes and convex hulls the resource stores
//!   (their triangles as authored) and the boxes (12 triangles from the node's box). Each mesh is named after
//!   its collision node and uses a material named after the node's physical material(s) (`concrete`,
//!   `wood`..., or `none`), which is how a game rewrite can pick friction or sounds; nothing else about the
//!   material is meaningful.
//! - `capsules`: the capsule shapes (tree trunks are capsules), which a triangle model cannot hold.
//!
//! Not carried: point nodes (no geometry), the 8 sphere nodes of the whole game, the resource's per-node
//! bullet-trace flags beyond `behavior`, and which faces are edges (the edge-flag words of the BVH).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use rewrite_bridge::{Material, Mesh, Model, Node};
use wt_blk::dxp_and_grp::grp_pack::GrpPack;

use crate::collision::{Collision, CollisionNode, NodeKind, COLLISION};
use crate::level::Level;
use crate::rendinst::ResourceIndex;
use crate::vehicle;

/// A capsule shape, in the object's space (bridge axes)
#[derive(Debug, Clone, PartialEq)]
pub struct CapsuleShape {
    /// The collision node's name
    pub name: String,
    pub a: [f32; 3],
    pub b: [f32; 3],
    /// m
    pub radius: f32,
    pub materials: Vec<String>,
    /// The node's behaviour bits, see [`CollisionNode::behavior`]
    pub behavior: u16,
}

/// The collision of one placed object, in its own space with the bridge's axes (the space of the object's
/// model in the scene)
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectCollision {
    /// The object's name: the name of its model in the scene
    pub name: String,
    /// The resource it came from, `<name>_collision`
    pub resource: String,
    /// The triangle shapes (`None` when the object has only capsules or points)
    pub model: Option<Model>,
    pub capsules: Vec<CapsuleShape>,
    /// Bounding box of the whole resource, `[min, max]`, in the same axes
    pub bbox: [[f32; 3]; 2],
}

impl ObjectCollision {
    /// Triangles of `model`
    pub fn triangles(&self) -> usize {
        self.model.as_ref().map_or(0, |m| m.meshes.iter().map(|x| x.indices.len() / 3).sum())
    }
}

/// The collision of every distinct object a level places
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LevelCollision {
    pub level: String,
    pub objects: Vec<ObjectCollision>,
    /// Placed objects with no collision resource in any pack
    pub without_collision: Vec<String>,
    /// Collision resources that failed to parse, with the reason
    pub failed: Vec<(String, String)>,
}

impl LevelCollision {
    /// The collision of a placed object by its model's name in the scene
    pub fn object(&self, name: &str) -> Option<&ObjectCollision> {
        self.objects.iter().find(|o| o.name == name)
    }

    pub fn triangles(&self) -> usize {
        self.objects.iter().map(ObjectCollision::triangles).sum()
    }
}

/// Dagor's `(x, y, z)` to the bridge's `(z, y, x)`
fn turn(p: [f32; 3]) -> [f32; 3] {
    [p[2], p[1], p[0]]
}

fn det(tm: &[[f32; 3]; 4]) -> f32 {
    let [a, b, c] = [tm[0], tm[1], tm[2]];
    a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])
}

/// The 12 triangles of a node's box (Dagor winding: outward, counter-clockwise in the game's own reading)
fn box_geometry(n: &CollisionNode) -> (Vec<[f32; 3]>, Vec<u32>) {
    let [lo, hi] = n.bbox;
    let corner = |i: usize| [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }];
    let positions: Vec<[f32; 3]> = (0..8).map(corner).collect();
    // Faces as quads of corner indices, counter-clockwise seen from outside for the game's axes
    let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    let mut indices = Vec::with_capacity(36);
    for q in quads {
        indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
    }
    (positions, indices)
}

/// Converts a collision resource to the bridge's axes and shapes
pub fn object_collision(name: &str, c: &Collision) -> ObjectCollision {
    let mut materials: Vec<Material> = Vec::new();
    let mut material_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut meshes = Vec::new();
    let mut capsules = Vec::new();
    for n in &c.nodes {
        let scale = det(&n.tm).abs().cbrt();
        match n.kind {
            NodeKind::Mesh | NodeKind::Convex | NodeKind::Box => {
                let (positions, indices) = if n.kind == NodeKind::Box { box_geometry(n) } else { (n.positions.clone(), n.indices.clone()) };
                if indices.is_empty() {
                    continue;
                }
                let label = if n.materials.is_empty() { "none".to_string() } else { n.materials.join("+") };
                let material = *material_of.entry(label.clone()).or_insert_with(|| {
                    materials.push(Material { name: label, metallic: 0.0, roughness: 1.0, double_sided: true, ..Material::default() });
                    materials.len() - 1
                });
                let mut mesh = Mesh { name: n.name.clone(), node: 0, material, ..Mesh::default() };
                mesh.positions = positions.iter().map(|&p| turn(n.to_object(p))).collect();
                // The swap of x and z is a mirror, which turns every triangle over; a node transform that
                // mirrors turns it back
                let flip = det(&n.tm) >= 0.0;
                for t in indices.chunks_exact(3) {
                    if flip {
                        mesh.indices.extend_from_slice(&[t[0], t[2], t[1]]);
                    } else {
                        mesh.indices.extend_from_slice(t);
                    }
                }
                meshes.push(mesh);
            }
            NodeKind::Capsule => {
                if let Some(cap) = n.capsule {
                    capsules.push(CapsuleShape {
                        name: n.name.clone(),
                        a: turn(n.to_object(cap.a)),
                        b: turn(n.to_object(cap.b)),
                        radius: cap.radius * scale,
                        materials: n.materials.clone(),
                        behavior: n.behavior,
                    });
                }
            }
            NodeKind::Points | NodeKind::Sphere => {}
        }
    }
    let model = (!meshes.is_empty()).then(|| Model { name: format!("{name}_collision"), nodes: vec![Node::new(name, None)], meshes, materials, skins: Vec::new() });
    ObjectCollision { name: name.to_string(), resource: c.name.clone(), model, capsules, bbox: [turn(c.bbox[0]), turn(c.bbox[1])] }
}

/// Loads the collision of one object from the pack that holds it
pub fn load_object(index: &ResourceIndex, name: &str) -> Result<Option<ObjectCollision>> {
    let resource = format!("{name}_collision");
    let Some(path) = index.find(COLLISION, &resource) else { return Ok(None) };
    let file = vehicle::map(path)?;
    let pack = GrpPack::parse(&file).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
    let res = pack.find(&resource, COLLISION).with_context(|| format!("{resource} vanished from {}", path.display()))?;
    let c = Collision::parse(&resource, pack.data(res)).with_context(|| format!("parsing {resource}"))?;
    Ok(Some(object_collision(name, &c)))
}

/// Loads the collision of every object a level places
pub fn load_level_collision(level_file: &Path, index: &ResourceIndex) -> Result<LevelCollision> {
    let name = level_file.file_stem().map_or("level".to_string(), |s| s.to_string_lossy().into_owned());
    let file = vehicle::map(level_file)?;
    let level = Level::parse(&file).with_context(|| format!("parsing {}", level_file.display()))?;
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for layer in level.rendinst_layers()? {
        for (pool, n) in layer.pools.iter().zip(layer.counts()) {
            if n > 0 {
                *names.entry(pool.name.clone()).or_default() += n;
            }
        }
    }
    let mut out = LevelCollision { level: name, ..LevelCollision::default() };
    // Pack by pack would map each file once; the objects are few hundred, so ask per object
    for object in names.keys() {
        match load_object(index, object) {
            Ok(Some(o)) => out.objects.push(o),
            Ok(None) => out.without_collision.push(object.clone()),
            Err(e) => out.failed.push((object.clone(), format!("{e:#}"))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::Capsule;

    fn node(kind: NodeKind, tm: [[f32; 3]; 4]) -> CollisionNode {
        CollisionNode {
            name: "n".into(),
            kind,
            materials: vec!["concrete".into()],
            behavior: 0x1b,
            tm,
            bbox: [[0.0, 0.0, 0.0], [2.0, 4.0, 6.0]],
            positions: Vec::new(),
            indices: Vec::new(),
            capsule: None,
        }
    }

    const IDENTITY: [[f32; 3]; 4] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0]];

    fn volume(m: &Mesh) -> f32 {
        let mut v = 0.0;
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
            v += a[0] * (b[1] * c[2] - b[2] * c[1]) + a[1] * (b[2] * c[0] - b[0] * c[2]) + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
        v / 6.0
    }

    #[test]
    fn boxes_become_closed_outward_meshes_in_bridge_axes() {
        let c = Collision { name: "b_collision".into(), bbox: [[0.0; 3], [2.0, 4.0, 6.0]], nodes: vec![node(NodeKind::Box, IDENTITY)] };
        let o = object_collision("b", &c);
        let m = o.model.unwrap();
        m.validate().unwrap();
        assert_eq!((m.meshes[0].positions.len(), m.meshes[0].indices.len()), (8, 36));
        // Dagor's x (2 m) becomes the bridge's z; a closed outward box has volume 2 * 4 * 6, counter-clockwise
        assert!((volume(&m.meshes[0]) - 48.0).abs() < 1e-4, "{}", volume(&m.meshes[0]));
        assert!(m.meshes[0].positions.iter().all(|p| (0.0..=6.0).contains(&p[0]) && (0.0..=2.0).contains(&p[2])));
        assert_eq!(m.materials[0].name, "concrete");
        assert_eq!(o.bbox, [[0.0; 3], [6.0, 4.0, 2.0]]);
    }

    #[test]
    fn mirroring_transforms_keep_triangles_facing_out() {
        // The same box under a transform that mirrors x
        let mirrored = [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0]];
        let c = Collision { name: "b_collision".into(), bbox: [[0.0; 3]; 2], nodes: vec![node(NodeKind::Box, mirrored)] };
        let m = object_collision("b", &c).model.unwrap();
        assert!((volume(&m.meshes[0]) - 48.0).abs() < 1e-4, "{}", volume(&m.meshes[0]));
    }

    #[test]
    fn capsules_are_placed_and_scaled() {
        let mut n = node(NodeKind::Capsule, [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0], [1.0, 0.0, 0.0]]);
        n.capsule = Some(Capsule { a: [0.0, 0.0, 0.0], b: [0.0, 5.0, 0.0], radius: 0.5 });
        let c = Collision { name: "t_collision".into(), bbox: [[0.0; 3]; 2], nodes: vec![n, node(NodeKind::Points, IDENTITY)] };
        let o = object_collision("t", &c);
        assert!(o.model.is_none());
        let cap = &o.capsules[0];
        assert_eq!((cap.a, cap.b, cap.radius), ([0.0, 0.0, 1.0], [0.0, 10.0, 1.0], 1.0));
    }
}
