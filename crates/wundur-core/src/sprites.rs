//! Sprite sheets: cut the separate pictures out of a sheet (one an AI made, with a flat or
//! transparent background, or a fixed grid) and pack them into a tight sheet, with a JSON beside
//! it in TexturePacker's "JSON hash" layout, which Phaser, PixiJS and the Godot and Unity
//! importers read. Wundur's `pack_sprites` tool comes here; it all runs on this machine.
//!
//! Two ways to pack:
//! - `frames`: every sprite in a cell of one size, aligned on a shared anchor (feet on one
//!   baseline by default), so an animation doesn't jump. A grid cut keeps each sprite where it
//!   sat in its cell.
//! - `atlas`: each sprite trimmed and packed as tight as it goes (MaxRects), for props and icons.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};
use serde_json::{Value, json};

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn right(&self) -> u32 {
        self.x + self.w
    }
    fn bottom(&self) -> u32 {
        self.y + self.h
    }
    fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
    /// The gap between two rectangles (0 when they touch or overlap).
    fn gap(&self, o: &Rect) -> u32 {
        let dx = o.x.saturating_sub(self.right()).max(self.x.saturating_sub(o.right()));
        let dy = o.y.saturating_sub(self.bottom()).max(self.y.saturating_sub(o.bottom()));
        dx.max(dy)
    }
}

/// One sprite cut from a source picture.
#[derive(Clone, Debug)]
pub struct Sprite {
    pub name: String,
    /// Which source picture it came from, and where it was there.
    pub source: usize,
    pub rect: Rect,
    /// Its pixels, background cleared.
    pub image: RgbaImage,
    /// Where it sat in its grid cell (a grid cut), and the cell's size.
    pub in_cell: Option<(u32, u32, u32, u32)>,
}

/// How a source picture's background was found.
#[derive(Clone, Debug, PartialEq)]
pub enum Background {
    Transparent,
    /// A flat colour, and the share of the border that matched it.
    Flat([u8; 3], f32),
}

/// How sprites are cut from a picture.
#[derive(Clone, Debug)]
pub struct Cut {
    /// Cut by a fixed grid (columns, rows) instead of finding shapes.
    pub grid: Option<(u32, u32)>,
    /// How different from the background a pixel must be to belong to a sprite (0–255).
    pub tolerance: u8,
    /// Shapes closer than this many pixels are one sprite (a sword and its hand, a spark).
    /// `None`: picked from the picture's size.
    pub merge: Option<u32>,
    /// Shapes smaller than this many pixels are dropped as specks. `None`: from the size.
    pub min_pixels: Option<u32>,
}

impl Default for Cut {
    fn default() -> Self {
        Cut { grid: None, tolerance: 32, merge: None, min_pixels: None }
    }
}

/// The sprites cut from one picture, and what was found about it.
pub struct Cutting {
    pub sprites: Vec<Sprite>,
    pub background: Background,
}

/// Cut the sprites out of `img` (source number `source`), in reading order.
pub fn cut(img: &RgbaImage, source: usize, opts: &Cut) -> Cutting {
    let (w, h) = img.dimensions();
    let background = find_background(img, opts.tolerance);
    let bg = background_mask(img, &background, opts.tolerance);
    let cleared = clear_background(img, &bg, &background, opts.tolerance);
    let fg = |x: u32, y: u32| !bg[(y * w + x) as usize] && cleared.get_pixel(x, y)[3] > 0;
    let mut sprites = Vec::new();
    if let Some((cols, rows)) = opts.grid {
        let (cw, ch) = (w / cols.max(1), h / rows.max(1));
        for r in 0..rows {
            for c in 0..cols {
                let cell = Rect { x: c * cw, y: r * ch, w: cw, h: ch };
                let mut tight: Option<Rect> = None;
                for y in cell.y..cell.bottom() {
                    for x in cell.x..cell.right() {
                        if fg(x, y) {
                            let p = Rect { x, y, w: 1, h: 1 };
                            tight = Some(tight.map_or(p, |t| t.union(&p)));
                        }
                    }
                }
                if let Some(t) = tight {
                    let image = image::imageops::crop_imm(&cleared, t.x, t.y, t.w, t.h).to_image();
                    sprites.push(Sprite { name: String::new(), source, rect: t, image, in_cell: Some((t.x - cell.x, t.y - cell.y, cw, ch)) });
                }
            }
        }
        return Cutting { sprites, background };
    }

    // Shapes: 8-connected runs of sprite pixels.
    let mut label = vec![0u32; (w * h) as usize];
    let mut shapes: Vec<(Rect, u32)> = Vec::new();
    let mut queue = VecDeque::new();
    for y in 0..h {
        for x in 0..w {
            if label[(y * w + x) as usize] != 0 || !fg(x, y) {
                continue;
            }
            let id = shapes.len() as u32 + 1;
            let mut rect = Rect { x, y, w: 1, h: 1 };
            let mut count = 0;
            label[(y * w + x) as usize] = id;
            queue.push_back((x, y));
            while let Some((px, py)) = queue.pop_front() {
                count += 1;
                rect = rect.union(&Rect { x: px, y: py, w: 1, h: 1 });
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (nx, ny) = (px as i32 + dx, py as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let (nx, ny) = (nx as u32, ny as u32);
                        let i = (ny * w + nx) as usize;
                        if label[i] == 0 && fg(nx, ny) {
                            label[i] = id;
                            queue.push_back((nx, ny));
                        }
                    }
                }
            }
            shapes.push((rect, count));
        }
    }

    // Shapes close together are one sprite.
    let short = w.min(h);
    let merge = opts.merge.unwrap_or((short / 128).max(2));
    let mut groups: Vec<(Rect, u32, Vec<usize>)> = shapes.iter().enumerate().map(|(i, (r, n))| (*r, *n, vec![i])).collect();
    // Each group takes in every other close to it, again while it grows.
    let mut a = 0;
    while a < groups.len() {
        let mut grew = false;
        let mut b = 0;
        while b < groups.len() {
            if b != a && groups[a].0.gap(&groups[b].0) <= merge {
                let (rb, nb, mb) = groups.remove(b);
                if b < a {
                    a -= 1;
                }
                groups[a].0 = groups[a].0.union(&rb);
                groups[a].1 += nb;
                groups[a].2.extend(mb);
                grew = true;
            } else {
                b += 1;
            }
        }
        if !grew {
            a += 1;
        }
    }
    let min_pixels = opts.min_pixels.unwrap_or(((w * h) / 40_000).max(6));
    groups.retain(|g| g.1 >= min_pixels);
    let mut group = vec![usize::MAX; shapes.len()];
    for (gi, g) in groups.iter().enumerate() {
        for &s in &g.2 {
            group[s] = gi;
        }
    }

    for (gi, (rect, _, _)) in groups.iter().enumerate() {
        // Only this sprite's own pixels: a neighbour reaching into its box stays out.
        let mut image = RgbaImage::new(rect.w, rect.h);
        for y in 0..rect.h {
            for x in 0..rect.w {
                let (sx, sy) = (rect.x + x, rect.y + y);
                let l = label[(sy * w + sx) as usize];
                if l != 0 && group[l as usize - 1] == gi {
                    image.put_pixel(x, y, *cleared.get_pixel(sx, sy));
                }
            }
        }
        sprites.push(Sprite { name: String::new(), source, rect: *rect, image, in_cell: None });
    }
    reading_order(&mut sprites);
    Cutting { sprites, background }
}

