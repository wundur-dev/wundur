//! One card's data, and what its actions do to it. The data is six scopes a manifest reads:
//!
//! - `form`: what the person has filled in (written by inputs and by `set`).
//! - `ui`: how the card is shown (the step, what's folded open; written by `set` and `toggle`).
//! - `data`: the service's answer for this card, replaced by each call's answer.
//! - `catalog`, `account`: what the service offers and the person's plan, given by the app.
//! - `card`: the app's own facts: `busy`, `working`, `action`, `error`, `error_code`, `width`.
//!
//! Running an action changes `form` and `ui` at once and hands back [`Effect`]s for what only
//! the app can do (ask the service, save files, open something). A call or a save is waited
//! for: the rest of the action runs when [`CardState::finish`] is told how it went.

use serde_json::{Value, json};

use super::expr::{self, Scope};
use super::manifest::{Call, Manifest, Step};

/// Names a loop gave, for the node an action was clicked in.
pub type Locals = Vec<(String, Value)>;

/// A request as it goes out, its holes filled.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub name: String,
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
    pub spends: bool,
    /// Files to read and put in the body, each as base64: the body's name for them, and
    /// their paths.
    pub files: Vec<(String, Vec<String>)>,
}

/// What the app does for a card.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Ask the service, then [`CardState::finish`] with `id` and its answer.
    Call { id: u64, request: Request },
    /// Save `files` (links, or objects with a `url` and a `name`) under `folder` in the
    /// project, under names not already there, then [`CardState::finish`] with `id` and the
    /// saved paths as a list. `name` is the action it's part of.
    Save { id: u64, name: String, files: Value, folder: String },
    Open(String),
    Play(String),
    Close,
    App(String),
}

/// Why a call or save didn't work.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Failure {
    pub message: String,
    pub code: String,
}

struct Waiting {
    id: u64,
    into: Vec<String>,
    rest: Vec<Step>,
    locals: Locals,
}

pub struct CardState {
    pub root: Value,
    /// Goes up with every change, so whoever draws it knows to draw again.
    pub revision: u64,
    waiting: Option<Waiting>,
    /// The refresh call in flight, if one is.
    refreshing: Option<u64>,
    next: u64,
}

/// Actions run inside actions only this deep (one that runs itself would never end).
const DEPTH: usize = 8;

impl CardState {
    pub fn new(manifest: &Manifest) -> Self {
        let root = json!({
            "form": manifest.form,
            "ui": manifest.ui,
            "data": null,
            "catalog": null,
            "account": null,
            "card": { "busy": false, "working": "", "action": "", "error": "", "error_code": "", "width": 0 },
        });
        Self { root, revision: 0, waiting: None, refreshing: None, next: 1 }
    }

    pub fn scope<'a>(&'a self, locals: &'a [(String, Value)]) -> Scope<'a> {
        Scope { root: &self.root, locals }
    }

    /// Whether a call or save is being waited for.
    pub fn busy(&self) -> bool {
        self.waiting.is_some()
    }

    /// Replace one scope (`data`, `catalog`, `account`, or `form` and `ui` when restoring).
    pub fn put(&mut self, scope: &str, value: Value) {
        self.root[scope] = value;
        self.revision += 1;
    }

    /// Write one value, as an input does.
    pub fn write(&mut self, place: &[String], value: Value) {
        if expr::read(&self.root, place) != &value {
            expr::write(&mut self.root, place, value);
            self.revision += 1;
        }
    }

    /// The card's width as last drawn, for `card.width` and `responsive` rules.
    pub fn set_width(&mut self, width: f32) {
        let width = width.round();
        if self.root["card"]["width"].as_f64() != Some(width as f64) {
            self.root["card"]["width"] = expr::number(width as f64);
            self.revision += 1;
        }
    }

    fn card(&mut self, key: &str, value: Value) {
        self.root["card"][key] = value;
    }

    fn fail(&mut self, failure: Failure) {
        self.card("error", Value::String(failure.message));
        self.card("error_code", Value::String(failure.code));
    }

    fn settle(&mut self) {
        self.waiting = None;
        self.card("busy", json!(false));
        self.card("working", json!(""));
        self.card("action", json!(""));
    }

