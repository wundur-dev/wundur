//! Drawing a posed model from one camera, on the CPU: a depth-tested visibility pass, then
//! each visible pixel shaded once (a studio light setup that moves with the camera, so every
//! view is lit the same way).

use glam::{Mat3, Vec2, Vec3, Vec4};

use crate::gltf::{Alpha, Material, Model, TexRef, Texture};
use crate::pose::Scene;

#[derive(Clone, Copy)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    /// Vertical field of view, radians.
    pub fov: f32,
}

pub(crate) struct Basis {
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    fwd: Vec3,
}

impl Camera {
    fn basis(&self) -> Basis {
        let fwd = (self.target - self.eye).normalize();
        let right = fwd.cross(self.up).normalize();
        Basis { eye: self.eye, right, up: right.cross(fwd), fwd }
    }
}

struct RasterTri {
    orig: u32,
    s: [Vec2; 3],
    iz: [f32; 3],
    /// Each corner's place in the original triangle (they differ from the corners when the
    /// near plane cut it).
    bary: [Vec3; 3],
    /// Half log2 of texture-coordinate area per pixel area: the mip level, before the size.
    lod: f32,
    front: bool,
}

static DEFAULT_MATERIAL: Material = Material {
    name: String::new(),
    base: Vec4::ONE,
    base_tex: None,
    metallic: 1.0,
    roughness: 1.0,
    mr_tex: None,
    normal_tex: None,
    occlusion_tex: None,
    emissive: Vec3::ZERO,
    emissive_tex: None,
    alpha: Alpha::Opaque,
    double_sided: false,
    unlit: false,
};

/// What a texture that couldn't be read shows: unmistakable.
const MISSING: Vec4 = Vec4::new(1.0, 0.0, 1.0, 1.0);

/// How a picture looks around the model.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    /// A dark backdrop instead of the light grey one.
    pub dark: bool,
    /// A floor under the model with its soft shadow.
    pub floor: bool,
}

