//! The New Project wizard: game or other work; for a game, its engine (from the engine packs,
//! [`wundur_core::engines`]) and 2D or 3D; then its name and where it goes. Engines that need
//! something this Mac doesn't have are greyed out (Unity without its editor) or flagged (Godot,
//! Node.js), with where to get it.
//!
//! "Help me choose" on the engine step asks where the game runs, its type, the camera and code or
//! editor, then lists the engines that fit, best first, with why ([`wundur_core::engine_fit`]).

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::engine_fit::{self, Answers, Pick, Platform, Workflow};
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
    // Help me choose.
    Platform,
    Type,
    Art,
    Camera,
    Workflow,
    Result,
}

impl Step {
    fn in_quiz(self) -> bool {
        matches!(self, Step::Platform | Step::Type | Step::Art | Step::Camera | Step::Workflow | Step::Result)
    }
}

/// 2D, 3D or isometric.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GameType {
    Flat,
    Deep,
    Isometric,
}

/// The answers to "Help me choose" so far.
#[derive(Default)]
struct Quiz {
    platforms: Vec<Platform>,
    first: Option<Platform>,
    game_type: Option<GameType>,
    dimension: Option<Dimension>,
    camera: Option<String>,
    /// Answered (None inside: they don't mind).
    workflow: Option<Option<Workflow>>,
}

impl Quiz {
    fn answers(&self) -> Option<Answers> {
        if self.platforms.is_empty() {
            return None;
        }
        Some(Answers { platforms: self.platforms.clone(), first: self.first, dimension: self.dimension?, camera: self.camera.clone(), workflow: self.workflow.flatten() })
    }

    fn toggle(&mut self, p: Platform) {
        if let Some(at) = self.platforms.iter().position(|x| *x == p) {
            self.platforms.remove(at);
        } else {
            self.platforms.push(p);
            self.platforms.sort_by_key(|x| Platform::ALL.iter().position(|a| a == x));
        }
        if self.platforms.len() < 2 || self.first.is_some_and(|f| !self.platforms.contains(&f)) {
            self.first = None;
        }
    }
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
    quiz: Quiz,
    /// The engine came from Help me choose (Back from the details goes to the list).
    via_quiz: bool,
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
        let mut wizard = Self { step: Step::Kind, kind: None, offers, engine: None, dimension: None, name, default_parent: parent.clone(), parent, error: None, working: None, quiz: Quiz::default(), via_quiz: false, _subs: vec![sub] };
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
        self.via_quiz = false;
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

    /// The engines that fit the answers, best first.
    fn picks(&self) -> Vec<Pick> {
        let Some(answers) = self.quiz.answers() else { return Vec::new() };
        let engines: Vec<Engine> = self.offers.iter().map(|o| o.engine.clone()).collect();
        let ready: Vec<bool> = self.offers.iter().map(|o| o.checks.as_ref().is_none_or(|c| c.iter().all(Check::ok))).collect();
        engine_fit::recommend(&engines, &answers, &ready)
    }

    /// Take an engine from the list, with the 2D or 3D the answers gave.
    fn pick_recommended(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.offers.get(ix).is_none_or(Offer::blocked) {
            return;
        }
        self.engine = Some(ix);
        self.dimension = self.quiz.dimension;
        self.via_quiz = true;
        self.go(Step::Details, window, cx);
    }

