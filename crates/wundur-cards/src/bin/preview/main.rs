//! The card preview: one manifest drawn with the app's own renderer, again on every save,
//! against a scenario's made-up service (see `scenario.rs`). For designing a card: force each
//! state, try each width, click through its actions and watch what the app would be asked to
//! do. Nothing is sent anywhere and nothing is spent.
//!
//! ```sh
//! cargo run -p wundur-cards --bin wundur-card-preview -- cards/sound-effect.json
//! ```
//!
//! Options: `--scenario <file>` (else the manifest's name with `.scenario.json`), `--state
//! <name>`, `--width <points>`, `--dark`, and `--do <action>` (run an action once it's shown;
//! may be repeated). `WUNDUR_BACKGROUND=1` opens it without taking the keyboard, and
//! `WUNDUR_WINDOW=x,y,w,h` places it.

mod scenario;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use wundur_cards::{CardEvent, CardView};
use wundur_core::cards::{Effect, Manifest, Problem};

use scenario::Scenario;

struct Args {
    manifest: PathBuf,
    scenario: PathBuf,
    state: Option<String>,
    width: Option<f32>,
    dark: bool,
    run: Vec<String>,
}

const USAGE: &str = "wundur-card-preview <manifest.json> [--scenario <file>] [--state <name>] [--width <points>] [--dark] [--do <action>]...";

