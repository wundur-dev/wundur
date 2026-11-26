//! A sprite sheet in a tab: the sheet with each frame boxed and numbered, and beside it the
//! frames played in order as an animation (or one frame, picked by clicking it on the sheet).
//! It reads the sheet's JSON (TexturePacker's layout, which `pack_sprites` writes), and draws
//! the sheet once over a checkerboard so clear parts show as clear; the preview is the same
//! picture, cropped to the frame.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::assets::IconName;
use wundur_core::sprites::{Rect, SheetInfo};

/// Whether `path` is a sprite sheet this view shows: a JSON with frames, or a PNG with one beside it.
pub fn is_sheet(path: &Path) -> bool {
    let json = json_of(path);
    // Not reading a big data file whole just to ask.
    if std::fs::metadata(&json).map_or(true, |m| m.len() > 8 << 20) {
        return false;
    }
    std::fs::read_to_string(json).is_ok_and(|t| t.contains("\"frames\"") && t.contains("\"frame\""))
}

pub enum SpriteEvent {
    Activated,
    Closed,
}

impl EventEmitter<SpriteEvent> for SpriteView {}
impl EventEmitter<PanelEvent> for SpriteView {}

struct Loaded {
    info: SheetInfo,
    /// The sheet over a checkerboard, ready to draw.
    image: Arc<RenderImage>,
    size: (u32, u32),
}

enum State {
    Loading,
    Failed(String),
    Ready(Loaded),
}

pub struct SpriteView {
    /// The sheet's JSON.
    pub path: PathBuf,
    focus: FocusHandle,
    state: State,
    /// The frame shown in the preview.
    frame: usize,
    hover: Option<usize>,
    playing: bool,
    fps: u32,
    boxes: bool,
    /// Where the sheet was last drawn, for the pointer.
    drawn_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    player: Option<Task<()>>,
    load_task: Option<Task<()>>,
}