    /// Run an action: `name` is what it's known by (for `card.action` and a call's name).
    pub fn run(&mut self, manifest: &Manifest, name: &str, steps: &[Step], locals: Locals) -> Vec<Effect> {
        self.revision += 1;
        self.fail(Failure::default());
        let mut effects = Vec::new();
        self.go(manifest, name, steps.to_vec(), locals, &mut effects, 0);
        effects
    }

    /// A call or save came back: its answer is written where the step said, and the rest of
    /// its action runs; or its failure is the card's error, and the rest doesn't.
    pub fn finish(&mut self, manifest: &Manifest, id: u64, result: Result<Value, Failure>) -> Vec<Effect> {
        if self.refreshing == Some(id) {
            self.refreshing = None;
            // A refresh that fails is asked again next time; the card says nothing.
            if let Ok(answer) = result
                && let Some(refresh) = &manifest.refresh
                && let Ok(place) = self.scope(&[]).place(&refresh.call.into)
            {
                self.write(&place, answer);
            }
            return Vec::new();
        }
        let Some(waiting) = self.waiting.take_if(|w| w.id == id) else { return Vec::new() };
        self.revision += 1;
        let name = self.root["card"]["action"].as_str().unwrap_or_default().to_string();
        self.settle();
        let mut effects = Vec::new();
        match result {
            Ok(answer) => {
                expr::write(&mut self.root, &waiting.into, answer);
                self.go(manifest, &name, waiting.rest, waiting.locals, &mut effects, 0);
            }
            Err(failure) => self.fail(failure),
        }
        effects
    }

    fn go(&mut self, manifest: &Manifest, name: &str, steps: Vec<Step>, locals: Locals, effects: &mut Vec<Effect>, depth: usize) -> bool {
        let mut steps = steps.into_iter();
        while let Some(step) = steps.next() {
            match step {
                Step::Set(pairs) => {
                    for (path, value) in pairs {
                        let scope = self.scope(&locals);
                        if let (Ok(place), value) = (scope.place(&path), scope.fill(&value))
                            && matches!(place[0].as_str(), "form" | "ui")
                        {
                            expr::write(&mut self.root, &place, value);
                        }
                    }
                }
                Step::Toggle(path) => {
                    if let Ok(place) = self.scope(&locals).place(&path)
                        && matches!(place[0].as_str(), "form" | "ui")
                    {
                        let on = expr::truthy(expr::read(&self.root, &place));
                        expr::write(&mut self.root, &place, json!(!on));
                    }
                }
                Step::Check { when, message } => {
                    if !self.scope(&locals).test(&when) {
                        let message = self.scope(&locals).text(&message);
                        self.fail(Failure { message, code: "check".into() });
                        return false;
                    }
                }
                Step::Reset(scope) => match scope.as_str() {
                    "form" => self.root["form"] = manifest.form.clone(),
                    "ui" => self.root["ui"] = manifest.ui.clone(),
                    _ => {}
                },
                Step::Open(target) => effects.push(Effect::Open(self.scope(&locals).text(&target))),
                Step::Play(target) => effects.push(Effect::Play(self.scope(&locals).text(&target))),
                Step::Close => effects.push(Effect::Close),
                Step::App(what) => effects.push(Effect::App(what)),
                Step::Run(action) => {
                    let Some(inner) = manifest.actions.get(&action).filter(|_| depth < DEPTH) else { continue };
                    // An action that waits takes what's left of this one with it.
                    let mut inner = inner.clone();
                    let waits = inner.iter().any(|s| matches!(s, Step::Call(_) | Step::Save(_) | Step::Run(_)));
                    if waits {
                        inner.extend(steps.by_ref());
                    }
                    if !self.go(manifest, &action, inner, locals.clone(), effects, depth + 1) {
                        return false;
                    }
                    if waits {
                        return true;
                    }
                }
                Step::Call(call) => {
                    // One thing at a time: a click while something is waited for does nothing.
                    if self.waiting.is_some() {
                        return false;
                    }
                    let Ok(into) = self.scope(&locals).place(&call.into) else { return false };
                    let request = self.request(&call, &locals);
                    let id = self.wait(name, &call.working, into, steps.collect(), locals);
                    effects.push(Effect::Call { id, request });
                    return true;
                }
                Step::Save(save) => {
                    if self.waiting.is_some() {
                        return false;
                    }
                    let scope = self.scope(&locals);
                    let Ok(into) = scope.place(&save.into) else { return false };
                    let (files, folder) = (scope.fill(&save.files), scope.text(&save.folder));
                    let id = self.wait(name, "Saving", into, steps.collect(), locals);
                    effects.push(Effect::Save { id, name: name.to_string(), files, folder });
                    return true;
                }
            }
        }
        true
    }

