//! Wundur's model renderer: glTF and GLB models drawn to a still image on the CPU, with no
//! window, GPU or browser, so an agent can look at a model (or compare several) from fixed
//! angles under the same camera and light, with the facts about each beside it.
//!
//! [`render`] makes one contact sheet: a row per model, a column per view.

// Fixed-size chunks read more plainly as slices here (glam takes slices).
#![allow(clippy::chunks_exact_to_as_chunks)]

pub mod export;
mod font;
pub mod gltf;
pub mod pose;
mod raster;
pub mod view;

pub use glam;
pub use raster::{Camera, Look};

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::Vec3;
use image::{Rgb, RgbImage};

use crate::gltf::Model;
use crate::pose::Scene;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Front,
    Back,
    Left,
    Right,
    ThreeQuarter,
    BackThreeQuarter,
    Top,
    Bottom,
}

impl View {
    pub const DEFAULT: [View; 4] = [View::Front, View::Left, View::Back, View::ThreeQuarter];

    pub fn parse(s: &str) -> Option<View> {
        Some(match s.trim().to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
            "front" => View::Front,
            "back" | "rear" => View::Back,
            "left" | "side" => View::Left,
            "right" => View::Right,
            "three-quarter" | "3/4" | "34" | "threequarter" | "front-three-quarter" | "quarter" => View::ThreeQuarter,
            "back-three-quarter" | "three-quarter-back" | "back-3/4" | "rear-three-quarter" => View::BackThreeQuarter,
            "top" | "above" => View::Top,
            "bottom" | "below" | "under" => View::Bottom,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Front => "front",
            View::Back => "back",
            View::Left => "left side",
            View::Right => "right side",
            View::ThreeQuarter => "three-quarter",
            View::BackThreeQuarter => "back three-quarter",
            View::Top => "top",
            View::Bottom => "bottom",
        }
    }

    /// From the target toward the camera, and the camera's up. glTF models face +Z with +Y
    /// up, so their left side is +X.
    fn direction(self) -> (Vec3, Vec3) {
        let (az, el): (f32, f32) = match self {
            View::Front => (0.0, 8.0),
            View::Back => (180.0, 8.0),
            View::Left => (90.0, 8.0),
            View::Right => (-90.0, 8.0),
            View::ThreeQuarter => (40.0, 20.0),
            View::BackThreeQuarter => (140.0, 20.0),
            View::Top => return (Vec3::Y, Vec3::Z),
            View::Bottom => return (-Vec3::Y, Vec3::Z),
        };
        let (az, el) = (az.to_radians(), el.to_radians());
        (Vec3::new(az.sin() * el.cos(), el.sin(), az.cos() * el.cos()), Vec3::Y)
    }
}

pub struct Item {
    pub path: PathBuf,
    /// How the sheet and the report name it.
    pub label: String,
    /// Its path as the person knows it (in the project).
    pub shown_path: String,
    /// Which way the model faces: "+z" (glTF's convention, the default), "-z", "+x", "-x".
    /// It's turned to face +Z, so "front" is its front.
    pub facing: Option<String>,
}

/// The turn about +Y (degrees) that makes a model facing `f` face +Z.
pub fn facing_turn(f: &str) -> Option<f32> {
    Some(match f.trim().to_ascii_lowercase().as_str() {
        "+z" | "z" | "" => 0.0,
        "-z" => 180.0,
        "+x" | "x" => -90.0,
        "-x" => 90.0,
        _ => return None,
    })
}

pub struct Request {
    pub models: Vec<Item>,
    pub views: Vec<View>,
    /// An animation (part of its name) to pose the models in; none: their rest pose.
    pub animation: Option<String>,
    /// When in it: seconds ("1.2"), or a share of it ("50%").
    pub time: Option<String>,
    /// A close-up: a joint or node name (part of it: "hand", "foot_l", "head"), a region
    /// ("front top", "bottom"), or a point in the model's box as fractions ("0.5,0.1,0.9").
    pub focus: Option<String>,
    /// How much closer a close-up is (default 3).
    pub zoom: f32,
    /// Keep the models' sizes relative to each other (one camera distance for all), not each
    /// filling its cell.
    pub same_scale: bool,
    /// Pixels per view (default: what fits a picture an agent can read).
    pub cell: Option<u32>,
}

