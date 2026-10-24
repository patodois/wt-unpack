//! Renders a level's rewrite-bridge scene from a camera with a small software rasteriser, to check the
//! scene by eye. It uses only the scene the provider returns, the way any other app would.
//!
//! The picture is the scene's albedo with a headlight (60% flat, 40% by the surface's angle to the
//! camera), on a flat background: the game's sun, sky and fog are not in the data, so none is drawn.
//!
//! ```sh
//! cargo run --release --features bridge --example render_scene -- \
//!   $GAME levels/avg_berlin.bin out.png --eye 2200,420,300 --at 2200,40,2000 --size 1280x720
//! ```
//!
//! `--eye` and `--at` are in the game's world axes (x, height, z), `--fov` is vertical degrees. Several
//! pictures from one load: `--view "out.png|x,y,z|x,y,z|fov"`, repeated.
//!
//! `--leaves` also draws the leaf cards of trees and bushes, which a `Scene` cannot hold because they face
//! the camera: they come from `WarThunder::load_level_with_leaves`, and each card is expanded for the
//! view being drawn, the way the game does with the wind at rest (`LeafCard::corners_facing`).

use std::path::PathBuf;
use std::time::Instant;

use rewrite_bridge::{AlphaMode, Instance, Model, Provider, Scene, SceneModel};
use wt_unpack::bridge::WarThunder;
use wt_unpack::scene::SceneLeaves;

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-20);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// Quaternion x, y, z, w rotates v
fn rotate(q: [f32; 4], v: V3) -> V3 {
    let u = [q[0], q[1], q[2]];
    let uv = cross(u, v);
    let uuv = cross(u, uv);
    [0, 1, 2].map(|k| v[k] + 2.0 * (q[3] * uv[k] + uuv[k]))
}

struct Camera {
    eye: V3,
    right: V3,
    up: V3,
    forward: V3,
    focal: f32,
    w: usize,
    h: usize,
}

impl Camera {
    fn view(&self, p: V3) -> V3 {
        let d = sub(p, self.eye);
        [dot(d, self.right), dot(d, self.up), dot(d, self.forward)]
    }
}

/// A vertex in view space with its attributes
#[derive(Clone, Copy)]
struct Vtx {
    p: V3,
    n: V3,
    uv: [f32; 2],
}

fn lerp_vtx(a: &Vtx, b: &Vtx, t: f32) -> Vtx {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Vtx { p: [l(a.p[0], b.p[0]), l(a.p[1], b.p[1]), l(a.p[2], b.p[2])], n: [l(a.n[0], b.n[0]), l(a.n[1], b.n[1]), l(a.n[2], b.n[2])], uv: [l(a.uv[0], b.uv[0]), l(a.uv[1], b.uv[1])] }
}

struct Band {
    y0: usize,
    y1: usize,
    color: Vec<[u8; 3]>,
    depth: Vec<f32>,
}

struct Tex<'a> {
    w: usize,
    h: usize,
    rgba: &'a [u8],
    /// Edges clamp instead of wrapping (terrain tiles)
    clamp: bool,
}

const NEAR: f32 = 0.5;