fn args() -> Result<Args, String> {
    let mut given = std::env::args().skip(1);
    let (mut manifest, mut scenario, mut state, mut width, mut dark, mut run) = (None, None, None, None, false, Vec::new());
    while let Some(arg) = given.next() {
        let mut value = |what: &str| given.next().ok_or(format!("{arg} needs {what}"));
        match arg.as_str() {
            "--scenario" => scenario = Some(PathBuf::from(value("a file")?)),
            "--state" => state = Some(value("a state's name")?),
            "--width" => width = Some(value("a width")?.parse::<f32>().map_err(|_| "--width takes a number".to_string())?),
            "--do" => run.push(value("an action's name")?),
            "--dark" => dark = true,
            "--light" => dark = false,
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            _ => manifest = Some(PathBuf::from(arg)),
        }
    }
    let manifest = manifest.ok_or("which manifest?")?;
    let scenario = scenario.unwrap_or_else(|| manifest.with_extension("scenario.json"));
    Ok(Args { manifest, scenario, state, width, dark, run })
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

const WIDTHS: [f32; 4] = [380., 520., 680., 860.];

/// How many lines of what happened are kept.
const LOG: usize = 200;

struct Preview {
    manifest_path: PathBuf,
    scenario_path: PathBuf,
    /// When each file was last read as changed.
    stamps: (Option<SystemTime>, Option<SystemTime>),
    manifest: Option<Arc<Manifest>>,
    /// Why the manifest or the scenario couldn't be read; the last good card stays up.
    broken: Vec<String>,
    problems: Vec<Problem>,
    scenario: Scenario,
    state: Option<String>,
    card: Option<Entity<CardView>>,
    /// Goes up with each new card, so an answer on its way to an old one is dropped.
    generation: u64,
    /// How many times each call has been answered.
    asked: HashMap<String, usize>,
    width: Option<f32>,
    dark: bool,
    log: Vec<String>,
    _watch: Option<Subscription>,
}

impl Preview {
    fn new(args: &Args, cx: &mut Context<Self>) -> Self {
        let mut preview = Self {
            manifest_path: args.manifest.clone(),
            scenario_path: args.scenario.clone(),
            stamps: (None, None),
            manifest: None,
            broken: Vec::new(),
            problems: Vec::new(),
            scenario: Scenario::default(),
            state: args.state.clone(),
            card: None,
            generation: 0,
            asked: HashMap::new(),
            width: args.width,
            dark: args.dark,
            log: Vec::new(),
            _watch: None,
        };
        preview.load(true, true, cx);
        // The files again whenever they're saved.
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                if this.update(cx, |p, cx| p.look(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Actions asked for on the command line, once the card has been drawn.
        let run = args.run.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for action in run {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = this.update(cx, |p, cx| {
                    p.say(format!("--do {action}"));
                    if let Some(card) = &p.card {
                        card.update(cx, |c, cx| c.run(&action, cx));
                    }
                });
            }
        })
        .detach();
        preview
    }

    fn say(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG {
            self.log.remove(0);
        }
    }

    fn look(&mut self, cx: &mut Context<Self>) {
        let now = (modified(&self.manifest_path), modified(&self.scenario_path));
        let (manifest, scenario) = (now.0 != self.stamps.0, now.1 != self.stamps.1);
        if manifest || scenario {
            self.load(manifest, scenario, cx);
        }
    }

    /// Read what changed. A new layout is drawn over the card's data as it is (as the app does
    /// for a card already in a chat); a new scenario starts the card again.
    fn load(&mut self, manifest: bool, scenario: bool, cx: &mut Context<Self>) {
        self.stamps = (modified(&self.manifest_path), modified(&self.scenario_path));
        self.broken.clear();
        let mut fresh = self.card.is_none();
        if scenario || self.manifest.is_none() {
            match std::fs::read_to_string(&self.scenario_path) {
                Ok(text) => match Scenario::parse(&text) {
                    Ok(s) => {
                        self.scenario = s;
                        fresh = true;
                    }
                    Err(e) => self.broken.push(format!("{}: {e}", self.scenario_path.display())),
                },
                // No scenario is fine: the card is drawn as it starts, and its calls go unanswered.
                Err(_) => self.scenario = Scenario::default(),
            }
        }
        if manifest || self.manifest.is_none() {
            match std::fs::read_to_string(&self.manifest_path).map_err(|e| e.to_string()).and_then(|text| Manifest::parse(&text)) {
                Ok(m) => {
                    self.problems = m.check();
                    let m = Arc::new(m);
                    if let Some(card) = self.card.as_ref().filter(|_| !fresh) {
                        card.update(cx, |c, cx| c.set_manifest(m.clone(), cx));
                        self.say(format!("layout reloaded ({} problem{})", self.problems.len(), if self.problems.len() == 1 { "" } else { "s" }));
                    }
                    self.manifest = Some(m);
                }
                Err(e) => self.broken.push(format!("{}: {e}", self.manifest_path.display())),
            }
        }
        if fresh {
            self.start(cx);
        }
        cx.notify();
    }

    /// A new card in the chosen state (the scenario's first, until one is chosen).
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(manifest) = self.manifest.clone() else { return };
        if self.state.as_ref().is_none_or(|s| self.scenario.state(s).is_none()) {
            self.state = self.scenario.states.first().map(|(name, _)| name.clone());
        }
        let base = self.scenario_path.parent().map(Path::to_path_buf).unwrap_or_default();
        let card = cx.new(|cx| CardView::new(manifest, base, cx));
        let (catalog, account) = (self.scenario.catalog.clone(), self.scenario.account.clone());
        let over = self.state.as_ref().and_then(|s| self.scenario.state(s)).cloned().unwrap_or(Value::Null);
        card.update(cx, |c, cx| {
            c.update_state(cx, |s| {
                s.put("catalog", catalog);
                s.put("account", account);
                for scope in ["form", "ui", "card", "account", "catalog"] {
                    if let Some(given) = over.get(scope) {
                        let mut merged = s.root[scope].clone();
                        scenario::merge(&mut merged, given);
                        s.put(scope, merged);
                    }
                }
                if let Some(data) = over.get("data") {
                    s.put("data", data.clone());
                }
            })
        });
        self._watch = Some(cx.subscribe(&card, |p, _, event: &CardEvent, cx| {
            if let CardEvent::Effect(effect) = event {
                p.effect(effect.clone(), cx);
            }
        }));
        self.card = Some(card);
        self.generation += 1;
        self.asked.clear();
        self.say(format!("— {} —", self.state.clone().unwrap_or_else(|| "as it starts".into())));
        cx.notify();
    }

    /// What the app would do for the card, done by the scenario instead.
    fn effect(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::Call { id, request } => {
                let body = request.body.as_ref().map(|b| format!(" {b}")).unwrap_or_default();
                let files: String = request.files.iter().map(|(key, paths)| format!("  +{} file{} as `{key}`", paths.len(), if paths.len() == 1 { "" } else { "s" })).collect();
                self.say(format!("call {}: {} {}{body}{files}{}", request.name, request.method, request.path, if request.spends { "  (spends)" } else { "" }));
                let nth = *self.asked.entry(request.name.clone()).and_modify(|n| *n += 1).or_insert(0);
                let root = self.card.as_ref().map(|c| c.read(cx).state().root.clone()).unwrap_or(Value::Null);
                let data = root["data"].clone();
                let reply = self.scenario.reply(&request.name, nth, &root).unwrap_or_else(|| scenario::Reply {
                    after: 0.4,
                    // A refresh with no reply leaves things as they are; anything else says so.
                    result: if request.name == "refresh" { Ok(data) } else { Err(wundur_core::cards::Failure { message: format!("The scenario has no reply for `{}`.", request.name), code: "no_reply".into() }) },
                });
                self.answer(id, request.name, reply.after, reply.result, cx);
            }
            Effect::Save { id, name, files, folder } => {
                self.say(format!("save into {folder}: {files}"));
                // The saved paths: the scenario's reply for `<action>:save` (files beside it, so
                // the card has something to show), else where each would have gone.
                let key = if name.is_empty() { "save".to_string() } else { format!("{name}:save") };
                if let Some(reply) = self.scenario.reply(&key, 0, &Value::Null) {
                    self.answer(id, key, reply.after, reply.result, cx);
                    cx.notify();
                    return;
                }
                let saved: Vec<Value> = files
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|f| {
                        let link = f.as_str().or_else(|| f.get("url").and_then(Value::as_str)).unwrap_or_default();
                        let name = f.get("name").and_then(Value::as_str).unwrap_or_else(|| link.rsplit('/').next().unwrap_or("file"));
                        json!(format!("{folder}/{name}"))
                    })
                    .collect();
                self.answer(id, "save".into(), 0.4, Ok(Value::Array(saved)), cx);
            }
            Effect::Open(target) => self.say(format!("open {target}")),
            Effect::Play(target) => self.say(format!("play {target}")),
            Effect::Close => self.say("close: the card would leave the chat".into()),
            Effect::App(what) => self.say(format!("app: {what}")),
        }
        cx.notify();
    }

    fn answer(&mut self, id: u64, name: String, after: f64, result: Result<Value, wundur_core::cards::Failure>, cx: &mut Context<Self>) {
        let generation = self.generation;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_secs_f64(after)).await;
            let _ = this.update(cx, |p, cx| {
                if p.generation != generation {
                    return;
                }
                p.say(match &result {
                    Ok(_) => format!("  {name} answered after {after}s"),
                    Err(f) => format!("  {name} failed after {after}s: {}", f.message),
                });
                if let Some(card) = &p.card {
                    card.update(cx, |c, cx| c.finish(id, result, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_dark(&mut self, dark: bool, cx: &mut Context<Self>) {
        self.dark = dark;
        Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
        wundur_cards::theme::colors(dark, cx);
        cx.refresh_windows();
    }
}

fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Button {
    let b = Button::new(id).xsmall().label(label);
    if on { b.outline() } else { b.ghost() }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, chrome, danger, warning) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.sidebar, theme.danger, theme.warning);
        let label = |text: &'static str| div().text_xs().text_color(muted).child(text);
        let title = match &self.manifest {
            Some(m) => format!("{}  ·  version {}", m.id, if m.version.is_empty() { "none" } else { &m.version }),
            None => self.manifest_path.display().to_string(),
        };

        let mut states = div().flex().flex_wrap().items_center().gap_1().child(label("State"));
        for (n, (name, _)) in self.scenario.states.iter().enumerate() {
            let pick = name.clone();
            states = states.child(chip(("state", n), name.clone(), self.state.as_deref() == Some(name)).on_click(cx.listener(move |p, _, _, cx| {
                p.state = Some(pick.clone());
                p.start(cx);
            })));
        }
        if self.scenario.states.is_empty() {
            states = states.child(div().text_xs().text_color(muted).child(format!("none ({} has no states)", self.scenario_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())));
        }
        states = states.child(Button::new("restart").xsmall().ghost().label("Start again").on_click(cx.listener(|p, _, _, cx| p.start(cx))));

        let mut widths = div().flex().items_center().gap_1().child(label("Width"));
        for w in WIDTHS {
            widths = widths.child(chip(("width", w as usize), format!("{w}"), self.width == Some(w)).on_click(cx.listener(move |p, _, _, cx| {
                p.width = Some(w);
                cx.notify();
            })));
        }
        widths = widths.child(chip("width-fill", "Fill", self.width.is_none()).on_click(cx.listener(|p, _, _, cx| {
            p.width = None;
            cx.notify();
        })));
        let dark = self.dark;
        let look = div().flex().items_center().gap_1().child(chip("light", "Light", !dark).on_click(cx.listener(|p, _, _, cx| p.set_dark(false, cx)))).child(chip("dark", "Dark", dark).on_click(cx.listener(|p, _, _, cx| p.set_dark(true, cx))));

        let toolbar = div()
            .px_3()
            .py_2()
            .bg(chrome)
            .border_b_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().items_center().gap_3().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(title)).child(div().flex_1()).child(widths).child(look))
            .child(states);

        // What's wrong: a file that couldn't be read, then what the manifest's check found.
        let mut wrong = div().flex().flex_col();
        for line in &self.broken {
            wrong = wrong.child(div().px_3().py_1().bg(danger.opacity(0.1)).text_xs().text_color(danger).child(line.clone()));
        }
        for p in &self.problems {
            let color = if p.fatal { danger } else { warning };
            wrong = wrong.child(div().px_3().py_1().bg(color.opacity(0.1)).flex().gap_2().text_xs().child(div().flex_none().text_color(color).child(if p.fatal { "Can't be drawn" } else { "Check" })).child(div().text_color(fg).child(format!("{}: {}", p.place, p.text))));
        }

        // The card, as wide as asked, on the chat's ground.
        let card = self.card.clone().map(|card| match self.width {
            Some(w) => div().w(px(w)).max_w_full().child(card),
            None => div().w_full().child(card),
        });
        let stage = div().id("stage").flex_1().min_h_0().overflow_y_scroll().bg(bg).p_4().flex().flex_col().items_center().children(card);

        let mut lines = div().id("log").h(px(150.)).flex_none().overflow_y_scroll().bg(chrome).border_t_1().border_color(border).px_3().py_2().flex().flex_col().gap(px(2.)).font_family(cx.theme().mono_font_family.clone()).text_xs().text_color(muted);
        for line in self.log.iter().rev().take(60) {
            lines = lines.child(div().child(line.clone()));
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .font_family(".SystemUIFont")
            .font_weight(FontWeight(wundur_cards::theme::UI_WEIGHT))
            .child(toolbar)
            .child(wrong)
            .child(stage)
            .child(lines)
    }
}