/// `scene` (of `model`) from `cam`, `w`×`h` pixels, as display colours (0..1), background in.
/// Bands of rows are drawn on every core.
pub fn draw(model: &Model, scene: &Scene, cam: &Camera, w: usize, h: usize, look: &Look) -> Vec<Vec3> {
    let b = cam.basis();
    let focal = (h as f32 * 0.5) / (cam.fov * 0.5).tan();
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let dist = (cam.target - cam.eye).length();
    let near = (dist * 1e-3).max(1e-5);
    let mat = |t: usize| -> &Material { scene.tri_mat[t].and_then(|m| model.materials.get(m as usize)).unwrap_or(&DEFAULT_MATERIAL) };

    // Every triangle, culled, cut at the near plane and projected.
    let mut rts: Vec<RasterTri> = Vec::with_capacity(scene.tris.len());
    for (t, tri) in scene.tris.iter().enumerate() {
        let p = tri.map(|i| scene.pos[i as usize]);
        let g = (p[1] - p[0]).cross(p[2] - p[0]);
        let front = g.dot(b.eye - p[0]) > 0.0;
        let m = mat(t);
        if !front && !m.double_sided {
            continue;
        }
        let corners: Vec<(Vec3, Vec3)> = (0..3).map(|k| (Vec3::new((p[k] - b.eye).dot(b.right), (p[k] - b.eye).dot(b.up), (p[k] - b.eye).dot(b.fwd)), Vec3::AXES[k])).collect();
        let poly = if corners.iter().all(|c| c.0.z >= near) {
            corners
        } else if corners.iter().all(|c| c.0.z < near) {
            continue;
        } else {
            clip_near(&corners, near)
        };
        let uv = tri.map(|i| scene.uv0[i as usize]);
        for k in 1..poly.len().saturating_sub(1) {
            let c = [poly[0], poly[k], poly[k + 1]];
            let s = c.map(|(v, _)| Vec2::new(cx + focal * v.x / v.z, cy - focal * v.y / v.z));
            let area = edge(s[0], s[1], s[2]).abs();
            if area < 1e-9 {
                continue;
            }
            let cuv = c.map(|(_, bb)| uv[0] * bb.x + uv[1] * bb.y + uv[2] * bb.z);
            let uv_area = ((cuv[1] - cuv[0]).perp_dot(cuv[2] - cuv[0])).abs().max(1e-12);
            rts.push(RasterTri { orig: t as u32, s, iz: c.map(|(v, _)| 1.0 / v.z), bary: c.map(|(_, bb)| bb), lod: 0.5 * (uv_area / area).log2(), front });
        }
    }

    let lights = Lights::new(&b);
    // The floor: at the model's feet, seen only from above it.
    let floor = (look.floor && b.eye.y > scene.min.y + 1e-4).then(|| Shadow::new(scene));
    let mut out = vec![Vec3::ZERO; w * h];
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16).min(h.div_ceil(16)).max(1);
    let band = h.div_ceil(threads);
    std::thread::scope(|sc| {
        for (bi, chunk) in out.chunks_mut(band * w).enumerate() {
            let (rts, lights, floor, b) = (&rts, &lights, &floor, &b);
            sc.spawn(move || {
                let y0 = bi * band;
                let bh = chunk.len() / w;
                let mut depth = vec![0f32; w * bh];
                let mut id = vec![u32::MAX; w * bh];
                // Visibility: the nearest triangle at each pixel.
                for (ri, rt) in rts.iter().enumerate() {
                    let s = rt.s;
                    let ymin = s.iter().map(|p| p.y).fold(f32::MAX, f32::min);
                    let ymax = s.iter().map(|p| p.y).fold(f32::MIN, f32::max);
                    if ymax < y0 as f32 || ymin > (y0 + bh) as f32 {
                        continue;
                    }
                    let area = edge(s[0], s[1], s[2]);
                    let x0 = s.iter().map(|p| p.x).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
                    let x1 = (s.iter().map(|p| p.x).fold(f32::MIN, f32::max).ceil() as isize).min(w as isize - 1);
                    let ya = (ymin.floor().max(y0 as f32)) as usize;
                    let yb = (ymax.ceil() as isize).min((y0 + bh) as isize - 1);
                    if x1 < 0 || yb < ya as isize {
                        continue;
                    }
                    let m = mat(rt.orig as usize);
                    let cutoff = match m.alpha {
                        Alpha::Mask(c) => Some(c),
                        Alpha::Blend => Some(0.5),
                        Alpha::Opaque => None,
                    };
                    for y in ya..=yb as usize {
                        for x in x0..=x1 as usize {
                            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                            let (w0, w1, w2) = (edge(s[1], s[2], p) / area, edge(s[2], s[0], p) / area, edge(s[0], s[1], p) / area);
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }
                            let iz = w0 * rt.iz[0] + w1 * rt.iz[1] + w2 * rt.iz[2];
                            let at = (y - y0) * w + x;
                            if iz <= depth[at] {
                                continue;
                            }
                            if let Some(c) = cutoff {
                                let ob = orig_bary(rt, w0, w1, w2);
                                if base_color(model, scene, m, rt, ob).w < c {
                                    continue;
                                }
                            }
                            depth[at] = iz;
                            id[at] = ri as u32;
                        }
                    }
                }
                // Shading.
                for ly in 0..bh {
                    let y = y0 + ly;
                    let bg = background(y as f32 / h as f32, look.dark);
                    for x in 0..w {
                        let at = ly * w + x;
                        let ri = id[at];
                        if ri == u32::MAX {
                            chunk[at] = match floor {
                                Some(f) => {
                                    let dir = (b.fwd + b.right * ((x as f32 + 0.5 - cx) / focal) - b.up * ((y as f32 + 0.5 - cy) / focal)).normalize();
                                    let t = (scene.min.y - b.eye.y) / dir.y;
                                    if dir.y < 0.0 && t > 0.0 {
                                        let hit = b.eye + dir * t;
                                        bg * (1.0 - 0.55 * f.at(hit.x, hit.z))
                                    } else {
                                        bg
                                    }
                                }
                                None => bg,
                            };
                            continue;
                        }
                        let rt = &rts[ri as usize];
                        let s = rt.s;
                        let area = edge(s[0], s[1], s[2]);
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let (w0, w1, w2) = (edge(s[1], s[2], p) / area, edge(s[2], s[0], p) / area, edge(s[0], s[1], p) / area);
                        let ob = orig_bary(rt, w0, w1, w2);
                        chunk[at] = shade(model, scene, mat(rt.orig as usize), rt, ob, b, lights);
                    }
                }
            });
        }
    });
    out
}

