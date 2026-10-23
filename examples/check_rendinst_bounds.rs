//! Looks for placed-object vertices far from their model's origin (or outside its bounding box).
//!
//! `check_rendinst_bounds <game dir> [far metres, default 10000]`
//!
//! Decodes, one model at a time, every vertex the elements of every LOD of every `RendInst` resource
//! in the game's packs use: facing-leaves elements as [`LeafVertex`] and the rest with the buffer's
//! declaration, as `scene` does. With `NAIVE=1` it decodes everything with the declaration, which is
//! what the reader did before facing leaves were told apart. Prints each model with a vertex beyond the
//! far distance, then a summary by material class.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use wt_blk::dxp_and_grp::grp_pack::GrpPack;
use wt_unpack::blk::{self, Value};
use wt_unpack::rendinst::{is_facing_leaves, ResourceIndex, RiModel, REND_INST};

/// Material classes of every model, in material order, from riDesc.bin (patched copy first)
fn classes(game: &std::path::Path) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for rel in ["patch/content/base/res/riDesc.bin", "content/base/res/riDesc.bin"] {
        let Ok(raw) = fs::read(game.join(rel)) else { continue };
        let root = blk::parse(&raw, None).unwrap();
        drop(raw);
        for b in root.blocks {
            let list = b
                .blocks
                .iter()
                .find(|x| x.name == "mat")
                .map(|m| m.blocks.iter().map(|x| x.params.iter().find_map(|(n, v)| if n == "cls" { if let Value::Str(s) = v { Some(s.clone()) } else { None } } else { None }).unwrap_or_default()).collect())
                .unwrap_or_default();
            out.entry(b.name).or_insert(list);
        }
    }
    out
}

/// What the facing-leaves decode is checked against: the model's own `ImpostorData`
#[derive(Default)]
struct LeafStats {
    models: usize,
    elems: usize,
    vertices: usize,
    strides: BTreeMap<usize, usize>,
    /// Vertices whose uv leaves 0..1, and whose normal is not a unit vector
    uv_outside: usize,
    normal_off: usize,
    /// Models whose LOD 0 leaf vertices are exactly the file's shadow points (same centre and corner, to half precision)
    shadow_matched: usize,
    shadow_mismatched: Vec<String>,
    no_shadow: Vec<String>,
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.0011 * b.abs() + 2e-4
}

