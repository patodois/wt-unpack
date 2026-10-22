//! Terrain products built from a level: a regular height grid (from the `HM2` heightmap, or by
//! rasterising the landmesh's collision mesh) and terrain meshes for glTF export.

use std::fs;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::json;

use crate::level::{Heightmap, TriMesh};
use crate::vehicle::{Material, Mesh, Node, Vehicle};

/// Heights in metres on a regular grid in Dagor's world space: sample (x, y) is at world
/// `origin + (x, y) * cell_size` in X and Z. `NaN` marks samples nothing covers.
pub struct HeightGrid {
    pub width: usize,
    pub height: usize,
    pub cell_size: f32,
    pub origin: [f32; 2],
    pub heights: Vec<f32>,
    /// Where the heights come from, e.g. `HM2`
    pub source: &'static str,
}

impl HeightGrid {
    pub fn from_heightmap(hm: &Heightmap) -> Self {
        HeightGrid {
            width: hm.width as usize,
            height: hm.height as usize,
            cell_size: hm.cell_size,
            origin: hm.origin,
            heights: hm.data.iter().map(|&v| hm.metres(v)).collect(),
            source: "HM2",
        }
    }

    /// Rasterises a terrain mesh seen from above, keeping the highest surface at each sample
    pub fn rasterize(mesh: &TriMesh, cell_size: f32) -> Result<Self> {
        ensure!(cell_size > 0.0, "cell size must be positive");
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &mesh.positions {
            lo = [lo[0].min(p[0]), lo[1].min(p[2])];
            hi = [hi[0].max(p[0]), hi[1].max(p[2])];
        }
        ensure!(lo[0] <= hi[0], "the mesh has no vertices");
        let width = ((hi[0] - lo[0]) / cell_size).floor() as usize + 1;
        let height = ((hi[1] - lo[1]) / cell_size).floor() as usize + 1;
        ensure!(width <= 16384 && height <= 16384, "{width}x{height} grid is too large; use a bigger cell size");
        let mut heights = vec![f32::NAN; width * height];
        for tri in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| mesh.positions[tri[k] as usize]);
            // Grid coordinates of the corners
            let g = |p: [f32; 3]| [(p[0] - lo[0]) / cell_size, (p[2] - lo[1]) / cell_size, p[1]];
            let (a, b, c) = (g(a), g(b), g(c));
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if area.abs() < 1e-12 {
                continue;
            }
            let x0 = a[0].min(b[0]).min(c[0]).ceil().max(0.0) as usize;
            let x1 = (a[0].max(b[0]).max(c[0]).floor() as usize).min(width - 1);
            let y0 = a[1].min(b[1]).min(c[1]).ceil().max(0.0) as usize;
            let y1 = (a[1].max(b[1]).max(c[1]).floor() as usize).min(height - 1);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f32, y as f32);
                    let wb = ((px - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (py - a[1])) / area;
                    let wc = ((b[0] - a[0]) * (py - a[1]) - (px - a[0]) * (b[1] - a[1])) / area;
                    let wa = 1.0 - wb - wc;
                    const EPS: f32 = -1e-5;
                    if wa < EPS || wb < EPS || wc < EPS {
                        continue;
                    }
                    let h = wa * a[2] + wb * b[2] + wc * c[2];
                    let cell = &mut heights[y * width + x];
                    if cell.is_nan() || h > *cell {
                        *cell = h;
                    }
                }
            }
        }
        Ok(HeightGrid { width, height, cell_size, origin: lo, heights, source: "landmesh ray tracer" })
    }

    /// Fills uncovered samples from another grid (nearest sample), e.g. the hole a landmesh leaves
    /// where the `HM2` heightmap takes over. Returns how many samples were filled.
    pub fn fill_from(&mut self, other: &HeightGrid) -> usize {
        let mut filled = 0;
        for y in 0..self.height {
            for x in 0..self.width {
                let i = y * self.width + x;
                if !self.heights[i].is_nan() {
                    continue;
                }
                let wx = self.origin[0] + x as f32 * self.cell_size;
                let wz = self.origin[1] + y as f32 * self.cell_size;
                let ox = ((wx - other.origin[0]) / other.cell_size).round();
                let oy = ((wz - other.origin[1]) / other.cell_size).round();
                if ox >= 0.0 && oy >= 0.0 && (ox as usize) < other.width && (oy as usize) < other.height {
                    let h = other.heights[oy as usize * other.width + ox as usize];
                    if !h.is_nan() {
                        self.heights[i] = h;
                        filled += 1;
                    }
                }
            }
        }
        if filled > 0 {
            self.source = "landmesh ray tracer + HM2";
        }
        filled
    }

    /// Lowest and highest covered height, if any sample is covered
    pub fn range(&self) -> Option<(f32, f32)> {
        let mut it = self.heights.iter().copied().filter(|h| !h.is_nan());
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), h| (lo.min(h), hi.max(h))))
    }

    pub fn covered(&self) -> usize {
        self.heights.iter().filter(|h| !h.is_nan()).count()
    }

    /// Writes a 16-bit grayscale PNG (row 0 = lowest world Z, uncovered samples 0) and a JSON next to
    /// it with the world placement and the mapping from pixel values to metres
    pub fn write_png(&self, path: &Path) -> Result<()> {
        let (lo, hi) = self.range().context("the height grid covers nothing")?;
        let span = (hi - lo).max(1e-3);
        let mut samples = Vec::with_capacity(self.heights.len() * 2);
        for &h in &self.heights {
            let v = if h.is_nan() { 0 } else { (((h - lo) / span) * 65535.0).round().clamp(0.0, 65535.0) as u16 };
            samples.extend_from_slice(&v.to_be_bytes());
        }
        let mut buf = Vec::new();
        let mut enc = png::Encoder::new(&mut buf, self.width as u32, self.height as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Sixteen);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&samples)?;
        writer.finish()?;
        fs::write(path, buf).with_context(|| format!("writing {}", path.display()))?;

        let size = [self.width as f32 * self.cell_size, self.height as f32 * self.cell_size];
        let meta = json!({
            "source": self.source,
            "width": self.width,
            "height": self.height,
            "cell_size": self.cell_size,
            "origin_x": self.origin[0],
            "origin_z": self.origin[1],
            "world_size_x": size[0],
            "world_size_z": size[1],
            "height_min": lo,
            "height_max": hi,
            "covered_fraction": self.covered() as f64 / self.heights.len() as f64,
            "note": "pixel (x, y) is at world X = origin_x + x * cell_size, Z = origin_z + y * cell_size; \
                     metres = height_min + (height_max - height_min) * value / 65535; value 0 also marks uncovered samples",
        });
        let json_path = path.with_extension("json");
        fs::write(&json_path, serde_json::to_vec_pretty(&meta)?).with_context(|| format!("writing {}", json_path.display()))?;
        Ok(())
    }

    /// A grid mesh of every `step`-th sample, skipping quads with uncovered corners
    pub fn mesh(&self, step: usize) -> TriMesh {
        let step = step.max(1);
        let (nx, ny) = ((self.width - 1) / step + 1, (self.height - 1) / step + 1);
        let mut mesh = TriMesh::default();
        let mut index = vec![u32::MAX; nx * ny];
        for gy in 0..ny {
            for gx in 0..nx {
                let (x, y) = (gx * step, gy * step);
                let h = self.heights[y * self.width + x];
                if !h.is_nan() {
                    index[gy * nx + gx] = mesh.positions.len() as u32;
                    let wx = self.origin[0] + x as f32 * self.cell_size;
                    let wz = self.origin[1] + y as f32 * self.cell_size;
                    mesh.positions.push([wx, h, wz]);
                }
            }
        }
        for gy in 0..ny.saturating_sub(1) {
            for gx in 0..nx - 1 {
                let i = |x: usize, y: usize| index[y * nx + x];
                let q = [i(gx, gy), i(gx + 1, gy), i(gx, gy + 1), i(gx + 1, gy + 1)];
                if q.contains(&u32::MAX) {
                    continue;
                }
                // Counter-clockwise from above in Dagor's left-handed space
                mesh.indices.extend_from_slice(&[q[0], q[2], q[1], q[1], q[2], q[3]]);
            }
        }
        mesh
    }
}

