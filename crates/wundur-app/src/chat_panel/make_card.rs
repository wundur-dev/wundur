//! Quick-make cards in the chat: an image or a sound effect from the person's Wundur account,
//! opened from the title bar's Create menu. The person writes the prompt and picks the options;
//! Wundur runs the same account tool an agent would (`wundur_core::account_tools`) and saves the
//! result under `assets/generated/` in the session's folder. Nothing is spent until Make.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::Disableable;
use gpui_kit::component::input::{Input, InputState};
use serde_json::json;
use wundur_core::account;

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MakeKind {
    Image,
    Sound,
}

impl MakeKind {
    fn title(self) -> &'static str {
        match self {
            MakeKind::Image => "New image",
            MakeKind::Sound => "New sound effect",
        }
    }
}

pub(super) enum MakeState {
    Editing,
    Working,
    Done { files: Vec<PathBuf>, text: String },
    Failed(String),
}

/// One card: what to make, its options, and where it has got.
pub(super) struct MakeCard {
    kind: MakeKind,
    prompt: Option<Entity<InputState>>,
    seconds: Option<Entity<InputState>>,
    quality: &'static str,
    size: &'static str,
    transparent: bool,
    looped: bool,
    state: MakeState,
}

static NEXT: AtomicU64 = AtomicU64::new(1);

const QUALITIES: [(&str, &str); 3] = [("low", "Draft"), ("medium", "Good"), ("high", "Best")];
const SIZES: [(&str, &str); 3] = [("1024x1024", "Square"), ("1536x1024", "Wide"), ("1024x1536", "Tall")];

impl ChatPanel {
    /// Open a card for a new image or sound at the end of the chat.
    pub fn start_make(&mut self, kind: MakeKind, cx: &mut Context<Self>) {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        self.makes.insert(
            id,
            MakeCard { kind, prompt: None, seconds: None, quality: "medium", size: "1024x1024", transparent: false, looped: false, state: MakeState::Editing },
        );
        self.session.update(cx, |s, cx| {
            s.chat.items.push(Item::Make(id));
            s.chat.revision += 1;
            cx.notify();
        });
    }