/// A soft shadow on the floor: how much of the model is just above each spot, blurred.
struct Shadow {
    origin: Vec2,
    cell: f32,
    n: usize,
    v: Vec<f32>,
}

impl Shadow {
    const N: usize = 160;

    fn new(scene: &Scene) -> Shadow {
        let size = scene.max - scene.min;
        let biggest = size.max_element().max(1e-6);
        let half = size.x.max(size.z) * 0.5 + biggest * 0.4;
        let c = Vec2::new((scene.min.x + scene.max.x) * 0.5, (scene.min.z + scene.max.z) * 0.5);
        let n = Self::N;
        let cell = 2.0 * half / n as f32;
        let origin = c - Vec2::splat(half);
        let reach = (size.y * 0.35).max(1e-6);
        let mut v = vec![0f32; n * n];
        for tri in &scene.tris {
            let p = tri.map(|i| scene.pos[i as usize]);
            let low = p.iter().map(|q| q.y).fold(f32::MAX, f32::min);
            let strength = 1.0 - ((low - scene.min.y) / reach).clamp(0.0, 1.0);
            if strength <= 0.0 {
                continue;
            }
            let g = p.map(|q| (Vec2::new(q.x, q.z) - origin) / cell);
            let area = edge(g[0], g[1], g[2]);
            if area.abs() < 1e-9 {
                // Edge-on from above: mark its corners.
                for q in g {
                    let (x, y) = (q.x as isize, q.y as isize);
                    if x >= 0 && y >= 0 && (x as usize) < n && (y as usize) < n {
                        let i = y as usize * n + x as usize;
                        v[i] = v[i].max(strength);
                    }
                }
                continue;
            }
            let x0 = g.iter().map(|q| q.x).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
            let x1 = (g.iter().map(|q| q.x).fold(f32::MIN, f32::max).ceil() as isize).min(n as isize - 1);
            let y0 = g.iter().map(|q| q.y).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
            let y1 = (g.iter().map(|q| q.y).fold(f32::MIN, f32::max).ceil() as isize).min(n as isize - 1);
            for y in y0..=y1.max(0) as usize {
                for x in x0..=x1.max(0) as usize {
                    let q = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let (a, bb, cc) = (edge(g[1], g[2], q) / area, edge(g[2], g[0], q) / area, edge(g[0], g[1], q) / area);
                    if a >= 0.0 && bb >= 0.0 && cc >= 0.0 {
                        let i = y * n + x;
                        v[i] = v[i].max(strength);
                    }
                }
            }
        }
        // Three box blurs, across then down: close to a Gaussian.
        let r = 3isize;
        let mut tmp = vec![0f32; n * n];
        for _ in 0..3 {
            for y in 0..n {
                for x in 0..n {
                    let mut sum = 0.0;
                    for d in -r..=r {
                        sum += v[y * n + (x as isize + d).clamp(0, n as isize - 1) as usize];
                    }
                    tmp[y * n + x] = sum / (2 * r + 1) as f32;
                }
            }
            for y in 0..n {
                for x in 0..n {
                    let mut sum = 0.0;
                    for d in -r..=r {
                        sum += tmp[(y as isize + d).clamp(0, n as isize - 1) as usize * n + x];
                    }
                    v[y * n + x] = sum / (2 * r + 1) as f32;
                }
            }
        }
        Shadow { origin, cell, n, v }
    }

