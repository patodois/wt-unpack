
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use wt_unpack::blk::SharedContext;
use wt_unpack::vromfs::Vromfs;
use wt_blk::dxp_and_grp::{
    ddsx::{DdsxHeader, Texture},
    dynmodel::DynModel,
    geom_node_tree::GeomNodeTree,
    grp_pack::{class_id, GrpPack},
};
use wt_unpack::level::Level;
use wt_unpack::terrain::{self, HeightGrid};
use wt_unpack::sound::{self, Codec, Fsb5};
use wt_unpack::{blk, gltf, vehicle, vromfs};

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
    /// List the blocks of a level file (levels/*.bin), or export its terrain and placed objects
    Level {
        file: PathBuf,
        /// Write the terrain heights as a 16-bit grayscale PNG, with its placement in a JSON next to it
        #[arg(long)]
        heightmap: Option<PathBuf>,
        /// Export the terrain mesh to glTF in this directory
        #[arg(long)]
        gltf: Option<PathBuf>,
        /// Terrain source; auto takes the HM2 heightmap when the level has one, else the landmesh
        #[arg(long, value_enum, default_value_t = TerrainSource::Auto)]
        terrain: TerrainSource,
        /// Grid step in metres when rasterising the landmesh (default: its detail grid step)
        #[arg(long)]
        cell: Option<f32>,
        /// List the placed objects (rendinst pools) with counts and sample transforms
        #[arg(long)]
        objects: bool,
        /// Write the textures stored inside the level (e.g. its lightmap) as PNG files to this directory
        #[arg(long)]
        textures: Option<PathBuf>,
    },
    /// Parse every level file in a folder and summarise what decodes
    CheckLevels { dir: PathBuf },
    /// Print one BLK file (a loose file, or `archive.vromfs.bin:path/inside.blk`)
    Cat {
        target: String,
        #[arg(long, value_enum, default_value_t = BlkFormat::Text)]
        blk: BlkFormat,
    },
    /// Read FMOD sound banks (`sound/*.assets.bank`)
    Sound {
        #[command(subcommand)]
        command: SoundCommand,
    },
}