impl SpriteView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut v = SpriteView {
            path: json_of(&path),
            focus: cx.focus_handle(),
            state: State::Loading,
            frame: 0,
            hover: None,
            playing: false,
            fps: 8,
            boxes: true,
            drawn_at: Rc::new(Cell::new(None)),
            player: None,
            load_task: None,
        };
        v.reload(window, cx);
        v
    }

    /// Read the sheet again (it was packed again under the same name).
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            let loaded = cx.background_spawn(async move { load(&path) }).await;
            let _ = this.update_in(cx, |v, window, cx| {
                let old = std::mem::replace(&mut v.state, match loaded {
                    Ok(l) => State::Ready(l),
                    Err(e) => State::Failed(e),
                });
                if let State::Ready(old) = old {
                    cx.drop_image(old.image, None);
                }
                let n = v.frames().len();
                v.frame = v.frame.min(n.saturating_sub(1));
                // An animation plays as it opens.
                if n > 1 && v.mode() == "frames" && !v.playing {
                    v.play(window, cx);
                }
                cx.notify();
            });
        }));
    }

    fn frames(&self) -> &[(String, Rect)] {
        match &self.state {
            State::Ready(l) => &l.info.frames,
            _ => &[],
        }
    }

    fn mode(&self) -> &str {
        match &self.state {
            State::Ready(l) => &l.info.mode,
            _ => "",
        }
    }

    fn play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.frames().len() < 2 {
            return;
        }
        self.playing = true;
        self.player = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let fps = this.update(cx, |v, _| v.fps).unwrap_or(8).max(1);
                cx.background_executor().timer(Duration::from_millis(1000 / fps as u64)).await;
                let go = this
                    .update(cx, |v, cx| {
                        if !v.playing {
                            return false;
                        }
                        let n = v.frames().len().max(1);
                        v.frame = (v.frame + 1) % n;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        self.playing = false;
        self.player = None;
        cx.notify();
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.frames().len() as isize;
        if n == 0 {
            return;
        }
        self.pause(cx);
        self.frame = (self.frame as isize + by).rem_euclid(n) as usize;
    }

    /// The frame under a point on the sheet, if any.
    fn frame_at(&self, at: Point<Pixels>) -> Option<usize> {
        let State::Ready(l) = &self.state else { return None };
        let fit = fit(self.drawn_at.get()?, l.size);
        let scale = fit.size.width / px(l.size.0 as f32);
        let x = ((at.x - fit.origin.x) / px(1.0)) / scale;
        let y = ((at.y - fit.origin.y) / px(1.0)) / scale;
        l.info.frames.iter().position(|(_, r)| x >= r.x as f32 && y >= r.y as f32 && x < (r.x + r.w) as f32 && y < (r.y + r.h) as f32)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let bar = div().flex_none().min_h(px(34.)).px_2().py_0p5().flex().flex_wrap().items_center().justify_between().gap_x_2().border_b_1().border_color(border);
        let State::Ready(l) = &self.state else { return bar };
        let fill = l.info.fill.map(|f| format!(" · {:.0}% covered", f * 100.0)).unwrap_or_default();
        let cell = l.info.cell.map(|(w, h, c, r)| format!(" · {c}×{r} cells of {w}×{h}")).unwrap_or_default();
        let facts = format!("{} sprites · {}×{}{cell}{fill}", l.info.frames.len(), l.size.0, l.size.1);
        let playing = self.playing;
        let several = l.info.frames.len() > 1;
        let fps = self.fps;
        let controls = div()
            .flex()
            .items_center()
            .gap_1()
            .when(several, |d| {
                d.child(Button::new("sv-prev").ghost().xsmall().icon(IconName::ArrowLeft).tooltip("Previous frame").on_click(cx.listener(|v, _, _, cx| v.step(-1, cx))))
                    .child(
                        Button::new("sv-play")
                            .ghost()
                            .xsmall()
                            .icon(if playing { IconName::Pause } else { IconName::Play })
                            .tooltip(if playing { "Pause" } else { "Play the frames in order" })
                            .on_click(cx.listener(move |v, _, window, cx| if playing { v.pause(cx) } else { v.play(window, cx) })),
                    )
                    .child(Button::new("sv-next").ghost().xsmall().icon(IconName::ArrowRight).tooltip("Next frame").on_click(cx.listener(|v, _, _, cx| v.step(1, cx))))
                    .child(Button::new("sv-slower").ghost().xsmall().icon(IconName::Minus).tooltip("Slower").on_click(cx.listener(|v, _, _, cx| {
                        v.fps = (v.fps.saturating_sub(if v.fps > 12 { 4 } else { 1 })).max(1);
                        cx.notify();
                    })))
                    .child(div().text_xs().text_color(muted).w(px(42.)).flex().justify_center().child(format!("{fps} fps")))
                    .child(Button::new("sv-faster").ghost().xsmall().icon(IconName::Plus).tooltip("Faster").on_click(cx.listener(|v, _, _, cx| {
                        v.fps = (v.fps + if v.fps >= 12 { 4 } else { 1 }).min(60);
                        cx.notify();
                    })))
            })
            .child(Button::new("sv-boxes").ghost().xsmall().icon(IconName::Square).selected(self.boxes).tooltip("Show each frame's box").on_click(cx.listener(|v, _, _, cx| {
                v.boxes = !v.boxes;
                cx.notify();
            })))
            .child(Button::new("sv-reload").ghost().xsmall().icon(IconName::RefreshCw).tooltip("Read the sheet again").on_click(cx.listener(|v, _, window, cx| v.reload(window, cx))));
        bar.child(div().min_w_0().truncate().text_xs().text_color(muted).child(facts)).child(controls)
    }

    /// The sheet, fitted to the space, with the frames' boxes.
    fn sheet(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let State::Ready(l) = &self.state else { return div().id("sv-sheet") };
        let theme = cx.theme();
        let (accent, line) = (theme.primary, theme.muted_foreground.opacity(0.55));
        let image = l.image.clone();
        let size = l.size;
        let rects: Vec<Rect> = l.info.frames.iter().map(|f| f.1).collect();
        let (current, hover, boxes) = (self.frame, self.hover, self.boxes);
        let drawn_at = self.drawn_at.clone();
        let canvas = canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                drawn_at.set(Some(bounds));
                let at = fit(bounds, size);
                let _ = window.paint_image(at, at, Corners::default(), image, 0, false);
                let scale = at.size.width / px(size.0 as f32);
                let place = |r: &Rect| Bounds {
                    origin: point(at.origin.x + px(r.x as f32 * scale), at.origin.y + px(r.y as f32 * scale)),
                    size: gpui_kit::size(px(r.w as f32 * scale), px(r.h as f32 * scale)),
                };
                for (i, r) in rects.iter().enumerate() {
                    let b = place(r);
                    if i == current {
                        window.paint_quad(quad(b, px(0.), transparent_black(), px(2.), accent, BorderStyle::Solid));
                    } else if Some(i) == hover {
                        window.paint_quad(quad(b, px(0.), accent.opacity(0.12), px(1.), accent, BorderStyle::Solid));
                    } else if boxes {
                        window.paint_quad(outline(b, line, BorderStyle::Dashed));
                    }
                }
            },
        )
        .size_full();
        div()
            .id("sv-sheet")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p_3()
            .overflow_hidden()
            .cursor_pointer()
            .child(canvas)
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _, cx| {
                let at = v.frame_at(e.position);
                if at != v.hover {
                    v.hover = at;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|v, e: &ClickEvent, window, cx| {
                window.focus(&v.focus, cx);
                if let Some(i) = v.frame_at(e.position()) {
                    v.pause(cx);
                    v.frame = i;
                }
            }))
    }

    /// The current frame, as large as fits.
    fn preview(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (border, muted, fg) = (theme.border, theme.muted_foreground, theme.foreground);
        let State::Ready(l) = &self.state else { return div() };
        let Some((name, r)) = l.info.frames.get(self.frame).cloned() else { return div() };
        let image = l.image.clone();
        let size = l.size;
        let canvas = canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                // The frame fitted to the box, and the whole sheet placed so the frame lands there.
                let at = fit(bounds, (r.w, r.h));
                let scale = at.size.width / px(r.w.max(1) as f32);
                let whole = Bounds {
                    origin: point(at.origin.x - px(r.x as f32 * scale), at.origin.y - px(r.y as f32 * scale)),
                    size: gpui_kit::size(px(size.0 as f32 * scale), px(size.1 as f32 * scale)),
                };
                let _ = window.paint_image(at, whole, Corners::default(), image, 0, false);
            },
        )
        .size_full();
        let count = l.info.frames.len();
        div()
            // A share of a narrow tab, so the sheet keeps most of it.
            .flex_none()
            .w(relative(0.35))
            .min_w(px(110.))
            .max_w(px(280.))
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(border)
            .child(div().flex_1().min_h_0().p_3().child(canvas))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .text_xs()
                    .child(div().text_color(fg).truncate().child(name))
                    .child(div().text_color(muted).child(format!("Frame {} of {count} · {}×{} at {},{}", self.frame + 1, r.w, r.h, r.x, r.y))),
            )
    }
}