    fn at(&self, x: f32, z: f32) -> f32 {
        let g = (Vec2::new(x, z) - self.origin) / self.cell - Vec2::splat(0.5);
        let (x0, y0) = (g.x.floor(), g.y.floor());
        let (tx, ty) = (g.x - x0, g.y - y0);
        let get = |x: f32, y: f32| -> f32 {
            if x < 0.0 || y < 0.0 || x as usize >= self.n || y as usize >= self.n {
                0.0
            } else {
                self.v[y as usize * self.n + x as usize]
            }
        };
        let top = get(x0, y0) * (1.0 - tx) + get(x0 + 1.0, y0) * tx;
        let bot = get(x0, y0 + 1.0) * (1.0 - tx) + get(x0 + 1.0, y0 + 1.0) * tx;
        top * (1.0 - ty) + bot * ty
    }
}

fn edge(a: Vec2, b: Vec2, p: Vec2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

/// Perspective-correct place in the original triangle from screen weights.
fn orig_bary(rt: &RasterTri, w0: f32, w1: f32, w2: f32) -> Vec3 {
    let (a, b, c) = (w0 * rt.iz[0], w1 * rt.iz[1], w2 * rt.iz[2]);
    let sum = (a + b + c).max(1e-20);
    (rt.bary[0] * a + rt.bary[1] * b + rt.bary[2] * c) / sum
}

fn clip_near(poly: &[(Vec3, Vec3)], near: f32) -> Vec<(Vec3, Vec3)> {
    let mut out = Vec::new();
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (ina, inb) = (a.0.z >= near, b.0.z >= near);
        if ina {
            out.push(a);
        }
        if ina != inb {
            let t = (near - a.0.z) / (b.0.z - a.0.z);
            out.push((a.0.lerp(b.0, t), a.1.lerp(b.1, t)));
        }
    }
    out
}

fn interp<T: Copy + std::ops::Mul<f32, Output = T> + std::ops::Add<Output = T>>(v: &[T], tri: [u32; 3], b: Vec3) -> T {
    v[tri[0] as usize] * b.x + v[tri[1] as usize] * b.y + v[tri[2] as usize] * b.z
}

fn uv_for(scene: &Scene, tri: [u32; 3], b: Vec3, r: &TexRef) -> Vec2 {
    let uv = if r.tex_coord == 1 { interp(&scene.uv1, tri, b) } else { interp(&scene.uv0, tri, b) };
    match r.transform {
        Some((off, rot, scale)) => {
            let s = uv * scale;
            let (sn, cs) = rot.sin_cos();
            Vec2::new(cs * s.x + sn * s.y, -sn * s.x + cs * s.y) + off
        }
        None => uv,
    }
}

/// A texture's texel at `uv` (raw 0..1, bilinear on the level `lod` picks); magenta when it
/// couldn't be read.
fn tex(model: &Model, r: &TexRef, uv: Vec2, lod: f32) -> Vec4 {
    let Some(t) = r.image.and_then(|i| model.images.get(i)).and_then(Option::as_ref) else { return MISSING };
    sample(t, r.wrap, uv, lod)
}