    fn wait(&mut self, name: &str, working: &str, into: Vec<String>, rest: Vec<Step>, locals: Locals) -> u64 {
        let id = self.next;
        self.next += 1;
        self.card("busy", json!(true));
        self.card("working", json!(working));
        self.card("action", json!(name));
        self.waiting = Some(Waiting { id, into, rest, locals });
        id
    }

    fn request(&self, call: &Call, locals: &[(String, Value)]) -> Request {
        let scope = self.scope(locals);
        let files = call
            .files
            .iter()
            .map(|(key, paths)| (key.clone(), scope.fill(paths).as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()))
            .filter(|(_, paths): &(String, Vec<String>)| !paths.is_empty())
            .collect();
        Request { name: call.name.clone(), method: call.method.clone(), path: scope.text(&call.path), body: call.body.as_ref().map(|b| scope.fill(b)), spends: call.spends, files }
    }

    /// The manifest's refresh, if it's due: its condition holds and none is in flight. The app
    /// asks this after each answer and every `every` seconds, and performs the call it gets.
    pub fn refresh(&mut self, manifest: &Manifest) -> Option<Effect> {
        let refresh = manifest.refresh.as_ref()?;
        if self.refreshing.is_some() || !self.scope(&[]).test(&refresh.when) {
            return None;
        }
        let id = self.next;
        self.next += 1;
        self.refreshing = Some(id);
        Some(Effect::Call { id, request: self.request(&refresh.call, &[]) })
    }

    /// The manifest's refresh asked once whatever its condition says, to catch up a card whose
    /// data may be behind the service's: an answer that came before the work it started
    /// showed, or a card read back from its record. Nothing while the card has no data yet,
    /// or while an action's call or another refresh is on its way.
    pub fn refresh_once(&mut self, manifest: &Manifest) -> Option<Effect> {
        let refresh = manifest.refresh.as_ref()?;
        if self.refreshing.is_some() || self.waiting.is_some() || !expr::truthy(&self.root["data"]) {
            return None;
        }
        let id = self.next;
        self.next += 1;
        self.refreshing = Some(id);
        Some(Effect::Call { id, request: self.request(&refresh.call, &[]) })
    }

    /// Whether `id` is a refresh on its way (not an action's call or save).
    pub fn is_refresh(&self, id: u64) -> bool {
        self.refreshing == Some(id)
    }

    /// Whether the manifest's refresh wants asking (now or once the one in flight is back).
    pub fn refreshes(&self, manifest: &Manifest) -> bool {
        manifest.refresh.as_ref().is_some_and(|r| self.scope(&[]).test(&r.when))
    }

    /// What to keep so the card comes back as it was: the manifest it started with and what
    /// the person and the service put in it.
    pub fn snapshot(&self, manifest: &Manifest) -> Value {
        json!({ "manifest": manifest.id, "version": manifest.version, "form": self.root["form"], "ui": self.root["ui"], "data": self.root["data"] })
    }

    /// Put back what [`CardState::snapshot`] kept.
    pub fn restore(&mut self, snapshot: &Value) {
        for scope in ["form", "ui", "data"] {
            if let Some(v) = snapshot.get(scope) {
                self.root[scope] = v.clone();
            }
        }
        self.revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::read(&json!({
            "id": "sound",
            "version": "1",
            "form": { "prompt": "", "seconds": 2, "loop": false },
            "ui": { "step": "edit" },
            "refresh": { "call": { "path": "/v1/sounds/{data.id}" }, "while": "data.status == 'running'", "every": 1 },
            "actions": {
                "make": [
                    { "check": "form.prompt", "message": "Say what the sound is first." },
                    { "call": { "path": "/v1/sounds", "body": { "prompt": "{form.prompt}", "seconds": "{form.seconds}", "loop": "{form.loop}" }, "spends": true, "working": "Making it" } },
                    { "set": { "ui.step": "done" } },
                ],
                "keep": [{ "save": { "files": "{data.files}", "folder": "assets/{data.id}", "into": "ui.saved" } }, { "open": "{ui.saved.0}" }],
                "again": [{ "reset": "form" }, { "set": { "ui.step": "edit" } }],
                "make_then_keep": [{ "run": "make" }, { "run": "keep" }],
            },
            "card": { "type": "column" },
        }))
        .unwrap()
    }