#[allow(clippy::too_many_arguments)]
fn raster(band: &mut Band, cam: &Camera, tri: [Vtx; 3], tex: Option<&Tex>, base: [f32; 4], mask: Option<f32>, double_sided: bool) {
    let proj = |v: &Vtx| [cam.w as f32 * 0.5 + v.p[0] / v.p[2] * cam.focal, cam.h as f32 * 0.5 - v.p[1] / v.p[2] * cam.focal];
    let s = [proj(&tri[0]), proj(&tri[1]), proj(&tri[2])];
    let area = (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
    if area == 0.0 {
        return;
    }
    // Screen y points down, so a counter-clockwise triangle seen from the front has a negative area
    if area > 0.0 && !double_sided {
        return;
    }
    let (minx, maxx) = (s[0][0].min(s[1][0]).min(s[2][0]), s[0][0].max(s[1][0]).max(s[2][0]));
    let (miny, maxy) = (s[0][1].min(s[1][1]).min(s[2][1]), s[0][1].max(s[1][1]).max(s[2][1]));
    let y_lo = (miny.floor().max(band.y0 as f32)) as i64;
    let y_hi = (maxy.ceil().min(band.y1 as f32 - 1.0)) as i64;
    let x_lo = minx.floor().max(0.0) as i64;
    let x_hi = maxx.ceil().min(cam.w as f32 - 1.0) as i64;
    if y_lo > y_hi || x_lo > x_hi {
        return;
    }
    let inv_area = 1.0 / area;
    let iw = [1.0 / tri[0].p[2], 1.0 / tri[1].p[2], 1.0 / tri[2].p[2]];
    for y in y_lo..=y_hi {
        let py = y as f32 + 0.5;
        for x in x_lo..=x_hi {
            let px = x as f32 + 0.5;
            let w0 = ((s[1][0] - px) * (s[2][1] - py) - (s[2][0] - px) * (s[1][1] - py)) * inv_area;
            let w1 = ((s[2][0] - px) * (s[0][1] - py) - (s[0][0] - px) * (s[2][1] - py)) * inv_area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            // Perspective-correct weights
            let (a, b, c) = (w0 * iw[0], w1 * iw[1], w2 * iw[2]);
            let inv = 1.0 / (a + b + c);
            let z = inv;
            let idx = (y as usize - band.y0) * cam.w + x as usize;
            if z >= band.depth[idx] {
                continue;
            }
            let (a, b, c) = (a * inv, b * inv, c * inv);
            let mut rgb = [base[0], base[1], base[2]];
            if let Some(t) = tex {
                let u = tri[0].uv[0] * a + tri[1].uv[0] * b + tri[2].uv[0] * c;
                let v = tri[0].uv[1] * a + tri[1].uv[1] * b + tri[2].uv[1] * c;
                // Bilinear, wrapping
                let fx = (u - u.floor()) * t.w as f32 - 0.5;
                let fy = (v - v.floor()) * t.h as f32 - 0.5;
                let (x0, y0) = (fx.floor(), fy.floor());
                let (ax, ay) = (fx - x0, fy - y0);
                let clamp = t.clamp;
                let wrap = |i: f32, n: usize| if clamp { (i as i64).clamp(0, n as i64 - 1) as usize } else { (i as i64).rem_euclid(n as i64) as usize };
                let mut acc = [0.0f32; 4];
                for (dx, dy, wt) in [(0.0, 0.0, (1.0 - ax) * (1.0 - ay)), (1.0, 0.0, ax * (1.0 - ay)), (0.0, 1.0, (1.0 - ax) * ay), (1.0, 1.0, ax * ay)] {
                    let i = (wrap(y0 + dy, t.h) * t.w + wrap(x0 + dx, t.w)) * 4;
                    for k in 0..4 {
                        acc[k] += wt * f32::from(t.rgba[i + k]);
                    }
                }
                if let Some(cut) = mask {
                    if acc[3] / 255.0 < cut {
                        continue;
                    }
                }
                for k in 0..3 {
                    rgb[k] *= acc[k] / 255.0;
                }
            }
            let n = norm([tri[0].n[0] * a + tri[1].n[0] * b + tri[2].n[0] * c, tri[0].n[1] * a + tri[1].n[1] * b + tri[2].n[1] * c, tri[0].n[2] * a + tri[1].n[2] * b + tri[2].n[2] * c]);
            let p = [tri[0].p[0] * a + tri[1].p[0] * b + tri[2].p[0] * c, tri[0].p[1] * a + tri[1].p[1] * b + tri[2].p[1] * c, tri[0].p[2] * a + tri[1].p[2] * b + tri[2].p[2] * c];
            let facing = dot(n, norm(p)).abs();
            let shade = 0.6 + 0.4 * facing;
            band.depth[idx] = z;
            band.color[idx] = [0, 1, 2].map(|k| (rgb[k] * shade * 255.0).clamp(0.0, 255.0) as u8);
        }
    }
}

/// Clips a view-space triangle against the near plane and rasterises the pieces
#[allow(clippy::too_many_arguments)]
fn draw_tri(band: &mut Band, cam: &Camera, t: [Vtx; 3], tex: Option<&Tex>, base: [f32; 4], mask: Option<f32>, double_sided: bool) {
    let inside = [t[0].p[2] >= NEAR, t[1].p[2] >= NEAR, t[2].p[2] >= NEAR];
    let n_in = inside.iter().filter(|&&b| b).count();
    match n_in {
        0 => {}
        3 => raster(band, cam, t, tex, base, mask, double_sided),
        _ => {
            let mut poly = Vec::new();
            for i in 0..3 {
                let (a, b) = (&t[i], &t[(i + 1) % 3]);
                let (ia, ib) = (inside[i], inside[(i + 1) % 3]);
                if ia {
                    poly.push(*a);
                }
                if ia != ib {
                    let f = (NEAR - a.p[2]) / (b.p[2] - a.p[2]);
                    poly.push(lerp_vtx(a, b, f));
                }
            }
            for i in 1..poly.len().saturating_sub(1) {
                raster(band, cam, [poly[0], poly[i], poly[i + 1]], tex, base, mask, double_sided);
            }
        }
    }
}

/// Bounding sphere of a model: centre and radius, in the model's space
fn bounds(m: &Model) -> (V3, f32) {
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for mesh in &m.meshes {
        for p in &mesh.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    if lo[0] > hi[0] {
        return ([0.0; 3], 0.0);
    }
    let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    (c, len(sub(hi, c)))
}

fn len(a: V3) -> f32 {
    dot(a, a).sqrt()
}

fn render(scene: &Scene, leaves: Option<&SceneLeaves>, cam: &Camera, threads: usize, background: [u8; 3]) -> Vec<[u8; 3]> {
    let bounds: Vec<Option<(V3, f32)>> = scene.models.iter().map(|m| if let SceneModel::Inline(m) = m { Some(bounds(m)) } else { None }).collect();
    let rows = cam.h.div_ceil(threads);
    let mut bands: Vec<Band> = (0..threads)
        .map(|i| {
            let (y0, y1) = (i * rows, ((i + 1) * rows).min(cam.h));
            Band { y0, y1, color: vec![background; (y1.saturating_sub(y0)) * cam.w], depth: vec![f32::MAX; (y1.saturating_sub(y0)) * cam.w] }
        })
        .collect();
    let half_tan = (cam.h as f32 * 0.5) / cam.focal;
    let half_tan_x = (cam.w as f32 * 0.5) / cam.focal;
    // How far any corner of a model's leaf cards can be from the model's origin, for culling
    let leaf_radius: Vec<f32> = leaves
        .map(|l| l.models.iter().map(|m| m.sets.iter().flat_map(|s| &s.cards).map(|c| len(c.centre) + c.corners.iter().map(|k| len(k.offset)).fold(0.0, f32::max)).fold(0.0, f32::max)).collect())
        .unwrap_or_default();
    std::thread::scope(|s| {
        for band in bands.iter_mut() {
            let bounds = &bounds;
            let leaf_radius = &leaf_radius;
            s.spawn(move || {
                for inst in &scene.instances {
                    draw_instance(band, scene, cam, inst, bounds[inst.model], half_tan, half_tan_x);
                }
                if let Some(leaves) = leaves {
                    for inst in &leaves.instances {
                        draw_leaves(band, leaves, cam, inst, leaf_radius[inst.model], half_tan, half_tan_x);
                    }
                }
            });
        }
    });
    bands.into_iter().flat_map(|b| b.color).collect()
}

fn draw_instance(band: &mut Band, scene: &Scene, cam: &Camera, inst: &Instance, bound: Option<(V3, f32)>, half_tan: f32, half_tan_x: f32) {
    let SceneModel::Inline(model) = &scene.models[inst.model] else { return };
    let t = &inst.transform;
    let (c, r) = bound.unwrap_or(([0.0; 3], 0.0));
    let scale = t.scale[0].abs().max(t.scale[1].abs()).max(t.scale[2].abs());
    let world_c = {
        let s = [c[0] * t.scale[0], c[1] * t.scale[1], c[2] * t.scale[2]];
        let r = rotate(t.rotation, s);
        [r[0] + t.translation[0], r[1] + t.translation[1], r[2] + t.translation[2]]
    };
    let vc = cam.view(world_c);
    let rad = r * scale;
    let is_ground = model.name == "terrain" || model.name == "water";
    if !is_ground {
        if vc[2] + rad < NEAR || vc[2] <= 0.0 && vc[2] + rad < NEAR {
            return;
        }
        if vc[0].abs() > vc[2].max(0.0) * half_tan_x + rad || vc[1].abs() > vc[2].max(0.0) * half_tan + rad {
            return;
        }
        // Smaller than half a pixel
        if vc[2] > 0.0 && rad / vc[2] * cam.focal < 0.5 {
            return;
        }
    }
    for mesh in &model.meshes {
        let material = &model.materials[mesh.material];
        let tex = material.base_color.as_ref().map(|i| Tex { w: i.width as usize, h: i.height as usize, rgba: &i.rgba, clamp: is_ground });
        let mask = if let AlphaMode::Mask(c) = material.alpha { Some(c) } else { None };
        let verts: Vec<Vtx> = (0..mesh.positions.len())
            .map(|i| {
                let p = mesh.positions[i];
                let s = [p[0] * t.scale[0], p[1] * t.scale[1], p[2] * t.scale[2]];
                let w = rotate(t.rotation, s);
                let w = [w[0] + t.translation[0], w[1] + t.translation[1], w[2] + t.translation[2]];
                let n = mesh.normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
                let nw = rotate(t.rotation, n);
                Vtx { p: cam.view(w), n: [dot(nw, cam.right), dot(nw, cam.up), dot(nw, cam.forward)], uv: mesh.uvs.get(i).copied().unwrap_or([0.0; 2]) }
            })
            .collect();
        let mirrored = (t.scale[0] < 0.0) as u8 + (t.scale[1] < 0.0) as u8 + (t.scale[2] < 0.0) as u8 & 1 == 1;
        for tri in mesh.indices.chunks_exact(3) {
            let mut v = [verts[tri[0] as usize], verts[tri[1] as usize], verts[tri[2] as usize]];
            if mirrored {
                v.swap(1, 2);
            }
            // Skip triangles wholly behind the near plane or outside the band's rows early
            if v.iter().all(|x| x.p[2] < NEAR) {
                continue;
            }
            draw_tri(band, cam, v, tex.as_ref(), material.base_color_factor, mask, material.double_sided);
        }
    }
}

/// Draws the leaf cards of one placed object, each expanded for the camera: the corners' offsets are in the
/// camera's frame (x right, y up, z towards the viewer), carried into the object's space by the inverse
/// of the placement's rotation and then placed with the whole transform, as the engine's leaf code does
fn draw_leaves(band: &mut Band, leaves: &SceneLeaves, cam: &Camera, inst: &Instance, radius: f32, half_tan: f32, half_tan_x: f32) {
    let t = &inst.transform;
    // The whole object is out of sight, or smaller than half a pixel
    let reach = radius * t.scale[0].abs().max(t.scale[1].abs()).max(t.scale[2].abs());
    let vo = cam.view(t.translation);
    if vo[2] + reach < NEAR || vo[0].abs() > vo[2].max(0.0) * half_tan_x + reach || vo[1].abs() > vo[2].max(0.0) * half_tan + reach || vo[2] > 0.0 && reach / vo[2] * cam.focal < 0.5 {
        return;
    }
    let back = [-cam.forward[0], -cam.forward[1], -cam.forward[2]];
    let inverse = [-t.rotation[0], -t.rotation[1], -t.rotation[2], t.rotation[3]];
    let place = |p: V3| {
        let s = [p[0] * t.scale[0], p[1] * t.scale[1], p[2] * t.scale[2]];
        let w = rotate(t.rotation, s);
        [w[0] + t.translation[0], w[1] + t.translation[1], w[2] + t.translation[2]]
    };
    let (right, up, toward) = (rotate(inverse, cam.right), rotate(inverse, cam.up), rotate(inverse, back));
    for set in &leaves.models[inst.model].sets {
        let tex = set.base_color.as_ref().map(|i| Tex { w: i.width as usize, h: i.height as usize, rgba: &i.rgba, clamp: false });
        let mask = if let AlphaMode::Mask(c) = set.alpha { Some(c) } else { None };
        for card in &set.cards {
            let nw = rotate(t.rotation, card.normal);
            let n = [dot(nw, cam.right), dot(nw, cam.up), dot(nw, cam.forward)];
            let corners = card.corners_facing(right, up, toward).map(place);
            let v: Vec<Vtx> = corners.iter().zip(&card.corners).map(|(p, c)| Vtx { p: cam.view(*p), n, uv: c.uv }).collect();
            for tri in card.triangles {
                let tri = [v[tri[0] as usize], v[tri[1] as usize], v[tri[2] as usize]];
                if tri.iter().all(|x| x.p[2] < NEAR) {
                    continue;
                }
                draw_tri(band, cam, tri, tex.as_ref(), [1.0; 4], mask, true);
            }
        }
    }
}

fn parse3(s: &str) -> V3 {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse().expect("number")).collect();
    [v[0], v[1], v[2]]
}

struct View {
    out: PathBuf,
    eye: V3,
    at: V3,
    fov: f32,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() >= 3,
        "usage: render_scene <game> <levels/x.bin> [<out.png> --eye x,y,z --at x,y,z] [--view out.png|x,y,z|x,y,z|fov]... [--size WxH] [--fov deg] [--ssaa n] [--lod m] [--background r,g,b] [--leaves]"
    );
    let (game, id) = (PathBuf::from(&args[1]), args[2].clone());
    let mut views: Vec<View> = Vec::new();
    let mut single = (None::<PathBuf>, [2048.0, 600.0, 200.0], [2048.0, 40.0, 2048.0]);
    let (mut w, mut h, mut fov, mut ssaa) = (1280usize, 720usize, 60.0f32, 2usize);
    let mut lod = None;
    let mut with_leaves = false;
    let mut background = [196u8, 204, 214];
    let mut i = 3;
    if args.get(3).is_some_and(|a| !a.starts_with("--")) {
        single.0 = Some(PathBuf::from(&args[3]));
        i = 4;
    }
    while i < args.len() {
        if args[i] == "--leaves" {
            with_leaves = true;
            i += 1;
            continue;
        }
        let val = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--eye" => single.1 = parse3(&val),
            "--at" => single.2 = parse3(&val),
            "--fov" => fov = val.parse()?,
            "--ssaa" => ssaa = val.parse()?,
            "--lod" => lod = Some(val.parse::<f32>()?),
            "--view" => {
                let p: Vec<&str> = val.split('|').collect();
                anyhow::ensure!(p.len() == 4, "--view out.png|x,y,z|x,y,z|fov");
                views.push(View { out: PathBuf::from(p[0]), eye: parse3(p[1]), at: parse3(p[2]), fov: p[3].parse()? });
            }
            "--background" => {
                let b = parse3(&val);
                background = [b[0] as u8, b[1] as u8, b[2] as u8];
            }
            "--size" => {
                let (a, b) = val.split_once('x').expect("WxH");
                (w, h) = (a.parse()?, b.parse()?);
            }
            other => anyhow::bail!("unknown option {other}"),
        }
        i += 2;
    }
    if let Some(out) = single.0.take() {
        views.push(View { out, eye: single.1, at: single.2, fov });
    }
    anyhow::ensure!(!views.is_empty(), "no view asked for");
    let mut options = wt_unpack::scene::SceneOptions::default();
    if let Some(l) = lod {
        options.lod_distance_m = l;
    }
    let wt = WarThunder::new(&game).with_scene_options(options);
    let t0 = Instant::now();
    let (scene, leaves) = if with_leaves {
        let (scene, _, leaves) = wt.load_level_with_leaves(&id)?;
        (scene, Some(leaves))
    } else {
        (wt.load_scene(&id).map_err(|e| anyhow::anyhow!("{e}"))?, None)
    };
    eprintln!("scene {}: {} models, {} instances, loaded in {:.1} s", scene.name, scene.models.len(), scene.instances.len(), t0.elapsed().as_secs_f32());
    if let Some(l) = &leaves {
        eprintln!("leaf cards: {} models, {} cards, {} placements", l.models.len(), l.card_count(), l.instances.len());
    }
    for v in &views {
        render_view(&scene, leaves.as_ref(), v, w, h, ssaa, background)?;
    }
    Ok(())
}

