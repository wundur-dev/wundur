//! Everything made for the shown session's project, over the whole window (as Settings is):
//! the pictures, sounds and 3D models in its `assets/` folder (see `wundur_core::library`),
//! newest first, with unfinished 3D assets ahead of them. Filtered by kind and by name. A
//! picture opens full size, a sound plays, a model or a step-by-step asset opens in its tab;
//! the menu on each shows it in the file manager or moves it to the Trash.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::host::Host;
use wundur_core::library::{self, Entry, Kind};

/// What the shell hears from the page.
pub enum AssetsEvent {
    Close,
    /// A step-by-step asset's folder: its tab.
    OpenAsset(PathBuf),
    /// A model file: the model viewer.
    OpenModel(PathBuf),
}

impl EventEmitter<AssetsEvent> for AssetsPage {}

const TILE_W: f32 = 176.;
const THUMB_H: f32 = 132.;
const TILE_H: f32 = THUMB_H + 50.;
const GAP: f32 = 12.;
const PAD: f32 = 24.;
/// The toolbar beside the page (the page is told the window's width, not its own).
const TOOLBAR_W: f32 = 56.;

pub struct AssetsPage {
    focus: FocusHandle,
    host: Arc<dyn Host>,
    folder: PathBuf,
    entries: Arc<Vec<Entry>>,
    /// Read at least once.
    loaded: bool,
    filter: Option<Kind>,
    search: Entity<InputState>,
    _search: Subscription,
}

impl AssetsPage {
    pub fn new(host: Arc<dyn Host>, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search by name"));
        let _search = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());
        let mut page = Self { focus: cx.focus_handle(), host, folder, entries: Arc::default(), loaded: false, filter: None, search, _search };
        page.refresh(cx);
        page
    }

    pub fn folder(&self) -> &std::path::Path {
        &self.folder
    }

    /// Read the folder again, off the UI thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.host.is_remote() {
            self.loaded = true;
            return;
        }
        let folder = self.folder.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let found = cx.background_spawn(async move { library::scan(&folder) }).await;
            let _ = this.update(cx, |p, cx| {
                p.entries = Arc::new(found);
                p.loaded = true;
                cx.notify();
            });
        })
        .detach();
    }

    /// Show only `kind` (the control channel's `assets filter`, and the chips).
    pub fn set_filter(&mut self, kind: Option<Kind>, cx: &mut Context<Self>) {
        self.filter = kind;
        cx.notify();
    }

    /// What it shows, for tests: how many of each kind, and the filter.
    pub fn state(&self) -> String {
        let count = |k: Kind| self.entries.iter().filter(|e| e.kind == k).count();
        format!("folder {}, loaded {}, images {}, models {}, audio {}, filter {:?}", self.folder.display(), self.loaded, count(Kind::Image), count(Kind::Model), count(Kind::Audio), self.filter)
    }

    fn open(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        match entry.kind {
            _ if entry.asset => cx.emit(AssetsEvent::OpenAsset(entry.path.clone())),
            Kind::Image => crate::image_view::show(entry.path.clone(), cx),
            Kind::Audio => crate::chat_panel::play_sound(&entry.path, cx),
            Kind::Model if matches!(entry.path.extension().and_then(|e| e.to_str()), Some("glb" | "gltf")) => cx.emit(AssetsEvent::OpenModel(entry.path.clone())),
            Kind::Model => cx.reveal_path(&entry.path),
        }
    }

    /// Move it to the Trash once the person says so.
    fn discard(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        let detail = if entry.asset { "Moves the asset's whole folder (its views and models) to the Trash." } else { "Moves it to the Trash." };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove {}?", entry.name),
            Some(detail),
            &[PromptButton::Ok("Move to Trash".into()), PromptButton::Cancel("Cancel".into())],
            cx,
        );
        let host = self.host.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let path = entry.path.clone();
            let gone = cx.background_spawn(async move { wundur_core::files::discard(host.as_ref(), &path) }).await;
            if let Err(e) = gone {
                eprintln!("wundur: could not remove {}: {e}", entry.path.display());
            }
            let _ = this.update(cx, |p, cx| p.refresh(cx));
        })
        .detach();
    }

    fn tile(entry: &Entry, ix: usize, me: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, ground, accent) = (theme.muted_foreground, theme.border, theme.secondary, theme.ring);
        let thumb = match (&entry.thumb, entry.kind) {
            (Some(t), _) => crate::thumbs::picture(t, crate::thumbs::TILE).size_full().object_fit(ObjectFit::Contain).into_any_element(),
            (None, Kind::Audio) => Icon::new(gpui_kit::assets::IconName::AudioLines).size(px(34.)).text_color(muted).into_any_element(),
            (None, Kind::Model) => Icon::new(gpui_kit::assets::IconName::Box).size(px(34.)).text_color(muted).into_any_element(),
            (None, Kind::Image) => Icon::new(gpui_kit::assets::IconName::Image).size(px(34.)).text_color(muted).into_any_element(),
        };
        let what = match (entry.kind, entry.asset) {
            (_, true) => "3D asset".to_string(),
            (Kind::Image, _) => format!("Image · {}", size(entry.bytes)),
            (Kind::Model, _) => format!("3D model · {}", size(entry.bytes)),
            (Kind::Audio, _) => format!("Sound · {}", size(entry.bytes)),
        };
        let (open, menu_me, menu_entry) = (me.clone(), me.clone(), entry.clone());
        let clicked = entry.clone();
        div()
            .id(("asset-tile", ix))
            .w(px(TILE_W))
            .h(px(TILE_H))
            .flex_none()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .overflow_hidden()
            .cursor_pointer()
            .hover(move |d| d.border_color(accent))
            .child(
                div()
                    .relative()
                    .h(px(THUMB_H))
                    .w_full()
                    .bg(ground)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(thumb)
                    .when(entry.kind == Kind::Audio, |d| {
                        d.child(div().absolute().bottom(px(6.)).right(px(6.)).child(Icon::new(gpui_kit::assets::IconName::Play).size(px(14.)).text_color(muted)))
                    })
                    .when(entry.in_progress, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(6.))
                                .left(px(6.))
                                .px_1p5()
                                .rounded_full()
                                .bg(accent)
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(gpui_kit::white())
                                .child("In progress"),
                        )
                    }),
            )
            .child(
                div()
                    .px_2()
                    .py_1p5()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(div().text_xs().truncate().child(entry.name.clone()))
                    .child(div().text_size(px(11.)).text_color(muted).truncate().child(what)),
            )
            .on_click(move |_, _, cx| {
                let _ = open.update(cx, |p, cx| p.open(&clicked, cx));
            })
            .context_menu(move |menu, _, _| {
                let (a, b) = (menu_me.clone(), menu_me.clone());
                let (opened, shown, copied, removed) = (menu_entry.clone(), menu_entry.path.clone(), menu_entry.path.clone(), menu_entry.clone());
                menu.item(PopupMenuItem::new("Open").on_click(move |_, _, cx| {
                    let _ = a.update(cx, |p, cx| p.open(&opened, cx));
                }))
                .item(PopupMenuItem::new(if cfg!(target_os = "macos") { "Show in Finder" } else { "Show in folder" }).on_click(move |_, _, cx| cx.reveal_path(&shown)))
                .item(PopupMenuItem::new("Copy path").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.display().to_string()))))
                .separator()
                .item(PopupMenuItem::new("Move to Trash").on_click(move |_, window, cx| {
                    let _ = b.update(cx, |p, cx| p.discard(removed.clone(), window, cx));
                }))
            })
            .into_any_element()
    }
}

