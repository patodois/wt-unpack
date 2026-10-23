//! Collision resources: the shapes the game collides and traces against, one per placed object (class
//! `ACE50000`, named `<rendinst>_collision` in the `.grp` packs, e.g. `berlin_city_build_1_collision`).
//!
//! Layout of version 3 (the only one in the game's packs), from `collisionGameResLoad.cpp` and
//! `collResStream.h`:
//!
//! ```text
//! u32 label            0xACE5_0003 (the class id and the version)
//! u32 block header     bits 30..31: codec (1 zstd, 2 Oodle), bits 0..29: size of the packed body
//! body                 (Oodle: u32 unpacked size first)
//!   header (108 bytes)  f32 bounding sphere centre (3) + r^2; f32 bind-trace sphere (4); f32 bbox[6];
//!                       f32 sphere radius; u32 flags; u32 counts[11] (elements of each array below)
//!   N x bind record (8 bytes: f32 max scale, u8 flags, u8 status, u16 behaviour)
//!   arrays in this order, with element sizes (N = counts[0]):
//!     0 nodes      N x 48   u32 name offset, u32 index count (faces x 3), u16 behaviour flags,
//!                           i16 material (>= 0 an index into the material names; -1 none; <= -2 the
//!                           slice -id - 2 of the material pool), u16 inside-of node, u16 capsule or
//!                           plane offset, u16 plane count, u8 flags, u8 type, f32 bbox[6] (node-local),
//!                           f32 radius around the box centre (< 0: no vertices)
//!     1 tm         N x 48   the node's authored transform: three axis columns, then the position
//!     2 inverse tm 0 or N x 48
//!     3 relative   0 or N x 48
//!     4 chunks     one per node with indices, in node order: f32 scale[3], invScale[3], bmin[3] (36
//!                           bytes), then a `dBLi` quad BLAS (see `level::read_quad_blas`) whose vertices
//!                           are 21-bit coordinates in 1/32 units: position = bmin + q / 32 * invScale
//!     5 (not stored)
//!     6 material pool  u16: slices of [count, ids...]
//!     7 capsules   28 each  f32 a[3], f32 radius, f32 b[3]
//!     8 planes     16 each  f32 normal[3], f32 distance
//!     9 names      bytes, NUL-terminated strings
//!     10 material names  bytes, NUL-terminated strings
//!   tail records (u32 tag, u32 size, payload), skipped
//! ```
//!
//! Space: Dagor's, the placed object's own (left-handed, Y up, metres). A node's vertices are in the node's
//! space; `tm` takes them to the object's.

use anyhow::{bail, ensure, Context, Result};

use crate::bytes::Reader;
use crate::level::{read_quad_blas, unpack_sized};

/// The class id of collision resources
pub const COLLISION: u32 = 0xACE5_0000;

const VERSION: u32 = 3;
const ARRAYS: usize = 11;

/// What a node is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A triangle mesh
    Mesh,
    /// A set of points (no geometry here)
    Points,
    /// A box: the node's bounding box
    Box,
    /// A sphere inscribed in the node's bounding box
    Sphere,
    /// A capsule
    Capsule,
    /// A convex hull, as triangles
    Convex,
}

impl NodeKind {
    fn of(t: u8) -> Result<Self> {
        Ok(match t {
            0 => Self::Mesh,
            1 => Self::Points,
            2 => Self::Box,
            3 => Self::Sphere,
            4 => Self::Capsule,
            5 => Self::Convex,
            other => bail!("collision node type {other}"),
        })
    }
}

/// A capsule: the segment `a`..`b` swept by `radius`, in the node's space
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capsule {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub radius: f32,
}

/// One shape of a collision resource
#[derive(Debug, Clone, PartialEq)]
pub struct CollisionNode {
    /// The node's name in the resource ("" when it has none)
    pub name: String,
    pub kind: NodeKind,
    /// Physical materials of the node (`Plane`, `concrete`, `wood`..., names the game's physmat tables
    /// define), one when the node has one, several when it has a set (per-face sets are not stored), none when it has none
    pub materials: Vec<String>,
    /// The game's `BehaviorFlag` bits: 1 traceable (bullets), 2 physics-collidable, 4 solid (trace without
    /// culling), 8 allow hole, 16 damage required, 32 cut required, 64 check side, 128 allow bullet decal
    pub behavior: u16,
    /// The node's authored transform to the object's space: the x, y and z axis columns, then the position
    pub tm: [[f32; 3]; 4],
    /// The node's bounding box in its own space: `[min, max]`
    pub bbox: [[f32; 3]; 2],
    /// Triangle mesh and convex nodes: vertices in the node's space, and the triangles' corners (indices
    /// into `positions`, three per triangle)
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// The capsule of a capsule node
    pub capsule: Option<Capsule>,
}

impl CollisionNode {
    /// Whether bullets hit it (`behavior` bit 0)
    pub fn traceable(&self) -> bool {
        self.behavior & 1 != 0
    }