#[derive(Subcommand)]
enum SoundCommand {
    /// List the samples of a bank: rate, channels, length and name
    List { bank: PathBuf },
    /// Extract samples: Vorbis to Ogg (copied losslessly) and everything else to WAV
    Extract {
        bank: PathBuf,
        /// Output directory
        #[arg(short, long)]
        out: PathBuf,
        /// Only extract samples whose name contains this text
        #[arg(long)]
        filter: Option<String>,
        /// Output format; `wav` also decodes Vorbis
        #[arg(long, value_enum, default_value_t = AudioFormat::Auto)]
        format: AudioFormat,
    },
    /// Parse every bank in a folder and count samples per codec; `--decode` also decodes them all
    Check {
        dir: PathBuf,
        #[arg(long)]
        decode: bool,
    },
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum AudioFormat {
    /// Ogg for Vorbis, WAV for the rest
    Auto,
    Wav,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum TerrainSource {
    Auto,
    Hm2,
    Landmesh,
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
        Command::Level { file, heightmap, gltf, terrain, cell, objects, textures } => {
            level_cmd(&file, heightmap.as_deref(), gltf.as_deref(), terrain, cell, objects, textures.as_deref())
        }
        Command::CheckLevels { dir } => check_levels(&dir),
        Command::Sound { command } => match command {
            SoundCommand::List { bank } => sound_list(&bank),
            SoundCommand::Extract { bank, out, filter, format } => sound_extract(&bank, &out, filter.as_deref(), format),
            SoundCommand::Check { dir, decode } => sound_check(&dir, decode),
        },
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
    let pack = GrpPack::parse(&file)?;
    let Some(model) = model else {
        for r in &pack.resources {
            println!("{:08X} {:>10}  {}", r.class_id, pack.data(r).len(), r.name);
        }
        return Ok(());
    };
    let res = pack.find(model, class_id::DYN_MODEL).with_context(|| format!("no DynModel named {model}"))?;
    let dm = DynModel::parse(pack.data(res))?;
    for (label, list) in [("vdata", &dm.vdata), ("skin vdata", &dm.skin_vdata)] {
        for (i, vd) in list.iter().enumerate() {
            let (mut lo, mut hi) = ([f32::MAX; 4], [f32::MIN; 4]);
            for v in 0..vd.vertex_count() {
                let vx = dm.vertex(vd, v)?;
                for (k, x) in [vx.uv[0], vx.uv[1], vx.uv1[0], vx.uv1[1]].into_iter().enumerate() {
                    lo[k] = lo[k].min(x);
                    hi[k] = hi[k].max(x);
                }
            }
            let decl: Vec<String> = vd.channels.iter().map(|c| format!("t{} u{}.{}", c.ty, c.usage, c.index)).collect();
            println!(
                "{label} {i}: {} verts x {}, {} indices, uv0+uv1 {lo:?}..{hi:?}, channels {}",
                vd.vertex_count(),
                vd.stride,
                vd.indices.len(),
                decl.join(", ")
            );
        }
    }
    for (l, lod) in dm.lods.iter().enumerate() {
        let tris: usize = lod.rigids.iter().flat_map(|r| &r.elems).map(|e| e.triangles).sum();
        println!("lod {l}: range {} m, {} rigids, {} skins, {tris} rigid triangles", lod.range, lod.rigids.len(), lod.skins.len());
    }
    // The skeleton: each node with its parent, model-space position and the LOD0 parts it carries
    if let Some(res) = pack.find(&format!("{model}_skeleton"), class_id::GEOM_NODE_TREE) {
        let tree = GeomNodeTree::parse(pack.data(res))?;
        let lod0 = dm.lods.first();
        for (i, n) in tree.nodes.iter().enumerate() {
            let parts = lod0.map_or(0, |l| l.rigids.iter().filter(|r| r.node == n.name).count());
            let parent = n.parent.map_or("-".to_string(), |p| tree.nodes[p].name.clone());
            let [x, y, z] = [n.wtm[12], n.wtm[13], n.wtm[14]];
            // Most nodes are axis-aligned; show the axes of those that are not
            let m = &n.wtm;
            let identity = [0, 1, 2].iter().all(|&c| (0..3).all(|r| (m[c * 4 + r] - f32::from(u8::from(r == c))).abs() < 1e-4));
            let axes = if identity {
                String::new()
            } else {
                let axis = |c: usize| format!("[{:.3}, {:.3}, {:.3}]", m[c * 4], m[c * 4 + 1], m[c * 4 + 2]);
                format!(", axes x {} y {} z {}", axis(0), axis(1), axis(2))
            };
            println!("node {i} {} (parent {parent}) at [{x:.3}, {y:.3}, {z:.3}], {parts} rigids{axes}", n.name);
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
        let pack = match GrpPack::parse(&file) {
            Ok(p) => p,
            Err(e) => {
                errors.entry(format!("grp: {e}")).or_default().push(path.display().to_string());
                continue;
            }
        };
        for r in &pack.resources {
            let data = pack.data(r);
            let result = match r.class_id {
                class_id::DYN_MODEL => {
                    models += 1;
                    DynModel::parse(data).map(|m| {
                        tris += m.lods.first().map_or(0, |l| l.rigids.iter().flat_map(|r| &r.elems).map(|e| e.triangles).sum::<usize>());
                        for vd in m.vdata.iter().chain(&m.skin_vdata) {
                            for &i in &vd.indices {
                                index_sum = index_sum.wrapping_add(u64::from(i));
                            }
                        }
                    })
                }
                class_id::GEOM_NODE_TREE => {
                    skeletons += 1;
                    GeomNodeTree::parse(data).map(|_| ())
                }
                _ => continue,
            };
            match result {
                Ok(()) if r.class_id == class_id::DYN_MODEL => ok += 1,
                Ok(()) => {}
                Err(err) => {
                    // Group by message without numbers so similar failures land together
                    let key: String = err.to_string().chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                    errors.entry(key).or_default().push(format!("{}:{}", path.file_name().unwrap().to_string_lossy(), r.name));
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

fn level_cmd(
    path: &Path,
    heightmap: Option<&Path>,
    gltf_dir: Option<&Path>,
    source: TerrainSource,
    cell: Option<f32>,
    objects: bool,
    textures: Option<&Path>,
) -> Result<()> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let level = Level::parse(&raw).with_context(|| format!("parsing {}", path.display()))?;
    println!("textures declared: {}", level.tex_count);
    println!("{} blocks:", level.blocks.len());
    for b in &level.blocks {
        println!("  {:<4} at {:#010x}  {:>10} bytes{}", b.tag, b.offset, b.data.len(), if b.codec != 0 { format!("  codec {}", b.codec) } else { String::new() });
    }
    if let Some(info) = level.export_info()? {
        let sections: Vec<&str> = info.sections.iter().map(|(t, _)| t.as_str()).collect();
        println!("exporter: {} (hashed sections: {})", info.version, sections.join(" "));
    }
    let resources = level.required_resources()?;
    println!("required resources: {}", resources.len());
    if let Some(refs) = level.texture_refs()? {
        println!("texture names: {} (packs: {})", refs.textures.len(), if refs.packs.is_empty() { "-".into() } else { refs.packs.join(", ") });
    }
    for (name, ddsx) in level.inline_textures()? {
        let header = DdsxHeader::parse(ddsx).map_err(|e| anyhow::anyhow!("{name}: {e:?}"))?;
        println!("inline texture: {name} ({}x{} {}, {} bytes)", header.width, header.height, header.format_name(), ddsx.len());
        if let Some(dir) = textures {
            let png = Texture::from_ddsx(header, &ddsx[DdsxHeader::SIZE..])
                .and_then(|t| t.to_png())
                .map_err(|e| anyhow::anyhow!("{name}: {e:?}"))?;
            // Texture names end in '*' and may carry "?options"; keep a file-system-safe stem
            let stem: String = name.split(['*', '?']).next().unwrap_or("texture").chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
            fs::create_dir_all(dir)?;
            let out = dir.join(format!("{stem}.png"));
            fs::write(&out, png)?;
            eprintln!("wrote {}", out.display());
        }
    }
    if let Some(b) = level.world_bbox()? {
        println!("world box: ({:.0}, {:.0}, {:.0}) .. ({:.0}, {:.0}, {:.0})", b[0], b[1], b[2], b[3], b[4], b[5]);
    }
    let lmesh = level.landmesh()?;
    if let Some(lm) = &lmesh {
        let e = lm.extent();
        println!(
            "landmesh: {}x{} cells of {} m from ({}, {}) to ({}, {}), detail grid {} m, ray tracer: {}",
            lm.cells[0], lm.cells[1], lm.land_cell_size, e[0], e[1], e[2], e[3], lm.grid_cell_size, lm.tracer_kind()
        );
    }
    let hm = level.heightmap()?;
    if let Some(h) = &hm {
        println!(
            "heightmap: {}x{} at {} m from ({}, {}), heights {} .. {} m, version {}{}",
            h.width, h.height, h.cell_size, h.origin[0], h.origin[1], h.height_min, h.height_min + h.height_scale, h.version,
            if h.mirrored { ", mirrored beyond its edges" } else { "" }
        );
    }

    if heightmap.is_some() || gltf_dir.is_some() {
        let use_hm2 = match source {
            TerrainSource::Hm2 => true,
            TerrainSource::Landmesh => false,
            TerrainSource::Auto => hm.is_some(),
        };
        let name = path.file_stem().map_or("terrain".into(), |s| s.to_string_lossy().into_owned());
        let (grid, mesh) = if use_hm2 {
            let grid = HeightGrid::from_heightmap(hm.as_ref().context("the level has no HM2 heightmap")?);
            // At most about 1024 vertices along each side
            let step = grid.width.max(grid.height).div_ceil(1024).max(1);
            let mesh = grid.mesh(step);
            (grid, mesh)
        } else {
            let lm = lmesh.as_ref().context("the level has no landmesh")?;
            let mesh = lm.tracer_mesh()?.context("the landmesh has no ray tracer")?;
            eprintln!("landmesh ray tracer: {} vertices, {} triangles", mesh.positions.len(), mesh.indices.len() / 3);
            let mut grid = HeightGrid::rasterize(&mesh, cell.unwrap_or(lm.grid_cell_size))?;
            // The landmesh leaves out the cells the heightmap covers
            if let Some(h) = &hm {
                let filled = grid.fill_from(&HeightGrid::from_heightmap(h));
                if filled > 0 {
                    eprintln!("filled {filled} samples the landmesh leaves to the HM2 heightmap");
                }
            }
            (grid, mesh)
        };
        if let Some(png) = heightmap {
            grid.write_png(png)?;
            let (lo, hi) = grid.range().unwrap_or_default();
            eprintln!(
                "wrote {} ({}x{} from {}, {:.1} .. {:.1} m, {:.1}% covered) and {}",
                png.display(), grid.width, grid.height, grid.source, lo, hi,
                100.0 * grid.covered() as f64 / grid.heights.len() as f64,
                png.with_extension("json").display()
            );
        }
        if let Some(dir) = gltf_dir {
            let model = terrain::to_model(&name, &mesh);
            gltf::write(&model, dir)?;
            eprintln!("wrote {} ({} vertices, {} triangles)", dir.join(format!("{name}.gltf")).display(), mesh.positions.len(), mesh.indices.len() / 3);
        }
    }

    if objects {
        for (li, layer) in level.rendinst_layers()?.iter().enumerate() {
            let counts = layer.counts();
            let instances = layer.instances()?;
            println!(
                "rendinst layer {li}: {}x{} cells of {} m from ({}, {}), {} land classes, {} pools, {} placed objects",
                layer.cells_w, layer.cells_h, layer.cell_size as f32 * layer.grid2world, layer.world0[0], layer.world0[1],
                layer.land_classes.len(), layer.pools.len(), instances.len()
            );
            let mut order: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] > 0).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(counts[i]));
            for &i in order.iter().take(25) {
                let p = &layer.pools[i];
                println!("  {:>7}  {}{}", counts[i], p.name, if p.pos_only { "  (position + scale only)" } else { "" });
            }
            if order.len() > 25 {
                println!("  ... {} more pools with objects", order.len() - 25);
            }
            let step = (instances.len() / 6).max(1);
            for inst in instances.iter().step_by(step).take(6) {
                let t = &inst.tm;
                let scale = (t[0][0] * t[0][0] + t[0][1] * t[0][1] + t[0][2] * t[0][2]).sqrt();
                println!(
                    "  sample {}: pos ({:.1}, {:.1}, {:.1}), x axis ({:.3}, {:.3}, {:.3}), scale {:.2}",
                    layer.pools[inst.pool].name, t[3][0], t[3][1], t[3][2], t[0][0], t[0][1], t[0][2], scale
                );
            }
        }
    }
    Ok(())
}

fn check_levels(dir: &Path) -> Result<()> {
    let mut paths: Vec<_> = fs::read_dir(dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bin")).collect();
    paths.sort();
    let (mut full, mut hm2, mut tracer, mut no_terrain, mut objects) = (0, 0, 0, 0, 0usize);
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let raw = fs::read(path)?;
        let summary = (|| -> Result<String> {
            let level = Level::parse(&raw)?;
            level.export_info()?;
            let res = level.required_resources()?.len();
            let tex = level.texture_refs()?.map_or(0, |t| t.textures.len());
            level.inline_textures()?;
            level.world_bbox()?;
            let mut terrain = Vec::new();
            if let Some(h) = level.heightmap()? {
                hm2 += 1;
                terrain.push(format!("HM2 {}x{}", h.width, h.height));
            }
            let lm = level.landmesh()?.context("no landmesh")?;
            if let Some(mesh) = lm.tracer_mesh()? {
                tracer += 1;
                terrain.push(format!("{} ray tracer {} tris", lm.tracer_kind(), mesh.indices.len() / 3));
            }
            if terrain.is_empty() {
                no_terrain += 1;
            }
            let mut n = 0;
            for layer in level.rendinst_layers()? {
                let inst = layer.instances()?;
                ensure_counts(&layer.counts(), inst.len())?;
                n += inst.len();
            }
            objects += n;
            Ok(format!("{} blocks, {res} resources, {tex} textures, {}, {n} objects", level.blocks.len(), terrain.join(", ")))
        })();
        match summary {
            Ok(s) => {
                full += 1;
                println!("ok    {name}: {s}");
            }
            Err(e) => println!("FAIL  {name}: {e:#}"),
        }
    }
    println!(
        "{full}/{} levels parse fully; terrain: {hm2} with HM2, {tracer} with a land ray tracer, {no_terrain} with neither; {objects} placed objects",
        paths.len()
    );
    Ok(())
}

fn ensure_counts(counts: &[usize], decoded: usize) -> Result<()> {
    let total: usize = counts.iter().sum();
    anyhow::ensure!(total == decoded, "counters promise {total} objects, {decoded} decoded");
    Ok(())
}

/// Calls `f` with each FSB5 of a bank and returns how many there were.
fn with_fsbs(path: &Path, mut f: impl FnMut(usize, &Fsb5) -> Result<()>) -> Result<usize> {
    let map = sound::map(path)?;
    let ranges = sound::fsb5_ranges(&map).with_context(|| format!("reading {}", path.display()))?;
    for (i, range) in ranges.iter().enumerate() {
        let fsb = Fsb5::parse(&map[range.clone()]).with_context(|| format!("FSB5 at offset {}", range.start))?;
        f(i, &fsb)?;
    }
    Ok(ranges.len())
}

fn no_samples_note(path: &Path) {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    match name.strip_suffix(".bank").filter(|n| !n.ends_with(".assets")) {
        Some(stem) => eprintln!("{name} holds no sample data; its samples are in {stem}.assets.bank"),
        None => eprintln!("{name} holds no sample data"),
    }
}

fn sound_list(path: &Path) -> Result<()> {
    let fsbs = with_fsbs(path, |i, fsb| {
        println!("FSB5 {i}: version {}, {:?}, {} samples", fsb.version, fsb.codec, fsb.samples.len());
        for (k, s) in fsb.samples.iter().enumerate() {
            let looped = s.loop_points.map_or(String::new(), |(a, b)| format!("  loop {a}..{b}"));
            println!(
                "{k:>6} {:>6} Hz {}ch {:>9} frames {:>8.3}s {:>9} bytes  {}{looped}",
                s.rate,
                s.channels,
                s.frames,
                s.duration(),
                s.data.len(),
                s.name.as_deref().unwrap_or("-")
            );
        }
        Ok(())
    })?;
    if fsbs == 0 {
        no_samples_note(path);
    }
    Ok(())
}

/// A file name from a sample name: path separators and control characters become `_`.
fn sample_file_name(name: Option<&str>, fsb: usize, index: usize) -> String {
    match name {
        Some(n) if !n.is_empty() && n != "." && n != ".." => {
            n.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect()
        }
        _ => format!("sample_{fsb}_{index}"),
    }
}

fn sound_extract(path: &Path, out: &Path, filter: Option<&str>, format: AudioFormat) -> Result<()> {
    fs::create_dir_all(out)?;
    let (mut written, mut failed) = (0, 0);
    let mut used = std::collections::HashSet::new();
    let fsbs = with_fsbs(path, |i, fsb| {
        for (k, s) in fsb.samples.iter().enumerate() {
            if filter.is_some_and(|needle| !s.name.as_deref().unwrap_or("").contains(needle)) {
                continue;
            }
            let ogg = fsb.codec == Codec::Vorbis && format == AudioFormat::Auto;
            let mut stem = sample_file_name(s.name.as_deref(), i, k);
            if !used.insert(stem.clone()) {
                stem = format!("{stem}_{i}_{k}");
                used.insert(stem.clone());
            }
            let dest = out.join(format!("{stem}.{}", if ogg { "ogg" } else { "wav" }));
            let result = (|| -> Result<()> {
                let mut w = std::io::BufWriter::new(fs::File::create(&dest)?);
                if ogg {
                    sound::vorbis::write_ogg(s, fsb.data(s), &mut w)?;
                } else {
                    fsb.decode(s)?.write_wav(&mut w)?;
                }
                use std::io::Write;
                w.flush()?;
                Ok(())
            })();
            match result {
                Ok(()) => written += 1,
                Err(e) => {
                    let _ = fs::remove_file(&dest);
                    eprintln!("{}: {e:#}", s.name.as_deref().unwrap_or("-"));
                    failed += 1;
                }
            }
        }
        Ok(())
    })?;
    if fsbs == 0 {
        no_samples_note(path);
    }
    eprintln!("wrote {written} samples to {} ({failed} failed)", out.display());
    Ok(())
}

fn sound_check(dir: &Path, decode: bool) -> Result<()> {
    use std::collections::BTreeMap;
    let mut paths: Vec<_> = fs::read_dir(dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bank")).collect();
    paths.sort();
    let (mut banks, mut fsbs, mut with_samples) = (0, 0, 0);
    let mut per_codec: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    let mut layouts: BTreeMap<(u8, u32), usize> = BTreeMap::new();
    let mut setups: BTreeMap<u32, usize> = BTreeMap::new();
    let mut errors: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut decoded, mut exact, mut longer) = (0, 0, 0);
    for path in paths {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        banks += 1;
        let result = with_fsbs(&path, |_, fsb| {
            fsbs += 1;
            for s in &fsb.samples {
                let e = per_codec.entry(format!("{:?}", fsb.codec)).or_default();
                e.0 += 1;
                e.1 += s.duration();
                *layouts.entry((s.channels, s.rate)).or_default() += 1;
                if let Some(crc) = s.vorbis_setup {
                    *setups.entry(crc).or_default() += 1;
                }
                if !decode {
                    continue;
                }
                let mut check = || -> Result<()> {
                    if fsb.codec == Codec::Vorbis {
                        // Raw decode length, before trimming to the header's length
                        let raw = sound::vorbis::decode(s, fsb.data(s))?;
                        let frames = raw.len() / usize::from(s.channels);
                        if frames == s.frames as usize {
                            exact += 1;
                        } else if frames > s.frames as usize {
                            longer += 1;
                        }
                        sound::vorbis::write_ogg(s, fsb.data(s), std::io::sink())?;
                    }
                    fsb.decode(s)?;
                    decoded += 1;
                    Ok(())
                };
                if let Err(err) = check() {
                    let key: String = format!("{err:#}").chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                    errors.entry(key).or_default().push(format!("{file}:{}", s.name.as_deref().unwrap_or("-")));
                }
            }
            Ok(())
        });
        match result {
            Ok(0) => {}
            Ok(_) => with_samples += 1,
            Err(err) => errors.entry(format!("bank: {err:#}")).or_default().push(file),
        }
    }
    println!("{banks} banks, {with_samples} with sample data in {fsbs} FSB5 sample banks");
    for (codec, (n, secs)) in &per_codec {
        println!("{codec:>10}: {n} samples, {:.1} hours", secs / 3600.0);
    }
    let layouts: Vec<String> = layouts.iter().map(|((c, r), n)| format!("{c}ch {r} Hz: {n}")).collect();
    println!("layouts: {}", layouts.join(", "));
    let setups: Vec<String> = setups.iter().map(|(c, n)| format!("{c:08X}: {n}")).collect();
    println!("Vorbis setups ({}): {}", setups.len(), setups.join(", "));
    if decode {
        println!("decoded {decoded} samples; Vorbis decoded length equal to the header {exact}, longer {longer}");
    }
    for (msg, list) in &errors {
        println!("{:>6}  {msg}\n        e.g. {}", list.len(), list.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    }
    Ok(())
}