/// A file's size, short: `812 KB`, `4.2 MB`.
fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

impl Focusable for AssetsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AssetsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border, sel) = (theme.foreground, theme.muted_foreground, theme.border, theme.secondary);
        let query = self.search.read(cx).value().trim().to_lowercase();
        let named: Vec<&Entry> = self.entries.iter().filter(|e| query.is_empty() || e.name.to_lowercase().contains(&query)).collect();
        let count = |k: Option<Kind>| named.iter().filter(|e| k.is_none_or(|k| e.kind == k)).count();
        let chips: [(&str, &str, Option<Kind>); 4] =
            [("assets-all", "All", None), ("assets-images", "Images", Some(Kind::Image)), ("assets-3d", "3D", Some(Kind::Model)), ("assets-audio", "Audio", Some(Kind::Audio))];
        let filter = self.filter;
        let shown: Arc<Vec<Entry>> = Arc::new(named.iter().filter(|e| filter.is_none_or(|k| e.kind == k)).map(|e| (*e).clone()).collect());

        let width = f32::from(window.viewport_size().width) - TOOLBAR_W - PAD * 2.;
        let cols = (((width + GAP) / (TILE_W + GAP)).floor() as usize).max(1);
        let rows = shown.len().div_ceil(cols);
        let me = cx.entity().downgrade();
        let grid = {
            let (shown, me) = (shown.clone(), me.clone());
            uniform_list("assets-grid", rows, move |range, _window, cx| {
                range
                    .map(|row| {
                        let from = row * cols;
                        div()
                            .id(("assets-row", row))
                            .w_full()
                            .h(px(TILE_H + GAP))
                            .px(px(PAD))
                            .flex()
                            .gap(px(GAP))
                            .children(shown.iter().enumerate().skip(from).take(cols).map(|(ix, e)| Self::tile(e, ix, &me, cx)))
                    })
                    .collect()
            })
            .size_full()
        };
        let empty = if self.host.is_remote() {
            Some("Assets on another machine aren't listed here yet.")
        } else if !self.loaded {
            Some("Looking…")
        } else if self.entries.is_empty() {
            Some("Nothing here yet. Images, sounds and 3D assets made from the Create menu or by an agent show up here (the project's assets folder).")
        } else if shown.is_empty() {
            Some("Nothing matches.")
        } else {
            None
        };
        let project = self.folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        div()
            .id("assets-page")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(fg)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && !crate::image_view::is_open(cx) {
                    cx.stop_propagation();
                    cx.emit(AssetsEvent::Close);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .px(px(PAD))
                    .pt_4()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Assets"))
                            .child(div().text_xs().text_color(muted).child(project)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(chips.into_iter().map(|(id, label, kind)| {
                                div()
                                    .id(id)
                                    .px_2p5()
                                    .py_1()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(border)
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(filter == kind, |d| d.bg(sel).font_weight(FontWeight::SEMIBOLD))
                                    .when(filter != kind, |d| d.text_color(muted))
                                    .child(format!("{label} {}", count(kind)))
                                    .on_click(cx.listener(move |p, _, _, cx| p.set_filter(kind, cx)))
                            }))
                            .child(div().flex_1())
                            .child(div().w(px(220.)).child(Input::new(&self.search).small().cleanable(true)))
                            .child(
                                Button::new("assets-refresh")
                                    .icon(Icon::new(IconName::RotateCw))
                                    .ghost()
                                    .small()
                                    .tooltip("Look again")
                                    .on_click(cx.listener(|p, _, _, cx| p.refresh(cx))),
                            ),
                    ),
            )
            .child(match empty {
                Some(text) => div().flex_1().px(px(PAD)).pt_6().text_sm().text_color(muted).child(text).into_any_element(),
                None => div().flex_1().min_h_0().child(grid).into_any_element(),
            })
    }
}