/// Rows top to bottom (sprites whose heights mostly overlap share a row), each left to right.
fn reading_order(sprites: &mut Vec<Sprite>) {
    sprites.sort_by_key(|s| (s.rect.y, s.rect.x));
    let mut rows: Vec<Vec<Sprite>> = Vec::new();
    for s in sprites.drain(..) {
        let fits = rows.last().is_some_and(|row| {
            let (top, bottom) = row.iter().fold((u32::MAX, 0), |(t, b), r| (t.min(r.rect.y), b.max(r.rect.bottom())));
            let overlap = bottom.min(s.rect.bottom()).saturating_sub(top.max(s.rect.y));
            overlap * 2 >= s.rect.h.min(bottom - top)
        });
        if fits {
            rows.last_mut().unwrap().push(s);
        } else {
            rows.push(vec![s]);
        }
    }
    for mut row in rows {
        row.sort_by_key(|s| s.rect.x);
        sprites.extend(row);
    }
}

fn border(img: &RgbaImage) -> Vec<Rgba<u8>> {
    let (w, h) = img.dimensions();
    let mut out = Vec::new();
    for x in 0..w {
        out.push(*img.get_pixel(x, 0));
        out.push(*img.get_pixel(x, h - 1));
    }
    for y in 1..h.saturating_sub(1) {
        out.push(*img.get_pixel(0, y));
        out.push(*img.get_pixel(w - 1, y));
    }
    out
}

fn distance(p: &Rgba<u8>, c: [u8; 3]) -> u32 {
    (0..3).map(|i| (p[i] as i32 - c[i] as i32).unsigned_abs()).max().unwrap_or(0)
}

/// The background: transparent when most of the border is, else the border's commonest colour.
fn find_background(img: &RgbaImage, tolerance: u8) -> Background {
    let edge = border(img);
    if edge.is_empty() {
        return Background::Transparent;
    }
    let clear = edge.iter().filter(|p| p[3] < 16).count();
    if clear * 2 >= edge.len() {
        return Background::Transparent;
    }
    // The commonest colour, in buckets of 16 per channel, then the mean of that bucket.
    let mut counts = std::collections::HashMap::<(u8, u8, u8), (u32, [u64; 3])>::new();
    for p in &edge {
        let e = counts.entry((p[0] >> 4, p[1] >> 4, p[2] >> 4)).or_default();
        e.0 += 1;
        for i in 0..3 {
            e.1[i] += p[i] as u64;
        }
    }
    let (_, (n, sum)) = counts.into_iter().max_by_key(|(_, (n, _))| *n).unwrap();
    let c = [(sum[0] / n as u64) as u8, (sum[1] / n as u64) as u8, (sum[2] / n as u64) as u8];
    let matched = edge.iter().filter(|p| distance(p, c) <= tolerance as u32).count();
    Background::Flat(c, matched as f32 / edge.len() as f32)
}

/// The background's pixels: for a flat colour, those like it reached from the border (so a
/// white eye inside a sprite stays); for a transparent one, the clear pixels.
fn background_mask(img: &RgbaImage, bg: &Background, tolerance: u8) -> Vec<bool> {
    let (w, h) = img.dimensions();
    let mut mask = vec![false; (w * h) as usize];
    match bg {
        Background::Transparent => {
            for (i, p) in img.pixels().enumerate() {
                mask[i] = p[3] < 16;
            }
        }
        Background::Flat(c, _) => {
            let like = |x: u32, y: u32| {
                let p = img.get_pixel(x, y);
                p[3] < 16 || distance(p, *c) <= tolerance as u32
            };
            let mut queue = VecDeque::new();
            let seed = |x: u32, y: u32, mask: &mut Vec<bool>, q: &mut VecDeque<(u32, u32)>| {
                let i = (y * w + x) as usize;
                if !mask[i] && like(x, y) {
                    mask[i] = true;
                    q.push_back((x, y));
                }
            };
            for x in 0..w {
                seed(x, 0, &mut mask, &mut queue);
                seed(x, h - 1, &mut mask, &mut queue);
            }
            for y in 0..h {
                seed(0, y, &mut mask, &mut queue);
                seed(w - 1, y, &mut mask, &mut queue);
            }
            while let Some((x, y)) = queue.pop_front() {
                if x > 0 {
                    seed(x - 1, y, &mut mask, &mut queue);
                }
                if y > 0 {
                    seed(x, y - 1, &mut mask, &mut queue);
                }
                if x + 1 < w {
                    seed(x + 1, y, &mut mask, &mut queue);
                }
                if y + 1 < h {
                    seed(x, y + 1, &mut mask, &mut queue);
                }
            }
        }
    }
    mask
}

