//! Assembles a vehicle from the game files: meshes placed by their skeleton, materials with decoded
//! textures (camouflage and occlusion baked in, Dagor's packed normal split into PBR maps).
//!
//! The result is in a right-handed, Y-up space in metres (glTF and Bevy convention), independent of
//! any output format.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use wt_blk::dxp_and_grp::{
    dynmodel::{DynModel, Elem, Lod},
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

pub struct Material {
    pub name: String,
    /// Base colour, with camouflage and occlusion baked in where the material has them
    pub color: Option<Image>,
    /// glTF-style metallic-roughness map: G roughness, B metalness
    pub metal_rough: Option<Image>,
    /// Tangent-space normal map, OpenGL convention (green up)
    pub normal: Option<Image>,
    pub double_sided: bool,
    /// Alpha-tested (cut-out) material
    pub alpha_mask: bool,
    /// Stable names for the textures, derived from the game's texture names
    pub color_key: Option<String>,
    pub pbr_key: Option<String>,
}

#[derive(Default)]
pub struct Mesh {
    pub material: usize,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

pub struct Vehicle {
    pub name: String,
    /// One mesh per material
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
}

/// Texture name (without anything from the `*` on) -> pack holding a non-empty copy of it
pub fn texture_index(game: &Path) -> Result<HashMap<String, PathBuf>> {
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
            index.insert(key, path.clone());
        }
    }
    Ok(index)
}

/// Decodes textures by name, preferring the high-quality copy, caching opened packs
struct TextureSource<'a> {
    index: &'a HashMap<String, PathBuf>,
    packs: HashMap<PathBuf, Vec<u8>>,
}

impl TextureSource<'_> {
    fn decode(&mut self, tex: &str) -> Result<Option<Image>> {
        let Some((full, path)) = [format!("{tex}$hq"), tex.to_string()]
            .into_iter()
            .find_map(|n| self.index.get(&n).map(|p| (n, p.clone())))
        else {
            return Ok(None);
        };
        if !self.packs.contains_key(&path) {
            self.packs.insert(path.clone(), fs::read(&path)?);
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
}

impl MatInfo {
    /// Name of the baked colour texture, when the material needs baking
    fn bake_key(&self) -> Option<String> {
        let c = self.color.as_ref()?;
        (self.camo.is_some() || self.occlusion.is_some()).then(|| {
            format!("{c}__{}__{}", self.camo.as_deref().unwrap_or("nocamo"), self.occlusion.as_deref().unwrap_or("noao"))
        })
    }
}

/// Materials and texture names of a model, from dynModelDesc.bin (patched copy first)
fn material_infos(game: &Path, name: &str) -> Result<Vec<MatInfo>> {
    let mut desc = None;
    for rel in ["patch/content/base/res/dynModelDesc.bin", "content/base/res/dynModelDesc.bin"] {
        let Ok(raw) = fs::read(game.join(rel)) else { continue };
        let root = blk::parse(&raw, None).with_context(|| format!("parsing {rel}"))?;
        if let Some(b) = root.blocks.into_iter().find(|b| b.name == name) {
            desc = Some(b);
            break;
        }
    }
    let desc = desc.with_context(|| format!("{name} not found in dynModelDesc.bin"))?;
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
            infos.push(MatInfo {
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

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 { v.map(|x| x / len) } else { [0.0, 1.0, 0.0] }
}

/// Dagor is left-handed with Y up; mirror Z to get a right-handed space (winding is flipped too)
fn right_handed(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[1], -p[2]]
}

/// Builds one mesh per material. `skin_material_base` is where matS materials start.
fn build_meshes(
    model: &DynModel,
    lod: &Lod,
    nodes: &HashMap<String, [f32; 16]>,
    skin_material_base: usize,
) -> Vec<Mesh> {
    const IDENTITY: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    let mut meshes: HashMap<usize, Mesh> = HashMap::new();
    let mut missing_nodes = Vec::new();
    // Rigid parts are placed by their skeleton node; skinned meshes are stored in bind pose, in model space
    let parts = lod
        .rigids
        .iter()
        .map(|r| (r.node.as_str(), &r.elems, &model.vdata, 0, true))
        .chain(lod.skins.iter().map(|s| (s.node.as_str(), &s.elems, &model.skin_vdata, skin_material_base, false)));
    for (node, elems, vdata, material_base, placed) in parts {
        let tm = if !placed {
            &IDENTITY
        } else {
            // "@root" is the model origin
            nodes.get(node).unwrap_or_else(|| {
                if node != "@root" {
                    missing_nodes.push(node);
                }
                &IDENTITY
            })
        };
        for e in elems {
            let mesh = meshes.entry(material_base + e.material).or_default();
            mesh.material = material_base + e.material;
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
    out.sort_by_key(|m| m.material);
    out
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

pub fn load(game: &Path, pack: &Path, name: &str, lod: usize) -> Result<Vehicle> {
    let file = fs::read(pack).with_context(|| format!("reading {}", pack.display()))?;
    let grp = GrpPack::parse(&file)?;
    let find = |res: &str, class: u32| {
        grp.find(res, class).map(|r| grp.data(r)).with_context(|| format!("{res} not found in {}", pack.display()))
    };
    let model = DynModel::parse(find(name, class_id::DYN_MODEL)?)?;
    let tree = GeomNodeTree::parse(find(&format!("{name}_skeleton"), class_id::GEOM_NODE_TREE)?)?;
    let nodes: HashMap<String, [f32; 16]> = tree.nodes.into_iter().map(|n| (n.name, n.wtm)).collect();
    let lod = model.lods.get(lod).context("no such LOD")?;
    let infos = material_infos(game, name)?;
    let skin_material_base = infos.iter().position(|m| m.skinned).unwrap_or(infos.len());

    // UV triangles of every material that needs baking, grouped by texture combination
    let mut bake_tris: HashMap<String, Vec<camo::UvTriangle>> = HashMap::new();
    for info in &infos {
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

    let index = texture_index(game)?;
    let mut source = TextureSource { index: &index, packs: HashMap::new() };
    let mut materials = Vec::with_capacity(infos.len());
    for info in &infos {
        let bake_key = info.bake_key();
        let mut color = source.decode_or_warn(&info.color);
        if let (Some(img), Some(key)) = (color.as_mut(), &bake_key) {
            let camo_img = source.decode_or_warn(&info.camo);
            let ao_img = source.decode_or_warn(&info.occlusion);
            // Alpha-tested materials keep their alpha; masked ones use it as the paint mask
            let opaque = !info.class.contains("atest");
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
            alpha_mask: info.class.contains("atest"),
        });
    }

    Ok(Vehicle { name: name.to_string(), meshes: build_meshes(&model, lod, &nodes, skin_material_base), materials })
}