    /// Whether vehicles collide with it (`behavior` bit 1)
    pub fn physics_collidable(&self) -> bool {
        self.behavior & 2 != 0
    }

    /// A point of the node's space in the object's space
    pub fn to_object(&self, p: [f32; 3]) -> [f32; 3] {
        [0, 1, 2].map(|k| self.tm[0][k] * p[0] + self.tm[1][k] * p[1] + self.tm[2][k] * p[2] + self.tm[3][k])
    }
}

/// A placed object's collision: its shapes
#[derive(Debug, Clone, PartialEq)]
pub struct Collision {
    /// The resource's name, e.g. `berlin_city_build_1_collision`
    pub name: String,
    /// Bounding box of the whole resource, `[min, max]`
    pub bbox: [[f32; 3]; 2],
    pub nodes: Vec<CollisionNode>,
}

impl Collision {
    /// Triangles in all nodes
    pub fn triangles(&self) -> usize {
        self.nodes.iter().map(|n| n.indices.len() / 3).sum()
    }

    /// Parses the data of a collision resource of the given name
    pub fn parse(name: &str, res: &[u8]) -> Result<Self> {
        let mut r = Reader::new(res);
        let label = r.u32()?;
        ensure!(label & 0xFFFF_0000 == COLLISION, "not a collision resource (label {label:#010x})");
        ensure!(label & 0xFFFF == VERSION, "collision resource version {} is not supported", label & 0xFFFF);
        let head = r.u32()?;
        let (codec, size) = ((head >> 30) as u8, (head & 0x3FFF_FFFF) as usize);
        let packed = r.bytes(size)?;
        let body = if codec == 0 { packed.to_vec() } else { unpack_sized(codec, packed).context("collision body")? };
        Self::parse_body(name, &body)
    }

    fn parse_body(name: &str, body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body);
        r.skip(32)?;
        let bbox = [[r.f32()?, r.f32()?, r.f32()?], [r.f32()?, r.f32()?, r.f32()?]];
        r.skip(8)?;
        let mut counts = [0usize; ARRAYS];
        for c in &mut counts {
            *c = r.u32()? as usize;
        }
        let n = counts[0];
        ensure!(n <= 4096, "{n} collision nodes");
        ensure!(counts[5] == 0, "stored top-level tree");
        r.skip(n * 8)?;

        struct Rec {
            name_ofs: usize,
            indices: usize,
            behavior: u16,
            material: i16,
            planes_at: usize,
            kind: NodeKind,
            bbox: [[f32; 3]; 2],
        }
        let mut recs = Vec::with_capacity(n);
        for _ in 0..n {
            let name_ofs = r.u32()? as usize;
            let indices = r.u32()? as usize;
            let behavior = r.u16()?;
            let material = r.i16()?;
            r.skip(2)?; // inside-of node
            let planes_at = usize::from(r.u16()?);
            r.skip(2)?; // plane count
            r.skip(1)?; // flags
            let kind = NodeKind::of(r.u8()?)?;
            let bbox = [[r.f32()?, r.f32()?, r.f32()?], [r.f32()?, r.f32()?, r.f32()?]];
            r.skip(4)?;
            ensure!(indices.is_multiple_of(3) && indices <= 0x400_0000, "node with {indices} indices");
            recs.push(Rec { name_ofs, indices, behavior, material, planes_at, kind, bbox });
        }

        let mut tms = Vec::with_capacity(r.capacity_for(counts[1], 48));
        for _ in 0..counts[1] {
            tms.push([[r.f32()?, r.f32()?, r.f32()?], [r.f32()?, r.f32()?, r.f32()?], [r.f32()?, r.f32()?, r.f32()?], [r.f32()?, r.f32()?, r.f32()?]]);
        }
        ensure!(tms.len() == n, "{} transforms for {n} nodes", tms.len());
        r.skip(counts[2] * 48)?;
        r.skip(counts[3] * 48)?;

        // The chunks, in node order
        struct Chunk {
            bmin: [f32; 3],
            inv_scale: [f32; 3],
            verts: Vec<u64>,
            indices: Vec<u32>,
        }
        let mut chunks: Vec<Option<Chunk>> = Vec::with_capacity(n);
        for (i, rec) in recs.iter().enumerate() {
            if rec.indices == 0 {
                chunks.push(None);
                continue;
            }
            r.skip(12).with_context(|| format!("node {i}"))?; // scale
            let inv_scale = [r.f32()?, r.f32()?, r.f32()?];
            let bmin = [r.f32()?, r.f32()?, r.f32()?];
            let blas = read_quad_blas(&mut r).with_context(|| format!("node {i}"))?;
            ensure!(blas.indices.len() == rec.indices, "node {i}: {} indices, the record says {}", blas.indices.len(), rec.indices);
            chunks.push(Some(Chunk { bmin, inv_scale, verts: blas.verts, indices: blas.indices }));
        }
        let mut pool = Vec::with_capacity(r.capacity_for(counts[6], 2));
        for _ in 0..counts[6] {
            pool.push(r.u16()?);
        }
        let mut capsules = Vec::with_capacity(r.capacity_for(counts[7], 28));
        for _ in 0..counts[7] {
            let a = [r.f32()?, r.f32()?, r.f32()?];
            let radius = r.f32()?;
            capsules.push(Capsule { a, radius, b: [r.f32()?, r.f32()?, r.f32()?] });
        }
        r.skip(counts[8] * 16)?;
        let names = r.bytes(counts[9])?;
        let material_names = r.bytes(counts[10])?;

