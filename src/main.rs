mod blk;
mod bytes;
mod dynmodel;
mod gltf;
mod grp;
mod vromfs;

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use blk::SharedContext;
use vromfs::Vromfs;

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
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for v in 0..n {
            let uv = dm.vertex(i, v).uv;
            for k in 0..2 {
                lo[k] = lo[k].min(uv[k]);
                hi[k] = hi[k].max(uv[k]);
            }
        }
        let max_idx = vd.indices.iter().max().copied().unwrap_or(0);
        println!("vdata {i}: {n} verts, {} indices (max {max_idx}), uv raw {lo:?}..{hi:?}", vd.indices.len());
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
    }
    Ok(())
}

/// Texture name (without the trailing `*`) -> pack holding it, from the table dumps of every DXP pack
fn texture_index(game: &Path) -> Result<HashMap<String, PathBuf>> {
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

fn model_cmd(pack: &Path, name: &str, game: &Path, out: &Path, lod: usize) -> Result<()> {
    let file = fs::read(pack).with_context(|| format!("reading {}", pack.display()))?;
    let entries = grp::parse(&file)?;
    let find = |res: &str, class: u32| {
        entries
            .iter()
            .find(|e| e.name == res && e.class_id == class)
            .map(|e| &file[e.offset..e.offset + e.size])
            .with_context(|| format!("{res} not found in {}", pack.display()))
    };
    let model = dynmodel::DynModel::parse(find(name, grp::DYN_MODEL)?)?;
    let nodes = dynmodel::parse_skeleton(find(&format!("{name}_skeleton"), grp::GEOM_NODE_TREE)?)?;
    let lod = model.lods.get(lod).context("no such LOD")?;
    // Materials and texture names come from dynModelDesc.bin, patched copy first
    let desc_paths = ["patch/content/base/res/dynModelDesc.bin", "content/base/res/dynModelDesc.bin"];
    let mut desc = None;
    for rel in desc_paths {
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
        .map(|b| b.params.iter().filter_map(|(_, v)| match v { blk::Value::Str(s) => Some(s.trim_end_matches('*').to_string()), _ => None }).collect())
        .unwrap_or_default();

    eprintln!("indexing texture packs...");
    let index = texture_index(game)?;
    let mut packs: HashMap<PathBuf, Vec<u8>> = HashMap::new();
    let mut decode_rgba = |tex: &str| -> Result<Option<(u32, u32, Vec<u8>)>> {
        let Some((full, path)) = [format!("{tex}$hq"), tex.to_string()].into_iter().find_map(|n| index.get(&n).map(|p| (n, p.clone()))) else {
            return Ok(None);
        };
        let data = match packs.get(&path) {
            Some(d) => d,
            None => packs.entry(path.clone()).or_insert(fs::read(&path)?),
        };
        let pack = wt_blk::dxp_and_grp::dxp::DxpPack::parse(data)?;
        let i = pack
            .entries
            .iter()
            .position(|e| e.name.split('*').next() == Some(full.as_str()) && !e.header.is_empty())
            .context("texture vanished")?;
        let texture = pack.texture(i)?;
        Ok(Some((texture.width(), texture.height(), texture.to_rgba()?)))
    };
    let write_png = |file_name: &str, (w, h, rgba): &(u32, u32, Vec<u8>)| -> Result<()> {
        fs::create_dir_all(out)?;
        let mut buf = Vec::new();
        let mut enc = png::Encoder::new(&mut buf, *w, *h);
        enc.set_color(png::ColorType::Rgba);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
        writer.finish()?;
        fs::write(out.join(file_name), buf)?;
        Ok(())
    };

    let mut materials = Vec::new();
    let mut skin_material_base = 0;
    for block in ["matR", "matS"] {
        if block == "matS" {
            skin_material_base = materials.len();
        }
        let Some(list) = child(block) else { continue };
        for (i, m) in list.blocks.iter().enumerate() {
            let get = |k: &str| m.params.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let class = match get("cls") { Some(blk::Value::Str(s)) => s.clone(), _ => String::new() };
            let tex = |k: &str| match get(k) { Some(blk::Value::Int(t)) => tex_names.get(*t as usize).cloned(), _ => None };
            // Masked tank shaders take the camouflage in t1; other masked shaders use t1 for something else
            let camo = tex("t1").filter(|t| class.contains("masked") && t.contains("_camo"));
            let color = tex("t0");
            let mut decode = |t: &Option<String>| match t {
                Some(t) => decode_rgba(t).unwrap_or_else(|e| {
                    eprintln!("texture {t}: {e:#}");
                    None
                }),
                None => None,
            };
            let (base, camo_rgba) = (decode(&color), decode(&camo));
            let color_png = match (base, &color) {
                (Some(mut img), Some(c)) => {
                    let file_name = match (&camo, camo_rgba) {
                        (Some(k), Some(camo_img)) => {
                            apply_camo(&mut img.2, &camo_img.2, !class.contains("atest"));
                            format!("{c}__{k}.png")
                        }
                        _ => format!("{c}.png"),
                    };
                    if !out.join(&file_name).exists() {
                        write_png(&file_name, &img)?;
                    }
                    Some(file_name)
                }
                _ => None,
            };
            eprintln!("{block} {i}: {class} -> {} {}", color.as_deref().unwrap_or("(no texture)"), camo.as_deref().map(|k| format!("+ camo {k}")).unwrap_or_default());
            materials.push(gltf::Material {
                name: format!("{block}{i}_{class}"),
                color_png,
                double_sided: matches!(get("two_sided"), Some(blk::Value::Bool(true))),
                alpha_mask: class.contains("atest"),
            });
        }
    }
    gltf::write(&model, lod, &nodes, &materials, skin_material_base, out, name)?;
    eprintln!("wrote {}", out.join(format!("{name}.gltf")).display());
    Ok(())
}

/// Approximates Dagor's masked tank materials: the base texture's alpha marks painted areas, where the
/// camouflage colour is applied and modulated by the base texture's detail (dirt, scratches).
/// Uses the camouflage's average colour, which is exact for single-colour schemes like US olive drab.
fn apply_camo(base: &mut [u8], camo: &[u8], opaque: bool) {
    let mean = |px: &mut dyn Iterator<Item = &[u8]>| {
        let (mut s, mut n) = ([0f64; 3], 0f64);
        for p in px {
            for k in 0..3 {
                s[k] += f64::from(p[k]);
            }
            n += 1.0;
        }
        if n > 0.0 { s.map(|x| x / n) } else { [128.0; 3] }
    };
    let camo_mean = mean(&mut camo.chunks_exact(4));
    // Average detail of the fully painted areas maps to the plain camouflage colour
    let painted = mean(&mut base.chunks_exact(4).filter(|p| p[3] > 200));
    let painted_luma = (painted[0] + painted[1] + painted[2]) / 3.0;
    for p in base.chunks_exact_mut(4) {
        let mask = f64::from(p[3]) / 255.0;
        let luma = (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0;
        let detail = luma / painted_luma.max(1.0);
        for k in 0..3 {
            let paint = (camo_mean[k] * detail).min(255.0);
            p[k] = (f64::from(p[k]) * (1.0 - mask) + paint * mask).round() as u8;
        }
        if opaque {
            p[3] = 255;
        }
    }
}

fn check_cmd(dir: &Path) -> Result<()> {
    let mut errors: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let (mut models, mut ok, mut skeletons, mut tris) = (0, 0, 0, 0usize);
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
    println!("{ok}/{models} DynModels parsed ({tris} LOD0 rigid triangles), {skeletons} skeletons");
    for (msg, list) in &errors {
        println!("{:>5}  {msg}\n       e.g. {}", list.len(), list.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    }
    Ok(())
}