/// Wraps a terrain mesh as a one-material [`Vehicle`] for the glTF writer: right-handed (Z mirrored),
/// every triangle turned to face up (the ray tracer streams do not keep one winding), smooth normals,
/// UVs spanning the mesh's XZ extent
pub fn to_model(name: &str, mesh: &TriMesh) -> Vehicle {
    let positions: Vec<[f32; 3]> = mesh.positions.iter().map(|p| [p[0], p[1], -p[2]]).collect();
    let face_normal = |t: &[u32]| {
        let [a, b, c] = [0, 1, 2].map(|k| positions[t[k] as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
    };
    let mut indices = mesh.indices.clone();
    for t in indices.chunks_exact_mut(3) {
        if face_normal(t)[1] < 0.0 {
            t.swap(1, 2);
        }
    }
    let mut normals = vec![[0.0f32; 3]; positions.len()];
    for t in indices.chunks_exact(3) {
        let n = face_normal(t);
        for &i in t {
            for k in 0..3 {
                normals[i as usize][k] += n[k];
            }
        }
    }
    for n in &mut normals {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        *n = if len > 0.0 { [n[0] / len, n[1] / len, n[2] / len] } else { [0.0, 1.0, 0.0] };
    }
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in &positions {
        lo = [lo[0].min(p[0]), lo[1].min(p[2])];
        hi = [hi[0].max(p[0]), hi[1].max(p[2])];
    }
    let span = [(hi[0] - lo[0]).max(1e-3), (hi[1] - lo[1]).max(1e-3)];
    let uvs = positions.iter().map(|p| [(p[0] - lo[0]) / span[0], (p[2] - lo[1]) / span[1]]).collect();
    Vehicle {
        name: name.to_owned(),
        meshes: vec![Mesh { material: 0, node: 0, positions, normals, uvs, indices }],
        materials: vec![Material {
            name: "terrain".into(),
            color: None,
            metal_rough: None,
            normal: None,
            double_sided: false,
            alpha_mask: false,
            color_key: None,
            pbr_key: None,
        }],
        nodes: vec![Node { name: "terrain".into(), parent: None, pivot: [0.0; 3] }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 x 10 m square at height 5, split along its diagonal
    fn square() -> TriMesh {
        TriMesh {
            positions: vec![[0.0, 5.0, 0.0], [10.0, 5.0, 0.0], [0.0, 5.0, 10.0], [10.0, 5.0, 10.0]],
            indices: vec![0, 2, 1, 1, 2, 3],
        }
    }

    #[test]
    fn rasterizing_covers_the_whole_square() {
        let grid = HeightGrid::rasterize(&square(), 1.0).unwrap();
        assert_eq!((grid.width, grid.height), (11, 11));
        assert_eq!(grid.covered(), 121);
        assert_eq!(grid.range(), Some((5.0, 5.0)));
    }

    #[test]
    fn rasterizing_interpolates_and_keeps_the_top_surface() {
        let mut mesh = square();
        mesh.positions[3][1] = 15.0;
        // A second, lower copy must not show through
        mesh.positions.extend([[0.0, -5.0, 0.0], [10.0, -5.0, 0.0], [0.0, -5.0, 10.0]]);
        mesh.indices.extend([4, 6, 5]);
        let grid = HeightGrid::rasterize(&mesh, 5.0).unwrap();
        assert_eq!(grid.heights, [5.0, 5.0, 5.0, 5.0, 5.0, 10.0, 5.0, 10.0, 15.0]);
    }

    #[test]
    fn holes_fill_from_a_finer_grid() {
        let mut coarse = HeightGrid::rasterize(&square(), 5.0).unwrap();
        coarse.heights[4] = f32::NAN;
        let mut fine = HeightGrid::rasterize(&square(), 1.0).unwrap();
        fine.heights[5 * 11 + 5] = 7.0;
        assert_eq!(coarse.fill_from(&fine), 1);
        assert_eq!(coarse.heights[4], 7.0);
        assert_eq!(coarse.covered(), 9);
    }

    #[test]
    fn grid_mesh_skips_uncovered_quads() {
        let mut grid = HeightGrid::rasterize(&square(), 5.0).unwrap();
        assert_eq!(grid.mesh(1).indices.len(), 4 * 6);
        assert_eq!(grid.mesh(2).indices.len(), 6);
        grid.heights[0] = f32::NAN;
        let mesh = grid.mesh(1);
        assert_eq!((mesh.positions.len(), mesh.indices.len()), (8, 3 * 6));
    }

    #[test]
    fn model_faces_up_in_gltf_space() {
        let model = to_model("t", &square());
        let m = &model.meshes[0];
        assert!(m.normals.iter().all(|n| (n[1] - 1.0).abs() < 1e-6), "{:?}", m.normals);
        assert_eq!(m.uvs[3], [1.0, 0.0]);
    }

    #[test]
    fn png_and_json_are_written() {
        let out = std::env::temp_dir().join(format!("wt-unpack-terrain-test-{}", std::process::id()));
        fs::create_dir_all(&out).unwrap();
        let mut grid = HeightGrid::rasterize(&square(), 5.0).unwrap();
        grid.heights[8] = 25.0;
        let png_path = out.join("h.png");
        grid.write_png(&png_path).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(&png_path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height, info.bit_depth), (3, 3, png::BitDepth::Sixteen));
        assert_eq!(&buf[16..18], &[0xFF, 0xFF]);
        let meta: serde_json::Value = serde_json::from_slice(&fs::read(out.join("h.json")).unwrap()).unwrap();
        assert_eq!(meta["height_max"], 25.0);
        assert_eq!(meta["world_size_x"], 15.0);
        fs::remove_dir_all(&out).unwrap();
    }
}