pub struct Sheet {
    pub image: RgbImage,
    /// The facts about each model, and the warnings, as text.
    pub report: String,
    pub warnings: usize,
    pub failed: usize,
}

const FOV: f32 = 30.0 * std::f32::consts::PI / 180.0;
const SUPERSAMPLE: usize = 2;
const LABEL: u32 = 22;

struct Loaded {
    model: Model,
    scene: Scene,
    pose: String,
    focus: Option<(Vec3, String)>,
    facing: String,
}

/// Draw the sheet `req` asks for.
pub fn render(req: &Request) -> Sheet {
    let views: Vec<View> = if req.views.is_empty() { View::DEFAULT.to_vec() } else { req.views.clone() };
    let loaded: Vec<Result<Loaded, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = req.models.iter().map(|it| s.spawn(move || load_one(it, req))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the renderer stopped on it".into()))).collect()
    });

    let (rows, cols) = (req.models.len().max(1) as u32, views.len() as u32);
    let cell = req.cell.unwrap_or_else(|| {
        let by_area = (1_150_000.0 / (rows * cols) as f64).sqrt() as u32;
        by_area.min(1568 / cols).min((1568 - LABEL) / rows - LABEL).min(512)
    });
    let cell = cell.clamp(96, 1024);

    // Cameras: per model and view (one distance across models when sizes are kept).
    let mut cams: Vec<Vec<Option<Camera>>> = loaded
        .iter()
        .map(|l| views.iter().map(|&v| l.as_ref().ok().map(|l| frame(&l.scene, v, l.focus.as_ref().map(|f| f.0), req.zoom))).collect())
        .collect();
    if req.same_scale && req.focus.is_none() {
        for vi in 0..views.len() {
            let far = cams.iter().filter_map(|r| r[vi]).map(|c| (c.eye - c.target).length()).fold(0.0, f32::max);
            for row in cams.iter_mut() {
                if let Some(c) = row[vi].as_mut() {
                    let d = (c.eye - c.target).normalize();
                    c.eye = c.target + d * far;
                }
            }
        }
    }

    // Draw every view, on every core.
    let jobs: Vec<(usize, usize)> = (0..loaded.len()).flat_map(|m| (0..views.len()).map(move |v| (m, v))).collect();
    let next = AtomicUsize::new(0);
    let px = cell as usize * SUPERSAMPLE;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(jobs.len().max(1));
    let mut tiles: Vec<Option<Vec<Vec3>>> = (0..jobs.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let j = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(m, v)) = jobs.get(j) else { break };
                        if let (Ok(l), Some(cam)) = (&loaded[m], cams[m][v]) {
                            done.push((j, raster::draw(&l.model, &l.scene, &cam, px, px, &raster::Look::default())));
                        }
                    }
                    done
                })
            })
            .collect();
        for w in workers {
            for (j, t) in w.join().unwrap_or_default() {
                tiles[j] = Some(t);
            }
        }
    });

    let (width, height) = (cols * cell, LABEL + rows * (LABEL + cell));
    let mut img = RgbImage::from_pixel(width, height, Rgb([34, 36, 40]));
    for (vi, v) in views.iter().enumerate() {
        let mut name = v.name().to_uppercase();
        if req.focus.is_some() {
            name.push_str(" CLOSE");
        }
        let x = vi as u32 * cell + cell.saturating_sub(font::width(&name, 2)) / 2;
        font::draw(&mut img, x, 4, &name, 2, [220, 222, 226], (vi as u32 + 1) * cell);
    }
    let mut report = String::new();
    let (mut warnings, mut failed) = (0, 0);
    for (mi, (it, l)) in req.models.iter().zip(&loaded).enumerate() {
        let top = LABEL + mi as u32 * (LABEL + cell);
        for x in 0..width {
            for y in top..top + LABEL {
                img.put_pixel(x, y, Rgb([48, 51, 56]));
            }
        }
        let label = it.label.to_uppercase();
        font::draw(&mut img, 6, top + 4, &label, 2, [240, 240, 240], width);
        let after = 6 + font::width(&label, 2) + 12;
        match l {
            Ok(l) => {
                let tris = l.scene.tris.len();
                let n = l.model.facts.warnings.len() + l.model.facts.textures.iter().filter(|t| t.error.is_some()).count();
                warnings += n;
                let short = format!("{} TRIS", count_short(tris));
                font::draw(&mut img, after, top + 4, &short, 2, [170, 176, 186], width);
                if n > 0 {
                    let w = format!("! {n} WARNING{}", if n == 1 { "" } else { "S" });
                    font::draw(&mut img, after + font::width(&short, 2) + 12, top + 4, &w, 2, [255, 110, 100], width);
                }
                report.push_str(&describe(it, l, req));
            }
            Err(why) => {
                failed += 1;
                font::draw(&mut img, after, top + 4, "COULD NOT LOAD", 2, [255, 110, 100], width);
                report.push_str(&format!("{} ({}): couldn't be loaded: {why}\n\n", it.label, it.shown_path));
            }
        }
        for vi in 0..views.len() {
            let x0 = vi as u32 * cell;
            let y0 = top + LABEL;
            match &tiles[mi * views.len() + vi] {
                Some(t) => {
                    for y in 0..cell {
                        for x in 0..cell {
                            let mut c = Vec3::ZERO;
                            for sy in 0..SUPERSAMPLE {
                                for sx in 0..SUPERSAMPLE {
                                    c += t[(y as usize * SUPERSAMPLE + sy) * px + x as usize * SUPERSAMPLE + sx];
                                }
                            }
                            let c = c / (SUPERSAMPLE * SUPERSAMPLE) as f32 * 255.0;
                            img.put_pixel(x0 + x, y0 + y, Rgb([c.x.round() as u8, c.y.round() as u8, c.z.round() as u8]));
                        }
                    }
                }
                None => {
                    for y in 0..cell {
                        for x in 0..cell {
                            img.put_pixel(x0 + x, y0 + y, Rgb([70, 60, 62]));
                        }
                    }
                }
            }
            // A hairline between views.
            if vi > 0 {
                for y in y0..y0 + cell {
                    img.put_pixel(x0, y, Rgb([34, 36, 40]));
                }
            }
        }
    }
    Sheet { image: img, report: report.trim_end().to_string(), warnings, failed }
}