/// The picture with its background cleared. On a flat colour, the sprite's outermost ring of
/// pixels (blended with the background when it was drawn) is made partly clear and has the
/// background taken out of its colour, so a sprite cut from white has no white halo.
fn clear_background(img: &RgbaImage, mask: &[bool], bg: &Background, tolerance: u8) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = img.clone();
    for (i, p) in out.pixels_mut().enumerate() {
        if mask[i] {
            *p = Rgba([0, 0, 0, 0]);
        }
    }
    let Background::Flat(c, _) = bg else { return out };
    let reach = (tolerance as f32 * 3.0).max(1.0);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if mask[i] {
                continue;
            }
            let edge = [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 && mask[(ny as u32 * w + nx as u32) as usize]
            });
            if !edge {
                continue;
            }
            let p = *img.get_pixel(x, y);
            let d = distance(&p, *c) as f32;
            if d >= reach {
                continue;
            }
            let a = (d / reach).clamp(0.15, 1.0);
            let mut q = [0u8; 4];
            for k in 0..3 {
                q[k] = ((p[k] as f32 - c[k] as f32 * (1.0 - a)) / a).round().clamp(0.0, 255.0) as u8;
            }
            q[3] = (p[3] as f32 * a).round() as u8;
            out.put_pixel(x, y, Rgba(q));
        }
    }
    out
}

/// How sprites sit on the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Frames,
    Atlas,
}

/// Where a sprite sits in its cell (frames).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Bottom,
    Center,
}

#[derive(Clone, Debug)]
pub struct Pack {
    pub mode: Mode,
    pub padding: u32,
    pub anchor: Anchor,
    /// Frames: columns (default: what makes the sheet squarest).
    pub columns: Option<u32>,
    /// Atlas: sides rounded up to powers of two.
    pub power_of_two: bool,
    pub max_size: u32,
}

impl Default for Pack {
    fn default() -> Self {
        Pack { mode: Mode::Frames, padding: 2, anchor: Anchor::Bottom, columns: None, power_of_two: false, max_size: 4096 }
    }
}

/// A packed sheet: the picture, where each sprite went, and its cells (frames).
pub struct Sheet {
    pub image: RgbaImage,
    /// Each sprite's place on the sheet (its cell, in frames mode).
    pub places: Vec<Rect>,
    pub cell: Option<(u32, u32, u32, u32)>,
    /// How much of the sheet the sprites' pixels cover (0–1).
    pub fill: f32,
}

/// Pack `sprites` into one sheet.
pub fn pack(sprites: &[Sprite], opts: &Pack) -> Result<Sheet, String> {
    if sprites.is_empty() {
        return Err("no sprites to pack".into());
    }
    let pad = opts.padding;
    let (places, size, cell) = match opts.mode {
        Mode::Frames => {
            let grid = sprites.iter().all(|s| s.in_cell.is_some());
            let (cw, ch) = if grid {
                sprites.iter().fold((0, 0), |(w, h), s| {
                    let c = s.in_cell.unwrap();
                    (w.max(c.2), h.max(c.3))
                })
            } else {
                sprites.iter().fold((0, 0), |(w, h), s| (w.max(s.image.width()), h.max(s.image.height())))
            };
            let n = sprites.len() as u32;
            let cols = opts.columns.filter(|c| *c > 0).unwrap_or_else(|| {
                // The squarest sheet.
                (1..=n).min_by_key(|c| {
                    let rows = n.div_ceil(*c);
                    let (sw, sh) = (c * (cw + pad), rows * (ch + pad));
                    (sw.max(sh) as u64 * 1000 / sw.min(sh).max(1) as u64, rows * c - n)
                })
                .unwrap_or(1)
            });
            let rows = n.div_ceil(cols);
            let places: Vec<Rect> = (0..n).map(|i| Rect { x: pad + (i % cols) * (cw + pad), y: pad + (i / cols) * (ch + pad), w: cw, h: ch }).collect();
            (places, (pad + cols * (cw + pad), pad + rows * (ch + pad)), Some((cw, ch, cols, rows)))
        }
        Mode::Atlas => {
            let (places, size) = max_rects(sprites, pad, opts.power_of_two, opts.max_size)?;
            (places, size, None)
        }
    };
    if size.0 > opts.max_size || size.1 > opts.max_size {
        return Err(format!("the sheet would be {}×{}, over the {} limit; pack fewer sprites, or raise max_size", size.0, size.1, opts.max_size));
    }
    let mut image = RgbaImage::new(size.0, size.1);
    let mut covered = 0u64;
    for (s, place) in sprites.iter().zip(&places) {
        let (sw, sh) = s.image.dimensions();
        let (ox, oy) = match (opts.mode, s.in_cell) {
            (Mode::Atlas, _) => (0, 0),
            (Mode::Frames, Some((x, y, _, _))) => (x, y),
            (Mode::Frames, None) => match opts.anchor {
                Anchor::Bottom => ((place.w - sw) / 2, place.h - sh),
                Anchor::Center => ((place.w - sw) / 2, (place.h - sh) / 2),
            },
        };
        image::imageops::overlay(&mut image, &s.image, (place.x + ox) as i64, (place.y + oy) as i64);
        covered += s.image.pixels().filter(|p| p[3] > 0).count() as u64;
    }
    let fill = covered as f32 / (size.0 as u64 * size.1 as u64).max(1) as f32;
    Ok(Sheet { image, places, cell, fill })
}

