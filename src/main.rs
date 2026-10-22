mod blk;
mod bytes;
mod vromfs;

use std::fs;
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
