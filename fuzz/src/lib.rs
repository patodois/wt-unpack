//! Helpers shared by the fuzz targets: what the CLI and the scene builder do with a parsed level section.

use wt_unpack::level::{Heightmap, LandMesh, RendInstLayer};
use wt_unpack::terrain::HeightGrid;

/// The grid built from a heightmap (skipped when it is large: the harness itself must not be what
/// allocates gigabytes)
pub fn use_heightmap(hm: &Heightmap) {
    if hm.data.len() <= 1 << 22 {
        let grid = HeightGrid::from_heightmap(hm);
        let _ = grid.range();
        let _ = grid.covered();
        let _ = grid.height_at(1.0, 2.0);
        let _ = grid.height_near(1.0, 2.0);
        write_png(&grid);
        let mesh = grid.mesh(64);
        let _ = HeightGrid::rasterize(&mesh, 8.0);
    }
}

/// The CLI's `grid.write_png(path)` (`level --heightmap out.png`), to a file that is removed again
fn write_png(grid: &HeightGrid) {
    let path = std::env::temp_dir().join(format!("wt-unpack-fuzz-{}.png", std::process::id()));
    let _ = grid.write_png(&path);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("json"));
}

/// Everything read out of a landmesh: the ray tracer, the detail map and the ground weights. `hm` is the level's
/// heightmap, if it has one, as the CLI fills the holes of the rasterised landmesh from it
pub fn use_landmesh(lm: &LandMesh, hm: Option<&Heightmap>) {
    let _ = (lm.tracer_kind(), lm.extent());
    if let Ok(Some(mesh)) = lm.tracer_mesh() {
        if mesh.positions.len() <= 1 << 20 {
            // What `level --terrain landmesh --heightmap` does: the file's own cell size, then the holes from
            // the heightmap, then the PNG. The grid itself is whatever size the mesh asks for (up to 16384 x 16384);
            // the steps after it only run on a grid of a few million samples
            if let Ok(mut grid) = HeightGrid::rasterize(&mesh, lm.grid_cell_size) {
                if grid.heights.len() <= 1 << 22 {
                    if let Some(hm) = hm.filter(|hm| hm.data.len() <= 1 << 22) {
                        let _ = grid.fill_from(&HeightGrid::from_heightmap(hm));
                    }
                    write_png(&grid);
                }
            }
        }
    }
    if let Ok(map) = lm.detail_map() {
        for cell in [0usize, 1, 2, 63, 1000] {
            let _ = (map.cell_blend_count(cell), map.cell_classes(cell));
        }
        if let Ok(w) = wt_unpack::landclass::GroundWeights::from_detail_map(&map, { let e = lm.extent(); [e[0], e[1]] }, 64.0) {
            let mut out = Vec::new();
            w.at(10.0, 10.0, &mut out);
            let _ = (w.texel(-5, 1 << 40), w.class_usage(), w.memory());
        }
    }
}

/// Counters and instances of a rendinst layer
pub fn use_layer(l: &RendInstLayer) {
    let _ = l.counts();
    let _ = l.instances();
}

/// The CRC32s of the Vorbis setup headers the library ships (a table of `"FVS1"`, count, then CRC, length and
/// packet per entry)
pub fn setup_crcs() -> &'static [u32] {
    static CRCS: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    CRCS.get_or_init(|| {
        let packed = include_bytes!("../../src/sound/vorbis_setups.bin.zst");
        let raw = zstd::decode_all(&packed[..]).expect("the shipped setup table unpacks");
        let count = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
        let (mut out, mut at) = (Vec::new(), 8);
        for _ in 0..count {
            out.push(u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()));
            let len = u32::from_le_bytes(raw[at + 4..at + 8].try_into().unwrap()) as usize;
            at += 8 + len;
        }
        out
    })
}