fn sample(t: &Texture, wrap: [u32; 2], uv: Vec2, lod: f32) -> Vec4 {
    let (w0, h0, _) = &t.levels[0];
    let level = (lod + 0.5 * ((*w0 as f32) * (*h0 as f32)).log2()).round().clamp(0.0, (t.levels.len() - 1) as f32) as usize;
    let (w, h, px) = &t.levels[level];
    let (w, h) = (*w as i64, *h as i64);
    let fx = wrap_coord(uv.x, wrap[0]) * w as f32 - 0.5;
    let fy = wrap_coord(uv.y, wrap[1]) * h as f32 - 0.5;
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let fetch = |x: i64, y: i64| -> Vec4 {
        let x = if wrap[0] == 33071 { x.clamp(0, w - 1) } else { x.rem_euclid(w) };
        let y = if wrap[1] == 33071 { y.clamp(0, h - 1) } else { y.rem_euclid(h) };
        let i = ((y * w + x) * 4) as usize;
        Vec4::new(px[i] as f32, px[i + 1] as f32, px[i + 2] as f32, px[i + 3] as f32) / 255.0
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let top = fetch(x0, y0).lerp(fetch(x0 + 1, y0), tx);
    let bot = fetch(x0, y0 + 1).lerp(fetch(x0 + 1, y0 + 1), tx);
    top.lerp(bot, ty)
}

fn wrap_coord(v: f32, mode: u32) -> f32 {
    match mode {
        33071 => v.clamp(0.0, 1.0),
        33648 => {
            let f = v.rem_euclid(2.0);
            if f > 1.0 { 2.0 - f } else { f }
        }
        _ => v.rem_euclid(1.0),
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Base colour in linear light, alpha in w.
fn base_color(model: &Model, scene: &Scene, m: &Material, rt: &RasterTri, b: Vec3) -> Vec4 {
    let tri = scene.tris[rt.orig as usize];
    let mut c = m.base * interp(&scene.col, tri, b);
    if let Some(r) = &m.base_tex {
        let t = tex(model, r, uv_for(scene, tri, b, r), rt.lod);
        c *= Vec4::new(srgb_to_linear(t.x), srgb_to_linear(t.y), srgb_to_linear(t.z), t.w);
    }
    c
}

struct Lights {
    /// Direction to the light, and its strength.
    dirs: [(Vec3, f32); 3],
    key: Vec3,
}

impl Lights {
    fn new(b: &Basis) -> Lights {
        let to_cam = -b.fwd;
        let key = (b.right * -0.45 + b.up * 0.75 + to_cam * 0.6).normalize();
        let fill = (b.right * 0.75 + b.up * 0.1 + to_cam * 0.55).normalize();
        let rim = (b.right * 0.2 + b.up * 0.55 - to_cam * 0.8).normalize();
        Lights { dirs: [(key, 2.1), (fill, 0.55), (rim, 0.9)], key }
    }
}

const SKY: Vec3 = Vec3::new(0.62, 0.66, 0.72);
const GROUND: Vec3 = Vec3::new(0.26, 0.24, 0.22);

fn env(dir: Vec3, rough: f32, key: Vec3) -> Vec3 {
    let base = GROUND.lerp(SKY, (dir.y * 0.5 + 0.5).clamp(0.0, 1.0));
    let sharp = 2.0 + (1.0 - rough).powi(2) * 300.0;
    let soft_box = dir.dot(key).max(0.0).powf(sharp) * (0.3 + 3.0 * (1.0 - rough));
    base + Vec3::splat(soft_box)
}

fn background(t: f32, dark: bool) -> Vec3 {
    // Already display values: a soft studio grey (or near black), lighter at the top.
    if dark {
        Vec3::new(0.19, 0.20, 0.22).lerp(Vec3::new(0.09, 0.095, 0.105), t)
    } else {
        Vec3::new(0.80, 0.81, 0.83).lerp(Vec3::new(0.60, 0.61, 0.63), t)
    }
}

fn shade(model: &Model, scene: &Scene, m: &Material, rt: &RasterTri, b: Vec3, cam: &Basis, l: &Lights) -> Vec3 {
    let tri = scene.tris[rt.orig as usize];
    let p = interp(&scene.pos, tri, b);
    let base = base_color(model, scene, m, rt, b);
    if m.unlit {
        return base.truncate().map(linear_to_srgb);
    }
    let [p0, p1, p2] = tri.map(|i| scene.pos[i as usize]);
    let geo = (p1 - p0).cross(p2 - p0).normalize_or_zero();
    let mut n = if scene.tri_flat[rt.orig as usize] { geo } else { interp(&scene.nrm, tri, b).normalize_or(geo) };
    if !rt.front {
        n = -n;
    }
    if let Some(r) = &m.normal_tex
        && r.image.is_some()
    {
        let [u0, u1, u2] = tri.map(|i| if r.tex_coord == 1 { scene.uv1[i as usize] } else { scene.uv0[i as usize] });
        let (e1, e2, d1, d2) = (p1 - p0, p2 - p0, u1 - u0, u2 - u0);
        let det = d1.perp_dot(d2);
        if det.abs() > 1e-12 {
            let t = ((e1 * d2.y - e2 * d1.y) / det).reject_from_normalized(n).normalize_or_zero();
            let bt = ((e2 * d1.x - e1 * d2.x) / det).reject_from_normalized(n).normalize_or_zero();
            let s = tex(model, r, uv_for(scene, tri, b, r), rt.lod);
            let ts = Vec3::new((s.x * 2.0 - 1.0) * r.amount, (s.y * 2.0 - 1.0) * r.amount, s.z * 2.0 - 1.0);
            // glTF's v runs down the image; its normal maps' green points up it.
            let tbn = Mat3::from_cols(t, -bt, n);
            n = (tbn * ts).normalize_or(n);
        }
    }
    let (mut metal, mut rough) = (m.metallic, m.roughness);
    if let Some(r) = &m.mr_tex {
        let s = tex(model, r, uv_for(scene, tri, b, r), rt.lod);
        rough *= s.y;
        metal *= s.z;
    }
    let rough = rough.clamp(0.04, 1.0);
    let metal = metal.clamp(0.0, 1.0);
    let ao = match &m.occlusion_tex {
        Some(r) if r.image.is_some() => 1.0 + r.amount * (tex(model, r, uv_for(scene, tri, b, r), rt.lod).x - 1.0),
        _ => 1.0,
    };
    let albedo = base.truncate();
    let v = (cam.eye - p).normalize_or(-cam.fwd);
    let ndv = n.dot(v).max(1e-3);
    let f0 = Vec3::splat(0.04).lerp(albedo, metal);
    let diffuse = albedo * (1.0 - metal);
    let a = rough * rough;
    let k = (rough + 1.0).powi(2) / 8.0;
    let mut c = Vec3::ZERO;
    for &(ld, li) in &l.dirs {
        let ndl = n.dot(ld);
        if ndl <= 0.0 {
            continue;
        }
        let hv = (ld + v).normalize();
        let ndh = n.dot(hv).max(0.0);
        let vdh = v.dot(hv).max(0.0);
        let f = f0 + (Vec3::ONE - f0) * (1.0 - vdh).powi(5);
        let d = a * a / (std::f32::consts::PI * (ndh * ndh * (a * a - 1.0) + 1.0).powi(2));
        let g = (ndl / (ndl * (1.0 - k) + k)) * (ndv / (ndv * (1.0 - k) + k));
        let spec = f * (d * g / (4.0 * ndl * ndv + 1e-4));
        c += (diffuse * (Vec3::ONE - f) + spec * std::f32::consts::PI) * ndl * li;
    }
    let fr = f0 + (Vec3::splat(1.0 - rough).max(f0) - f0) * (1.0 - ndv).powi(5);
    let refl = (-v).reflect(n);
    let ambient = GROUND.lerp(SKY, n.y * 0.5 + 0.5) * 0.75;
    c += (diffuse * ambient * (Vec3::ONE - fr) + fr * env(refl, rough, l.key)) * ao;
    let mut emissive = m.emissive;
    if let Some(r) = &m.emissive_tex {
        let s = tex(model, r, uv_for(scene, tri, b, r), rt.lod);
        emissive *= Vec3::new(srgb_to_linear(s.x), srgb_to_linear(s.y), srgb_to_linear(s.z));
    }
    c += emissive;
    aces(c).map(linear_to_srgb)
}

fn aces(x: Vec3) -> Vec3 {
    let x = x * 0.9;
    (x * (x * 2.51 + 0.03)) / (x * (x * 2.43 + 0.59) + 0.14)
}