        let cstr = |bytes: &[u8], at: usize| -> String {
            let tail = bytes.get(at..).unwrap_or_default();
            String::from_utf8_lossy(tail.split(|&b| b == 0).next().unwrap_or_default()).into_owned()
        };
        // Material names by index: the strings of the table in order
        let table: Vec<String> = material_names.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        let material = |id: i16| -> Vec<String> {
            let get = |i: usize| table.get(i).cloned();
            match id {
                0.. => get(id as usize).into_iter().collect(),
                -1 => Vec::new(),
                _ => {
                    let at = (-i32::from(id) - 2) as usize;
                    let count = pool.get(at).copied().map_or(0, usize::from);
                    (1..=count).filter_map(|k| pool.get(at + k)).filter_map(|&m| get(usize::from(m))).collect()
                }
            }
        };

        let mut nodes = Vec::with_capacity(n);
        for (i, (rec, chunk)) in recs.iter().zip(chunks).enumerate() {
            let (positions, indices) = match chunk {
                Some(c) => (
                    c.verts
                        .iter()
                        .map(|v| {
                            let q = [v & 0x1F_FFFF, (v >> 21) & 0x1F_FFFF, (v >> 42) & 0x1F_FFFF];
                            [0, 1, 2].map(|k| c.bmin[k] + q[k] as f32 / 32.0 * c.inv_scale[k])
                        })
                        .collect(),
                    c.indices,
                ),
                None => (Vec::new(), Vec::new()),
            };
            let capsule = (rec.kind == NodeKind::Capsule).then(|| capsules.get(rec.planes_at).copied()).flatten();
            nodes.push(CollisionNode {
                name: cstr(names, rec.name_ofs),
                kind: rec.kind,
                materials: material(rec.material),
                behavior: rec.behavior,
                tm: tms[i],
                bbox: rec.bbox,
                positions,
                indices,
                capsule,
            });
        }
        Ok(Collision { name: name.to_string(), bbox, nodes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_an_error() {
        assert!(Collision::parse("x", &[]).is_err());
        assert!(Collision::parse("x", &[0xFF; 64]).is_err());
        // The right label, an unsupported version
        assert!(Collision::parse("x", &[2, 0, 0xE5, 0xAC, 0, 0, 0, 0]).unwrap_err().to_string().contains("version 2"));
    }

    /// A body with no nodes whose array counts are as given
    fn body_with_counts(counts: [u32; ARRAYS]) -> Vec<u8> {
        let mut body = vec![0u8; 32 + 24 + 8];
        for c in counts {
            body.extend_from_slice(&c.to_le_bytes());
        }
        body
    }

    #[test]
    fn array_counts_the_body_cannot_hold_reserve_nothing() {
        // Transforms (48 bytes each), the u16 pool and the capsules (28 bytes each), one at a time at a count
        // that would take gigabytes
        for (array, count) in [(1, 0x1000_0000u32), (6, 0x4000_0000), (7, 0x1000_0000)] {
            let mut counts = [0u32; ARRAYS];
            counts[array] = count;
            let (res, largest) = crate::alloc_probe::largest_allocation(|| Collision::parse_body("x", &body_with_counts(counts)));
            assert!(res.is_err(), "array {array}");
            assert!(largest < 1 << 20, "array {array} of {count}: asked for {largest} bytes");
        }
        // No arrays at all is an empty, valid resource
        assert!(Collision::parse_body("x", &body_with_counts([0; ARRAYS])).unwrap().nodes.is_empty());
    }

    #[test]
    fn nodes_place_their_points_with_the_transform() {
        let node = CollisionNode {
            name: String::new(),
            kind: NodeKind::Mesh,
            materials: Vec::new(),
            behavior: 3,
            // Axes: x -> y, y -> -x, z -> z; position (10, 20, 30)
            tm: [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [10.0, 20.0, 30.0]],
            bbox: [[0.0; 3]; 2],
            positions: Vec::new(),
            indices: Vec::new(),
            capsule: None,
        };
        assert_eq!(node.to_object([1.0, 2.0, 3.0]), [10.0 - 2.0, 20.0 + 1.0, 33.0]);
        assert!(node.traceable() && node.physics_collidable());
    }
}