/// MaxRects (best short side fit), trying a range of widths and keeping the smallest sheet.
fn max_rects(sprites: &[Sprite], pad: u32, pot: bool, max: u32) -> Result<(Vec<Rect>, (u32, u32)), String> {
    let sizes: Vec<(u32, u32)> = sprites.iter().map(|s| (s.image.width() + pad, s.image.height() + pad)).collect();
    let area: u64 = sizes.iter().map(|(w, h)| *w as u64 * *h as u64).sum();
    let widest = sizes.iter().map(|s| s.0).max().unwrap_or(1);
    let start = ((area as f64).sqrt() as u32).max(widest);
    let mut candidates: Vec<u32> = (0..24).map(|k| start + start * k / 12).filter(|w| *w <= max).collect();
    if pot {
        candidates = candidates.into_iter().map(|w| w.next_power_of_two()).collect();
        candidates.dedup();
    }
    if candidates.is_empty() {
        candidates.push(widest);
    }
    // Biggest first: the usual order for MaxRects.
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((sizes[i].0.max(sizes[i].1), sizes[i].0 * sizes[i].1)));
    let mut best: Option<(u64, Vec<Rect>, (u32, u32))> = None;
    for bw in candidates {
        let inner = bw.saturating_sub(pad);
        let tall = sizes.iter().map(|s| s.1).sum::<u32>().max(1);
        let mut free = vec![Rect { x: 0, y: 0, w: inner, h: tall }];
        let mut places = vec![Rect::default(); sizes.len()];
        let mut fits = true;
        for &i in &order {
            let (w, h) = sizes[i];
            let pick = free.iter().filter(|f| f.w >= w && f.h >= h).min_by_key(|f| ((f.w - w).min(f.h - h), f.y, f.x)).copied();
            let Some(f) = pick else {
                fits = false;
                break;
            };
            let placed = Rect { x: f.x, y: f.y, w, h };
            places[i] = placed;
            // Split every free rectangle the new one overlaps.
            let mut next = Vec::new();
            for r in free.drain(..) {
                let overlaps = placed.x < r.right() && placed.right() > r.x && placed.y < r.bottom() && placed.bottom() > r.y;
                if !overlaps {
                    next.push(r);
                    continue;
                }
                if placed.x > r.x {
                    next.push(Rect { x: r.x, y: r.y, w: placed.x - r.x, h: r.h });
                }
                if placed.right() < r.right() {
                    next.push(Rect { x: placed.right(), y: r.y, w: r.right() - placed.right(), h: r.h });
                }
                if placed.y > r.y {
                    next.push(Rect { x: r.x, y: r.y, w: r.w, h: placed.y - r.y });
                }
                if placed.bottom() < r.bottom() {
                    next.push(Rect { x: r.x, y: placed.bottom(), w: r.w, h: r.bottom() - placed.bottom() });
                }
            }
            // Drop free rectangles inside others.
            let mut kept: Vec<Rect> = Vec::new();
            for (a, ra) in next.iter().enumerate() {
                let inside = next.iter().enumerate().any(|(b, rb)| {
                    a != b && rb.x <= ra.x && rb.y <= ra.y && rb.right() >= ra.right() && rb.bottom() >= ra.bottom() && (ra != rb || b < a)
                });
                if !inside {
                    kept.push(*ra);
                }
            }
            free = kept;
        }
        if !fits {
            continue;
        }
        let used_w = places.iter().map(|p| p.right()).max().unwrap_or(0) + pad;
        let used_h = places.iter().map(|p| p.bottom()).max().unwrap_or(0) + pad;
        let (sw, sh) = if pot { (used_w.next_power_of_two(), used_h.next_power_of_two()) } else { (used_w, used_h) };
        if sh > max {
            continue;
        }
        let score = sw as u64 * sh as u64 * 1000 + (sw.max(sh) - sw.min(sh)) as u64;
        if best.as_ref().is_none_or(|b| score < b.0) {
            // The sprite itself sits past the padding before it.
            let shown = places.iter().map(|p| Rect { x: p.x + pad, y: p.y + pad, w: p.w - pad, h: p.h - pad }).collect();
            best = Some((score, shown, (sw, sh)));
        }
    }
    best.map(|b| (b.1, b.2)).ok_or_else(|| format!("the sprites don't fit in a {max}×{max} sheet"))
}