fn load_one(it: &Item, req: &Request) -> Result<Loaded, String> {
    let model = gltf::load(&it.path)?;
    let (anim, pose_text) = match req.animation.as_deref().map(str::trim).filter(|a| !a.is_empty() && !matches!(a.to_ascii_lowercase().as_str(), "rest" | "none")) {
        None => (None, if model.animations.is_empty() { "rest pose".to_string() } else { "rest pose (no animation asked for)".to_string() }),
        Some(want) => {
            let lw = want.to_lowercase();
            match model.animations.iter().position(|a| a.name.to_lowercase() == lw).or_else(|| model.animations.iter().position(|a| a.name.to_lowercase().contains(&lw))) {
                Some(i) => {
                    let a = &model.animations[i];
                    let t = match req.time.as_deref().map(str::trim) {
                        Some(t) if t.ends_with('%') => t.trim_end_matches('%').trim().parse::<f32>().unwrap_or(0.0) / 100.0 * a.duration,
                        Some(t) => t.trim_end_matches('s').trim().parse::<f32>().unwrap_or(0.0),
                        None => 0.0,
                    };
                    let t = t.clamp(0.0, a.duration);
                    (Some((i, t)), format!("\"{}\" at {:.2}s of {:.2}s", a.name, t, a.duration))
                }
                None => (None, format!("rest pose (it has no animation called \"{want}\")")),
            }
        }
    };
    let mut scene = pose::pose(&model, anim);
    let facing = it.facing.as_deref().map(str::trim).filter(|f| !f.is_empty()).unwrap_or("+z");
    let turn = facing_turn(facing).ok_or_else(|| format!("facing \"{facing}\" isn't one of +z, -z, +x, -x"))?;
    if turn != 0.0 {
        scene.turn(turn.to_radians());
    }
    let focus = req.focus.as_deref().map(str::trim).filter(|f| !f.is_empty()).map(|f| focus_point(&model, &scene, f));
    Ok(Loaded { model, scene, pose: pose_text, focus, facing: facing.to_string() })
}

