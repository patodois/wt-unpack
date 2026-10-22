
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use wt_unpack::blk::SharedContext;
use wt_unpack::vromfs::Vromfs;
use wt_unpack::{blk, dynmodel, gltf, grp, vehicle, vromfs};

#[derive(Parser)]
#[command(version, about = "Read Dagor Engine vromfs archives and binary BLK files")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show header info and the file list of a .vromfs.bin archive
    List { archive: PathBuf },
    /// Extract every file of an archive, converting binary BLK files
    Extract {
        archive: PathBuf,
        /// Output directory
        #[arg(short, long)]
        out: PathBuf,
        /// How to write binary BLK files
        #[arg(long, value_enum, default_value_t = BlkFormat::Text)]
        blk: BlkFormat,
        /// Only extract files whose path contains this text
        #[arg(long)]
        filter: Option<String>,
    },
    /// List the resources of a .grp pack, or inspect one DynModel in it
    Grp {
        pack: PathBuf,
        /// Name of a DynModel to inspect
        #[arg(long)]
        model: Option<String>,
    },
    /// Export a DynModel (e.g. a tank) to glTF with its textures
    Model {
        /// The .grp pack holding the model
        pack: PathBuf,
        /// Model name, e.g. m4a1_76w_sherman
        name: String,
        /// Game folder with content/ and patch/ (used for dynModelDesc.bin and texture packs)
        #[arg(long, default_value = "/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game")]
        game: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value_t = 0)]
        lod: usize,
    },
    /// Parse every DynModel and skeleton in the .grp packs of a folder and summarise failures
    Check { dir: PathBuf },
    /// Print one BLK file (a loose file, or `archive.vromfs.bin:path/inside.blk`)
    Cat {
        target: String,
        #[arg(long, value_enum, default_value_t = BlkFormat::Text)]
        blk: BlkFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum BlkFormat {
    Text,
    Json,
    Raw,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::List { archive } => list(&archive),
        Command::Extract { archive, out, blk, filter } => extract(&archive, &out, blk, filter.as_deref()),
        Command::Cat { target, blk } => cat(&target, blk),
        Command::Grp { pack, model } => grp_cmd(&pack, model.as_deref()),
        Command::Check { dir } => check_cmd(&dir),
        Command::Model { pack, name, game, out, lod } => model_cmd(&pack, &name, &game, &out, lod),
    }
}

