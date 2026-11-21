//! The New Project wizard: game or other work; for a game, its engine (from the engine packs,
//! [`wundur_core::engines`]) and 2D or 3D; then its name and where it goes. Engines that need
//! something this Mac doesn't have are greyed out (Unity without its editor) or flagged (Godot,
//! Node.js), with where to get it.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::engines::{self, Check, Dimension, Engine, NewProject};

/// What the wizard ends with.
pub enum WizardEvent {
    /// A project was made in `folder`, called `name`; `notes` are what didn't go to plan.
    Created { folder: PathBuf, name: String, notes: Vec<String> },
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Kind,
    Engine,
    Dimension,
    Details,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Game,
    Other,
}

/// One engine on offer: its card's picture and what its check found (none: still looking).
struct Offer {
    engine: Engine,
    image: Option<Arc<Image>>,
    checks: Option<Vec<Check>>,
}

impl Offer {
    fn blocked(&self) -> bool {
        self.checks.as_deref().is_some_and(engines::blocked)
    }

    fn missing(&self) -> Vec<&Check> {
        self.checks.iter().flatten().filter(|c| !c.ok()).collect()
    }
}

pub struct ProjectWizard {
    step: Step,
    kind: Option<Kind>,
    offers: Vec<Offer>,
    engine: Option<usize>,
    dimension: Option<Dimension>,
    name: Entity<InputState>,
    /// Where it goes: the projects folder unless another was chosen.
    parent: PathBuf,
    /// The projects folder (Settings > General), made when the first project goes in it.
    default_parent: PathBuf,
    error: Option<String>,
    /// What creating is doing, while it does it.
    working: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WizardEvent> for ProjectWizard {}

impl ProjectWizard {
    pub fn new(parent: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let packs = wundur_core::packs::dir().map(|d| wundur_core::packs::installed(&d)).unwrap_or_default();
        let offers: Vec<Offer> = engines::all(&packs)
            .into_iter()
            .map(|engine| {
                let image = engine.image().map(|bytes| {
                    let format = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { ImageFormat::Png } else { ImageFormat::Jpeg };
                    Arc::new(Image::from_bytes(format, bytes.into_owned()))
                });
                Offer { engine, image, checks: None }
            })
            .collect();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("My Game"));
        let sub = cx.subscribe_in(&name, window, |s, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => s.create(cx),
            InputEvent::Change => {
                s.error = None;
                cx.notify();
            }
            _ => {}
        });
        let mut wizard = Self { step: Step::Kind, kind: None, offers, engine: None, dimension: None, name, default_parent: parent.clone(), parent, error: None, working: None, _subs: vec![sub] };
        wizard.check_all(cx);
        wizard
    }

    /// Look for each engine's tools, off the UI thread.
    fn check_all(&mut self, cx: &mut Context<Self>) {
        let engines: Vec<Engine> = self.offers.iter().map(|o| o.engine.clone()).collect();
        let task = cx.background_spawn(async move { engines.iter().map(engines::check).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                for (offer, checks) in s.offers.iter_mut().zip(found) {
                    offer.checks = Some(checks);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn chosen(&self) -> Option<&Offer> {
        self.offers.get(self.engine?)
    }

    fn pick_kind(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = Some(kind);
        self.go(if kind == Kind::Game { Step::Engine } else { Step::Details }, window, cx);
    }

    fn pick_engine(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.engine = Some(ix);
        let dims = &self.offers[ix].engine.spec.dimensions;
        if !self.dimension.is_some_and(|d| dims.contains(&d)) {
            self.dimension = dims.first().copied();
        }
        cx.notify();
    }

    fn go(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.step = step;
        self.error = None;
        if step == Step::Details {
            self.name.update(cx, |i, cx| {
                i.set_placeholder(if self.kind == Some(Kind::Game) { "My Game" } else { "My Project" }, window, cx);
                i.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// The step after the engine: 2D or 3D when it makes both, else the details.
    fn after_engine(&self) -> Step {
        match self.chosen() {
            Some(o) if o.engine.spec.dimensions.len() > 1 => Step::Dimension,
            _ => Step::Details,
        }
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = match self.step {
            Step::Kind => return,
            Step::Engine => Step::Kind,
            Step::Dimension => Step::Engine,
            Step::Details => match self.kind {
                Some(Kind::Game) if self.after_engine() == Step::Dimension => Step::Dimension,
                Some(Kind::Game) => Step::Engine,
                _ => Step::Kind,
            },
        };
        self.go(step, window, cx);
    }

    fn choose_parent(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(parent) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |s, cx| {
                s.parent = parent;
                s.error = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn plan(&self, cx: &App) -> NewProject {
        let game = match (self.kind, self.chosen(), self.dimension) {
            (Some(Kind::Game), Some(o), Some(d)) => Some((o.engine.clone(), d)),
            _ => None,
        };
        NewProject { name: self.name.read(cx).value().trim().to_string(), parent: self.parent.clone(), game }
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.step != Step::Details || self.working.is_some() {
            return;
        }
        let plan = self.plan(cx);
        if plan.parent == self.default_parent
            && !plan.parent.exists()
            && let Err(e) = std::fs::create_dir_all(&plan.parent)
        {
            self.error = Some(format!("Couldn't make {}: {e}", plan.parent.display()));
            cx.notify();
            return;
        }
        if let Err(e) = engines::target(&plan) {
            self.error = Some(e);
            cx.notify();
            return;
        }
        self.working = Some("Making the project…".into());
        self.error = None;
        cx.notify();
        let (tx, rx) = async_channel::unbounded::<String>();
        let name = plan.name.clone();
        let task = cx.background_spawn(async move { engines::create(&plan, &|step| drop(tx.try_send(step.to_string()))) });
        cx.spawn(async move |this, cx| {
            while let Ok(step) = rx.recv().await {
                if this.update(cx, |s, cx| {
                    s.working = Some(step);
                    cx.notify();
                }).is_err() {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let made = task.await;
            let _ = this.update(cx, |s, cx| {
                s.working = None;
                match made {
                    Ok(made) => cx.emit(WizardEvent::Created { folder: made.folder, name, notes: made.notes }),
                    Err(e) => {
                        s.error = Some(e);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Tests: drive the wizard without clicks. `game`, `other`, `engine <n>`, `next`,
    /// `dim 2d|3d`, `name <text>`, `parent <folder>`, `create`, `back`, `state`.
    pub fn command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = line.split_once(' ').unwrap_or((line, ""));
        match verb {
            "game" => self.pick_kind(Kind::Game, window, cx),
            "other" => self.pick_kind(Kind::Other, window, cx),
            "engine" => match arg.parse::<usize>() {
                Ok(ix) if ix < self.offers.len() => self.pick_engine(ix, cx),
                _ => return format!("no engine {arg}"),
            },
            "next" => {
                let step = self.after_engine();
                self.go(step, window, cx);
            }
            "dim" => {
                self.dimension = Dimension::from_id(arg);
                self.go(Step::Details, window, cx);
            }
            "name" => self.name.update(cx, |i, cx| i.set_value(arg.to_string(), window, cx)),
            "parent" => {
                self.parent = PathBuf::from(arg);
                cx.notify();
            }
            "create" => self.create(cx),
            "back" => self.back(window, cx),
            _ => {}
        }
        let checks: Vec<String> = self
            .offers
            .iter()
            .map(|o| format!("{}:{}", o.engine.spec.name, match &o.checks {
                None => "checking",
                Some(_) if o.blocked() => "blocked",
                Some(_) if !o.missing().is_empty() => "missing",
                Some(_) => "ok",
            }))
            .collect();
        format!("step={} engines=[{}] error={:?} working={:?}", self.title(), checks.join(" "), self.error, self.working)
    }

    /// "Step 2 of 4": where it is among the steps this project takes.
    fn step_label(&self) -> String {
        let mut steps = vec![Step::Kind];
        if self.kind != Some(Kind::Other) {
            steps.push(Step::Engine);
            if self.chosen().is_none_or(|o| o.engine.spec.dimensions.len() > 1) {
                steps.push(Step::Dimension);
            }
        }
        steps.push(Step::Details);
        let at = steps.iter().position(|s| *s == self.step).unwrap_or(0) + 1;
        format!("Step {at} of {}", steps.len())
    }

    fn title(&self) -> &'static str {
        match self.step {
            Step::Kind => "New project",
            Step::Engine => "Pick an engine",
            Step::Dimension => "2D or 3D?",
            Step::Details => {
                if self.kind == Some(Kind::Game) {
                    "Name your game"
                } else {
                    "Name your project"
                }
            }
        }
    }

    fn render_kind(&self, cx: &mut Context<Self>) -> AnyElement {
        let card = |id: &'static str, icon: IconName, title: &'static str, text: &'static str, kind: Kind, cx: &mut Context<Self>| {
            let theme = cx.theme();
            let (fg, muted, border, hover) = (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover);
            div()
                .id(id)
                .w(px(238.))
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(move |d| d.bg(hover))
                .child(Icon::new(icon).large().text_color(fg))
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(title))
                .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(text))
                .on_click(cx.listener(move |s, _, window, cx| s.pick_kind(kind, window, cx)))
        };
        div()
            .flex()
            .gap_3()
            .child(card("wizard-game", IconName::Star, "A game", "Pick an engine and get a starter project that runs, with notes for the agent on how the engine works.", Kind::Game, cx))
            .child(card("wizard-other", IconName::Folder, "Other work", "A folder with git, for anything else: an app, a site, writing.", Kind::Other, cx))
            .into_any_element()
    }

    fn render_engines(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, accent, hover, yellow) = (theme.foreground, theme.muted_foreground, theme.border, theme.primary, theme.secondary_hover, theme.yellow);
        let mut grid = div().flex().flex_wrap().gap_3();
        for (ix, offer) in self.offers.iter().enumerate() {
            let spec = &offer.engine.spec;
            let picked = self.engine == Some(ix);
            let blocked = offer.blocked();
            let status: Option<AnyElement> = match &offer.checks {
                None => Some(div().text_xs().text_color(muted).child("Checking this Mac…").into_any_element()),
                Some(_) if blocked => Some(div().text_xs().text_color(muted).child(format!("{} isn't on this Mac", spec.name)).into_any_element()),
                Some(_) => offer.missing().first().map(|c| {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(yellow)
                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                        .child(format!("No {} found", c.requirement.label))
                        .into_any_element()
                }),
            };
            let dims = spec.dimensions.iter().map(|d| d.label()).collect::<Vec<_>>().join(" · ");
            grid = grid.child(
                div()
                    .id(("wizard-engine", ix))
                    .w(px(292.))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .overflow_hidden()
                    .border_2()
                    .border_color(if picked { accent } else { border })
                    .cursor_pointer()
                    .hover(move |d| d.bg(hover))
                    .when(blocked, |d| d.opacity(0.5))
                    .child(
                        div().h(px(120.)).w_full().overflow_hidden().bg(border).children(
                            offer.image.clone().map(|i| img(i).w_full().h(px(120.)).object_fit(ObjectFit::Cover)),
                        ),
                    )
                    .child(
                        div()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .justify_between()
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(spec.name.clone()))
                                    .child(div().text_xs().text_color(muted).child(dims)),
                            )
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(fg).child(spec.tagline.clone()))
                            .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(spec.description.clone()))
                            .children(status),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| s.pick_engine(ix, cx))),
            );
        }
        let mut out = div().flex().flex_col().gap_3().child(grid);
        if let Some(offer) = self.chosen() {
            for check in offer.missing() {
                out = out.child(self.render_missing(offer, check, cx));
            }
        }
        out.into_any_element()
    }

    /// What an engine needs that isn't here, and where to get it.
    fn render_missing(&self, offer: &Offer, check: &Check, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
        let r = &check.requirement;
        let headline = if r.blocking {
            format!("{} isn't available on this Mac.", offer.engine.spec.name)
        } else {
            format!("{} isn't on this Mac. You can make the project now and install it later.", r.label)
        };
        let url = r.url.clone();
        div()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(fg).child(headline))
            .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(r.help.clone()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("wizard-get").small().label(format!("Get {}", if r.blocking { &offer.engine.spec.name } else { &r.label })).icon(IconName::ExternalLink).on_click(move |_, _, cx| cx.open_url(&url)))
                    .child(Button::new("wizard-recheck").small().ghost().label("Check again").on_click(cx.listener(|s, _, _, cx| {
                        for o in &mut s.offers {
                            o.checks = None;
                        }
                        s.check_all(cx);
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    fn render_dimensions(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, accent, hover) = (theme.foreground, theme.muted_foreground, theme.border, theme.primary, theme.secondary_hover);
        let Some(offer) = self.chosen() else { return div().into_any_element() };
        let mut row = div().flex().gap_3();
        for d in offer.engine.spec.dimensions.clone() {
            let (title, text) = match d {
                Dimension::TwoD => ("2D", "Sprites and tilemaps, seen from the side or from above."),
                Dimension::ThreeD => ("3D", "Models, lights and a camera that moves through a world."),
            };
            let picked = self.dimension == Some(d);
            row = row.child(
                div()
                    .id(SharedString::from(format!("wizard-dim-{}", d.id())))
                    .w(px(238.))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded_lg()
                    .border_2()
                    .border_color(if picked { accent } else { border })
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(title))
                    .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(text))
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.dimension = Some(d);
                        s.go(Step::Details, window, cx);
                    })),
            );
        }
        row.into_any_element()
    }

    fn render_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, red) = (theme.foreground, theme.muted_foreground, theme.red);
        let plan = self.plan(cx);
        let target = engines::folder_name(&plan.name);
        let summary = match &plan.game {
            Some((engine, d)) => format!("A {} {} game, with a starter that runs and a git repository.", d.label(), engine.spec.name),
            None => "A folder with a git repository.".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xs().text_color(muted).child(summary))
            .child(div().flex().flex_col().gap_1().child(div().text_xs().text_color(fg).child("Name")).child(Input::new(&self.name).small()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(fg).child("Goes in"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(tilde(&self.parent)))
                            .when(self.parent != self.default_parent, |d| {
                                d.child(Button::new("wizard-parent-default").small().ghost().label("Use default").on_click(cx.listener(|s, _, _, cx| {
                                    s.parent = s.default_parent.clone();
                                    s.error = None;
                                    cx.notify();
                                })))
                            })
                            .child(Button::new("wizard-parent").small().ghost().label("Choose…").on_click(cx.listener(|s, _, _, cx| s.choose_parent(cx)))),
                    ),
            )
            .when(!target.is_empty(), |d| d.child(div().text_xs().text_color(muted).child(format!("Makes {}", tilde(&self.parent.join(&target))))))
            .when(self.parent == self.default_parent, |d| {
                d.child(div().text_xs().text_color(muted).child("Your projects folder. Change it in Settings > General."))
            })
            .children(self.working.clone().map(|w| div().text_xs().text_color(fg).child(w)))
            .children(self.error.clone().map(|e| div().text_xs().line_height(rems(1.4)).text_color(red).child(e)))
            .into_any_element()
    }
}

impl Render for ProjectWizard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border, bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.background);
        let body = match self.step {
            Step::Kind => self.render_kind(cx),
            Step::Engine => self.render_engines(cx),
            Step::Dimension => self.render_dimensions(cx),
            Step::Details => self.render_details(cx),
        };
        let busy = self.working.is_some();
        let next: Option<AnyElement> = match self.step {
            Step::Kind | Step::Dimension => None,
            Step::Engine => {
                let ok = self.chosen().is_some_and(|o| o.checks.is_some() && !o.blocked());
                Some(
                    Button::new("wizard-next")
                        .small()
                        .primary()
                        .label("Continue")
                        .disabled(!ok)
                        .on_click(cx.listener(|s, _, window, cx| {
                            let step = s.after_engine();
                            s.go(step, window, cx);
                        }))
                        .into_any_element(),
                )
            }
            Step::Details => Some(
                Button::new("wizard-create")
                    .small()
                    .primary()
                    .label(if busy { "Making…" } else { "Create" })
                    .disabled(busy)
                    .on_click(cx.listener(|s, _, _, cx| s.create(cx)))
                    .into_any_element(),
            ),
        };
        let wide = self.step == Step::Engine;
        div()
            .id("project-wizard")
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(70.))
            .bg(gpui_kit::black().opacity(0.25))
            .occlude()
            .capture_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && s.working.is_none() {
                    cx.emit(WizardEvent::Cancelled);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w(px(if wide { 640. } else { 520. }))
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
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(self.title()))
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(muted).child(self.step_label()))
                            .child(
                                Button::new("wizard-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .disabled(busy)
                                    .tooltip("Close (Esc)")
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(WizardEvent::Cancelled))),
                            ),
                    )
                    .child(body)
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(self.step != Step::Kind, |d| {
                                d.child(Button::new("wizard-back").small().ghost().label("Back").disabled(busy).on_click(cx.listener(|s, _, window, cx| s.back(window, cx))))
                            })
                            .child(div().flex_1())
                            .child(Button::new("wizard-cancel").small().ghost().label("Cancel").disabled(busy).on_click(cx.listener(|_, _, _, cx| cx.emit(WizardEvent::Cancelled))))
                            .children(next),
                    ),
            )
    }
}

/// A path as the person reads it: the home folder as `~`.
pub fn tilde(path: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(std::path::Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