fn main() {
    let args = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("wundur-card-preview: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let background = wundur_core::env_var("BACKGROUND").is_ok_and(|v| v != "0");
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        Theme::change(if args.dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
        wundur_cards::theme::colors(args.dark, cx);
        cx.set_window_appearance(Some(if args.dark { WindowAppearance::Dark } else { WindowAppearance::Light }));
        // `WUNDUR_WINDOW=x,y[,w,h]` places the window (for screenshots taken from behind).
        let placed = wundur_core::env_var("WINDOW").ok().and_then(|v| {
            let n: Vec<f32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            (n.len() >= 2).then(|| WindowBounds::Windowed(Bounds { origin: point(px(n[0]), px(n[1])), size: size(px(n.get(2).copied().unwrap_or(960.)), px(n.get(3).copied().unwrap_or(900.))) }))
        });
        let options = WindowOptions {
            window_bounds: Some(placed.unwrap_or_else(|| WindowBounds::centered(size(px(960.), px(900.)), cx))),
            titlebar: Some(TitlebarOptions { title: Some(format!("Card preview: {}", args.manifest.display()).into()), ..Default::default() }),
            focus: !background,
            window_min_size: Some(size(px(420.), px(360.))),
            ..Default::default()
        };
        if let Err(e) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|cx| Preview::new(&args, cx))) {
            eprintln!("wundur-card-preview: could not open the window: {e}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if !background {
            cx.activate(true);
        }
    });
}