    /// Tests: `make image|sound [prompt]` opens a card with the prompt written in (Make is
    /// left to the person: it spends from the account).
    pub(super) fn make_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (what, prompt) = line.split_once(' ').unwrap_or((line, ""));
        let kind = match what {
            "image" => MakeKind::Image,
            "sound" => MakeKind::Sound,
            _ => return "make image|sound [prompt]".into(),
        };
        self.start_make(kind, cx);
        self.ensure_make_fields(window, cx);
        if let Some(input) = self.makes.iter().max_by_key(|(k, _)| **k).and_then(|(_, c)| c.prompt.clone()).filter(|_| !prompt.is_empty()) {
            let prompt = prompt.to_string();
            input.update(cx, |i, cx| i.set_value(prompt, window, cx));
        }
        format!("{} card open", kind.title())
    }

    /// Before drawing: the cards' inputs (made with the window).
    pub(super) fn ensure_make_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for card in self.makes.values_mut() {
            if card.prompt.is_none() {
                let hint = match card.kind {
                    MakeKind::Image => "What the image shows, e.g. a gold coin icon, pixel art, top-down",
                    MakeKind::Sound => "The sound, e.g. a wooden door creaking open in a stone hall",
                };
                card.prompt = Some(cx.new(|cx| InputState::new(window, cx).placeholder(hint)));
            }
            if card.kind == MakeKind::Sound && card.seconds.is_none() {
                card.seconds = Some(cx.new(|cx| {
                    let mut i = InputState::new(window, cx).placeholder("Seconds");
                    i.set_value("2", window, cx);
                    i
                }));
            }
        }
    }

    fn close_make(&mut self, id: u64, cx: &mut Context<Self>) {
        self.makes.remove(&id);
        self.session.update(cx, |s, cx| {
            s.chat.items.retain(|i| !matches!(i, Item::Make(n) if *n == id));
            s.chat.revision += 1;
            cx.notify();
        });
    }

    /// Run the card's account tool off the main thread.
    fn run_make(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(card) = self.makes.get_mut(&id) else { return };
        let prompt = card.prompt.as_ref().map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
        if prompt.is_empty() {
            card.state = MakeState::Failed(match card.kind {
                MakeKind::Image => "Say what the image shows first.".into(),
                MakeKind::Sound => "Say what the sound is first.".into(),
            });
            cx.notify();
            return;
        }
        let (name, args) = match card.kind {
            MakeKind::Image => ("make_image", json!({ "prompt": prompt, "quality": card.quality, "size": card.size, "transparent": card.transparent })),
            MakeKind::Sound => {
                let seconds = card.seconds.as_ref().and_then(|i| i.read(cx).value().trim().parse::<f64>().ok()).unwrap_or(2.0).clamp(0.5, 30.0);
                ("sound_effect", json!({ "text": prompt, "seconds": seconds, "loop": card.looped }))
            }
        };
        card.state = MakeState::Working;
        let cwd = self.session.read(cx).cwd().to_path_buf();
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let ran = cx.background_spawn(async move { wundur_core::account_tools::run(name, &args, &cwd) }).await;
            let _ = this.update(cx, |p, cx| {
                if let Some(card) = p.makes.get_mut(&id) {
                    card.state = if ran.failed { MakeState::Failed(ran.text) } else { MakeState::Done { files: ran.files, text: ran.text } };
                }
                p.list.remeasure();
                cx.notify();
            });
        })
        .detach();
    }

    /// Change a card's options; the card redraws.
    fn edit_make(&mut self, id: u64, cx: &mut Context<Self>, f: impl FnOnce(&mut MakeCard)) {
        if let Some(card) = self.makes.get_mut(&id) {
            f(card);
        }
        cx.notify();
    }

    pub(super) fn render_make(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(card) = self.makes.get(&id) else { return div().into_any_element() };
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        let key = |what: &str| SharedString::from(format!("make-{id}-{what}"));
        let kind = card.kind;
        let body = if !account::shared().is_signed_in() {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().text_xs().text_color(muted).child("Images and sound effects come from your Wundur account. Sign in to make them here."))
                .child(Button::new(key("cancel")).small().ghost().label("Cancel").on_click(cx.listener(move |p, _, _, cx| p.close_make(id, cx))))
                .child(Button::new(key("signin")).small().primary().label("Sign in").on_click(|_, _, cx| crate::settings_account::show_sign_in(cx)))
                .into_any_element()
        } else {
            match &card.state {
                MakeState::Done { files, text } => {
                    let file = files.first().cloned();
                    let mut col = div().flex().flex_col().gap_2();
                    if kind == MakeKind::Image
                        && let Some(f) = &file
                    {
                        col = col.child(
                            div()
                                .size(px(220.))
                                .rounded(px(10.))
                                .overflow_hidden()
                                .border_1()
                                .border_color(border)
                                .child(img(f.clone()).size_full().object_fit(ObjectFit::Contain)),
                        );
                    }
                    let open = file.clone();
                    col.child(div().text_xs().text_color(muted).child(text.clone()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(kind == MakeKind::Sound, |d| {
                                    let f = file.clone();
                                    d.child(Button::new(key("play")).small().outline().icon(gpui_kit::assets::IconName::Play).label("Play").on_click(move |_, _, cx| {
                                        if let Some(f) = &f {
                                            play(f, cx);
                                        }
                                    }))
                                })
                                .when_some(open, |d, f| d.child(Button::new(key("open")).small().ghost().label("Open").on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenFile(f.clone(), None))))))
                                .child(div().flex_1())
                                .child(Button::new(key("close")).small().ghost().label("Done").on_click(cx.listener(move |p, _, _, cx| p.close_make(id, cx))))
                                .child(Button::new(key("again")).small().outline().label("Make another").on_click(cx.listener(move |p, _, _, cx| p.edit_make(id, cx, |c| c.state = MakeState::Editing)))),
                        )
                        .into_any_element()
                }
                state => {
                    let working = matches!(state, MakeState::Working);
                    let error = if let MakeState::Failed(e) = state { Some(e.clone()) } else { None };
                    let mut options = div().flex().flex_wrap().items_center().gap_1();
                    match kind {
                        MakeKind::Image => {
                            for (q, label) in QUALITIES {
                                let on = card.quality == q;
                                options = options.child(
                                    Button::new(key(&format!("q-{q}")))
                                        .xsmall()
                                        .label(label)
                                        .when(on, |b| b.outline())
                                        .when(!on, |b| b.ghost())
                                        .on_click(cx.listener(move |p, _, _, cx| p.edit_make(id, cx, |c| c.quality = q))),
                                );
                            }
                            options = options.child(div().w(px(10.)));
                            for (s, label) in SIZES {
                                let on = card.size == s;
                                options = options.child(
                                    Button::new(key(&format!("s-{s}")))
                                        .xsmall()
                                        .label(label)
                                        .when(on, |b| b.outline())
                                        .when(!on, |b| b.ghost())
                                        .on_click(cx.listener(move |p, _, _, cx| p.edit_make(id, cx, |c| c.size = s))),
                                );
                            }
                            options = options.child(div().w(px(10.))).child(
                                Button::new(key("transparent"))
                                    .xsmall()
                                    .label("Transparent")
                                    .when(card.transparent, |b| b.outline())
                                    .when(!card.transparent, |b| b.ghost())
                                    .on_click(cx.listener(move |p, _, _, cx| p.edit_make(id, cx, |c| c.transparent = !c.transparent))),
                            );
                        }
                        MakeKind::Sound => {
                            if let Some(seconds) = &card.seconds {
                                options = options.child(div().w(px(70.)).child(Input::new(seconds).small())).child(div().text_xs().text_color(muted).child("seconds (0.5–30)"));
                            }
                            options = options.child(div().w(px(10.))).child(
                                Button::new(key("loop"))
                                    .xsmall()
                                    .label("Loop")
                                    .when(card.looped, |b| b.outline())
                                    .when(!card.looped, |b| b.ghost())
                                    .on_click(cx.listener(move |p, _, _, cx| p.edit_make(id, cx, |c| c.looped = !c.looped))),
                            );
                        }
                    }
                    let spend = match kind {
                        MakeKind::Image => "Uses one image from your month.",
                        MakeKind::Sound => "Uses the seconds you ask for.",
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(card.prompt.as_ref().map(Input::new))
                        .child(options)
                        .children(error.map(|e| div().text_xs().text_color(danger).child(e)))
                        .child(
                            div()
                                .pt_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_xs().text_color(muted).child(if working { "Making it…" } else { spend }))
                                .child(div().flex_1())
                                .child(Button::new(key("cancel")).small().ghost().label("Cancel").disabled(working).on_click(cx.listener(move |p, _, _, cx| p.close_make(id, cx))))
                                .child(Button::new(key("make")).small().primary().label("Make").loading(working).disabled(working).on_click(cx.listener(move |p, _, _, cx| p.run_make(id, cx)))),
                        )
                        .into_any_element()
                }
            }
        };
        make_frame(key("card"), kind, body, cx)
    }
}