fn render_view(scene: &Scene, leaves: Option<&SceneLeaves>, view: &View, w: usize, h: usize, ssaa: usize, background: [u8; 3]) -> anyhow::Result<()> {
    // The world axes of the game (x, height, z) become the bridge's (z, height, x)
    let to_bridge = |p: V3| [p[2], p[1], p[0]];
    let (eye_b, at_b) = (to_bridge(view.eye), to_bridge(view.at));
    let forward = norm(sub(at_b, eye_b));
    let right = norm(cross(forward, [0.0, 1.0, 0.0]));
    // Right-handed: with +Y up and looking along -Z, +X is to the right
    let up = cross(right, forward);
    let (rw, rh) = (w * ssaa, h * ssaa);
    let focal = rh as f32 * 0.5 / (view.fov.to_radians() * 0.5).tan();
    let cam = Camera { eye: eye_b, right, up, forward, focal, w: rw, h: rh };
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let t1 = Instant::now();
    let pixels = render(scene, leaves, &cam, threads, background);
    eprintln!("rendered {rw}x{rh} on {threads} threads in {:.1} s", t1.elapsed().as_secs_f32());

    // Downsample
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            for sy in 0..ssaa {
                for sx in 0..ssaa {
                    let p = pixels[(y * ssaa + sy) * rw + x * ssaa + sx];
                    for k in 0..3 {
                        acc[k] += u32::from(p[k]);
                    }
                }
            }
            let n = (ssaa * ssaa) as u32;
            rgb.extend(acc.map(|a| (a / n) as u8));
        }
    }
    let mut buf = Vec::new();
    let mut enc = png::Encoder::new(&mut buf, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgb)?;
    std::fs::write(&view.out, buf)?;
    eprintln!("wrote {}", view.out.display());
    Ok(())
}