    fn run(state: &mut CardState, m: &Manifest, name: &str) -> Vec<Effect> {
        state.run(m, name, &m.actions[name].clone(), Vec::new())
    }

    #[test]
    fn a_check_that_fails_stops_the_action_and_says_why() {
        let m = manifest();
        let mut s = CardState::new(&m);
        assert!(run(&mut s, &m, "make").is_empty());
        assert_eq!(s.root["card"]["error"], "Say what the sound is first.");
        assert!(!s.busy());
    }

    #[test]
    fn a_call_is_waited_for_and_the_rest_runs_on_its_answer() {
        let m = manifest();
        let mut s = CardState::new(&m);
        s.write(&["form".into(), "prompt".into()], json!("a door creaking"));
        let effects = run(&mut s, &m, "make");
        let [Effect::Call { id, request }] = effects.as_slice() else { panic!("{effects:?}") };
        assert_eq!((request.name.as_str(), request.method.as_str(), request.path.as_str(), request.spends), ("make", "POST", "/v1/sounds", true));
        assert_eq!(request.body, Some(json!({ "prompt": "a door creaking", "seconds": 2, "loop": false })), "values keep their types");
        assert_eq!((s.root["card"]["busy"].clone(), s.root["card"]["working"].clone(), s.root["card"]["action"].clone()), (json!(true), json!("Making it"), json!("make")));
        assert_eq!(s.root["ui"]["step"], "edit", "the rest waits");
        assert!(run(&mut s, &m, "make").is_empty(), "a second click while it waits does nothing");
        assert!(s.finish(&m, *id, Ok(json!({ "id": "snd_1", "status": "done" }))).is_empty());
        assert_eq!(s.root["data"]["id"], "snd_1");
        assert_eq!(s.root["ui"]["step"], "done");
        assert_eq!(s.root["card"]["busy"], false);
    }

    #[test]
    fn a_call_that_fails_is_the_card_s_error_and_the_rest_doesn_t_run() {
        let m = manifest();
        let mut s = CardState::new(&m);
        s.write(&["form".into(), "prompt".into()], json!("rain"));
        let effects = run(&mut s, &m, "make");
        let [Effect::Call { id, .. }] = effects.as_slice() else { panic!() };
        s.finish(&m, *id, Err(Failure { message: "You're out of seconds this month.".into(), code: "quota".into() }));
        assert_eq!(s.root["card"]["error"], "You're out of seconds this month.");
        assert_eq!(s.root["card"]["error_code"], "quota");
        assert_eq!(s.root["ui"]["step"], "edit");
        assert!(!s.busy());
        // The next action clears it.
        run(&mut s, &m, "again");
        assert_eq!(s.root["card"]["error"], "");
    }

    #[test]
    fn a_save_hands_its_paths_to_what_follows() {
        let m = manifest();
        let mut s = CardState::new(&m);
        s.put("data", json!({ "id": "snd_1", "files": [{ "url": "https://x/a.wav" }] }));
        let effects = run(&mut s, &m, "keep");
        let [Effect::Save { id, name, files, folder }] = effects.as_slice() else { panic!("{effects:?}") };
        assert_eq!((name.as_str(), files, folder.as_str()), ("keep", &json!([{ "url": "https://x/a.wav" }]), "assets/snd_1"));
        let after = s.finish(&m, *id, Ok(json!(["assets/snd_1/a.wav"])));
        assert_eq!(after, [Effect::Open("assets/snd_1/a.wav".into())]);
    }

