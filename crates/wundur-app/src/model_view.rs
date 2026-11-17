//! A 3D model in a tab, drawn by Wundur itself (no web view): turn, pan and zoom it, play its
//! animations with the camera following, a floor with its shadow, light or dark.
//!
//! Frames are drawn on a thread of their own by a [`FrameRenderer`] from `wundur-render`;
//! [`renderer`] picks which, so the CPU one used now can be swapped (for a GPU one) without
//! this view changing. While something moves, frames are drawn smaller and stretched; once it
//! settles, one is drawn at full size.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_render::glam::Vec3;
use wundur_render::gltf::Model;
use wundur_render::view::{CpuRenderer, FrameRenderer, FrameSpec, Orbit};
use wundur_render::Look;

/// The renderer model tabs draw with. Swap it here.
pub fn renderer() -> Box<dyn FrameRenderer> {
    Box::new(CpuRenderer)
}

/// Whether `path` is a model this view shows.
pub fn is_model(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("glb") || e.eq_ignore_ascii_case("gltf"))
}

pub enum ModelEvent {
    Activated,
    Closed,
}

impl EventEmitter<ModelEvent> for ModelView {}
impl EventEmitter<PanelEvent> for ModelView {}

/// Where the camera starts: a three-quarter view from a little above.
const HOME: (f32, f32) = (35.0, 12.0);

struct Loaded {
    model: Arc<Model>,
    /// Triangles, size, textures: one line.
    summary: String,
    warnings: Vec<String>,
    /// Each animation's name and length.
    clips: Vec<(String, f32)>,
    /// The rest pose, to frame the model in any shape of view.
    rest: Arc<wundur_render::pose::Scene>,
    home: Orbit,
}

enum State {
    Loading,
    Failed(String),
    Ready(Loaded),
}

struct Job {
    orbit: Orbit,
    anim: Option<(usize, f32)>,
    follow: bool,
    look: Look,
    width: u32,
    height: u32,
    best: bool,
}

struct Done {
    image: Arc<RenderImage>,
    ms: f32,
}