/// Where a close-up looks, and how that was found.
fn focus_point(model: &Model, scene: &Scene, f: &str) -> (Vec3, String) {
    let size = scene.max - scene.min;
    let nums: Vec<f32> = f.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if nums.len() == 3 {
        return (scene.min + size * Vec3::new(nums[0], nums[1], nums[2]), format!("the point {f} of its box"));
    }
    let mut frac = Vec3::splat(0.5);
    let lower = f.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut region = !words.is_empty();
    for word in &words {
        match *word {
            "front" => frac.z = 0.9,
            "back" | "rear" => frac.z = 0.1,
            "top" => frac.y = 0.9,
            "bottom" => frac.y = 0.1,
            "left" => frac.x = 0.9,
            "right" => frac.x = 0.1,
            "center" | "centre" | "middle" => {}
            _ => region = false,
        }
    }
    if region {
        return (scene.min + size * frac, format!("its {f}"));
    }
    // A joint or node by name: skeleton joints first.
    let want = f.to_lowercase().replace([' ', '-'], "_");
    let joints: Vec<usize> = model.skins.iter().flat_map(|s| s.joints.iter().copied()).collect();
    let named = |i: &usize| model.nodes[*i].name.to_lowercase().replace([' ', '-'], "_");
    let mut hits: Vec<usize> = joints.iter().copied().filter(|i| named(i) == want).collect();
    if hits.is_empty() {
        hits = joints.iter().copied().filter(|i| named(i).contains(&want)).collect();
    }
    if hits.is_empty() {
        hits = (0..model.nodes.len()).filter(|i| named(i).contains(&want)).collect();
    }
    hits.dedup();
    match hits.first() {
        Some(&i) => {
            let others: Vec<&str> = hits[1..].iter().take(6).map(|&j| model.nodes[j].name.as_str()).collect();
            let more = if others.is_empty() { String::new() } else { format!(" (also matched: {})", others.join(", ")) };
            (scene.world[i].w_axis.truncate(), format!("the joint \"{}\"{more}", model.nodes[i].name))
        }
        None => {
            let names: Vec<&str> = joints.iter().map(|&j| model.nodes[j].name.as_str()).filter(|n| !n.is_empty()).take(60).collect();
            let known = if names.is_empty() { "it has no skeleton".to_string() } else { format!("its joints: {}", names.join(", ")) };
            (scene.center(), format!("its middle: nothing is called \"{f}\" (try a region like \"front top\", x,y,z fractions of its box, or a joint; {known})"))
        }
    }
}

/// A camera on `view` that fits the model in the frame (or looks closely at `focus`).
fn frame(scene: &Scene, view: View, focus: Option<Vec3>, zoom: f32) -> Camera {
    let (dir, up) = view.direction();
    if let Some(p) = focus {
        let r = scene.radius() / zoom.max(1.0);
        return Camera { eye: p + dir * (r / (FOV * 0.5).sin()), target: p, up, fov: FOV };
    }
    fit(scene, dir, up, 0.86, 1.0)
}

/// A camera looking along -`dir` (from the target toward the camera: `dir`) that fits every
/// vertex of `scene` within `fill` of a frame `aspect` times as wide as tall, centred.
pub(crate) fn fit(scene: &Scene, dir: Vec3, up: Vec3, fill: f32, aspect: f32) -> Camera {
    let half = (FOV * 0.5).tan();
    let right = (-dir).cross(up).normalize();
    let cup = right.cross(-dir);
    let step = (scene.pos.len() / 40_000).max(1);
    let t = half * fill;
    let tx = t * aspect.max(0.05);
    let mut target = scene.center();
    let mut dist = scene.radius() / (FOV * 0.5).sin();
    for _ in 0..2 {
        let mut need = 0f32;
        for p in scene.pos.iter().step_by(step) {
            let r = *p - target;
            let (x, y, z) = (r.dot(right), r.dot(cup), r.dot(dir));
            need = need.max(z + (x.abs() / tx).max(y.abs() / t));
        }
        dist = need.max(1e-4);
        let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
        for p in scene.pos.iter().step_by(step) {
            let r = *p - target;
            let depth = (dist - r.dot(dir)).max(1e-6);
            let s = glam::Vec2::new(r.dot(right), r.dot(cup)) / depth;
            lo = lo.min(s);
            hi = hi.max(s);
        }
        if scene.pos.is_empty() {
            break;
        }
        let mid = (lo + hi) * 0.5;
        target += right * (mid.x * dist) + cup * (mid.y * dist);
    }
    Camera { eye: target + dir * dist, target, up, fov: FOV }
}