/// The sheet's JSON, TexturePacker's "JSON hash": `frames` by name, an animation through all of
/// them in order, and Wundur's own facts under `meta.wundur` (the order, the mode, the cells,
/// where each sprite came from), which the sheet view reads.
pub fn sheet_json(sprites: &[Sprite], sheet: &Sheet, opts: &Pack, image_name: &str, animation: &str, sources: &[String]) -> Value {
    let mut frames = serde_json::Map::new();
    let mut from = serde_json::Map::new();
    for (s, place) in sprites.iter().zip(&sheet.places) {
        let pivot = match (opts.mode, opts.anchor) {
            (Mode::Frames, Anchor::Bottom) => json!({ "x": 0.5, "y": 1.0 }),
            _ => json!({ "x": 0.5, "y": 0.5 }),
        };
        frames.insert(
            s.name.clone(),
            json!({
                "frame": { "x": place.x, "y": place.y, "w": place.w, "h": place.h },
                "rotated": false,
                "trimmed": false,
                "spriteSourceSize": { "x": 0, "y": 0, "w": place.w, "h": place.h },
                "sourceSize": { "w": place.w, "h": place.h },
                "pivot": pivot,
            }),
        );
        from.insert(s.name.clone(), json!({ "source": s.source, "x": s.rect.x, "y": s.rect.y, "w": s.rect.w, "h": s.rect.h }));
    }
    let order: Vec<&str> = sprites.iter().map(|s| s.name.as_str()).collect();
    let mut wundur = json!({
        "mode": match opts.mode { Mode::Frames => "frames", Mode::Atlas => "atlas" },
        "padding": opts.padding,
        "order": order,
        "sources": sources,
        "from": from,
        "fill": (sheet.fill * 1000.0).round() / 1000.0,
    });
    if let Some((cw, ch, cols, rows)) = sheet.cell {
        wundur["cell"] = json!({ "w": cw, "h": ch, "columns": cols, "rows": rows });
    }
    json!({
        "frames": frames,
        "animations": { animation: order },
        "meta": {
            "app": "Wundur",
            "version": env!("CARGO_PKG_VERSION"),
            "image": image_name,
            "format": "RGBA8888",
            "size": { "w": sheet.image.width(), "h": sheet.image.height() },
            "scale": "1",
            "wundur": wundur,
        },
    })
}

/// A sheet as the view reads it back from its JSON.
#[derive(Clone, Debug, Default)]
pub struct SheetInfo {
    pub image: PathBuf,
    /// Names and places on the sheet, in order.
    pub frames: Vec<(String, Rect)>,
    pub mode: String,
    pub cell: Option<(u32, u32, u32, u32)>,
    pub fill: Option<f32>,
}

/// Read a sheet's JSON (or find the JSON beside a sheet's PNG).
pub fn read_sheet(path: &Path) -> Result<SheetInfo, String> {
    let json_path = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) { path.to_path_buf() } else { path.with_extension("json") };
    let text = std::fs::read_to_string(&json_path).map_err(|e| format!("{}: {e}", json_path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", json_path.display()))?;
    let dir = json_path.parent().unwrap_or(Path::new("."));
    let image = v["meta"]["image"].as_str().map(|i| dir.join(i)).unwrap_or_else(|| json_path.with_extension("png"));
    let rect = |f: &Value| Rect {
        x: f["x"].as_u64().unwrap_or(0) as u32,
        y: f["y"].as_u64().unwrap_or(0) as u32,
        w: f["w"].as_u64().unwrap_or(0) as u32,
        h: f["h"].as_u64().unwrap_or(0) as u32,
    };
    let mut frames: Vec<(String, Rect)> = Vec::new();
    match &v["frames"] {
        Value::Object(m) => {
            let order: Vec<String> = v["meta"]["wundur"]["order"].as_array().map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let names: Vec<String> = if order.is_empty() { m.keys().cloned().collect() } else { order };
            for n in names {
                if let Some(f) = m.get(&n) {
                    frames.push((n, rect(&f["frame"])));
                }
            }
        }
        // TexturePacker's "JSON array" layout.
        Value::Array(a) => {
            for f in a {
                frames.push((f["filename"].as_str().unwrap_or_default().to_string(), rect(&f["frame"])));
            }
        }
        _ => return Err(format!("{} has no frames", json_path.display())),
    }
    let w = &v["meta"]["wundur"];
    let cell = w["cell"].as_object().map(|c| {
        let g = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
        (g("w"), g("h"), g("columns"), g("rows"))
    });
    Ok(SheetInfo { image, frames, mode: w["mode"].as_str().unwrap_or("atlas").to_string(), cell, fill: w["fill"].as_f64().map(|f| f as f32) })
}

/// What `pack_sprites` answers.
#[derive(Debug, Default)]
pub struct Packed {
    pub text: String,
    pub failed: bool,
    /// The check picture: the sources with each sprite's box and number.
    pub check: Option<PathBuf>,
    /// The sheet's JSON, for the view.
    pub sheet: Option<PathBuf>,
}

fn fail(text: impl Into<String>) -> Packed {
    Packed { text: text.into(), failed: true, ..Default::default() }
}

fn resolve(cwd: &Path, p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if Path::new(p).is_absolute() => PathBuf::from(p),
        _ => cwd.join(p),
    }
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(['-', '_']) {
            out.push('_');
        }
    }
    let s = out.trim_matches(['-', '_']).to_string();
    if s.is_empty() { "sprite".into() } else { s }
}

