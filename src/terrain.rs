//! Terrain products built from a level: a regular height grid (from the `HM2` heightmap, or by
//! rasterising the landmesh's collision mesh) and terrain meshes for glTF export.

use std::fs;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::json;

use crate::level::{Heightmap, TriMesh};
use crate::vehicle::{Alpha, Material, Mesh, Node, Vehicle};

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
        ensure!(cell_size.is_finite() && cell_size > 0.0, "cell size must be positive and finite, not {cell_size}");
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for (i, p) in mesh.positions.iter().enumerate() {
            ensure!(p.iter().all(|c| c.is_finite()), "vertex {i} is not finite: {p:?}");
            lo = [lo[0].min(p[0]), lo[1].min(p[2])];
            hi = [hi[0].max(p[0]), hi[1].max(p[2])];
        }
        ensure!(lo[0] <= hi[0], "the mesh has no vertices");
        if let Some(bad) = mesh.indices.iter().find(|&&i| i as usize >= mesh.positions.len()) {
            anyhow::bail!("a triangle uses vertex index {bad} of {}", mesh.positions.len());
        }
        // Counted in f64: with finite vertices a span can still overflow f32, and `as usize` saturates
        let cells = |lo: f32, hi: f32| (f64::from(hi) - f64::from(lo)) / f64::from(cell_size);
        let (w, h) = (cells(lo[0], hi[0]).floor() + 1.0, cells(lo[1], hi[1]).floor() + 1.0);
        ensure!(w <= 16384.0 && h <= 16384.0, "{w}x{h} grid is too large; use a bigger cell size");
        let (width, height) = (w as usize, h as usize);
        // Up to 16384 x 16384 samples (1 GiB) on a mesh that really spans that much: ask for the memory without
        // aborting when it is not there
        let mut heights = Vec::new();
        heights
            .try_reserve_exact(width * height)
            .with_context(|| format!("not enough memory for a {width}x{height} grid ({} bytes); use a bigger cell size", width * height * 4))?;
        heights.resize(width * height, f32::NAN);
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

    /// Height at a world position, interpolated between the four samples around it; `NaN` outside the
    /// grid or where any of them is uncovered
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let fx = (x - self.origin[0]) / self.cell_size;
        let fz = (z - self.origin[1]) / self.cell_size;
        if !(fx >= 0.0 && fz >= 0.0) || fx > (self.width - 1) as f32 || fz > (self.height - 1) as f32 {
            return f32::NAN;
        }
        let (x0, z0) = ((fx.floor() as usize).min(self.width.saturating_sub(2)), (fz.floor() as usize).min(self.height.saturating_sub(2)));
        let (ax, az) = (fx - x0 as f32, fz - z0 as f32);
        let at = |dx: usize, dz: usize| self.heights[(z0 + dz).min(self.height - 1) * self.width + (x0 + dx).min(self.width - 1)];
        let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
        (a * (1.0 - ax) + b * ax) * (1.0 - az) + (c * (1.0 - ax) + d * ax) * az
    }

    /// Like [`HeightGrid::height_at`], but a position up to one sample beyond the grid's edge takes the edge's
    /// height (the rasterised landmesh stops at its last vertex, short of the world box's edge)
    pub fn height_near(&self, x: f32, z: f32) -> f32 {
        let (w, h) = ((self.width - 1) as f32 * self.cell_size, (self.height - 1) as f32 * self.cell_size);
        // A grid built by hand can carry any cell size. Where an extent is negative, NaN or infinite there is no edge to
        // snap to, and `clamp` panics when its bounds are out of order or NaN
        if !(w >= 0.0 && h >= 0.0 && w.is_finite() && h.is_finite()) {
            return f32::NAN;
        }
        let (dx, dz) = (x - self.origin[0], z - self.origin[1]);
        if dx < -self.cell_size || dz < -self.cell_size || dx > w + self.cell_size || dz > h + self.cell_size {
            return f32::NAN;
        }
        self.height_at(self.origin[0] + dx.clamp(0.0, w), self.origin[1] + dz.clamp(0.0, h))
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
        meshes: vec![Mesh { positions, normals, uvs, indices, ..Mesh::default() }],
        materials: vec![Material {
            name: "terrain".into(),
            color: None,
            metal_rough: None,
            normal: None,
            double_sided: false,
            alpha: Alpha::Opaque,
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

    fn grid_of(width: usize, height: usize, cell_size: f32) -> HeightGrid {
        HeightGrid { width, height, cell_size, origin: [0.0, 0.0], heights: vec![1.0; width * height], source: "HM2" }
    }

    #[test]
    fn a_grid_with_a_cell_size_that_is_no_length_has_no_height_near_anything() {
        // `clamp` panics when its bounds (the extents, 0 .. (n - 1) * cell size) are out of order or NaN. A finite
        // query only gets past the range check when an extent is NaN or infinite; a NaN query gets past it whatever
        // the extents are, so each bad cell size is tried with both.
        for cell_size in [-2.0, f32::NEG_INFINITY, f32::NAN, f32::INFINITY] {
            for (width, height) in [(2, 2), (1, 2), (2, 1)] {
                let grid = grid_of(width, height, cell_size);
                assert!(grid.height_near(f32::NAN, f32::NAN).is_nan(), "{width}x{height} at {cell_size}, NaN query");
                assert!(grid.height_near(1.0, 1.0).is_nan(), "{width}x{height} at {cell_size}, finite query");
            }
        }
        // The extent in x is fine (0 x -2 = -0) and only the extent in z is negative: dropping the check of z shows here
        assert!(grid_of(1, 2, -2.0).height_near(f32::NAN, f32::NAN).is_nan());
        // A 2x1 grid with an infinite cell size: the extent in x is infinite and the one in z is 0 x inf = NaN, so a
        // finite query gets past the range check and `h >= 0.0` is what stops it. It says nothing about `is_finite`:
        // the cases of one infinite extent next to a fine one are in the test of the infinite extent
        assert!(grid_of(2, 1, f32::INFINITY).height_near(1.0, 1.0).is_nan());
    }

    #[test]
    fn a_grid_one_sample_thick_still_answers() {
        // A level that rasterises to 4 x 1 samples has an extent of 0 in z: that is a length, not "no length"
        let mut grid = grid_of(4, 1, 2.0);
        grid.heights = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(grid.height_near(3.0, 0.0), 2.5);
        // Up to one sample beyond the edge in z (2 m) takes the edge's height, farther out there is none
        assert_eq!(grid.height_near(3.0, 1.5), 2.5);
        assert_eq!(grid.height_near(3.0, -2.0), 2.5);
        assert!(grid.height_near(3.0, 2.5).is_nan());
        // Likewise a column
        let mut grid = grid_of(1, 4, 2.0);
        grid.heights = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(grid.height_near(0.5, 3.0), 2.5);
    }

    #[test]
    fn a_grid_with_an_infinite_extent_has_no_height_near_anything() {
        // A finite cell size can still overflow f32 over the grid (3 x 3e38); an extent that is not a position has no
        // edge to snap to, and answering with the first sample everywhere would be a made-up height
        for cell_size in [f32::INFINITY, 3.0e38] {
            let grid = grid_of(4, 4, cell_size);
            assert!(grid.height_near(5.0, 5.0).is_nan(), "{cell_size}");
        }
        // One infinite extent next to a fine one (3 x 3e38 overflows, 0 x 3e38 is 0): a finite query gets past the
        // range check and `clamp(0, inf)` does not panic, so only `is_finite` of the infinite one stops it. Each
        // `is_finite` is the only thing between its extent and a made-up height
        assert!(grid_of(4, 1, 3.0e38).height_near(5.0, 0.0).is_nan());
        assert!(grid_of(1, 4, 3.0e38).height_near(0.0, 5.0).is_nan());
        // A grid of one sample per side has no extent whatever the cell size, and answers; so does an extent that is
        // large but still finite (one cell of 3e38)
        assert_eq!(grid_of(1, 1, 3.0e38).height_near(0.0, 0.0), 1.0);
        assert_eq!(grid_of(2, 2, 3.0e38).height_near(1.0, 1.0), 1.0);
    }

    #[test]
    fn a_mesh_that_spans_more_than_the_old_sample_limit_is_rasterised() {
        // 4097 x 4097 samples are more than 2^24, the limit this crate once had: a mesh of three vertices spanning
        // 4096 m at 1 m asks for the whole grid (67 MB of heights, one allocation of exactly that size)
        let mesh = TriMesh { positions: vec![[0.0, 0.0, 0.0], [4096.0, 0.0, 4096.0], [0.0, 0.0, 4096.0]], indices: vec![0, 1, 2] };
        let (grid, largest) = crate::alloc_probe::largest_allocation(|| HeightGrid::rasterize(&mesh, 1.0));
        let grid = grid.unwrap();
        assert_eq!((grid.width, grid.height, grid.heights.len()), (4097, 4097, 4097 * 4097));
        assert!(grid.heights.len() > 1 << 24);
        assert_eq!(largest, 4097 * 4097 * 4);
        // The triangle covers the lower half of the square
        assert_eq!(grid.heights[0], 0.0);
        assert!(grid.heights[4096].is_nan());
    }

    #[test]
    fn a_grid_side_is_limited_to_16384_samples() {
        // A strip along x, then along z: each side has its own limit
        let strip = |span: f32, along_z: bool| {
            let end = if along_z { [0.0, 0.0, span] } else { [span, 0.0, 0.0] };
            TriMesh { positions: vec![[0.0, 0.0, 0.0], end, [0.0, 0.0, 0.0]], indices: vec![0, 1, 2] }
        };
        for along_z in [false, true] {
            let (grid, largest) = crate::alloc_probe::largest_allocation(|| HeightGrid::rasterize(&strip(16383.0, along_z), 1.0));
            let grid = grid.unwrap();
            assert_eq!((grid.width.max(grid.height), grid.width.min(grid.height), largest), (16384, 1, 16384 * 4), "along z: {along_z}");
            let (res, largest) = crate::alloc_probe::largest_allocation(|| HeightGrid::rasterize(&strip(16384.0, along_z), 1.0));
            assert!(res.err().is_some_and(|e| e.to_string().contains("too large")), "along z: {along_z}");
            assert!(largest < 16384 * 4, "asked for {largest} bytes");
        }
    }

    #[test]
    fn a_grid_that_does_not_fit_in_memory_is_an_error_not_an_abort() {
        // The system allocator is told to refuse any single allocation of 32 MB or more, as it would when the
        // memory is not there; a 4097 x 4097 grid is 67 MB
        let mesh = TriMesh { positions: vec![[0.0, 0.0, 0.0], [4096.0, 0.0, 4096.0], [0.0, 0.0, 4096.0]], indices: vec![0, 1, 2] };
        let res = crate::alloc_probe::refusing_from(32 << 20, || HeightGrid::rasterize(&mesh, 1.0));
        let msg = res.err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(msg.contains("not enough memory for a 4097x4097 grid"), "{msg}");
        // A grid under the threshold goes through
        assert!(crate::alloc_probe::refusing_from(32 << 20, || HeightGrid::rasterize(&mesh, 2.0)).is_ok());
    }

    #[test]
    fn the_largest_grid_is_asked_for_and_not_refused_by_a_limit() {
        // 16383 m at 1 m is 16384 x 16384 samples, the largest the per-side limit lets through: 1 GiB of heights. The
        // allocator is told to refuse anything from 512 MiB, so nothing is touched, and the error must be the
        // allocation's: a limit of any kind on the samples (below 16384 x 16384) would answer first with its own
        let mesh = TriMesh { positions: vec![[0.0, 0.0, 0.0], [16_383.0, 0.0, 16_383.0], [0.0, 0.0, 16_383.0]], indices: vec![0, 1, 2] };
        let res = crate::alloc_probe::refusing_from(1 << 29, || HeightGrid::rasterize(&mesh, 1.0));
        let msg = res.err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(msg.contains("not enough memory for a 16384x16384 grid"), "{msg}");
    }

    #[test]
    fn rasterizing_refuses_inputs_that_are_not_a_grid() {
        let msg = |mesh: &TriMesh, cell: f32| HeightGrid::rasterize(mesh, cell).err().map(|e| e.to_string()).unwrap_or_default();
        // An infinite vertex made (hi - lo) / cell infinite: `as usize` saturated and `+ 1` overflowed
        let mut mesh = square();
        mesh.positions[3][0] = f32::INFINITY;
        assert!(msg(&mesh, 1.0).contains("not finite"), "{}", msg(&mesh, 1.0));
        mesh.positions[3] = [f32::NAN, 5.0, 10.0];
        assert!(msg(&mesh, 1.0).contains("not finite"));
        // Finite, but a span of 6e38 metres: the f32 subtraction alone overflows
        let mut mesh = square();
        mesh.positions[0][0] = -3.0e38;
        mesh.positions[1][0] = 3.0e38;
        assert!(msg(&mesh, 16.0).contains("too large"), "{}", msg(&mesh, 16.0));
        // A cell size that is zero, negative, NaN or infinite
        for cell in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(msg(&square(), cell).contains("cell size"), "{cell}");
        }
        // A triangle that names a vertex the mesh does not have
        let mut mesh = square();
        mesh.indices[5] = 9;
        assert!(msg(&mesh, 1.0).contains("index"), "{}", msg(&mesh, 1.0));
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
    fn heights_interpolate_and_stop_at_the_edge() {
        let mut mesh = square();
        mesh.positions[3][1] = 15.0;
        let grid = HeightGrid::rasterize(&mesh, 5.0).unwrap();
        assert_eq!(grid.height_at(5.0, 5.0), 5.0);
        assert_eq!(grid.height_at(5.0, 10.0), 10.0);
        assert_eq!(grid.height_at(10.0, 10.0), 15.0);
        assert!((grid.height_at(7.5, 5.0) - 7.5).abs() < 1e-5);
        assert!(grid.height_at(-0.1, 5.0).is_nan() && grid.height_at(5.0, 10.1).is_nan());
        // Within one sample of the edge the edge's height stands in; farther out there is none
        assert_eq!(grid.height_near(-1.0, 5.0), grid.height_at(0.0, 5.0));
        assert_eq!(grid.height_near(5.0, 14.0), grid.height_at(5.0, 10.0));
        assert!(grid.height_near(-5.1, 5.0).is_nan());
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
