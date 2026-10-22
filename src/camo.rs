//! Bakes a tank's camouflage into its base colour texture.
//!
//! Dagor's masked tank materials (`dynamic_masked_tank`) take a base texture whose alpha marks the
//! painted areas and a camouflage texture mapped through the mesh's second UV set. The game's shader
//! is not public, so this approximates it: painted texels get the camouflage colour modulated by the
//! base texture's detail (dirt, scratches); unpainted texels keep the base colour.
//!
//! The ambient occlusion map is laid out on the same second UV set (a unique, non-overlapping unwrap),
//! while the base texture is a tiling detail atlas on the first set; the occlusion darkens it.
//!
//! The base texture is rasterised triangle by triangle in its own UV space, so each texel can look up
//! the camouflage and occlusion through the second UV set. Texels no triangle covers get the
//! camouflage's average and no occlusion.

/// One triangle: per vertex `[u0, v0, u1, v1]`, already in texture units (0..1 per repeat)
pub type UvTriangle = [[f32; 4]; 3];

pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub rgba: &'a [u8],
}

impl Image<'_> {
    /// Bilinear sample with wrapping, in 0..1 texture units
    fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let wrap = |i: f32, n: usize| (i as i64).rem_euclid(n as i64) as usize;
        let px = |xi: f32, yi: f32| {
            let o = (wrap(yi, self.height) * self.width + wrap(xi, self.width)) * 4;
            [0, 1, 2].map(|k| f32::from(self.rgba[o + k]))
        };
        let (a, b, c, d) = (px(x0, y0), px(x0 + 1.0, y0), px(x0, y0 + 1.0), px(x0 + 1.0, y0 + 1.0));
        [0, 1, 2].map(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            top + (bottom - top) * fy
        })
    }

    fn mean(&self) -> [f32; 3] {
        let n = (self.rgba.len() / 4).max(1) as f64;
        let mut s = [0f64; 3];
        for p in self.rgba.chunks_exact(4) {
            for k in 0..3 {
                s[k] += f64::from(p[k]);
            }
        }
        s.map(|x| (x / n) as f32)
    }
}

pub fn bake(
    base: &mut [u8],
    width: usize,
    height: usize,
    camo: Option<&Image>,
    occlusion: Option<&Image>,
    triangles: &[UvTriangle],
    opaque: bool,
) {
    let original = base.to_vec();
    let luma = |p: &[u8]| (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0;

    // The average detail of fully painted areas maps to the plain camouflage colour
    let (mut sum, mut n) = (0f64, 0f64);
    for p in original.chunks_exact(4).filter(|p| p[3] > 200) {
        sum += f64::from(luma(p));
        n += 1.0;
    }
    let painted_luma = if n > 0.0 { (sum / n) as f32 } else { 128.0 };

    // `colour`: camouflage colour at this texel (None: keep the base colour); `ao`: occlusion factor
    let paint = |o: usize, colour: Option<[f32; 3]>, ao: f32, out: &mut [u8]| {
        let p = &original[o..o + 4];
        let mask = f32::from(p[3]) / 255.0;
        let detail = luma(p) / painted_luma.max(1.0);
        for k in 0..3 {
            let base_k = f32::from(p[k]);
            let mixed = match colour {
                Some(c) => base_k * (1.0 - mask) + (c[k] * detail).min(255.0) * mask,
                None => base_k,
            };
            out[o + k] = (mixed * ao).round().clamp(0.0, 255.0) as u8;
        }
        if opaque {
            out[o + 3] = 255;
        }
    };

    // Fallback for texels outside every triangle
    let mean = camo.map(Image::mean);
    for o in (0..original.len()).step_by(4) {
        paint(o, mean, 1.0, base);
    }

    for tri in triangles {
        let pts = tri.map(|v| [v[0] * width as f32, v[1] * height as f32]);
        let area = (pts[1][0] - pts[0][0]) * (pts[2][1] - pts[0][1]) - (pts[2][0] - pts[0][0]) * (pts[1][1] - pts[0][1]);
        if area.abs() < 1e-6 {
            continue;
        }
        let min_x = pts.iter().map(|p| p[0]).fold(f32::MAX, f32::min).floor() as i64;
        let max_x = pts.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil() as i64;
        let min_y = pts.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor() as i64;
        let max_y = pts.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as i64;
        // Skip degenerate mappings (e.g. huge or broken UVs) instead of looping over millions of texels
        if (max_x - min_x) * (max_y - min_y) > (width * height) as i64 {
            continue;
        }
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((pts[1][0] - cx) * (pts[2][1] - cy) - (pts[2][0] - cx) * (pts[1][1] - cy)) / area;
                let w1 = ((pts[2][0] - cx) * (pts[0][1] - cy) - (pts[0][0] - cx) * (pts[2][1] - cy)) / area;
                let w2 = 1.0 - w0 - w1;
                // Slightly generous edges so seams between triangles are covered
                if w0 < -0.01 || w1 < -0.01 || w2 < -0.01 {
                    continue;
                }
                let u = w0 * tri[0][2] + w1 * tri[1][2] + w2 * tri[2][2];
                let v = w0 * tri[0][3] + w1 * tri[1][3] + w2 * tri[2][3];
                let tx = x.rem_euclid(width as i64) as usize;
                let ty = y.rem_euclid(height as i64) as usize;
                let ao = occlusion.map_or(1.0, |img| img.sample(u, v)[0] / 255.0);
                paint((ty * width + tx) * 4, camo.map(|c| c.sample(u, v)), ao, base);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2 RGBA image of one colour
    fn solid(rgba: [u8; 4]) -> Vec<u8> {
        rgba.repeat(4)
    }

    /// Two triangles covering the whole texture, mapping UV1 onto the same square
    fn full_square() -> Vec<UvTriangle> {
        let c = |u: f32, v: f32| [u, v, u, v];
        vec![[c(0., 0.), c(1., 0.), c(1., 1.)], [c(0., 0.), c(1., 1.), c(0., 1.)]]
    }

    #[test]
    fn painted_texels_take_the_camouflage_colour() {
        // Fully painted base whose detail equals the painted average: result is the camouflage colour
        let mut base = solid([100, 100, 100, 255]);
        let camo = solid([80, 70, 50, 255]);
        let camo = Image { width: 2, height: 2, rgba: &camo };
        bake(&mut base, 2, 2, Some(&camo), None, &full_square(), true);
        assert!(base.chunks_exact(4).all(|p| p == [80, 70, 50, 255]), "{base:?}");
    }

    #[test]
    fn unpainted_texels_keep_the_base_and_occlusion_darkens() {
        let mut base = solid([120, 60, 30, 0]);
        let ao = solid([128, 128, 128, 255]);
        let ao = Image { width: 2, height: 2, rgba: &ao };
        bake(&mut base, 2, 2, None, Some(&ao), &full_square(), false);
        // Alpha 0 means no paint; 128/255 occlusion roughly halves the colour; alpha is kept
        assert!(base.chunks_exact(4).all(|p| p == [60, 30, 15, 0]), "{base:?}");
    }

    #[test]
    fn uncovered_texels_get_the_average_camouflage() {
        let mut base = solid([100, 100, 100, 255]);
        let camo: Vec<u8> = [[0, 0, 0, 255], [200, 100, 0, 255], [0, 0, 0, 255], [200, 100, 0, 255]].concat();
        let camo = Image { width: 2, height: 2, rgba: &camo };
        bake(&mut base, 2, 2, Some(&camo), None, &[], true);
        assert!(base.chunks_exact(4).all(|p| p == [100, 50, 0, 255]), "{base:?}");
    }
}