/// The `pack_sprites` tool: `args` as an agent gives them, for a session in `cwd`.
pub fn pack_sprites(cwd: &Path, args: &Value) -> Packed {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let num = |k: &str| args.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))).map(|n| n as u32);
    let flag = |k: &str| args.get(k).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|t| t == "true")));
    let given: Vec<String> = match args.get("images").or(args.get("image")) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect(),
        Some(Value::String(m)) => m.split([',', '\n']).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect(),
        _ => Vec::new(),
    };
    if given.is_empty() {
        return fail("pack_sprites needs images: a sprite sheet's path, or several separated by commas.");
    }
    let mode = match s("mode").as_deref() {
        None | Some("frames") => Mode::Frames,
        Some("atlas") => Mode::Atlas,
        Some(m) => return fail(format!("mode is frames or atlas, not {m}.")),
    };
    let grid = match s("grid") {
        None => None,
        Some(g) => {
            let parts: Vec<u32> = g.split(['x', 'X', '×', ',']).filter_map(|p| p.trim().parse().ok()).collect();
            match parts[..] {
                [c, r] if c > 0 && r > 0 => Some((c, r)),
                _ => return fail(format!("grid is columns x rows, like 8x4, not {g}.")),
            }
        }
    };
    let split = flag("split").unwrap_or(true);
    let cut_opts = Cut { grid, tolerance: num("tolerance").map(|t| t.min(255) as u8).unwrap_or(32), merge: num("merge"), min_pixels: num("min_pixels") };
    let pack_opts = Pack {
        mode,
        padding: num("padding").unwrap_or(2).min(64),
        anchor: match s("anchor").as_deref() {
            Some("center") | Some("centre") => Anchor::Center,
            _ => Anchor::Bottom,
        },
        columns: num("columns"),
        power_of_two: flag("power_of_two").unwrap_or(false),
        max_size: num("max_size").unwrap_or(4096).clamp(64, 16384),
    };

    let mut sprites: Vec<Sprite> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    let mut pictures: Vec<RgbaImage> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for (i, g) in given.iter().enumerate() {
        let path = resolve(cwd, g);
        let img = match image::open(&path) {
            Ok(i) => i.to_rgba8(),
            Err(e) => return fail(format!("Couldn't read {g}: {e}")),
        };
        sources.push(g.clone());
        if !split {
            let rect = Rect { x: 0, y: 0, w: img.width(), h: img.height() };
            sprites.push(Sprite { name: String::new(), source: i, rect, image: img.clone(), in_cell: None });
            pictures.push(img);
            continue;
        }
        let c = cut(&img, i, &cut_opts);
        let stem = Path::new(g).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        match &c.background {
            Background::Transparent => notes.push(format!("{stem}: transparent background; {} sprites", c.sprites.len())),
            Background::Flat(col, share) => {
                notes.push(format!("{stem}: flat background #{:02x}{:02x}{:02x} (cleared); {} sprites", col[0], col[1], col[2], c.sprites.len()));
                if *share < 0.6 {
                    notes.push(format!(
                        "warning: only {:.0}% of {stem}'s border matches its background colour, so the background isn't flat (a gradient, noise, a fake checkerboard or a scene) and the cut is likely wrong. Remove the background first (edit_image, asking for a plain or transparent one) and pack again.",
                        share * 100.0
                    ));
                }
            }
        }
        if c.sprites.len() == 1 && grid.is_none() {
            notes.push(format!("warning: only one sprite found in {stem}: its sprites touch, or the background wasn't found. Look at the check picture; give grid (columns x rows) if it is a regular grid."));
        }
        sprites.extend(c.sprites);
        pictures.push(img);
    }
    if sprites.is_empty() {
        return fail(format!("No sprites found. {}", notes.join(" ")));
    }

    // "knight-walk-sheet.png" names its sprites knight_walk_00... and its sheet knight-walk-packed.png.
    let first = Path::new(&given[0]).file_stem().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "sprites".into());
    let lower = first.to_lowercase();
    let first = match ["-sheet", "_sheet", " sheet", "-spritesheet", "_spritesheet"].iter().find(|t| lower.ends_with(*t)) {
        Some(t) if first.len() > t.len() => first[..first.len() - t.len()].to_string(),
        _ => first,
    };
    let base = slug(&s("name").unwrap_or_else(|| first.clone()));
    let names: Vec<String> = s("names").map(|n| n.split(',').map(|x| slug(x.trim())).collect()).unwrap_or_default();
    let digits = sprites.len().to_string().len().max(2);
    for (i, sp) in sprites.iter_mut().enumerate() {
        sp.name = names.get(i).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("{base}_{i:0digits$}"));
    }
    if mode == Mode::Frames && grid.is_none() && sprites.len() > 1 {
        let areas: Vec<u64> = sprites.iter().map(|s| s.image.width() as u64 * s.image.height() as u64).collect();
        let (lo, hi) = (*areas.iter().min().unwrap(), *areas.iter().max().unwrap());
        if hi > lo.max(1) * 6 {
            notes.push("warning: the sprites differ a lot in size, so frame cells waste space; if they aren't frames of one animation, mode atlas packs them tighter.".into());
        }
    }

    let sheet = match pack(&sprites, &pack_opts) {
        Ok(s) => s,
        Err(e) => return fail(format!("Couldn't pack: {e}.")),
    };
    let out_png = match s("out") {
        Some(o) => {
            let p = resolve(cwd, &o);
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) { p } else { p.with_extension("png") }
        }
        None => resolve(cwd, &given[0]).with_file_name(format!("{first}-packed.png")),
    };
    let out_json = out_png.with_extension("json");
    if let Some(dir) = out_png.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = sheet.image.save(&out_png) {
        return fail(format!("Couldn't write {}: {e}", out_png.display()));
    }
    let image_name = out_png.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    let json = sheet_json(&sprites, &sheet, &pack_opts, &image_name, &base, &sources);
    if let Err(e) = std::fs::write(&out_json, serde_json::to_string_pretty(&json).unwrap_or_default()) {
        return fail(format!("Couldn't write {}: {e}", out_json.display()));
    }

    // The check picture.
    let check = if split {
        let path = cwd.join(crate::model_look::LOOKS).join(format!("{}-cut.png", base.replace('_', "-")));
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        check_picture(&pictures, &sprites).save(&path).ok().map(|_| path)
    } else {
        None
    };

    let shown = |p: &Path| p.strip_prefix(cwd).unwrap_or(p).display().to_string();
    let layout = match sheet.cell {
        Some((cw, ch, cols, rows)) => format!(
            "frames: {cols}×{rows} cells of {cw}×{ch}{}",
            if grid.is_some() { ", each sprite where it sat in its grid cell" } else if pack_opts.anchor == Anchor::Bottom { ", bottoms on one baseline" } else { ", centred" }
        ),
        None => "atlas: trimmed and packed tight".to_string(),
    };
    let mut text = format!(
        "Packed {} sprites ({layout}) into a {}×{} sheet, {:.0}% covered.\nWrote {} and {} (TexturePacker JSON hash: Phaser, PixiJS and the Godot and Unity importers read it; one animation \"{base}\" runs through the frames in order).\n{}",
        sprites.len(),
        sheet.image.width(),
        sheet.image.height(),
        sheet.fill * 100.0,
        shown(&out_png),
        shown(&out_json),
        notes.join("\n"),
    );
    if let Some(c) = &check {
        text.push_str(&format!(
            "\nThe picture: each source with every sprite's box and number in the order packed (saved as {}). Check that every sprite is one box: two boxes on one sprite means raise merge; one box over two sprites means lower it, or give grid.",
            shown(c)
        ));
    }
    Packed { text, failed: false, check, sheet: Some(out_json) }
}