/// The sheet's JSON for a path given as its JSON or its PNG.
fn json_of(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) { path.to_path_buf() } else { path.with_extension("json") }
}

/// `size` fitted inside `bounds`, centred, keeping its shape; whole pixels when it is drawn
/// larger, so pixel art stays square.
fn fit(bounds: Bounds<Pixels>, size: (u32, u32)) -> Bounds<Pixels> {
    let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let (bw, bh) = (bounds.size.width / px(1.0), bounds.size.height / px(1.0));
    let mut scale = (bw / w).min(bh / h);
    if scale > 1.0 {
        scale = scale.floor();
    }
    let (dw, dh) = (w * scale, h * scale);
    Bounds { origin: point(bounds.origin.x + px((bw - dw) / 2.0), bounds.origin.y + px((bh - dh) / 2.0)), size: gpui_kit::size(px(dw), px(dh)) }
}

/// Read the sheet and its picture, drawn over a checkerboard.
fn load(path: &Path) -> Result<Loaded, String> {
    let info = wundur_core::sprites::read_sheet(path)?;
    let img = image::open(&info.image).map_err(|e| format!("{}: {e}", info.image.display()))?.to_rgba8();
    let (w, h) = img.dimensions();
    let check = (w.max(h) / 64).max(8);
    let mut out = image::RgbaImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels() {
        let c: f32 = if ((x / check) + (y / check)) % 2 == 0 { 204. } else { 170. };
        let a = p[3] as f32 / 255.0;
        let mix = |v: u8| (v as f32 * a + c * (1.0 - a)).round() as u8;
        // GPUI wants BGRA.
        out.put_pixel(x, y, image::Rgba([mix(p[2]), mix(p[1]), mix(p[0]), 255]));
    }
    let image = Arc::new(RenderImage::new(vec![image::Frame::new(out)]));
    Ok(Loaded { info, image, size: (w, h) })
}

impl Focusable for SpriteView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SpriteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, muted) = (theme.background, theme.foreground, theme.muted_foreground);
        let root = div()
            .id("sprite-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| match e.keystroke.key.as_str() {
                "left" => v.step(-1, cx),
                "right" => v.step(1, cx),
                "space" => {
                    if v.playing {
                        v.pause(cx)
                    } else {
                        v.play(window, cx)
                    }
                }
                _ => {}
            }));
        match &self.state {
            State::Loading => root.child(div().flex_1().flex().items_center().justify_center().text_sm().text_color(muted).child(format!("Loading {}…", self.path.display()))),
            State::Failed(e) => root.child(div().flex_1().flex().items_center().justify_center().p_4().text_sm().text_color(muted).child(format!("Couldn't show the sheet: {e}"))),
            State::Ready(_) => {
                let toolbar = self.toolbar(cx);
                let sheet = self.sheet(cx);
                let preview = self.preview(cx);
                root.child(toolbar).child(div().flex_1().min_h_0().flex().child(sheet).child(preview))
            }
        }
    }
}

impl BasePanel for SpriteView {
    fn panel_name(&self) -> &'static str {
        "Sprites"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.playing = false;
        self.player = None;
        if let State::Ready(l) = std::mem::replace(&mut self.state, State::Loading) {
            cx.drop_image(l.image, None);
        }
        cx.emit(SpriteEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(SpriteEvent::Activated);
        }
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut s = PanelState::new(self.panel_name());
        s.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        s
    }
}

impl Panel for SpriteView {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let name = self.path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Sprites".into());
        div().flex().items_center().gap_1().child(Icon::new(IconName::Image).xsmall()).child(div().max_w(px(180.)).truncate().child(name))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn fit_keeps_shape_and_whole_pixels_when_larger() {
        let b = Bounds { origin: point(px(0.), px(0.)), size: gpui_kit::size(px(100.), px(50.)) };
        let f = fit(b, (20, 20));
        // 2.5× rounds down to 2×, centred.
        assert_eq!((f.size.width, f.size.height), (px(40.), px(40.)));
        assert_eq!((f.origin.x, f.origin.y), (px(30.), px(5.)));
        let f = fit(b, (400, 100));
        assert_eq!((f.size.width, f.size.height), (px(100.), px(25.)));
    }
}