pub struct ModelView {
    pub path: PathBuf,
    focus: FocusHandle,
    state: State,
    orbit: Orbit,
    clip: Option<usize>,
    time: f32,
    playing: bool,
    speed: f32,
    follow: bool,
    look: Look,
    frame: Option<Arc<RenderImage>>,
    /// The canvas in device pixels, and the window's scale.
    size: (u32, u32),
    scale: f32,
    jobs: Option<mpsc::Sender<Job>>,
    /// Bumped by every change; the settled frame waits for it to stop.
    seq: u64,
    drag: Option<(Point<Pixels>, bool)>,
    frame_ms: f32,
    slider: Entity<SliderState>,
    renderer_name: &'static str,
    note: Option<String>,
    /// An animation to play once the model has loaded (`show_model`'s).
    pending_clip: Option<String>,
    settle: Option<Task<()>>,
    player: Option<Task<()>>,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl ModelView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let slider = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001).default_value(0.));
        let sub = cx.subscribe_in(&slider, window, |v, _, ev: &SliderEvent, _, cx| {
            let (value, done) = match ev {
                SliderEvent::Change(x) => (x.end(), false),
                SliderEvent::Release(x) => (x.end(), true),
            };
            if let Some(len) = v.clip_len() {
                v.playing = false;
                v.player = None;
                v.time = value * len;
                v.request(!done, cx);
            }
        });
        let load_path = path.clone();
        let load = cx.spawn_in(window, async move |this, cx| {
            let loaded = cx.background_spawn(async move { load(&load_path) }).await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.loaded(loaded, cx);
                if let Some(clip) = v.pending_clip.take() {
                    v.play_named(&clip, window, cx);
                }
            });
        });
        Self {
            path,
            focus: cx.focus_handle(),
            state: State::Loading,
            orbit: Orbit { yaw: HOME.0, pitch: HOME.1, distance: 1.0, target: Vec3::ZERO },
            clip: None,
            time: 0.0,
            playing: false,
            speed: 1.0,
            follow: true,
            look: Look { dark: false, floor: true },
            frame: None,
            size: (0, 0),
            scale: 1.0,
            jobs: None,
            seq: 0,
            drag: None,
            frame_ms: 0.0,
            slider,
            renderer_name: "",
            note: None,
            pending_clip: None,
            settle: None,
            player: None,
            _subs: vec![sub],
            _tasks: vec![load],
        }
    }

    /// Play the animation whose name holds `name` (now, or once the model has loaded).
    pub fn play_named(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let want = name.trim().to_lowercase();
        match &self.state {
            State::Loading => self.pending_clip = Some(want),
            State::Ready(l) => {
                let found = l.clips.iter().position(|c| c.0.to_lowercase() == want).or_else(|| l.clips.iter().position(|c| c.0.to_lowercase().contains(&want)));
                if found.is_some() {
                    self.choose_clip(found, window, cx);
                }
            }
            State::Failed(_) => {}
        }
    }

    fn loaded(&mut self, loaded: Result<Loaded, String>, cx: &mut Context<Self>) {
        let l = match loaded {
            Ok(l) => l,
            Err(e) => {
                self.state = State::Failed(e);
                cx.notify();
                return;
            }
        };
        self.state = State::Ready(l);
        self.refit();
        let Some((home, model)) = self.ready().map(|l| (l.home, l.model.clone())) else { return };
        self.orbit = home;
        // The drawing thread: the newest job wins; it stops when this view is gone.
        let (tx, rx) = mpsc::channel::<Job>();
        let (dtx, drx) = async_channel::unbounded::<Done>();
        let mut r = renderer();
        self.renderer_name = r.name();
        std::thread::Builder::new()
            .name("model-view".into())
            .spawn(move || {
                let rest = wundur_render::pose::pose(&model, None);
                let rest_center = rest.center();
                let mut posed: (Option<(usize, u32)>, wundur_render::pose::Scene) = (None, rest);
                while let Ok(mut job) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        job = newer;
                    }
                    let key = job.anim.map(|(a, t)| (a, t.to_bits()));
                    if posed.0 != key {
                        posed = (key, wundur_render::pose::pose(&model, job.anim));
                    }
                    let scene = &posed.1;
                    let mut orbit = job.orbit;
                    if job.follow && job.anim.is_some() {
                        let d = scene.center() - rest_center;
                        orbit.target += Vec3::new(d.x, 0.0, d.z);
                    }
                    let t = Instant::now();
                    let mut img = r.render(&model, scene, &orbit.camera(), &FrameSpec { width: job.width, height: job.height, look: job.look, best: job.best });
                    let ms = t.elapsed().as_secs_f32() * 1000.0;
                    // gpui keeps images as BGRA.
                    for p in img.as_chunks_mut::<4>().0 {
                        p.swap(0, 2);
                    }
                    let image = Arc::new(RenderImage::new(vec![image::Frame::new(img)]));
                    if dtx.send_blocking(Done { image, ms }).is_err() {
                        break;
                    }
                }
            })
            .ok();
        self.jobs = Some(tx);
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(done) = drx.recv().await {
                if this.update(cx, |v, cx| v.got(done, cx)).is_err() {
                    break;
                }
            }
        }));
        self.request(false, cx);
        cx.notify();
    }

    fn got(&mut self, done: Done, cx: &mut Context<Self>) {
        if let Some(old) = self.frame.replace(done.image) {
            // Its pixels leave the GPU's image atlas.
            cx.drop_image(old, None);
        }
        self.frame_ms = done.ms;
        cx.notify();
    }

    fn ready(&self) -> Option<&Loaded> {
        match &self.state {
            State::Ready(l) => Some(l),
            _ => None,
        }
    }

    fn clip_len(&self) -> Option<f32> {
        let l = self.ready()?;
        self.clip.and_then(|c| l.clips.get(c)).map(|c| c.1.max(1e-3))
    }

    /// Draw a frame of what's set now. `moving`: something is changing (a drag, playback), so
    /// a quick smaller one now and a full one once it stops.
    fn request(&mut self, moving: bool, cx: &mut Context<Self>) {
        let (w, h) = self.size;
        let Some(tx) = &self.jobs else { return };
        if w == 0 || h == 0 {
            return;
        }
        self.seq += 1;
        let retina = self.scale >= 1.5;
        let shrink = if !moving {
            1.0
        } else if retina {
            0.5
        } else {
            0.75
        };
        let job = Job {
            orbit: self.orbit,
            anim: self.clip.map(|c| (c, self.time)),
            follow: self.follow,
            look: self.look,
            width: ((w as f32 * shrink) as u32).max(1),
            height: ((h as f32 * shrink) as u32).max(1),
            best: !moving && !retina,
        };
        let _ = tx.send(job);
        if moving {
            let seq = self.seq;
            self.settle = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(180)).await;
                let _ = this.update(cx, |v, cx| {
                    if v.seq == seq {
                        v.request(false, cx);
                    }
                });
            }));
        }
    }

    fn resized(&mut self, size: (u32, u32), scale: f32, cx: &mut Context<Self>) {
        if self.size != size || self.scale != scale {
            self.size = size;
            self.scale = scale;
            // Untouched, the view keeps the whole model in frame as its shape changes.
            let untouched = self.ready().is_some_and(|l| l.home == self.orbit);
            self.refit();
            if untouched && let Some(home) = self.ready().map(|l| l.home) {
                self.orbit = home;
            }
            self.request(false, cx);
        }
    }

    /// Frame the whole model for the view's present shape.
    fn refit(&mut self) {
        let aspect = if self.size.1 > 0 { self.size.0 as f32 / self.size.1 as f32 } else { 1.0 };
        if let State::Ready(l) = &mut self.state {
            l.home = Orbit::fit(&l.rest, HOME.0, HOME.1, aspect);
        }
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.refit();
        if let Some(home) = self.ready().map(|l| l.home) {
            self.orbit = home;
            self.request(false, cx);
            cx.notify();
        }
    }

    fn choose_clip(&mut self, clip: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.clip = clip;
        self.time = 0.0;
        self.slider.update(cx, |s, cx| s.set_value(0.0, window, cx));
        if clip.is_some() {
            self.play(window, cx);
        } else {
            self.playing = false;
            self.player = None;
            self.request(false, cx);
        }
        cx.notify();
    }

    fn play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.clip.is_none() {
            return;
        }
        self.playing = true;
        let mut last = Instant::now();
        self.player = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let now = Instant::now();
                let dt = now.duration_since(last).as_secs_f32();
                last = now;
                let go = this.update_in(cx, |v, window, cx| v.tick(dt, window, cx)).unwrap_or(false);
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
        self.request(false, cx);
        cx.notify();
    }

    fn tick(&mut self, dt: f32, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(len) = self.clip_len() else { return false };
        if !self.playing {
            return false;
        }
        self.time = (self.time + dt * self.speed).rem_euclid(len);
        let at = self.time / len;
        self.slider.update(cx, |s, cx| s.set_value(at, window, cx));
        self.request(true, cx);
        cx.notify();
        true
    }

    fn set_look(&mut self, look: Look, cx: &mut Context<Self>) {
        self.look = look;
        self.request(false, cx);
        cx.notify();
    }

    /// Send to Mixamo (it rigs and animates humanoids, with no API to hand a model to): export
    /// a ZIP of an OBJ and its textures (the form Mixamo takes), open mixamo.com, and show the
    /// ZIP in the file manager, so uploading is one drag.
    fn send_to_mixamo(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.ready() else { return };
        let model = l.model.clone();
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "model".into());
        let name: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let Some(out) = dirs::cache_dir().map(|d| d.join("wundur-studio").join("mixamo").join(format!("{name}-mixamo.zip"))) else { return };
        self.note = Some("Exporting for Mixamo…".into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (zip, n) = (out.clone(), name.clone());
            let done = cx.background_spawn(async move { wundur_render::export::obj_zip(&model, &n, &zip) }).await;
            let _ = this.update(cx, |v, cx| {
                v.note = Some(match done {
                    Ok(e) => {
                        cx.open_url("https://www.mixamo.com/");
                        cx.reveal_path(&out);
                        let mut said = format!(
                            "Exported {} ({} triangles, {} texture{}): in Mixamo, sign in, choose Upload Character and drop it there. Mixamo rigs humanoids; turn it to face you in its first step if it doesn't.",
                            out.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                            e.triangles,
                            e.textures,
                            if e.textures == 1 { "" } else { "s" }
                        );
                        for n in e.notes {
                            said.push_str(&format!(" Note: {n}."));
                        }
                        said
                    }
                    Err(e) => format!("Couldn't export for Mixamo: {e}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Save what's shown, drawn at its best, as a PNG where the person picks.
    fn save_png(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.ready() else { return };
        let model = l.model.clone();
        let (orbit, anim, follow, look) = (self.orbit, self.clip.map(|c| (c, self.time)), self.follow, self.look);
        let (w, h) = if self.size.0 > 0 { self.size } else { (1600, 1000) };
        let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "model".into());
        let picked = cx.prompt_for_new_path(&dir, Some(&format!("{stem}.png")));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(to))) = picked.await else { return };
            let saved = cx
                .background_spawn(async move {
                    let rest = wundur_render::pose::pose(&model, None);
                    let scene = wundur_render::pose::pose(&model, anim);
                    let mut orbit = orbit;
                    if follow && anim.is_some() {
                        let d = scene.center() - rest.center();
                        orbit.target += Vec3::new(d.x, 0.0, d.z);
                    }
                    let img = renderer().render(&model, &scene, &orbit.camera(), &FrameSpec { width: w, height: h, look, best: true });
                    img.save_with_format(&to, image::ImageFormat::Png).map(|_| to).map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.note = Some(match saved {
                    Ok(to) => format!("Saved {}", to.display()),
                    Err(e) => format!("Couldn't save: {e}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let clips = self.ready().map(|l| l.clips.clone()).unwrap_or_default();
        // Two groups that wrap onto a second row when the tab is narrow.
        let bar = div().flex_none().min_h(px(34.)).px_2().py_0p5().flex().flex_wrap().items_center().justify_between().gap_x_2().border_b_1().border_color(border);
        let mut anim = div().flex().flex_wrap().min_w_0().items_center().gap_1();
        if !clips.is_empty() {
            let me = cx.entity().downgrade();
            let chosen = self.clip;
            let label = chosen.and_then(|c| clips.get(c)).map(|c| c.0.clone()).unwrap_or_else(|| "Rest pose".into());
            let menu_clips = clips.clone();
            anim = anim.child(
                Button::new("mv-clip").ghost().xsmall().label(label).dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
                    let me0 = me.clone();
                    menu = menu.item(PopupMenuItem::new("Rest pose").checked(chosen.is_none()).on_click(move |_, window, cx| {
                        let _ = me0.update(cx, |v, cx| v.choose_clip(None, window, cx));
                    }));
                    for (i, (name, len)) in menu_clips.iter().enumerate() {
                        let me = me.clone();
                        menu = menu.item(PopupMenuItem::new(format!("{name}  ({len:.2}s)")).checked(chosen == Some(i)).on_click(move |_, window, cx| {
                            let _ = me.update(cx, |v, cx| v.choose_clip(Some(i), window, cx));
                        }));
                    }
                    menu
                }),
            );
            if let Some(len) = self.clip_len() {
                let playing = self.playing;
                anim = anim
                    .child(
                        Button::new("mv-play")
                            .ghost()
                            .xsmall()
                            .icon(if playing { gpui_kit::assets::IconName::Pause } else { gpui_kit::assets::IconName::Play })
                            .tooltip(if playing { "Pause" } else { "Play" })
                            .on_click(cx.listener(move |v, _, window, cx| if playing { v.pause(cx) } else { v.play(window, cx) })),
                    )
                    .child(div().flex_none().w(px(120.)).child(Slider::new(&self.slider)))
                    .child(div().text_xs().text_color(muted).w(px(78.)).child(format!("{:.2} / {:.2}s", self.time, len)));
                let me = cx.entity().downgrade();
                let speed = self.speed;
                anim = anim.child(Button::new("mv-speed").ghost().xsmall().label(format!("{}×", trim(speed))).dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
                    for s in [0.1f32, 0.25, 0.5, 1.0, 1.5, 2.0] {
                        let me = me.clone();
                        menu = menu.item(PopupMenuItem::new(format!("{}×", trim(s))).checked((s - speed).abs() < 1e-3).on_click(move |_, _, cx| {
                            let _ = me.update(cx, |v, cx| {
                                v.speed = s;
                                cx.notify();
                            });
                        }));
                    }
                    menu
                }));
                anim = anim.child(
                    Button::new("mv-follow").ghost().xsmall().label("Follow").selected(self.follow).tooltip("Keep the camera on the body as it moves").on_click(cx.listener(|v, _, _, cx| {
                        v.follow = !v.follow;
                        v.request(false, cx);
                        cx.notify();
                    })),
                );
            }
        }
        let _ = window;
        let look = self.look;
        let view = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(Button::new("mv-floor").ghost().xsmall().label("Floor").selected(look.floor).on_click(cx.listener(move |v, _, _, cx| v.set_look(Look { floor: !look.floor, ..look }, cx))))
            .child(Button::new("mv-dark").ghost().xsmall().label("Dark").selected(look.dark).on_click(cx.listener(move |v, _, _, cx| v.set_look(Look { dark: !look.dark, ..look }, cx))))
            .child(Button::new("mv-reset").ghost().xsmall().icon(gpui_kit::assets::IconName::RotateCcw).tooltip("Reset view (or double-click)").on_click(cx.listener(|v, _, _, cx| v.reset(cx))))
            .child(Button::new("mv-mixamo").ghost().xsmall().label("Mixamo").tooltip("Send to Mixamo: export it (OBJ and textures, zipped), open mixamo.com and show the file to drop in").on_click(cx.listener(|v, _, _, cx| v.send_to_mixamo(cx))))
            .child(Button::new("mv-save").ghost().xsmall().icon(gpui_kit::assets::IconName::Download).tooltip("Save as PNG").on_click(cx.listener(|v, _, _, cx| v.save_png(cx))));
        bar.child(anim).child(view)
    }

    fn stage(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let me = cx.entity().downgrade();
        let frame = self.frame.clone();
        let canvas = canvas(
            move |bounds, window, cx| {
                let s = window.scale_factor();
                let size = (((bounds.size.width / px(1.0)) * s).round().max(0.0) as u32, ((bounds.size.height / px(1.0)) * s).round().max(0.0) as u32);
                let me = me.clone();
                cx.defer(move |cx| {
                    let _ = me.update(cx, |v, cx| v.resized(size, s, cx));
                });
            },
            move |bounds, _, window, _| {
                if let Some(img) = frame {
                    let _ = window.paint_image(bounds, bounds, Corners::default(), img, 0, false);
                }
            },
        )
        .size_full();
        div()
            .id("mv-stage")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .cursor(if self.drag.is_some() { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .child(canvas)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, window, cx| {
                    window.focus(&v.focus, cx);
                    if e.click_count >= 2 {
                        v.reset(cx);
                        return;
                    }
                    v.drag = Some((e.position, e.modifiers.shift || e.modifiers.alt));
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    v.drag = Some((e.position, true));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _, cx| {
                let Some((last, pan)) = v.drag else { return };
                if e.pressed_button.is_none() {
                    v.drag = None;
                    cx.notify();
                    return;
                }
                let (dx, dy) = ((e.position.x - last.x) / px(1.0), (e.position.y - last.y) / px(1.0));
                v.drag = Some((e.position, pan));
                if pan {
                    let h = v.size.1 as f32 / v.scale.max(1.0);
                    v.orbit.pan(dx, dy, h);
                } else {
                    v.orbit.turn(dx, dy);
                }
                v.request(true, cx);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, cx| v.end_drag(cx)))
            .on_mouse_up(MouseButton::Right, cx.listener(|v, _, _, cx| v.end_drag(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|v, _, _, cx| v.end_drag(cx)))
            .on_mouse_up_out(MouseButton::Right, cx.listener(|v, _, _, cx| v.end_drag(cx)))
            .on_scroll(cx.listener(|v, e: &ScrollEvent, _, cx| {
                let dy = e.delta.pixel_delta(px(16.)).y / px(1.0);
                v.orbit.zoom(dy / 40.0);
                v.request(true, cx);
            }))
    }

    /// Tests and scripting (the control channel's `viewer …`): `clip <name>|rest`, `play`,
    /// `pause`, `time <seconds>`, `turn <dx> <dy>`, `zoom <steps>`, `dark`, `floor`, `follow`,
    /// `reset`, `state`.
    pub fn command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.trim().split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((line.trim(), ""));
        let nums: Vec<f32> = arg.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        match cmd {
            "clip" => {
                let want = arg.to_lowercase();
                let found = self.ready().and_then(|l| l.clips.iter().position(|c| c.0.to_lowercase().contains(&want)));
                if want == "rest" || found.is_some() {
                    self.choose_clip(found.filter(|_| want != "rest"), window, cx);
                } else {
                    return format!("no clip \"{arg}\"");
                }
            }
            "play" => self.play(window, cx),
            "pause" => self.pause(cx),
            "time" => {
                if let (Some(len), Some(&t)) = (self.clip_len(), nums.first()) {
                    self.playing = false;
                    self.player = None;
                    self.time = t.clamp(0.0, len);
                    let at = self.time / len;
                    self.slider.update(cx, |s, cx| s.set_value(at, window, cx));
                    self.request(false, cx);
                }
            }
            "turn" => {
                self.orbit.turn(nums.first().copied().unwrap_or(0.0), nums.get(1).copied().unwrap_or(0.0));
                self.request(true, cx);
            }
            "zoom" => {
                self.orbit.zoom(nums.first().copied().unwrap_or(1.0));
                self.request(true, cx);
            }
            "dark" => self.set_look(Look { dark: !self.look.dark, ..self.look }, cx),
            "floor" => self.set_look(Look { floor: !self.look.floor, ..self.look }, cx),
            "follow" => {
                self.follow = !self.follow;
                self.request(false, cx);
            }
            "reset" => self.reset(cx),
            "mixamo" => self.send_to_mixamo(cx),
            "state" => {}
            _ => return format!("unknown: {cmd}"),
        }
        cx.notify();
        format!(
            "clip={:?} time={:.2} playing={} yaw={:.0} pitch={:.0} dist={:.2} size={:?} frame_ms={:.1}",
            self.clip.and_then(|c| self.ready().and_then(|l| l.clips.get(c)).map(|c| c.0.clone())),
            self.time,
            self.playing,
            self.orbit.yaw,
            self.orbit.pitch,
            self.orbit.distance,
            self.size,
            self.frame_ms
        )
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }
}

fn trim(x: f32) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Read the model and what to say about it (off the UI thread).
fn load(path: &Path) -> Result<Loaded, String> {
    let model = wundur_render::gltf::load(path)?;
    let rest = wundur_render::pose::pose(&model, None);
    let size = rest.max - rest.min;
    let f = &model.facts;
    let tex: Vec<String> = f.textures.iter().map(|t| t.size.map(|(w, h)| format!("{w}×{h} {}", t.format)).unwrap_or_else(|| format!("{} (unread)", t.format))).collect();
    let mut summary = format!("{} triangles · {:.2} × {:.2} × {:.2}", thousands(rest.tris.len()), size.x, size.y, size.z);
    if !tex.is_empty() {
        summary.push_str(&format!(" · textures {}", tex.join(", ")));
    }
    if f.joints > 0 {
        summary.push_str(&format!(" · {} joints", f.joints));
    }
    let mut warnings: Vec<String> = f.textures.iter().filter_map(|t| t.error.as_ref().map(|e| format!("Texture #{} didn't load ({e}); it shows magenta", t.image))).collect();
    warnings.extend(f.warnings.iter().cloned());
    let clips = model.animations.iter().map(|a| (a.name.clone(), a.duration)).collect();
    let home = Orbit::fit(&rest, HOME.0, HOME.1, 1.0);
    Ok(Loaded { model: Arc::new(model), summary, warnings, clips, rest: Arc::new(rest), home })
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

impl Focusable for ModelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ModelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, muted, warn) = (theme.background, theme.foreground, theme.muted_foreground, theme.warning);
        let root = div().id("model-view").track_focus(&self.focus).size_full().flex().flex_col().bg(bg).text_color(fg);
        match &self.state {
            State::Loading => root.child(div().flex_1().flex().items_center().justify_center().text_sm().text_color(muted).child(format!("Loading {}…", self.path.display()))),
            State::Failed(e) => root.child(div().flex_1().flex().items_center().justify_center().p_4().text_sm().text_color(muted).child(format!("Couldn't show {}: {e}", self.path.display()))),
            State::Ready(l) => {
                let (summary, warnings) = (l.summary.clone(), l.warnings.clone());
                let overlay_fg = if self.look.dark { hsla(0., 0., 0.85, 0.85) } else { hsla(0., 0., 0.15, 0.75) };
                let status = format!("{} · {:.0} ms", self.renderer_name, self.frame_ms);
                let toolbar = self.toolbar(window, cx);
                let stage = self.stage(cx).child(
                    div()
                        .absolute()
                        .left(px(10.))
                        .top(px(8.))
                        .right(px(10.))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .text_xs()
                        .text_color(overlay_fg)
                        .child(summary)
                        .children(warnings.into_iter().map(|w| div().text_color(warn).child(format!("⚠ {w}"))))
                        .children(self.note.clone().map(|n| div().child(n))),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(10.))
                        .bottom(px(8.))
                        .right(px(10.))
                        .flex()
                        .text_xs()
                        .text_color(overlay_fg)
                        .gap_2()
                        .child(div().flex_1().min_w_0().truncate().child("Drag: turn · Shift-drag: pan · Scroll: zoom · Double-click: reset"))
                        .child(div().flex_none().child(status)),
                );
                root.child(toolbar).child(stage)
            }
        }
    }
}

impl BasePanel for ModelView {
    fn panel_name(&self) -> &'static str {
        "Model"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.playing = false;
        self.player = None;
        cx.emit(ModelEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(ModelEvent::Activated);
        }
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut s = PanelState::new(self.panel_name());
        s.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        s
    }
}

impl Panel for ModelView {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Model".into());
        div().flex().items_center().gap_1().child(Icon::new(gpui_kit::assets::IconName::Box).xsmall()).child(div().max_w(px(180.)).truncate().child(name))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
