//! The Play link sheet, from the title bar's Share button: the project's web build (its build
//! command, its folder, whether it uses threads) and its one link: Share or Update link, the
//! address with Copy and Open, its state and players, Extend and Turn off. Nothing goes up
//! until a click here. See [`wundur_core::play_link`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable as _, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::play_link::{self as pl, Build, Candidate, PlayLink, ShareRecord, ShareView, Step};

/// The sheet asks to be closed.
pub struct CloseSheet;

/// What the Share button is doing.
enum Phase {
    Idle,
    Building,
    /// The build is listed; the first Share (or one with warnings) waits for a second click.
    Confirm(Build),
    Sending(Step),
}

pub struct PlayLinkSheet {
    /// The session's folder (where the build runs and `project.json` is).
    root: PathBuf,
    /// Where the link is kept (the main checkout), once known.
    record_root: Option<PathBuf>,
    setting: PlayLink,
    /// What detection found, while the setting isn't saved yet.
    note: Option<String>,
    /// The folders in the project that look like web builds, newest first: the folder list.
    candidates: Vec<Candidate>,
    detecting: bool,
    build_input: Entity<InputState>,
    title_input: Entity<InputState>,
    record: Option<ShareRecord>,
    /// The link as the API last showed it.
    view: Option<ShareView>,
    /// The link is kept here but isn't on the signed-in account (or was deleted).
    missing: bool,
    loading: bool,
    phase: Phase,
    error: Option<String>,
    /// The error means the link is gone: offer a new one.
    error_gone: bool,
    signed_out: bool,
    confirm_off: bool,
    busy_other: Option<&'static str>,
    copied: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<CloseSheet> for PlayLinkSheet {}

impl PlayLinkSheet {
    /// For the project in `root`, called `name` (the first link's title).
    pub fn new(root: PathBuf, name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let build_input = cx.new(|cx| InputState::new(window, cx).placeholder("None: the folder is uploaded as it is"));
        let title: String = name.chars().take(60).collect();
        let title_input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let sub = cx.subscribe_in(&build_input, window, |s, input, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                s.setting.build = input.read(cx).value().trim().to_string();
            }
        });
        let mut sheet = Self {
            root,
            record_root: None,
            setting: PlayLink::default(),
            note: None,
            candidates: Vec::new(),
            detecting: true,
            build_input,
            title_input,
            record: None,
            view: None,
            missing: false,
            loading: false,
            phase: Phase::Idle,
            error: None,
            error_gone: false,
            signed_out: false,
            confirm_off: false,
            busy_other: None,
            copied: false,
            _subs: vec![sub],
        };
        sheet.load(window, cx);
        sheet
    }

    /// Read the setting (or detect one) and the link, off the UI thread; then ask the API how
    /// the link is.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.background_spawn(async move {
            let saved = pl::setting(&root);
            let detected = if saved.is_none() { Some(pl::detect(&root)) } else { None };
            let record_root = pl::record_root(&root);
            let record = pl::record(&record_root);
            let found = pl::candidates(&root);
            (saved, detected, record_root, record, found)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (saved, detected, record_root, record, found) = task.await;
            let _ = this.update_in(cx, |s, window, cx| {
                s.detecting = false;
                s.candidates = found;
                match (saved, detected) {
                    (Some(saved), _) => s.setting = saved,
                    (None, Some(d)) => {
                        s.setting = d.setting.unwrap_or_default();
                        s.note = Some(d.note);
                    }
                    (None, None) => {}
                }
                let build = s.setting.build.clone();
                s.build_input.update(cx, |i, cx| i.set_value(build, window, cx));
                s.record_root = Some(record_root);
                s.record = record;
                cx.notify();
                s.refresh(cx);
            });
        })
        .detach();
    }

    /// Ask the API how the link is now.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.record.as_ref().map(|r| r.id.clone()) else { return };
        self.loading = true;
        let task = cx.background_spawn(async move { pl::list() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.loading = false;
                match result {
                    Ok(shares) => {
                        s.signed_out = false;
                        match shares.into_iter().find(|v| v.id == id) {
                            Some(v) if v.gone() => s.forget(Some(format!("This project's link was {}. Share again to make a new one.", gone_word(&v.status)))),
                            Some(v) => {
                                s.missing = false;
                                s.view = Some(v);
                            }
                            None => s.missing = true,
                        }
                    }
                    Err(wundur_core::account::Error::SignedOut) => s.signed_out = true,
                    Err(e) => s.error = Some(pl::message(&e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Forget the link (gone): the next Share makes a new one.
    fn forget(&mut self, note: Option<String>) {
        if let Some(root) = &self.record_root {
            pl::forget_record(root);
        }
        self.record = None;
        self.view = None;
        self.missing = false;
        self.error_gone = false;
        self.error = note;
    }

    fn save_setting(&mut self, cx: &mut Context<Self>) {
        self.setting.build = self.build_input.read(cx).value().trim().to_string();
        match pl::save_setting(&self.root, &self.setting) {
            Ok(()) => self.note = None,
            Err(e) => self.error = Some(e),
        }
    }

    /// Use the build in `output` (from the folder list).
    fn pick(&mut self, output: String, cx: &mut Context<Self>) {
        self.setting.output = output;
        self.error = None;
        self.save_setting(cx);
        cx.notify();
    }

    /// When the chosen folder's build was made.
    fn chosen_built(&self) -> Option<std::time::SystemTime> {
        if let Some(c) = self.candidates.iter().find(|c| c.output == self.setting.output) {
            return Some(c.built);
        }
        let folder = self.setting.output_in(&self.root)?;
        std::fs::metadata(folder.join("index.html")).and_then(|m| m.modified()).ok()
    }

    /// A build newer than the chosen one, elsewhere in the project.
    fn newer(&self) -> Option<&Candidate> {
        let newest = self.candidates.first()?;
        if newest.output == self.setting.output {
            return None;
        }
        match self.chosen_built() {
            Some(t) if newest.built <= t => None,
            _ => Some(newest),
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |s, cx| {
                s.setting.output = match folder.strip_prefix(&s.root) {
                    Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
                    Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
                    Err(_) => folder.to_string_lossy().to_string(),
                };
                s.error = None;
                s.save_setting(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Share or Update link: build (if there's a command), list the build, then upload (the
    /// first time, or with warnings, after a second click).
    pub fn share(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Idle) {
            return;
        }
        self.save_setting(cx);
        let Some(folder) = self.setting.output_in(&self.root) else {
            self.error = Some("Choose the folder the web build goes in first.".into());
            cx.notify();
            return;
        };
        self.error = None;
        self.error_gone = false;
        self.confirm_off = false;
        let build = self.setting.build.clone();
        self.phase = if build.is_empty() { Phase::Sending(Step::Asking) } else { Phase::Building };
        cx.notify();
        let root = self.root.clone();
        let task = cx.background_spawn(async move {
            if !build.is_empty() {
                pl::run_build(&root, &build)?;
            }
            pl::scan(&folder)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Err(e) => {
                        s.phase = Phase::Idle;
                        s.error = Some(e);
                    }
                    Ok(b) if s.record.is_none() || !b.warnings.is_empty() => s.phase = Phase::Confirm(b),
                    Ok(b) => s.send(b, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Upload `build` and publish it, with progress.
    fn send(&mut self, build: Build, cx: &mut Context<Self>) {
        let Some(record_root) = self.record_root.clone() else { return };
        let existing = self.record.as_ref().map(|r| r.id.clone());
        let title = self.title_input.read(cx).value().trim().to_string();
        let title = if title.is_empty() { "My game".to_string() } else { title.chars().take(60).collect() };
        let threads = self.setting.threads;
        self.phase = Phase::Sending(Step::Asking);
        cx.notify();
        let step = Arc::new(Mutex::new(Step::Asking));
        let seen = step.clone();
        let task = cx.background_spawn(async move {
            let made = |r: ShareRecord| {
                let _ = pl::save_record(&record_root, &r);
            };
            let progress = move |s: Step| {
                if let Ok(mut now) = step.lock() {
                    *now = s;
                }
            };
            pl::publish(existing.as_deref(), &title, &build, threads, &progress, &made).map_err(|e| (e.to_string(), pl::is_gone(e.code())))
        });
        // Show the progress while it goes.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let now = seen.lock().map(|s| s.clone()).unwrap_or(Step::Asking);
                let going = this
                    .update(cx, |s, cx| {
                        if let Phase::Sending(_) = s.phase {
                            s.phase = Phase::Sending(now);
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.phase = Phase::Idle;
                // A new link was kept as soon as it was made.
                if s.record.is_none() {
                    s.record = s.record_root.as_ref().and_then(|r| pl::record(r));
                }
                match result {
                    Ok(view) => {
                        if let (Some(root), Some(r)) = (s.record_root.clone(), s.record.as_mut()) {
                            r.url = view.url.clone();
                            r.title = view.title.clone();
                            let _ = pl::save_record(&root, r);
                        }
                        s.missing = false;
                        s.view = Some(view);
                    }
                    Err((message, gone)) => {
                        s.error = Some(message);
                        s.error_gone = gone;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn extend(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.record.as_ref().map(|r| r.id.clone()) else { return };
        self.busy_other = Some("Extending…");
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { pl::extend(&id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy_other = None;
                match result {
                    Ok(v) => s.view = Some(v),
                    Err(e) => {
                        s.error_gone = pl::is_gone(e.code());
                        s.error = Some(pl::message(&e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn turn_off(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.record.as_ref().map(|r| r.id.clone()) else { return };
        self.busy_other = Some("Turning off…");
        self.confirm_off = false;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { pl::revoke(&id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy_other = None;
                match result {
                    Ok(()) => s.forget(Some("The link is off. Share again to make a new one.".into())),
                    Err(e) if pl::is_gone(e.code()) => s.forget(None),
                    Err(e) => s.error = Some(pl::message(&e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn working(&self) -> bool {
        !matches!(self.phase, Phase::Idle | Phase::Confirm(_)) || self.busy_other.is_some()
    }

    // ----- drawing -----

    fn render_setting(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let busy = self.working();
        let folder = if self.setting.output.is_empty() {
            "No web build yet".to_string()
        } else if self.setting.output == "." {
            "The project's folder".to_string()
        } else {
            format!("{}/", self.setting.output.trim_end_matches('/'))
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.note.clone().map(|n| div().text_xs().text_color(muted).child(n)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(fg).child("Build command"))
                    .child(Input::new(&self.build_input).small().disabled(busy)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(fg).child("Web build folder (with index.html at its top)"))
                    .child(self.render_folder_picker(folder, busy, cx))
                    .children(self.newer().map(|c| {
                        let output = c.output.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().text_xs().text_color(muted).child(format!("Newer build: {}/ ({}, built {})", c.output, c.kind.label(), ago(c.built))))
                            .child(Button::new("play-use-newer").xsmall().label("Use it").disabled(busy).on_click(cx.listener(move |s, _, _, cx| s.pick(output.clone(), cx))))
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().text_xs().text_color(fg).child("Build uses threads"))
                            .child(div().text_xs().text_color(muted).child(
                                "Needed for Godot with Thread Support or Unity multithreading. Blocks things the game loads from other sites.",
                            )),
                    )
                    .child(Switch::new("play-threads").checked(self.setting.threads).disabled(busy).on_click(cx.listener(|s, on: &bool, _, cx| {
                        s.setting.threads = *on;
                        s.save_setting(cx);
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    /// The chosen folder, and the list of builds in the project (newest first) to pick from,
    /// with Choose another folder… for one the list doesn't have.
    fn render_folder_picker(&self, folder: String, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let when = (!self.setting.output.is_empty()).then(|| self.chosen_built()).flatten().map(|t| format!("built {}", ago(t)));
        let shown = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(div().min_w_0().truncate().text_xs().text_color(if self.setting.output.is_empty() { muted } else { fg }).font_family(crate::fonts::code(cx)).child(folder))
            .children(when.map(|w| div().flex_none().text_xs().text_color(muted).child(w)));
        if self.candidates.is_empty() {
            return div()
                .flex()
                .items_center()
                .gap_2()
                .child(shown)
                .child(Button::new("play-folder").small().ghost().label("Choose…").disabled(busy).on_click(cx.listener(|s, _, _, cx| s.choose_folder(cx))))
                .into_any_element();
        }
        let me = cx.entity().downgrade();
        let list = self.candidates.clone();
        let chosen = self.setting.output.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(shown)
            .child(
                Button::new("play-folder")
                    .small()
                    .ghost()
                    .label(format!("{} found", count(list.len() as u64, "build")))
                    .dropdown_caret(true)
                    .disabled(busy)
                    .dropdown_menu(move |mut menu, _, _| {
                        for c in &list {
                            let me = me.clone();
                            let output = c.output.clone();
                            let label = format!("{}/   {} · built {}", c.output, c.kind.label(), ago(c.built));
                            menu = menu.item(PopupMenuItem::new(label).checked(c.output == chosen).on_click(move |_, _, cx| {
                                let _ = me.update(cx, |s, cx| s.pick(output.clone(), cx));
                            }));
                        }
                        let me = me.clone();
                        menu.separator().item(PopupMenuItem::new("Choose another folder…").on_click(move |_, _, cx| {
                            let _ = me.update(cx, |s, cx| s.choose_folder(cx));
                        }))
                    }),
            )
            .into_any_element()
    }

    fn render_link(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let record = self.record.as_ref()?;
        let theme = cx.theme();
        let (fg, muted, border, green, yellow, red) = (theme.foreground, theme.muted_foreground, theme.border, theme.green, theme.yellow, theme.red);
        let url = self.view.as_ref().map_or(record.url.clone(), |v| v.url.clone());
        let busy = self.working();
        let (u1, u2) = (url.clone(), url.clone());
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(v) = &self.view {
            let (label, color) = match v.status.as_str() {
                "live" => ("Live", green),
                "hidden" => ("Under review", yellow),
                "expired" => ("Expired", red),
                "uploading" => ("Not published yet", muted),
                other => (other, muted),
            };
            let when = match v.status.as_str() {
                "expired" => match (&v.expires_at, revivable(v)) {
                    (Some(e), true) => format!("Ended {}. You can extend it until {}.", day(e), v.revivable_until.as_deref().map(day).unwrap_or_default()),
                    (Some(e), false) => format!("Ended {}.", day(e)),
                    _ => String::new(),
                },
                "hidden" => "Players reported it; it's hidden while we look.".into(),
                _ => v.expires_at.as_deref().map(|e| format!("Ends {} unless people keep playing.", until(e))).unwrap_or_default(),
            };
            lines.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(div().size(px(7.)).rounded_full().bg(color))
                    .child(div().text_color(fg).font_weight(FontWeight::SEMIBOLD).child(label.to_string()))
                    .child(div().text_color(muted).child(when))
                    .into_any_element(),
            );
            lines.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!(
                        "{} today · {} in total · version {}{}",
                        count(v.players_today, "player"),
                        count(v.plays_total, "play"),
                        v.version,
                        if v.isolated { " · served for a threaded build" } else { "" }
                    ))
                    .into_any_element(),
            );
        } else if self.missing {
            lines.push(div().text_xs().text_color(muted).child("This link isn't on the account you're signed in with (or it was deleted).").into_any_element());
        } else if self.loading {
            lines.push(div().text_xs().text_color(muted).child("Checking the link…").into_any_element());
        }
        let can_extend = self.view.as_ref().is_some_and(|v| v.warning.as_deref() == Some("expires_soon") || (v.status == "expired" && revivable(v)));
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().truncate().text_sm().text_color(fg).font_family(crate::fonts::code(cx)).child(url))
                        .child(
                            Button::new("play-copy")
                                .xsmall()
                                .ghost()
                                .icon(if self.copied { IconName::Check } else { IconName::Copy })
                                .label(if self.copied { "Copied" } else { "Copy" })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(u1.clone()));
                                    s.copied = true;
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("play-open").xsmall().ghost().icon(gpui_kit::assets::IconName::ExternalLink).label("Open").on_click(move |_, _, cx| cx.open_url(&u2))),
                )
                .children(lines)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(can_extend, |d| {
                            d.child(Button::new("play-extend").small().label("Extend 7 days").disabled(busy).on_click(cx.listener(|s, _, _, cx| s.extend(cx))))
                        })
                        .when(self.missing, |d| {
                            d.child(Button::new("play-new").small().label("Make a new link").disabled(busy).on_click(cx.listener(|s, _, _, cx| {
                                s.forget(None);
                                cx.notify();
                            })))
                        })
                        .child(div().flex_1())
                        .when(!self.missing && !self.confirm_off, |d| {
                            d.child(Button::new("play-off").small().ghost().label("Turn off link").disabled(busy).on_click(cx.listener(|s, _, _, cx| {
                                s.confirm_off = true;
                                cx.notify();
                            })))
                        })
                        .when(self.confirm_off, |d| {
                            d.child(div().text_xs().text_color(muted).child("The link stops at once and its files are deleted."))
                                .child(Button::new("play-off-cancel").small().ghost().label("Keep it").on_click(cx.listener(|s, _, _, cx| {
                                    s.confirm_off = false;
                                    cx.notify();
                                })))
                                .child(Button::new("play-off-yes").small().danger().label("Turn off").on_click(cx.listener(|s, _, _, cx| s.turn_off(cx))))
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_phase(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, border, accent, yellow) = (theme.foreground, theme.muted_foreground, theme.border, theme.accent_foreground, theme.yellow);
        match &self.phase {
            Phase::Idle => None,
            Phase::Building => Some(div().text_xs().text_color(muted).child(format!("Building: {}", self.setting.build)).into_any_element()),
            Phase::Sending(step) => {
                let (text, frac) = match step {
                    Step::Asking => ("Getting ready…".to_string(), 0.),
                    Step::Uploading { done, total } => {
                        let f = if *total == 0 { 1. } else { *done as f32 / *total as f32 };
                        (format!("Uploading {} of {}", size(*done), size(*total)), f)
                    }
                    Step::Publishing => ("Publishing…".to_string(), 1.),
                };
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(text))
                        .child(div().h(px(4.)).w_full().rounded_full().bg(border).child(div().h_full().rounded_full().bg(accent).w(relative(frac.clamp(0., 1.)))))
                        .into_any_element(),
                )
            }
            Phase::Confirm(b) => {
                let first = self.record.is_none();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .child(div().text_xs().text_color(fg).child(format!(
                            "{} from {}: {}, {}.",
                            if first { "Ready to share" } else { "Ready to update" },
                            crate::project_wizard::tilde(&b.folder),
                            count(b.files.len() as u64, "file"),
                            size(b.bytes)
                        )))
                        .when(first, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().text_color(fg).child("Title (the link's name comes from it)"))
                                    .child(Input::new(&self.title_input).small()),
                            )
                            .child(div().text_xs().text_color(muted).child("Anyone with the link can play. It isn't listed anywhere, and it lasts 7 days, longer while people play."))
                        })
                        .children(b.warnings.iter().map(|w| div().text_xs().text_color(yellow).child(w.clone())))
                        .into_any_element(),
                )
            }
        }
    }
}

impl Render for PlayLinkSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, red) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.red);
        let busy = self.working();
        let has_link = self.record.is_some();
        let confirming = matches!(self.phase, Phase::Confirm(_));
        let main_label = match (&self.phase, self.busy_other) {
            (_, Some(b)) => b,
            (Phase::Building, _) => "Building…",
            (Phase::Sending(_), _) => if has_link { "Updating…" } else { "Sharing…" },
            (Phase::Confirm(_), _) => if has_link { "Upload and update" } else { "Upload and share" },
            (Phase::Idle, _) => if has_link { "Update link" } else { "Share" },
        };
        let body: AnyElement = if self.detecting {
            div().text_xs().text_color(muted).child("Looking for the web build…").into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_4()
                .children(self.render_link(cx))
                .child(self.render_setting(cx))
                .children(self.render_phase(cx))
                .when(self.signed_out, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().text_xs().text_color(muted).child("Sign in to share a play link."))
                            .child(Button::new("play-sign-in").small().label("Sign in").on_click(|_, _, cx| crate::settings_account::show_sign_in(cx))),
                    )
                })
                .children(self.error.clone().map(|e| {
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(div().flex_1().text_xs().text_color(red).whitespace_normal().child(e))
                        .when(self.error_gone, |d| {
                            d.child(Button::new("play-new-after").small().label("Make a new link").on_click(cx.listener(|s, _, _, cx| {
                                s.forget(None);
                                cx.notify();
                            })))
                        })
                }))
                .into_any_element()
        };
        div()
            .id("play-link-sheet")
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(70.))
            .bg(gpui_kit::black().opacity(0.25))
            .occlude()
            .capture_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && !s.working() {
                    if let Phase::Confirm(_) = s.phase {
                        s.phase = Phase::Idle;
                        cx.notify();
                    } else {
                        cx.emit(CloseSheet);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w(px(540.))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Play link"))
                            .child(div().flex_1())
                            .child(
                                Button::new("play-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip("Close (Esc)")
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSheet))),
                            ),
                    )
                    .child(div().text_xs().text_color(muted).child("A link to your game's web build for friends and testers. It only changes when you click Share or Update link."))
                    .child(body)
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1())
                            .when(confirming, |d| {
                                d.child(Button::new("play-back").small().ghost().label("Cancel").on_click(cx.listener(|s, _, _, cx| {
                                    s.phase = Phase::Idle;
                                    cx.notify();
                                })))
                            })
                            .when(!confirming, |d| d.child(Button::new("play-done").small().ghost().label("Close").disabled(busy).on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSheet)))))
                            .child(
                                Button::new("play-share")
                                    .small()
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::Share2)
                                    .label(main_label)
                                    .disabled(busy || self.detecting || self.setting.output.is_empty())
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        if let Phase::Confirm(_) = s.phase {
                                            if let Phase::Confirm(b) = std::mem::replace(&mut s.phase, Phase::Idle) {
                                                s.send(b, cx);
                                            }
                                        } else {
                                            s.share(cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}

/// An expired link can still be extended (within its grace).
fn revivable(v: &ShareView) -> bool {
    v.revivable_until.as_deref().and_then(parse).is_some_and(|t| t > chrono::Utc::now())
}

fn parse(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&chrono::Utc))
}

/// A date shortly: "Nov 28".
fn day(s: &str) -> String {
    parse(s).map(|t| t.with_timezone(&chrono::Local).format("%b %-d").to_string()).unwrap_or_else(|| s.to_string())
}

/// When something ends, from now: "in 5 days (Nov 28)", "today", "tomorrow".
fn until(s: &str) -> String {
    let Some(t) = parse(s) else { return s.to_string() };
    let days = (t.with_timezone(&chrono::Local).date_naive() - chrono::Local::now().date_naive()).num_days();
    match days {
        ..=0 => "today".into(),
        1 => "tomorrow".into(),
        n => format!("in {n} days ({})", day(s)),
    }
}

/// How long ago `t` was: "just now", "5 minutes ago", "2 hours ago", "3 days ago".
fn ago(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3600 => (secs / 60, "minute"),
        3600..86_400 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{} ago", count(n, unit))
}

fn count(n: u64, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_000_000. {
        format!("{:.1} MB", b / 1_000_000.)
    } else if b >= 1_000. {
        format!("{:.0} KB", b / 1_000.)
    } else {
        format!("{bytes} bytes")
    }
}

fn gone_word(status: &str) -> &'static str {
    match status {
        "removed" => "taken down",
        "deleted" => "deleted after it expired",
        _ => "turned off",
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would take the place of the standard one.
    use super::{ago, count, revivable, size, until};
    use wundur_core::play_link::ShareView;

    #[test]
    fn counts_and_sizes_read_well() {
        assert_eq!(count(1, "player"), "1 player");
        assert_eq!(count(3, "play"), "3 plays");
        assert_eq!(size(18_234_567), "18.2 MB");
        assert_eq!(size(4_200), "4 KB");
        assert_eq!(size(12), "12 bytes");
        let now = std::time::SystemTime::now();
        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - std::time::Duration::from_secs(5 * 60 + 3)), "5 minutes ago");
        assert_eq!(ago(now - std::time::Duration::from_secs(3600)), "1 hour ago");
        assert_eq!(ago(now - std::time::Duration::from_secs(3 * 86_400)), "3 days ago");
    }

    #[test]
    fn ends_are_said_from_today() {
        let soon = (chrono::Utc::now() + chrono::Duration::days(5)).to_rfc3339();
        assert!(until(&soon).starts_with("in 5 days ("), "{}", until(&soon));
        let past = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        assert_eq!(until(&past), "today");
        let v = ShareView { status: "expired".into(), revivable_until: Some(soon), ..Default::default() };
        assert!(revivable(&v));
        let v = ShareView { status: "expired".into(), revivable_until: Some(past), ..Default::default() };
        assert!(!revivable(&v));
    }
}