fn open(path: &Path) -> Result<Vromfs> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Vromfs::parse(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// Loads the shared name map and zstd dictionary that slim BLK files need.
fn shared_context(vfs: &Vromfs) -> Result<Option<SharedContext>> {
    let Some(nm) = vfs.get(vromfs::SHARED_NAMEMAP_NAME) else { return Ok(None) };
    let (mut ctx, dict_name) = SharedContext::from_namemap(&nm.data)?;
    ctx.dict = vfs.get(&dict_name).map(|f| f.data.clone());
    Ok(Some(ctx))
}

fn list(path: &Path) -> Result<()> {
    let vfs = open(path)?;
    println!("target:  {}", vfs.target);
    println!("version: {}", vfs.version.as_deref().unwrap_or("-"));
    println!("md5:     {}", if vfs.md5_ok { "ok" } else { "MISMATCH" });
    println!("files:   {}", vfs.files.len());
    for f in &vfs.files {
        println!("{:>10}  {}", f.data.len(), f.name.escape_debug());
    }
    Ok(())
}

fn render(data: &[u8], shared: Option<&SharedContext>, format: BlkFormat) -> Result<Vec<u8>> {
    let root = blk::parse(data, shared)?;
    Ok(match format {
        BlkFormat::Text => root.to_text().into_bytes(),
        BlkFormat::Json => {
            let mut out = serde_json::to_vec_pretty(&root.to_json())?;
            out.push(b'\n');
            out
        }
        BlkFormat::Raw => data.to_vec(),
    })
}

/// Rejects absolute paths and `..` so archive names cannot escape the output dir.
fn safe_join(base: &Path, name: &str) -> Option<PathBuf> {
    let rel = Path::new(name);
    rel.components().all(|c| matches!(c, Component::Normal(_))).then(|| base.join(rel))
}

fn extract(path: &Path, out: &Path, format: BlkFormat, filter: Option<&str>) -> Result<()> {
    let vfs = open(path)?;
    let shared = shared_context(&vfs)?;
    let (mut written, mut failed) = (0, 0);
    for f in &vfs.files {
        if filter.is_some_and(|needle| !f.name.contains(needle)) {
            continue;
        }
        if f.name == vromfs::SHARED_NAMEMAP_NAME {
            continue;
        }
        let Some(mut dest) = safe_join(out, &f.name) else {
            eprintln!("skipping unsafe path {:?}", f.name);
            continue;
        };
        let mut data = f.data.clone();
        if f.name.ends_with(".blk") && blk::is_binary(&f.data) && !matches!(format, BlkFormat::Raw) {
            match render(&f.data, shared.as_ref(), format) {
                Ok(converted) => {
                    data = converted;
                    if matches!(format, BlkFormat::Json) {
                        dest.set_extension("blk.json");
                    }
                }
                Err(e) => {
                    eprintln!("{}: {e:#} (kept raw)", f.name);
                    failed += 1;
                }
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, data).with_context(|| format!("writing {}", dest.display()))?;
        written += 1;
    }
    eprintln!("wrote {written} files to {} ({failed} BLK files could not be converted)", out.display());
    Ok(())
}

fn cat(target: &str, format: BlkFormat) -> Result<()> {
    let (data, shared) = match target.split_once(".vromfs.bin:") {
        Some((archive, inner)) => {
            let vfs = open(Path::new(&format!("{archive}.vromfs.bin")))?;
            let file = vfs.get(inner).with_context(|| format!("{inner} not found in archive"))?;
            (file.data.clone(), shared_context(&vfs)?)
        }
        None => (fs::read(target).with_context(|| format!("reading {target}"))?, None),
    };
    let out = if blk::is_binary(&data) { render(&data, shared.as_ref(), format)? } else { data };
    use std::io::Write;
    std::io::stdout().write_all(&out)?;
    Ok(())
}

fn grp_cmd(path: &Path, model: Option<&str>) -> Result<()> {
    let file = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let entries = grp::parse(&file)?;
    let Some(model) = model else {
        for e in &entries {
            println!("{:08X} {:>10} {:>10}  {}", e.class_id, e.offset, e.size, e.name);
        }
        return Ok(());
    };
    let e = entries
        .iter()
        .find(|e| e.name == model && e.class_id == grp::DYN_MODEL)
        .with_context(|| format!("no DynModel named {model}"))?;
    let dm = dynmodel::DynModel::parse(&file[e.offset..e.offset + e.size])?;
    for (i, vd) in dm.vdata.iter().enumerate() {
        let n = vd.vertices.len() / vd.stride;
        let (mut lo, mut hi) = ([f32::MAX; 4], [f32::MIN; 4]);
        for v in 0..n {
            let vx = dm.vertex(i, v);
            let uv = [vx.uv[0], vx.uv[1], vx.uv1[0], vx.uv1[1]];
            for k in 0..4 {
                lo[k] = lo[k].min(uv[k]);
                hi[k] = hi[k].max(uv[k]);
            }
        }
        let max_idx = vd.indices.iter().max().copied().unwrap_or(0);
        println!("vdata {i}: {n} verts, {} indices (max {max_idx}), raw uv0+uv1 {lo:?}..{hi:?}", vd.indices.len());
    }
    for (i, vd) in dm.skin_vdata.iter().enumerate() {
        let decl: Vec<String> = vd.channels.iter().map(|c| format!("t{} u{}.{}", c.ty, c.usage, c.index)).collect();
        println!("skin vdata {i}: {} verts x {}, {} indices: {}", vd.vertices.len() / vd.stride, vd.stride, vd.indices.len(), decl.join(", "));
    }
    for (l, lod) in dm.lods.iter().enumerate() {
        let tris: usize = lod.rigids.iter().flat_map(|r| &r.elems).map(|e| e.triangles).sum();
        println!("lod {l}: range {} m, {} rigids, {tris} triangles", lod.range, lod.rigids.len());
        for s in &lod.skins {
            println!("  skin on node {}: {:?}", s.node, s.elems);
        }
        if l == 0 && std::env::var_os("WT_PARTS").is_some() {
            for r in &lod.rigids {
                let mats: Vec<String> = r
                    .elems
                    .iter()
                    .map(|e| {
                        let vd = &dm.vdata[e.vdata];
                        let (mut lo, mut hi) = ([f32::MAX; 4], [f32::MIN; 4]);
                        for &i in &vd.indices[e.first_index..e.first_index + e.triangles * 3] {
                            let v = dm.vertex(e.vdata, i as usize + e.base_vertex);
                            for (k, x) in [v.uv[0], v.uv[1], v.uv1[0], v.uv1[1]].into_iter().enumerate() {
                                lo[k] = lo[k].min(x / 4096.0);
                                hi[k] = hi[k].max(x / 4096.0);
                            }
                        }
                        let r2 = |a: [f32; 4]| a.map(|x| (x * 100.0).round() / 100.0);
                        format!("m{}x{} uv1? {} uv {:?}..{:?}", e.material, e.triangles, vd.has_uv1(), r2(lo), r2(hi))
                    })
                    .collect();
                println!("  part {}: {}", r.node, mats.join(" | "));
            }
        }
    }
    Ok(())
}

fn model_cmd(pack: &Path, name: &str, game: &Path, out: &Path, lod: usize) -> Result<()> {
    let vehicle = vehicle::load(game, pack, name, lod)?;
    gltf::write(&vehicle, out)?;
    eprintln!("wrote {}", out.join(format!("{name}.gltf")).display());
    Ok(())
}

fn check_cmd(dir: &Path) -> Result<()> {
    let mut errors: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let (mut models, mut ok, mut skeletons, mut tris) = (0, 0, 0, 0usize);
    let mut index_sum = 0u64;
    let mut paths: Vec<_> = fs::read_dir(dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "grp")).collect();
    paths.sort();
    for path in paths {
        let file = fs::read(&path)?;
        let entries = match grp::parse(&file) {
            Ok(e) => e,
            Err(e) => {
                errors.entry(format!("grp: {e:#}")).or_default().push(path.display().to_string());
                continue;
            }
        };
        for e in &entries {
            let data = &file[e.offset..e.offset + e.size];
            let result = match e.class_id {
                grp::DYN_MODEL => {
                    models += 1;
                    dynmodel::DynModel::parse(data).map(|m| {
                        tris += m.lods[0].rigids.iter().flat_map(|r| &r.elems).map(|e| e.triangles).sum::<usize>();
                        for vd in m.vdata.iter().chain(&m.skin_vdata) {
                            for &i in &vd.indices {
                                index_sum = index_sum.wrapping_add(u64::from(i));
                            }
                        }
                    })
                }
                grp::GEOM_NODE_TREE => {
                    skeletons += 1;
                    dynmodel::parse_skeleton(data).map(|_| ())
                }
                _ => continue,
            };
            match result {
                Ok(()) if e.class_id == grp::DYN_MODEL => ok += 1,
                Ok(()) => {}
                Err(err) => {
                    // Group by message without numbers so similar failures land together
                    let key: String = format!("{err:#}").chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                    errors.entry(key).or_default().push(format!("{}:{}", path.file_name().unwrap().to_string_lossy(), e.name));
                }
            }
        }
    }
    println!("{ok}/{models} DynModels parsed ({tris} LOD0 rigid triangles), {skeletons} skeletons, index checksum {index_sum}");
    for (msg, list) in &errors {
        println!("{:>5}  {msg}\n       e.g. {}", list.len(), list.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    }
    Ok(())
}