    fn pick_type(&mut self, t: GameType, window: &mut Window, cx: &mut Context<Self>) {
        if self.quiz.game_type != Some(t) {
            self.quiz.camera = None;
        }
        self.quiz.game_type = Some(t);
        match t {
            GameType::Flat => self.quiz.dimension = Some(Dimension::TwoD),
            GameType::Deep => self.quiz.dimension = Some(Dimension::ThreeD),
            GameType::Isometric => {
                self.quiz.camera = Some(engine_fit::ISOMETRIC.into());
                return self.go(Step::Art, window, cx);
            }
        }
        self.go(Step::Camera, window, cx);
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = match self.step {
            Step::Kind => return,
            Step::Engine => Step::Kind,
            Step::Dimension => Step::Engine,
            Step::Platform => Step::Engine,
            Step::Type => Step::Platform,
            Step::Art | Step::Camera => Step::Type,
            Step::Workflow if self.quiz.game_type == Some(GameType::Isometric) => Step::Art,
            Step::Workflow => Step::Camera,
            Step::Result => Step::Workflow,
            Step::Details if self.via_quiz => Step::Result,
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
    /// `dim 2d|3d`, `name <text>`, `parent <folder>`, `create`, `back`, `state`; Help me choose:
    /// `help`, `platforms web,mobile`, `first <platform>|none`, `type 2d|3d|iso`, `art 2d|3d`,
    /// `camera <id>`, `workflow code|editor|any`, `pick <n>` (the n-th in the list).
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
            "help" => self.go(Step::Platform, window, cx),
            "platforms" => {
                self.quiz.platforms.clear();
                self.quiz.first = None;
                for p in arg.split(',').filter_map(|p| Platform::from_id(p.trim())) {
                    self.quiz.toggle(p);
                }
                self.go(Step::Type, window, cx);
            }
            "first" => {
                self.quiz.first = Platform::from_id(arg).filter(|p| self.quiz.platforms.contains(p));
                cx.notify();
            }
            "type" => match arg {
                "2d" => self.pick_type(GameType::Flat, window, cx),
                "3d" => self.pick_type(GameType::Deep, window, cx),
                "iso" => self.pick_type(GameType::Isometric, window, cx),
                _ => return format!("no type {arg}"),
            },
            "art" => {
                self.quiz.dimension = Dimension::from_id(arg);
                self.go(Step::Workflow, window, cx);
            }
            "camera" => {
                self.quiz.camera = Some(arg.to_string());
                self.go(Step::Workflow, window, cx);
            }
            "workflow" => {
                self.quiz.workflow = Some(match arg {
                    "code" => Some(Workflow::Code),
                    "editor" => Some(Workflow::Editor),
                    _ => None,
                });
                self.go(Step::Result, window, cx);
            }
            "pick" => match arg.parse::<usize>().ok().and_then(|n| self.picks().get(n).map(|p| p.index)) {
                Some(ix) => self.pick_recommended(ix, window, cx),
                None => return format!("no pick {arg}"),
            },
            _ => {}
        }
        if self.step == Step::Result {
            let list: Vec<String> = self.picks().iter().map(|p| self.offers[p.index].engine.spec.name.clone()).collect();
            return format!("step={} list=[{}]", self.title(), list.join(", "));
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
            if self.step.in_quiz() || (self.step == Step::Details && self.via_quiz) {
                steps.extend([Step::Platform, Step::Type]);
                steps.push(if self.quiz.game_type == Some(GameType::Isometric) { Step::Art } else { Step::Camera });
                steps.extend([Step::Workflow, Step::Result]);
            } else if self.chosen().is_none_or(|o| o.engine.spec.dimensions.len() > 1) {
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
            Step::Platform => "Where should it run?",
            Step::Type => "What type of game?",
            Step::Art => "Drawn art or 3D models?",
            Step::Camera => "Where is the camera?",
            Step::Workflow => "Code or an editor?",
            Step::Result => "Engines that fit",
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

    /// A card for one answer: a title, a line under it, outlined when picked.
    fn choice(
        id: impl Into<SharedString>,
        width: f32,
        title: &str,
        text: &str,
        picked: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, accent, hover) = (theme.foreground, theme.muted_foreground, theme.border, theme.primary, theme.secondary_hover);
        div()
            .id(ElementId::Name(id.into()))
            .test_support()
            .w(px(width))
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .border_2()
            .border_color(if picked { accent } else { border })
            .cursor_pointer()
            .hover(move |d| d.bg(hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(title.to_string()))
                    .when(picked, |d| d.child(Icon::new(IconName::Check).small().text_color(accent))),
            )
            .when(!text.is_empty(), |d| d.child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(text.to_string())))
            .on_click(cx.listener(move |s, _, window, cx| on_click(s, window, cx)))
            .into_any_element()
    }

    fn render_platforms(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut row = div().flex().flex_wrap().gap_2();
        for p in Platform::ALL {
            let text = match p {
                Platform::Web => "Plays in a browser, nothing to install.",
                Platform::Desktop => "Mac, Windows and Linux.",
                Platform::Mobile => "iPhone and Android.",
                Platform::Consoles => "Switch, PlayStation, Xbox.",
            };
            let picked = self.quiz.platforms.contains(&p);
            row = row.child(Self::choice(format!("wizard-platform-{}", p.id()), 238., p.label(), text, picked, cx, move |s, _, cx| {
                s.quiz.toggle(p);
                cx.notify();
            }));
        }
        let mut out = div().flex().flex_col().gap_3().child(div().text_xs().text_color(muted).child("Tick every one you want. You can change your mind later.")).child(row).child(
            div().child(Button::new("wizard-all").small().ghost().label("All of these (web, desktop, mobile)").on_click(cx.listener(|s, _, _, cx| {
                for p in [Platform::Web, Platform::Desktop, Platform::Mobile] {
                    if !s.quiz.platforms.contains(&p) {
                        s.quiz.toggle(p);
                    }
                }
                cx.notify();
            }))),
        );
        if self.quiz.platforms.len() > 1 {
            let mut firsts = div().flex().flex_wrap().gap_2();
            let options: Vec<Option<Platform>> = std::iter::once(None).chain(self.quiz.platforms.iter().copied().map(Some)).collect();
            for f in options {
                let label = f.map_or("No preference", Platform::label);
                let id = format!("wizard-first-{}", f.map_or("none", Platform::id));
                firsts = firsts.child(Self::choice(id, 112., label, "", self.quiz.first == f, cx, move |s, _, cx| {
                    s.quiz.first = f;
                    cx.notify();
                }));
            }
            out = out.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(cx.theme().foreground).child("Which comes first?"))
                    .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child("It counts double. On phones, web games run in a web view: capped at 60 fps on iPhone, and heavy scenes struggle."))
                    .child(firsts),
            );
        }
        out.into_any_element()
    }

    fn render_types(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.quiz.game_type;
        div()
            .flex()
            .gap_2()
            .child(Self::choice("wizard-type-2d", 156., "2D", "Flat art, seen from the side or from above.", t == Some(GameType::Flat), cx, |s, w, cx| s.pick_type(GameType::Flat, w, cx)))
            .child(Self::choice("wizard-type-3d", 156., "3D", "Models and a camera that moves through the world.", t == Some(GameType::Deep), cx, |s, w, cx| s.pick_type(GameType::Deep, w, cx)))
            .child(Self::choice("wizard-type-iso", 156., "Isometric", "An angled view from above: tactics, builders, classic RPGs.", t == Some(GameType::Isometric), cx, |s, w, cx| s.pick_type(GameType::Isometric, w, cx)))
            .into_any_element()
    }

    fn render_art(&self, cx: &mut Context<Self>) -> AnyElement {
        let d = self.quiz.dimension;
        let pick = |dim: Dimension| {
            move |s: &mut Self, w: &mut Window, cx: &mut Context<Self>| {
                s.quiz.dimension = Some(dim);
                s.go(Step::Workflow, w, cx);
            }
        };
        div()
            .flex()
            .gap_2()
            .child(Self::choice("wizard-art-2d", 238., "Drawn art", "Sprites on an isometric grid.", d == Some(Dimension::TwoD), cx, pick(Dimension::TwoD)))
            .child(Self::choice("wizard-art-3d", 238., "3D models", "A 3D world with a fixed, angled camera.", d == Some(Dimension::ThreeD), cx, pick(Dimension::ThreeD)))
            .into_any_element()
    }

    fn render_cameras(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dim) = self.quiz.dimension else { return div().into_any_element() };
        let mut row = div().flex().flex_wrap().gap_2();
        for cam in engine_fit::cameras(dim) {
            let id = cam.id;
            let picked = self.quiz.camera.as_deref() == Some(id);
            row = row.child(Self::choice(format!("wizard-camera-{id}"), 196., cam.title, cam.text, picked, cx, move |s, w, cx| {
                s.quiz.camera = Some(id.to_string());
                s.go(Step::Workflow, w, cx);
            }));
        }
        row.into_any_element()
    }

    fn render_workflow(&self, cx: &mut Context<Self>) -> AnyElement {
        let w = self.quiz.workflow;
        let pick = |answer: Option<Workflow>| {
            move |s: &mut Self, win: &mut Window, cx: &mut Context<Self>| {
                s.quiz.workflow = Some(answer);
                s.go(Step::Result, win, cx);
            }
        };
        div()
            .flex()
            .gap_2()
            .child(Self::choice("wizard-workflow-code", 156., "Code only", "The agent writes everything, and you play it in Wundur.", w == Some(Some(Workflow::Code)), cx, pick(Some(Workflow::Code))))
            .child(Self::choice("wizard-workflow-editor", 156., "An editor", "A visual editor to lay out levels and tweak by hand.", w == Some(Some(Workflow::Editor)), cx, pick(Some(Workflow::Editor))))
            .child(Self::choice("wizard-workflow-any", 156., "Don't mind", "Either is fine.", w == Some(None), cx, pick(None)))
            .into_any_element()
    }

    fn render_result(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, yellow) = (theme.foreground, theme.muted_foreground, theme.border, theme.yellow);
        let picks = self.picks();
        let name = |p: &Pick| self.offers[p.index].engine.spec.name.clone();
        let headline = match picks.as_slice() {
            [] => "None of the engines fit those answers. Go back and change one, or pick from the cards.".to_string(),
            [only] => format!("{} is the one that fits.", name(only)),
            [best, rest @ ..] => {
                let others: Vec<String> = rest.iter().map(name).collect();
                let others = match others.as_slice() {
                    [one] => one.clone(),
                    [init @ .., last] => format!("{} or {last}", init.join(", ")),
                    [] => String::new(),
                };
                format!("Based on what you picked, I recommend {}. {others} will also work.", name(best))
            }
        };
        let mut list = div().flex().flex_col().gap_2();
        for (rank, pick) in picks.iter().enumerate() {
            let offer = &self.offers[pick.index];
            let spec = &offer.engine.spec;
            let blocked = offer.blocked();
            let missing: Option<String> = match &offer.checks {
                Some(_) if blocked => Some(format!("{} isn't on this Mac", spec.name)),
                Some(_) => offer.missing().first().map(|c| format!("Needs {} (not on this Mac yet)", c.requirement.label)),
                None => None,
            };
            let ix = pick.index;
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .child(div().w(px(96.)).h(px(56.)).flex_none().rounded_md().overflow_hidden().bg(border).children(offer.image.clone().map(|i| img(i).w(px(96.)).h(px(56.)).object_fit(ObjectFit::Cover))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(format!("{}. {}", rank + 1, spec.name)))
                            .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(pick.reason.clone()))
                            .children(missing.map(|m| div().flex().items_center().gap_1().text_xs().text_color(yellow).child(Icon::new(IconName::TriangleAlert).xsmall()).child(m))),
                    )
                    .child({
                        let b = Button::new(("wizard-pick", ix)).small().label(format!("Pick {}", spec.name)).disabled(blocked).on_click(cx.listener(move |s, _, w, cx| s.pick_recommended(ix, w, cx)));
                        if rank == 0 { b.primary() } else { b.ghost() }
                    }),
            );
        }
        div().flex().flex_col().gap_3().child(div().text_sm().line_height(rems(1.4)).text_color(fg).child(headline)).child(list).into_any_element()
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
            Step::Platform => self.render_platforms(cx),
            Step::Type => self.render_types(cx),
            Step::Art => self.render_art(cx),
            Step::Camera => self.render_cameras(cx),
            Step::Workflow => self.render_workflow(cx),
            Step::Result => self.render_result(cx),
        };
        let busy = self.working.is_some();
        let next: Option<AnyElement> = match self.step {
            Step::Kind | Step::Dimension | Step::Type | Step::Art | Step::Camera | Step::Workflow | Step::Result => None,
            Step::Platform => Some(
                Button::new("wizard-next")
                    .small()
                    .primary()
                    .label("Continue")
                    .disabled(self.quiz.platforms.is_empty())
                    .on_click(cx.listener(|s, _, window, cx| s.go(Step::Type, window, cx)))
                    .into_any_element(),
            ),
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
        let wide = matches!(self.step, Step::Engine | Step::Result | Step::Camera);
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
                            .when(self.step == Step::Engine, |d| {
                                d.child(
                                    Button::new("wizard-help")
                                        .small()
                                        .ghost()
                                        .icon(IconName::Search)
                                        .label("Help me choose")
                                        .on_click(cx.listener(|s, _, window, cx| s.go(Step::Platform, window, cx))),
                                )
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

#[cfg(test)]
mod tests {
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, Entity, TestAppContext, WindowBounds, WindowOptions, px, size};

    use super::ProjectWizard;

    /// Help me choose: each question, Back through them, the list, and picking from it.
    #[gpui_kit::test]
    async fn help_me_choose_lists_the_engines_that_fit(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut wizard: Option<Entity<ProjectWizard>> = None;
        let handle = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds { origin: Default::default(), size: size(px(900.), px(800.)) })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let w = cx.new(|cx| ProjectWizard::new(std::env::temp_dir(), window, cx));
                wizard = Some(w.clone());
                w
            })
            .unwrap()
            .0
        });
        let wizard = wizard.unwrap();
        // Every engine ready, whatever this Mac has, so ties break the same everywhere.
        cx.run_until_parked();
        cx.update(|cx| {
            wizard.update(cx, |w, _| {
                for o in &mut w.offers {
                    o.checks = Some(Vec::new());
                }
            })
        });
        let run = |line: &str, cx: &mut TestAppContext| {
            let out = cx.update_window(handle, |_, window, cx| wizard.update(cx, |w, cx| w.command(line, window, cx))).unwrap();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
            cx.run_until_parked();
            out
        };

        run("game", cx);
        assert!(run("help", cx).starts_with("step=Where should it run?"));
        // Clicking platforms ticks them; with two, "Which comes first?" appears.
        cx.update_window(handle, |_, window, cx| window.click("wizard-platform-web", cx)).unwrap();
        cx.update_window(handle, |_, window, cx| window.click("wizard-platform-mobile", cx)).unwrap();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update_window(handle, |_, window, cx| window.click("wizard-first-mobile", cx)).unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let w = wizard.read(cx);
            assert_eq!(w.quiz.platforms.len(), 2);
            assert_eq!(w.quiz.first, Some(wundur_core::engine_fit::Platform::Mobile));
        });

        // Mobile first, 2D, side view, don't mind: the native engines lead, Phaser still fits.
        run("platforms web,mobile", cx);
        run("first mobile", cx);
        assert!(run("type 2d", cx).starts_with("step=Where is the camera?"));
        run("camera side", cx);
        assert_eq!(run("workflow any", cx), "step=Engines that fit list=[Godot, Unity, Phaser]");

        // Isometric skips the camera and asks drawn art or models; Back retraces it.
        run("back", cx);
        run("back", cx);
        assert!(run("back", cx).starts_with("step=What type of game?"));
        assert!(run("type iso", cx).starts_with("step=Drawn art or 3D models?"));
        assert!(run("art 3d", cx).starts_with("step=Code or an editor?"));
        // Tied on points (web and code only favour Three.js); mobile coming first breaks it.
        assert_eq!(run("workflow code", cx), "step=Engines that fit list=[Godot, Unity, Three.js]");

        // Picking from the list goes to the details with the answers' 3D, and Back returns here.
        assert!(run("pick 2", cx).starts_with("step=Name your game"));
        cx.update(|cx| {
            let w = wizard.read(cx);
            assert_eq!(w.chosen().map(|o| o.engine.spec.name.as_str()), Some("Three.js"));
            assert_eq!(w.dimension, Some(wundur_core::engines::Dimension::ThreeD));
            assert_eq!(w.step_label(), "Step 8 of 8");
        });
        assert!(run("back", cx).starts_with("step=Engines that fit"));
    }
}