    #[test]
    fn an_action_run_inside_another_takes_what_follows_with_it() {
        let m = manifest();
        let mut s = CardState::new(&m);
        s.write(&["form".into(), "prompt".into()], json!("rain"));
        let effects = run(&mut s, &m, "make_then_keep");
        let [Effect::Call { id, .. }] = effects.as_slice() else { panic!("{effects:?}") };
        let after = s.finish(&m, *id, Ok(json!({ "id": "snd_2", "files": ["https://x/b.wav"] })));
        assert!(matches!(after.as_slice(), [Effect::Save { folder, .. }] if folder == "assets/snd_2"), "the save follows the call: {after:?}");
        assert_eq!(s.root["ui"]["step"], "done");
    }

    #[test]
    fn reset_puts_the_form_back() {
        let m = manifest();
        let mut s = CardState::new(&m);
        s.write(&["form".into(), "prompt".into()], json!("rain"));
        s.write(&["ui".into(), "step".into()], json!("done"));
        run(&mut s, &m, "again");
        assert_eq!(s.root["form"]["prompt"], "");
        assert_eq!(s.root["ui"]["step"], "edit");
    }

    #[test]
    fn refresh_is_asked_while_its_condition_holds() {
        let m = manifest();
        let mut s = CardState::new(&m);
        assert!(s.refresh(&m).is_none(), "nothing is running");
        s.put("data", json!({ "id": "snd_1", "status": "running" }));
        let Some(Effect::Call { id, request }) = s.refresh(&m) else { panic!() };
        assert_eq!((request.name.as_str(), request.path.as_str()), ("refresh", "/v1/sounds/snd_1"));
        assert!(s.refresh(&m).is_none(), "one at a time");
        assert!(!s.busy(), "a refresh doesn't make the card busy");
        s.finish(&m, id, Err(Failure::default()));
        assert_eq!(s.root["card"]["error"], "", "a refresh that fails says nothing");
        let Some(Effect::Call { id, .. }) = s.refresh(&m) else { panic!("asked again") };
        s.finish(&m, id, Ok(json!({ "id": "snd_1", "status": "done" })));
        assert!(s.refresh(&m).is_none(), "done: no more asking");
    }

    #[test]
    fn a_card_that_may_be_behind_is_refreshed_once_whatever_it_says() {
        let m = manifest();
        let mut s = CardState::new(&m);
        assert!(s.refresh_once(&m).is_none(), "no data yet: nothing to ask about");
        // The service's answer says nothing is running (it hadn't started yet).
        s.put("data", json!({ "id": "snd_1", "status": "queued" }));
        assert!(s.refresh(&m).is_none(), "its own condition doesn't hold");
        let Some(Effect::Call { id, request }) = s.refresh_once(&m) else { panic!() };
        assert_eq!(request.path, "/v1/sounds/snd_1");
        assert!(s.is_refresh(id));
        assert!(s.refresh_once(&m).is_none(), "one at a time");
        s.finish(&m, id, Ok(json!({ "id": "snd_1", "status": "running" })));
        assert!(!s.is_refresh(id));
        assert!(s.refresh(&m).is_some(), "now it says so itself, and is refreshed as usual");
    }

    #[test]
    fn loop_names_reach_the_action_clicked_inside_them() {
        let m = Manifest::read(&json!({ "id": "x", "actions": { "show": [{ "set": { "ui.set": "{set.id}" } }] }, "card": { "type": "column" } })).unwrap();
        let mut s = CardState::new(&m);
        s.run(&m, "show", &m.actions["show"].clone(), vec![("set".into(), json!({ "id": "s2" }))]);
        assert_eq!(s.root["ui"]["set"], "s2");
    }

    #[test]
    fn a_snapshot_brings_the_card_back() {
        let m = manifest();
        let mut s = CardState::new(&m);
        s.write(&["form".into(), "prompt".into()], json!("rain"));
        s.put("data", json!({ "id": "snd_1" }));
        let kept = s.snapshot(&m);
        assert_eq!((kept["manifest"].clone(), kept["version"].clone()), (json!("sound"), json!("1")));
        let mut back = CardState::new(&m);
        back.restore(&kept);
        assert_eq!((back.root["form"]["prompt"].clone(), back.root["data"]["id"].clone()), (json!("rain"), json!("snd_1")));
    }
}