#[derive(Default)]
struct ClassStats {
    elems: usize,
    vertices: usize,
    far_elems: usize,
    far_vertices: usize,
    outside_elems: usize,
    outside_vertices: usize,
    worst: f32,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let game = PathBuf::from(&args[1]);
    let far: f32 = args.get(2).map_or(10_000.0, |s| s.parse().unwrap());
    let naive = std::env::var_os("NAIVE").is_some();
    let index = ResourceIndex::build(&game, &[REND_INST]).unwrap();
    let classes = classes(&game);
    let mut packs = Vec::new();
    for root in ["content/base/res", "patch/content/base/res"] {
        if let Ok(rd) = fs::read_dir(game.join(root)) {
            packs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "grp")));
        }
    }
    packs.sort();
    let mut stats: BTreeMap<String, ClassStats> = BTreeMap::new();
    let (mut models, mut failed, mut no_desc) = (0, 0, 0);
    let mut offenders = Vec::new();
    let mut leaf = LeafStats::default();
    let mut seen = HashSet::new();
    for path in packs {
        let file = wt_unpack::vehicle::map(&path).unwrap();
        let Ok(grp) = GrpPack::parse(&file) else { continue };
        for res in grp.resources.iter().filter(|r| r.class_id == REND_INST) {
            if index.find(REND_INST, &res.name) != Some(path.as_path()) || !seen.insert(res.name.clone()) {
                continue;
            }
            let m = match RiModel::parse(grp.data(res)) {
                Ok(m) => m,
                Err(e) => {
                    failed += 1;
                    println!("FAILED {}: {e:#}", res.name);
                    continue;
                }
            };
            models += 1;
            let Some(cls) = classes.get(&res.name) else {
                no_desc += 1;
                continue;
            };
            let size = (0..3).map(|k| m.bbox[1][k] - m.bbox[0][k]).fold(0.0f32, f32::max);
            let margin = 0.01 * size + 0.05;
            let mut model_far = Vec::new();
            let mut lod0_leaves: Vec<([f32; 3], [f32; 3])> = Vec::new();
            let mut had_leaves = false;
            for (l, lod) in m.lods.iter().enumerate() {
                for e in &lod.elems {
                    let class = cls.get(e.material).cloned().unwrap_or_default();
                    let leaves = is_facing_leaves(&class) && !naive;
                    let vd = &m.vdata[e.vdata];
                    let st = stats.entry(class.clone()).or_default();
                    st.elems += 1;
                    if leaves {
                        had_leaves = true;
                        leaf.elems += 1;
                        *leaf.strides.entry(vd.stride).or_default() += 1;
                    }
                    let mut seen_v = HashSet::new();
                    let (mut far_n, mut out_n, mut worst) = (0usize, 0usize, 0.0f32);
                    for &raw in &vd.indices[e.first_index..e.first_index + e.triangles * 3] {
                        let v = raw as usize + e.base_vertex;
                        if !seen_v.insert(v) {
                            continue;
                        }
                        let p = if leaves {
                            let lv = match m.leaf_vertex(vd, v) {
                                Ok(lv) => lv,
                                Err(e) => {
                                    println!("LEAF-ERROR {}: {e:#}", res.name);
                                    break;
                                }
                            };
                            if l == 0 {
                                lod0_leaves.push((lv.centre, lv.corner));
                            }
                            leaf.vertices += 1;
                            leaf.uv_outside += usize::from(lv.uv.iter().any(|&u| !(-0.001..=1.001).contains(&u)));
                            let len = lv.normal.iter().map(|x| x * x).sum::<f32>().sqrt();
                            leaf.normal_off += usize::from(!(0.96..=1.04).contains(&len));
                            lv.centre
                        } else {
                            m.vertex(vd, v).unwrap().pos
                        };
                        let d = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                        // NaN compares false: count it as out of bounds
                        let inside = (0..3).all(|k| p[k] >= m.bbox[0][k] - margin && p[k] <= m.bbox[1][k] + margin);
                        worst = worst.max(if d.is_nan() { f32::INFINITY } else { d });
                        far_n += usize::from(!(d <= far));
                        out_n += usize::from(!inside);
                    }
                    st.vertices += seen_v.len();
                    st.far_vertices += far_n;
                    st.outside_vertices += out_n;
                    st.far_elems += usize::from(far_n > 0);
                    st.outside_elems += usize::from(out_n > 0);
                    st.worst = st.worst.max(worst);
                    if far_n > 0 {
                        model_far.push(format!("lod{l} mat{} {class}: {far_n}/{} far, worst {worst:.4e}", e.material, seen_v.len()));
                    }
                }
            }
            if had_leaves {
                leaf.models += 1;
                match &m.impostor_data {
                    Some(i) if !i.shadow_points.is_empty() => {
                        // Each leaf vertex of LOD 0 must be one of the file's (centre, corner) records, and the other way round
                        // (the builder clamps the centre's Y to 0 and above)
                        let mut left: Vec<&[[f32; 3]; 2]> = i.shadow_points.iter().collect();
                        let mut unmatched = 0;
                        for (c, d) in lod0_leaves.iter().map(|(c, d)| ([c[0], c[1].max(0.0), c[2]], d)) {
                            let (c, d) = (&c, d);
                            match left.iter().position(|b| (0..3).all(|k| near(c[k], b[0][k]) && near(d[k], b[1][k]))) {
                                Some(at) => {
                                    left.swap_remove(at);
                                }
                                None => unmatched += 1,
                            }
                        }
                        if unmatched == 0 && left.is_empty() {
                            leaf.shadow_matched += 1;
                        } else {
                            leaf.shadow_mismatched.push(format!("{} ({} of {} leaf vertices unmatched, {} shadow points left)", res.name, unmatched, lod0_leaves.len(), left.len()));
                        }
                    }
                    _ => leaf.no_shadow.push(res.name.clone()),
                }
            }
            if !model_far.is_empty() {
                println!("FAR {} ({}): {}", res.name, path.file_name().unwrap().to_string_lossy(), model_far.join("; "));
                offenders.push(res.name.clone());
            }
        }
    }
    println!("\n{models} models, {failed} failed to parse, {no_desc} without a riDesc entry, {} with far vertices (> {far} m)", offenders.len());
    println!("{:<44} {:>8} {:>10} {:>9} {:>9} {:>9} {:>9} {:>12}", "class", "elems", "vertices", "far el", "far vtx", "outside el", "outside v", "worst |p|");
    for (c, s) in &stats {
        println!("{:<44} {:>8} {:>10} {:>9} {:>9} {:>9} {:>9} {:>12.4e}", c, s.elems, s.vertices, s.far_elems, s.far_vertices, s.outside_elems, s.outside_vertices, s.worst);
    }
    if !naive {
        println!(
            "\nfacing leaves: {} models, {} elements, {} vertices; strides {:?}; {} uv outside 0..1, {} normals off unit length; LOD 0 equals the file's shadow points in {} models",
            leaf.models, leaf.elems, leaf.vertices, leaf.strides, leaf.uv_outside, leaf.normal_off, leaf.shadow_matched
        );
        println!("shadow mismatches: {:?}", leaf.shadow_mismatched);
        println!("no shadow points: {:?}", leaf.no_shadow);
    }
    println!("OFFENDERS {}", offenders.join(" "));
}