/// The card's frame, as an asset card's, with the kind's icon.
fn make_frame(id: SharedString, kind: MakeKind, body: AnyElement, cx: &mut Context<ChatPanel>) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, border, bg, ground) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.secondary);
    let icon = match kind {
        MakeKind::Image => gpui_kit::assets::IconName::Image,
        MakeKind::Sound => gpui_kit::assets::IconName::AudioLines,
    };
    div()
        .id(id)
        .flex()
        .flex_col()
        .rounded(px(14.))
        .border_1()
        .border_color(border.opacity(0.8))
        .bg(bg)
        .shadow_sm()
        .overflow_hidden()
        .child(
            div()
                .px_3()
                .pt_3()
                .pb_2()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(26.)).flex_none().rounded(px(8.)).bg(ground).flex().items_center().justify_center().child(Icon::new(icon).small().text_color(muted)))
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(kind.title()))
                .child(div().text_xs().text_color(muted).child("Wundur account")),
        )
        .child(div().px_3().pb_3().child(body))
        .into_any_element()
}

/// Play a sound file: `afplay` on a Mac, the system's player elsewhere.
fn play(file: &std::path::Path, cx: &mut App) {
    if cfg!(target_os = "macos")
        && let Ok(mut child) = std::process::Command::new("afplay").arg(file).spawn()
    {
        std::thread::spawn(move || child.wait());
        return;
    }
    cx.open_with_system(file);
}