/// The sources side by side, each sprite boxed and numbered, scaled to at most 1024 wide.
fn check_picture(pictures: &[RgbaImage], sprites: &[Sprite]) -> RgbaImage {
    let gap = 8;
    let w = pictures.iter().map(|p| p.width()).sum::<u32>() + gap * pictures.len().saturating_sub(1) as u32;
    let h = pictures.iter().map(|p| p.height()).max().unwrap_or(1);
    let mut out = RgbaImage::from_pixel(w.max(1), h.max(1), Rgba([40, 40, 44, 255]));
    let mut offsets = Vec::new();
    let mut x = 0;
    for p in pictures {
        // A checkerboard behind, so clear parts show as clear.
        for py in 0..p.height() {
            for px in 0..p.width() {
                let c = if ((px / 8) + (py / 8)) % 2 == 0 { 200 } else { 160 };
                out.put_pixel(x + px, py, Rgba([c, c, c, 255]));
            }
        }
        image::imageops::overlay(&mut out, p, x as i64, 0);
        offsets.push(x);
        x += p.width() + gap;
    }
    let scale = if w > 1024 { 1024.0 / w as f32 } else { 1.0 };
    let mut out = if scale < 1.0 {
        image::imageops::resize(&out, 1024, ((h as f32 * scale).round() as u32).max(1), image::imageops::FilterType::Triangle)
    } else {
        out
    };
    let colours = [[255, 64, 129], [0, 200, 255], [255, 200, 0], [120, 255, 80]];
    let dot = (out.width().max(out.height()) / 300).clamp(1, 4);
    for (i, s) in sprites.iter().enumerate() {
        let c = colours[i % colours.len()];
        let ox = offsets[s.source];
        let r = Rect {
            x: ((ox + s.rect.x) as f32 * scale) as u32,
            y: (s.rect.y as f32 * scale) as u32,
            w: ((s.rect.w as f32 * scale).ceil() as u32).max(1),
            h: ((s.rect.h as f32 * scale).ceil() as u32).max(1),
        };
        outline(&mut out, r, Rgba([c[0], c[1], c[2], 255]), dot);
        number(&mut out, r.x + dot + 1, r.y + dot + 1, i, Rgba([c[0], c[1], c[2], 255]), dot);
    }
    out
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, c: Rgba<u8>) {
    for py in y..(y + h).min(img.height()) {
        for px in x..(x + w).min(img.width()) {
            img.put_pixel(px, py, c);
        }
    }
}

fn outline(img: &mut RgbaImage, r: Rect, c: Rgba<u8>, t: u32) {
    fill(img, r.x, r.y, r.w, t, c);
    fill(img, r.x, (r.bottom()).saturating_sub(t), r.w, t, c);
    fill(img, r.x, r.y, t, r.h, c);
    fill(img, r.right().saturating_sub(t), r.y, t, r.h, c);
}