fn count_short(n: usize) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}K", n as f32 / 1000.0),
        _ => format!("{:.2}M", n as f32 / 1_000_000.0),
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn num(v: f32) -> String {
    let a = v.abs();
    if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn kb(bytes: u64) -> String {
    if bytes >= 1 << 20 { format!("{:.1} MB", bytes as f64 / (1 << 20) as f64) } else { format!("{} KB", bytes.div_ceil(1024)) }
}

fn describe(it: &Item, l: &Loaded, req: &Request) -> String {
    let f = &l.model.facts;
    let s = &l.scene;
    let size = s.max - s.min;
    let mut out = format!("{} ({}, {}", it.label, it.shown_path, kb(f.file_bytes));
    if !f.generator.is_empty() {
        out.push_str(&format!(", made by {}", f.generator));
    }
    out.push_str(")\n");
    out.push_str(&format!(
        "  {} triangles, {} vertices; {} mesh{}, {} part{}, {} material{}\n",
        thousands(s.tris.len()),
        thousands(s.pos.len()),
        f.meshes,
        if f.meshes == 1 { "" } else { "es" },
        f.prims,
        if f.prims == 1 { "" } else { "s" },
        f.materials,
        if f.materials == 1 { "" } else { "s" }
    ));
    out.push_str(&format!("  size: {} wide, {} tall, {} long (front to back, as drawn), in model units; it stands from y {} to {}\n", num(size.x), num(size.y), num(size.z), num(s.min.y), num(s.max.y)));
    if f.textures.is_empty() {
        out.push_str("  textures: none\n");
    } else {
        let list: Vec<String> = f
            .textures
            .iter()
            .map(|t| {
                let dims = t.size.map(|(w, h)| format!("{w}×{h}")).unwrap_or_else(|| "?".into());
                let uses = if t.uses.is_empty() { "unused".to_string() } else { t.uses.join(", ") };
                let fmt = if t.format.is_empty() { "?" } else { &t.format };
                let err = t.error.as_ref().map(|e| format!(", FAILED TO LOAD: {e}")).unwrap_or_default();
                format!("#{} {dims} {fmt} {} ({uses}{err})", t.image, kb(t.bytes as u64))
            })
            .collect();
        out.push_str(&format!("  textures: {}\n", list.join("; ")));
    }
    if f.joints > 0 {
        out.push_str(&format!("  skeleton: {} joints\n", f.joints));
    }
    if !l.model.animations.is_empty() {
        let a: Vec<String> = l.model.animations.iter().map(|a| format!("{} {:.2}s", a.name, a.duration)).collect();
        out.push_str(&format!("  animations: {}\n", a.join(", ")));
    }
    if f.morph_targets > 0 {
        out.push_str(&format!("  morph targets: {} (drawn without them)\n", f.morph_targets));
    }
    out.push_str(&format!("  pose: {}; taken as facing {}{}\n", l.pose, l.facing, if l.facing == "+z" { " (glTF's convention)" } else { ", turned to face the front camera" }));
    if let Some((_, how)) = &l.focus {
        out.push_str(&format!("  close-up on {how}, {}× closer\n", num(req.zoom.max(1.0))));
    }
    if !f.extensions_used.is_empty() {
        out.push_str(&format!("  extensions: {}\n", f.extensions_used.join(", ")));
    }
    for t in f.textures.iter().filter(|t| t.error.is_some()) {
        out.push_str(&format!("  WARNING: texture #{} ({}) failed to load; it shows magenta\n", t.image, if t.uses.is_empty() { "unused".to_string() } else { t.uses.join(", ") }));
    }
    for w in &f.warnings {
        out.push_str(&format!("  WARNING: {w}\n"));
    }
    out.push('\n');
    out
}

/// A GLB of a unit cube in `color` (linear RGB), optionally with a base-colour texture whose
/// bytes are `texture` (any, to test what isn't an image): for tests.
#[doc(hidden)]
pub fn test_cube(color: [f32; 3], texture: Option<&[u8]>) -> Vec<u8> {
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ];
    let (mut pos, mut nrm, mut uv, mut idx) = (Vec::<f32>::new(), Vec::<f32>::new(), Vec::<f32>::new(), Vec::<u16>::new());
    for (n, a, b) in faces {
        let (n, a, b) = (Vec3::from(n), Vec3::from(a), Vec3::from(b));
        // a × b == n, so (0,0) (1,0) (1,1) (0,1) in a, b runs counter-clockwise seen from outside.
        let base = (pos.len() / 3) as u16;
        for (s, t) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (n + a * s + b * t) * 0.5;
            pos.extend(p.to_array());
            nrm.extend(n.to_array());
            uv.extend([(s + 1.0) / 2.0, (1.0 - t) / 2.0]);
        }
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let mut bin = Vec::new();
    let mut put = |bytes: &[u8]| {
        let off = bin.len();
        bin.extend_from_slice(bytes);
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        (off, bytes.len())
    };
    let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let (po, pl) = put(&f(&pos));
    let (no, nl) = put(&f(&nrm));
    let (uo, ul) = put(&f(&uv));
    let (io, il) = put(&idx.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>());
    let tex = texture.map(put);
    let mut views = vec![
        serde_json::json!({ "buffer": 0, "byteOffset": po, "byteLength": pl }),
        serde_json::json!({ "buffer": 0, "byteOffset": no, "byteLength": nl }),
        serde_json::json!({ "buffer": 0, "byteOffset": uo, "byteLength": ul }),
        serde_json::json!({ "buffer": 0, "byteOffset": io, "byteLength": il }),
    ];
    let mut pbr = serde_json::json!({ "baseColorFactor": [color[0], color[1], color[2], 1.0], "metallicFactor": 0.0, "roughnessFactor": 0.8 });
    let mut doc = serde_json::json!({
        "asset": { "version": "2.0", "generator": "wundur test" },
        "scene": 0, "scenes": [{ "nodes": [0] }], "nodes": [{ "mesh": 0, "name": "cube" }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 }, "indices": 3, "material": 0 }] }],
        "accessors": [
            { "bufferView": 0, "componentType": 5126, "count": 24, "type": "VEC3", "min": [-0.5, -0.5, -0.5], "max": [0.5, 0.5, 0.5] },
            { "bufferView": 1, "componentType": 5126, "count": 24, "type": "VEC3" },
            { "bufferView": 2, "componentType": 5126, "count": 24, "type": "VEC2" },
            { "bufferView": 3, "componentType": 5123, "count": 36, "type": "SCALAR" }
        ],
    });
    if let Some((to, tl)) = tex {
        views.push(serde_json::json!({ "buffer": 0, "byteOffset": to, "byteLength": tl }));
        doc["images"] = serde_json::json!([{ "bufferView": 4, "mimeType": "image/png" }]);
        doc["textures"] = serde_json::json!([{ "source": 0 }]);
        pbr["baseColorTexture"] = serde_json::json!({ "index": 0 });
    }
    doc["materials"] = serde_json::json!([{ "name": "paint", "pbrMetallicRoughness": pbr }]);
    doc["bufferViews"] = serde_json::Value::Array(views);
    doc["buffers"] = serde_json::json!([{ "byteLength": bin.len() }]);
    let mut json = serde_json::to_vec(&doc).unwrap_or_default();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((json.len() as u32).to_le_bytes());
    out.extend(0x4E4F534Au32.to_le_bytes());
    out.extend(&json);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(0x004E4942u32.to_le_bytes());
    out.extend(&bin);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wundur-render-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn item(path: PathBuf) -> Item {
        Item { label: "cube".into(), shown_path: path.display().to_string(), path, facing: None }
    }

    fn request(models: Vec<Item>) -> Request {
        Request { models, views: vec![View::Front], animation: None, time: None, focus: None, zoom: 3.0, same_scale: false, cell: Some(128) }
    }

    fn center(sheet: &Sheet, row: u32) -> [u8; 3] {
        let y = LABEL + row * (LABEL + 128) + LABEL + 64;
        sheet.image.get_pixel(64, y).0
    }

    #[test]
    fn a_red_cube_is_drawn_red_in_the_middle_of_its_view() {
        let dir = scratch("red");
        std::fs::write(dir.join("cube.glb"), test_cube([0.8, 0.05, 0.05], None)).unwrap();
        let sheet = render(&request(vec![item(dir.join("cube.glb"))]));
        assert_eq!((sheet.failed, sheet.warnings), (0, 0), "{}", sheet.report);
        assert_eq!(sheet.image.dimensions(), (128, LABEL + LABEL + 128));
        let [r, g, b] = center(&sheet, 0);
        assert!(r > 150 && r > g + 90 && r > b + 90, "{r} {g} {b}");
        // The corner is the backdrop.
        let corner = sheet.image.get_pixel(2, LABEL * 2 + 2).0;
        assert!(corner[0].abs_diff(corner[2]) < 20, "{corner:?}");
        assert!(sheet.report.contains("12 triangles, 24 vertices"), "{}", sheet.report);
        assert!(sheet.report.contains("1.00 wide, 1.00 tall, 1.00 long"), "{}", sheet.report);
    }

    #[test]
    fn a_texture_that_cant_be_read_is_magenta_and_warned_about() {
        let dir = scratch("broken");
        std::fs::write(dir.join("cube.glb"), test_cube([1.0, 1.0, 1.0], Some(b"not an image"))).unwrap();
        let sheet = render(&request(vec![item(dir.join("cube.glb"))]));
        assert_eq!(sheet.warnings, 1, "{}", sheet.report);
        assert!(sheet.report.contains("WARNING: texture #0 (base colour) failed to load"), "{}", sheet.report);
        let [r, g, b] = center(&sheet, 0);
        assert!(r > 150 && b > 150 && g < 90, "{r} {g} {b}");
    }

    #[test]
    fn several_models_make_a_row_each_and_a_missing_one_says_why() {
        let dir = scratch("rows");
        std::fs::write(dir.join("a.glb"), test_cube([0.05, 0.6, 0.05], None)).unwrap();
        let mut req = request(vec![item(dir.join("a.glb")), item(dir.join("gone.glb"))]);
        req.views = vec![View::Front, View::Top];
        let sheet = render(&req);
        assert_eq!(sheet.image.dimensions(), (256, LABEL + 2 * (LABEL + 128)));
        assert_eq!(sheet.failed, 1);
        assert!(sheet.report.contains("couldn't be loaded"), "{}", sheet.report);
        let [r, g, _] = center(&sheet, 0);
        assert!(g > r + 40, "green cube: {r} {g}");
    }

    #[test]
    fn views_and_facings_parse() {
        assert_eq!(View::parse("Three Quarter"), Some(View::ThreeQuarter));
        assert_eq!(View::parse("side"), Some(View::Left));
        assert_eq!(View::parse("sideways"), None);
        assert_eq!(facing_turn("+X"), Some(-90.0));
        assert_eq!(facing_turn("up"), None);
    }

    /// Draws real models: `WUNDUR_LOOK="a.glb b.glb" cargo test -p wundur-render --release
    /// look_at_real_models -- --ignored --nocapture` (the picture lands in the temp folder).
    #[test]
    #[ignore]
    fn look_at_real_models() {
        let list = std::env::var("WUNDUR_LOOK").expect("WUNDUR_LOOK");
        let sheet = render(&request(list.split_whitespace().map(|p| item(p.into())).collect()));
        let out = std::env::temp_dir().join("wundur-look.png");
        sheet.image.save(&out).unwrap();
        println!("{}\n{}", out.display(), sheet.report);
    }
}