/// A number in a 3×5 pixel font, each pixel `s` wide, on a dark plate.
fn number(img: &mut RgbaImage, x: u32, y: u32, n: usize, c: Rgba<u8>, s: u32) {
    const DIGITS: [[u8; 5]; 10] = [
        [7, 5, 5, 5, 7],
        [2, 6, 2, 2, 7],
        [7, 1, 7, 4, 7],
        [7, 1, 7, 1, 7],
        [5, 5, 7, 1, 1],
        [7, 4, 7, 1, 7],
        [7, 4, 7, 5, 7],
        [7, 1, 1, 1, 1],
        [7, 5, 7, 5, 7],
        [7, 5, 7, 1, 7],
    ];
    let text = n.to_string();
    let s = s.max(1) + 1;
    fill(img, x, y, (text.len() as u32 * 4 + 1) * s, 7 * s, Rgba([0, 0, 0, 200]));
    for (k, ch) in text.bytes().enumerate() {
        let d = DIGITS[(ch - b'0') as usize];
        for (row, bits) in d.iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    fill(img, x + (1 + k as u32 * 4 + col) * s, y + (1 + row as u32) * s, s, s, c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white sheet with `boxes` drawn as solid red rectangles.
    fn sheet(w: u32, h: u32, boxes: &[Rect]) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([255, 255, 255, 255]));
        for b in boxes {
            fill(&mut img, b.x, b.y, b.w, b.h, Rgba([200, 20, 20, 255]));
        }
        img
    }

    #[test]
    fn cuts_shapes_on_white_in_reading_order() {
        let boxes = [Rect { x: 120, y: 10, w: 30, h: 40 }, Rect { x: 10, y: 12, w: 20, h: 30 }, Rect { x: 50, y: 100, w: 25, h: 25 }];
        let c = cut(&sheet(200, 160, &boxes), 0, &Cut::default());
        assert!(matches!(c.background, Background::Flat([255, 255, 255], _)));
        let rects: Vec<Rect> = c.sprites.iter().map(|s| s.rect).collect();
        assert_eq!(rects, vec![boxes[1], boxes[0], boxes[2]]);
        // The background is cleared, the sprite kept.
        assert_eq!(c.sprites[0].image.get_pixel(5, 5)[3], 255);
    }

    #[test]
    fn near_shapes_merge_and_specks_drop() {
        // Two parts 1px apart are one sprite; a lone pixel is a speck.
        let boxes = [Rect { x: 10, y: 10, w: 10, h: 10 }, Rect { x: 21, y: 10, w: 4, h: 4 }, Rect { x: 100, y: 100, w: 1, h: 1 }];
        let c = cut(&sheet(160, 160, &boxes), 0, &Cut::default());
        assert_eq!(c.sprites.len(), 1);
        assert_eq!(c.sprites[0].rect, Rect { x: 10, y: 10, w: 15, h: 10 });
    }

    #[test]
    fn keeps_inner_background_coloured_pixels() {
        // A red ring with a white middle: the middle isn't reached from the border, so it stays.
        let mut img = sheet(60, 60, &[Rect { x: 10, y: 10, w: 30, h: 30 }]);
        fill(&mut img, 20, 20, 10, 10, Rgba([255, 255, 255, 255]));
        let c = cut(&img, 0, &Cut::default());
        assert_eq!(c.sprites.len(), 1);
        assert_eq!(c.sprites[0].image.get_pixel(15, 15)[3], 255);
    }

    #[test]
    fn transparent_background() {
        let mut img = RgbaImage::new(50, 50);
        fill(&mut img, 5, 5, 10, 20, Rgba([0, 0, 255, 255]));
        fill(&mut img, 30, 5, 10, 10, Rgba([0, 0, 255, 255]));
        let c = cut(&img, 0, &Cut::default());
        assert_eq!(c.background, Background::Transparent);
        assert_eq!(c.sprites.len(), 2);
    }

    #[test]
    fn grid_keeps_place_in_cell() {
        let boxes = [Rect { x: 10, y: 20, w: 10, h: 10 }, Rect { x: 55, y: 30, w: 10, h: 10 }];
        let c = cut(&sheet(100, 50, &boxes), 0, &Cut { grid: Some((2, 1)), ..Default::default() });
        assert_eq!(c.sprites.len(), 2);
        assert_eq!(c.sprites[0].in_cell, Some((10, 20, 50, 50)));
        assert_eq!(c.sprites[1].in_cell, Some((5, 30, 50, 50)));
        let sheet = pack(&c.sprites, &Pack { padding: 0, ..Default::default() }).unwrap();
        assert_eq!(sheet.cell.map(|c| (c.0, c.1)), Some((50, 50)));
        // The second sprite lands 5,30 into its cell.
        let p = sheet.places[1];
        assert_eq!(sheet.image.get_pixel(p.x + 5, p.y + 30)[3], 255);
        assert_eq!(sheet.image.get_pixel(p.x + 4, p.y + 30)[3], 0);
    }

    #[test]
    fn frames_share_a_baseline() {
        let boxes = [Rect { x: 10, y: 10, w: 20, h: 40 }, Rect { x: 60, y: 10, w: 10, h: 20 }];
        let c = cut(&sheet(100, 60, &boxes), 0, &Cut::default());
        let sheet = pack(&c.sprites, &Pack { padding: 0, ..Default::default() }).unwrap();
        assert_eq!(sheet.cell.map(|c| (c.0, c.1)), Some((20, 40)));
        // The short one sits at the bottom of its cell, centred.
        let p = sheet.places[1];
        assert_eq!(sheet.image.get_pixel(p.x + 5, p.y + 39)[3], 255);
        assert_eq!(sheet.image.get_pixel(p.x + 5, p.y + 10)[3], 0);
    }

    #[test]
    fn atlas_places_never_overlap_and_fit() {
        let sprites: Vec<Sprite> = (0..30)
            .map(|i| {
                let (w, h) = (8 + (i * 7) % 40, 6 + (i * 13) % 50);
                Sprite { name: format!("s{i}"), source: 0, rect: Rect { x: 0, y: 0, w, h }, image: RgbaImage::from_pixel(w, h, Rgba([1, 2, 3, 255])), in_cell: None }
            })
            .collect();
        let opts = Pack { mode: Mode::Atlas, padding: 2, ..Default::default() };
        let sheet = pack(&sprites, &opts).unwrap();
        for (i, a) in sheet.places.iter().enumerate() {
            assert!(a.right() + 2 <= sheet.image.width() && a.bottom() + 2 <= sheet.image.height());
            assert_eq!((a.w, a.h), sprites[i].image.dimensions());
            for b in &sheet.places[i + 1..] {
                // At least the padding between any two.
                assert!(a.gap(b) >= 2 || a.right() + 2 <= b.x || b.right() + 2 <= a.x || a.bottom() + 2 <= b.y || b.bottom() + 2 <= a.y, "{a:?} {b:?}");
            }
        }
        assert!(sheet.fill > 0.6, "fill {}", sheet.fill);
    }

    #[test]
    fn json_reads_back_in_order() {
        let dir = std::env::temp_dir().join(format!("wundur-sprites-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hero.png");
        sheet(200, 80, &[Rect { x: 10, y: 10, w: 30, h: 50 }, Rect { x: 80, y: 20, w: 30, h: 40 }, Rect { x: 150, y: 10, w: 30, h: 50 }]).save(&src).unwrap();
        let out = pack_sprites(&dir, &json!({ "images": "hero.png", "names": "idle,step,jump" }));
        assert!(!out.failed, "{}", out.text);
        let info = read_sheet(&dir.join("hero-packed.png")).unwrap();
        let names: Vec<&str> = info.frames.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, vec!["idle", "step", "jump"]);
        assert_eq!(info.mode, "frames");
        assert_eq!(info.cell.map(|c| (c.0, c.1)), Some((30, 50)));
        assert!(info.image.ends_with("hero-packed.png") && info.image.is_file());
        assert!(out.check.is_some_and(|c| c.is_file()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
