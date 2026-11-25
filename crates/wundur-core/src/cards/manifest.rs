//! A card manifest: one workflow's card as the service describes it. Its layout is a tree of
//! nodes, each a component this app draws (see [`COMPONENTS`]); its actions say what a click
//! does (see [`Step`]). Read leniently where that is safe (a property this app doesn't know is
//! ignored, and reported by [`Manifest::check`]) and strictly where it isn't: a component this
//! app doesn't have means the card can't be drawn, and the workflow isn't offered.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::expr;

/// The manifest format this app reads. A manifest with a later one isn't offered.
pub const SCHEMA: u64 = 1;

/// A component: its name, the properties it reads beyond [`COMMON`], whether it holds children.
pub struct Component {
    pub name: &'static str,
    pub props: &'static [&'static str],
    pub holds: bool,
}

/// Properties every node reads.
pub const COMMON: &[&str] = &[
    "type", "id", "when", "each", "as", "let", "responsive", "grow", "shrink", "basis", "width", "height", "min_width", "max_width", "min_height", "max_height", "align_self", "opacity", "tooltip",
];

const BOX: &[&str] = &[
    "children", "overlay", "gap", "padding", "align", "justify", "wrap", "background", "border", "border_top", "border_bottom", "border_left", "border_right", "radius", "clip", "shadow", "aspect", "click", "selected",
    "disabled", "divide",
];

const GRID: &[&str] = &[
    "children", "overlay", "gap", "padding", "align", "justify", "wrap", "background", "border", "border_top", "border_bottom", "border_left", "border_right", "radius", "clip", "shadow", "aspect", "click", "selected",
    "disabled", "divide", "columns",
];

/// Every component this app draws.
pub const COMPONENTS: &[Component] = &[
    Component { name: "row", props: BOX, holds: true },
    Component { name: "column", props: BOX, holds: true },
    Component { name: "grid", props: GRID, holds: true },
    Component { name: "text", props: &["text", "size", "weight", "color", "truncate", "align", "underline"], holds: false },
    Component { name: "icon", props: &["name", "size", "color"], holds: false },
    Component { name: "image", props: &["src", "fit", "radius", "border", "background", "aspect", "click"], holds: false },
    Component { name: "spinner", props: &["size", "color"], holds: false },
    Component { name: "placeholder", props: &["radius", "aspect"], holds: false },
    Component { name: "divider", props: &["vertical", "color"], holds: false },
    Component { name: "spacer", props: &[], holds: false },
    Component { name: "button", props: &["label", "icon", "icon_right", "style", "size", "disabled", "loading", "click"], holds: false },
    Component { name: "link", props: &["text", "icon", "size", "disabled", "click"], holds: false },
    Component { name: "input", props: &["bind", "placeholder", "number", "size", "disabled"], holds: false },
    Component { name: "textarea", props: &["bind", "placeholder", "rows", "disabled"], holds: false },
    Component { name: "segmented", props: &["bind", "options", "capitalize", "disabled", "style", "default"], holds: false },
    Component { name: "toggle", props: &["bind", "label", "icon", "size", "disabled", "on", "off"], holds: false },
    Component { name: "files", props: &["bind", "max", "label", "accept"], holds: false },
    Component { name: "steps", props: &["bind", "items", "reached", "default"], holds: false },
    Component { name: "use", props: &["part"], holds: false },
];

pub fn component(name: &str) -> Option<&'static Component> {
    COMPONENTS.iter().find(|c| c.name == name)
}

/// Properties read as conditions, as values, as somewhere to write, and as what a click does.
/// Every other string is text with holes.
const CONDITIONS: &[&str] = &["when", "selected", "disabled", "loading"];
const VALUES: &[&str] = &["each", "reached"];
const PLACES: &[&str] = &["bind"];
const ACTIONS: &[&str] = &["click"];

/// One node of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: String,
    pub props: Map<String, Value>,
    pub children: Vec<Node>,
    /// Laid over the node, taking its clicks, while the overlay's own `when` holds.
    pub overlay: Option<Box<Node>>,
}

impl Node {
    fn read(value: &Value, place: &str) -> Result<Node, String> {
        let Some(map) = value.as_object() else { return Err(format!("{place}: a node is an object with a `type`")) };
        let Some(kind) = map.get("type").and_then(Value::as_str) else { return Err(format!("{place}: a node needs a `type`")) };
        let place = format!("{place} > {kind}{}", map.get("id").and_then(Value::as_str).map(|i| format!("#{i}")).unwrap_or_default());
        let mut children = Vec::new();
        match map.get("children") {
            None => {}
            Some(Value::Array(items)) => {
                for (n, c) in items.iter().enumerate() {
                    children.push(Node::read(c, &format!("{place}[{n}]"))?);
                }
            }
            Some(_) => return Err(format!("{place}: `children` is a list of nodes")),
        }
        let overlay = map.get("overlay").map(|o| Node::read(o, &format!("{place} overlay"))).transpose()?.map(Box::new);
        let props = map.iter().filter(|(k, _)| !matches!(k.as_str(), "type" | "children" | "overlay")).map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(Node { kind: kind.to_string(), props, children, overlay })
    }

    pub fn str(&self, prop: &str) -> Option<&str> {
        self.props.get(prop).and_then(Value::as_str)
    }
}

/// A request to the service.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// What it's known by (a scenario answers it by this): its own `name`, else its action's.
    pub name: String,
    pub method: String,
    /// The path under the API's base, with holes: `/v1/assets/{data.id}/sets`.
    pub path: String,
    pub body: Option<Value>,
    /// Whether it uses something the person pays for. Only ever run from a click.
    pub spends: bool,
    /// What the card says while it waits ("Making the views").
    pub working: String,
    /// Where the answer goes: `data` unless said.
    pub into: String,
    /// Files from the person's disk sent with it: a name in the body, and the paths (a `files`
    /// component's value) whose contents go there, each as base64.
    pub files: Vec<(String, Value)>,
}

/// Files the service made, saved into the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    /// The files, with holes: a list of links, or of objects with a `url` (and a `name` to
    /// save it as, else the link's own).
    pub files: Value,
    /// The folder under the project, with holes: `assets/generated`.
    pub folder: String,
    /// Where the saved files' paths go.
    pub into: String,
}

/// One thing a click does. An action is a list of them, done in order; one that fails (a
/// check that doesn't hold, a call the service refuses) stops the rest.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Write values in `form` or `ui`.
    Set(Vec<(String, Value)>),
    /// Flip a value in `form` or `ui`.
    Toggle(String),
    /// Go on only if the condition holds; else show the message as the card's error.
    Check { when: String, message: String },
    Call(Call),
    Save(Save),
    /// Open a file or link: a picture in the viewer, a model in the model viewer.
    Open(String),
    /// Play a sound file or link.
    Play(String),
    /// Put `form` or `ui` back as the manifest starts them.
    Reset(String),
    /// Take the card out of the chat.
    Close,
    /// Something the app does itself: `sign_in`, `upgrade`, `request_access`.
    App(String),
    /// Another action, by name.
    Run(String),
}

/// Things the app does itself, for [`Step::App`].
pub const APP_ACTIONS: [&str; 3] = ["sign_in", "upgrade", "request_access"];

fn text(map: &Map<String, Value>, key: &str) -> String {
    map.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

impl Call {
    fn read(value: &Value, action: &str) -> Result<Call, String> {
        let Some(map) = value.as_object() else { return Err("a `call` is an object with a `path`".into()) };
        let path = text(map, "path");
        if path.is_empty() {
            return Err("a `call` needs a `path`".into());
        }
        let method = map.get("method").and_then(Value::as_str).unwrap_or(if map.contains_key("body") { "POST" } else { "GET" }).to_uppercase();
        if !matches!(method.as_str(), "GET" | "POST" | "PATCH" | "DELETE") {
            return Err(format!("a `call`'s method is GET, POST, PATCH or DELETE, not {method}"));
        }
        let name = match map.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None if !action.is_empty() => action.to_string(),
            None => format!("{method} {path}"),
        };
        let into = map.get("into").and_then(Value::as_str).unwrap_or("data").to_string();
        let files = map.get("files").and_then(Value::as_object).map(|f| f.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        Ok(Call { name, method, path, body: map.get("body").cloned(), spends: map.get("spends").is_some_and(expr::truthy), working: text(map, "working"), into, files })
    }
}

impl Step {
    fn read(value: &Value, action: &str) -> Result<Step, String> {
        let Some(map) = value.as_object() else { return Err("a step is an object, such as { \"set\": { … } }".into()) };
        let kinds: Vec<&str> = map.keys().map(String::as_str).filter(|k| *k != "message").collect();
        let [kind] = kinds.as_slice() else { return Err(format!("a step does one thing, not {}", if kinds.is_empty() { "nothing".to_string() } else { kinds.join(" and ") })) };
        let value = &map[*kind];
        let string = || value.as_str().map(str::to_string).ok_or(format!("`{kind}` takes text"));
        Ok(match *kind {
            "set" => {
                let Some(pairs) = value.as_object() else { return Err("`set` takes an object of places and values".into()) };
                Step::Set(pairs.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            }
            "toggle" => Step::Toggle(string()?),
            "check" => Step::Check { when: string()?, message: text(map, "message") },
            "call" => Step::Call(Call::read(value, action)?),
            "save" => {
                let Some(save) = value.as_object() else { return Err("`save` is an object with `files` and `folder`".into()) };
                let Some(files) = save.get("files").cloned() else { return Err("`save` needs `files`".into()) };
                Step::Save(Save { files, folder: save.get("folder").and_then(Value::as_str).unwrap_or("assets/generated").to_string(), into: save.get("into").and_then(Value::as_str).unwrap_or("ui.saved").to_string() })
            }
            "open" => Step::Open(string()?),
            "play" => Step::Play(string()?),
            "reset" => Step::Reset(string()?),
            "close" => Step::Close,
            "app" => Step::App(string()?),
            "run" => Step::Run(string()?),
            other => return Err(format!("`{other}` isn't something a step does")),
        })
    }
}

/// What a click does, as written: an action's name, one step, or a list of steps.
pub fn read_action(value: &Value, name: &str) -> Result<Vec<Step>, String> {
    match value {
        Value::String(action) => Ok(vec![Step::Run(action.clone())]),
        Value::Array(steps) => steps.iter().map(|s| Step::read(s, name)).collect(),
        Value::Object(_) => Ok(vec![Step::read(value, name)?]),
        _ => Err("an action is a name, a step or a list of steps".into()),
    }
}

/// Asking the service again while something is being made.
#[derive(Clone, Debug, PartialEq)]
pub struct Refresh {
    pub call: Call,
    /// Asked again for as long as this holds.
    pub when: String,
    /// Seconds between answers and the next asking.
    pub every: f64,
}

/// Something [`Manifest::check`] found. A fatal one means the card can't be drawn as meant
/// (the workflow isn't offered); the rest are mistakes this app draws around.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub place: String,
    pub text: String,
    pub fatal: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub schema: u64,
    pub id: String,
    /// Changes whenever the manifest does. A card keeps the one it started with.
    pub version: String,
    /// In the Create menu.
    pub label: String,
    pub icon: String,
    /// What `form` and `ui` start as.
    pub form: Value,
    pub ui: Value,
    pub refresh: Option<Refresh>,
    pub actions: BTreeMap<String, Vec<Step>>,
    pub parts: BTreeMap<String, Node>,
    pub card: Node,
    /// The card folded to one line (`ui.folded`), if it can be.
    pub row: Option<Node>,
}

impl Manifest {
    pub fn parse(source: &str) -> Result<Manifest, String> {
        let value: Value = serde_json::from_str(source).map_err(|e| format!("not JSON: {e}"))?;
        Manifest::read(&value)
    }

    pub fn read(value: &Value) -> Result<Manifest, String> {
        let Some(map) = value.as_object() else { return Err("a manifest is an object".into()) };
        let id = text(map, "id");
        if id.is_empty() {
            return Err("a manifest needs an `id`".into());
        }
        let object = |key: &str| match map.get(key) {
            None | Some(Value::Null) => Ok(Value::Object(Default::default())),
            Some(v) if v.is_object() => Ok(v.clone()),
            Some(_) => Err(format!("`{key}` is an object")),
        };
        let mut actions = BTreeMap::new();
        if let Some(Value::Object(named)) = map.get("actions") {
            for (name, action) in named {
                actions.insert(name.clone(), read_action(action, name).map_err(|e| format!("action `{name}`: {e}"))?);
            }
        }
        let mut parts = BTreeMap::new();
        if let Some(Value::Object(named)) = map.get("parts") {
            for (name, node) in named {
                parts.insert(name.clone(), Node::read(node, &format!("part `{name}`"))?);
            }
        }
        let refresh = match map.get("refresh") {
            None | Some(Value::Null) => None,
            Some(r) => {
                let call = Call::read(r.get("call").unwrap_or(&Value::Null), "refresh").map_err(|e| format!("refresh: {e}"))?;
                Some(Refresh { call, when: r.get("while").and_then(Value::as_str).unwrap_or("false").to_string(), every: r.get("every").and_then(Value::as_f64).unwrap_or(2.5).max(0.5) })
            }
        };
        let Some(card) = map.get("card") else { return Err("a manifest needs a `card` layout".into()) };
        Ok(Manifest {
            schema: map.get("schema").and_then(Value::as_u64).unwrap_or(1),
            id,
            version: map.get("version").map(expr::text_of).unwrap_or_default(),
            label: text(map, "label"),
            icon: text(map, "icon"),
            form: object("form")?,
            ui: object("ui")?,
            refresh,
            actions,
            parts,
            card: Node::read(card, "card")?,
            row: map.get("row").filter(|r| !r.is_null()).map(|r| Node::read(r, "row")).transpose()?,
        })
    }

    /// Whether this app can draw it as meant: a format it reads, and nothing fatal in it.
    pub fn supported(&self) -> bool {
        self.schema <= SCHEMA && !self.check().iter().any(|p| p.fatal)
    }

    /// Everything wrong with it, for whoever is writing it (the preview shows these).
    pub fn check(&self) -> Vec<Problem> {
        let mut found = Vec::new();
        if self.schema > SCHEMA {
            found.push(Problem { place: "manifest".into(), text: format!("format {} is newer than this app reads ({SCHEMA})", self.schema), fatal: true });
        }
        for (name, steps) in &self.actions {
            self.check_steps(steps, &format!("action `{name}`"), &mut found);
        }
        if let Some(r) = &self.refresh {
            check_condition(&r.when, "refresh `while`", &mut found);
            self.check_steps(&[Step::Call(r.call.clone())], "refresh", &mut found);
        }
        for (name, node) in &self.parts {
            self.check_node(node, &format!("part `{name}`"), &mut found);
        }
        self.check_node(&self.card, "card", &mut found);
        if let Some(row) = &self.row {
            self.check_node(row, "row", &mut found);
        }
        found
    }

    fn check_steps(&self, steps: &[Step], place: &str, found: &mut Vec<Problem>) {
        let mut problem = |text: String| found.push(Problem { place: place.to_string(), text, fatal: false });
        for step in steps {
            match step {
                Step::Set(pairs) => {
                    for (path, value) in pairs {
                        check_place(path, &mut problem);
                        check_templates(value, &mut problem);
                    }
                }
                Step::Toggle(path) | Step::Reset(path) => check_place(path, &mut problem),
                Step::Check { when, .. } => {
                    if let Err(e) = expr::parse(when) {
                        problem(e);
                    }
                }
                Step::Call(call) => {
                    check_templates(&Value::String(call.path.clone()), &mut problem);
                    if let Some(body) = &call.body {
                        check_templates(body, &mut problem);
                    }
                    if let Err(e) = expr::parse_path(&call.into) {
                        problem(e);
                    }
                    call.files.iter().for_each(|(_, paths)| check_templates(paths, &mut problem));
                }
                Step::Save(save) => {
                    check_templates(&save.files, &mut problem);
                    check_templates(&Value::String(save.folder.clone()), &mut problem);
                }
                Step::Open(target) | Step::Play(target) => check_templates(&Value::String(target.clone()), &mut problem),
                Step::App(what) => {
                    if !APP_ACTIONS.contains(&what.as_str()) {
                        problem(format!("`{what}` isn't something the app does (there are: {})", APP_ACTIONS.join(", ")));
                    }
                }
                Step::Run(name) => {
                    if !self.actions.contains_key(name) {
                        problem(format!("there's no action `{name}`"));
                    }
                }
                Step::Close => {}
            }
        }
    }

    fn check_node(&self, node: &Node, place: &str, found: &mut Vec<Problem>) {
        let place = format!("{place} > {}{}", node.kind, node.str("id").map(|i| format!("#{i}")).unwrap_or_default());
        let Some(component) = component(&node.kind) else {
            found.push(Problem { place, text: format!("this app has no `{}` component", node.kind), fatal: true });
            return;
        };
        let mut problems = Vec::new();
        let mut problem = |text: String| problems.push(text);
        let check_props = |props: &Map<String, Value>, problem: &mut dyn FnMut(String)| {
            for (key, value) in props {
                if !COMMON.contains(&key.as_str()) && !component.props.contains(&key.as_str()) && key != "under" {
                    problem(format!("`{key}` isn't a property of `{}`", node.kind));
                } else if CONDITIONS.contains(&key.as_str()) {
                    match value {
                        Value::String(s) => {
                            if let Err(e) = expr::parse(s) {
                                problem(e);
                            }
                        }
                        Value::Bool(_) => {}
                        _ => problem(format!("`{key}` is a condition")),
                    }
                } else if VALUES.contains(&key.as_str()) || (matches!(key.as_str(), "options" | "items") && value.is_string()) {
                    if let Err(e) = value.as_str().ok_or(format!("`{key}` names a value")).and_then(expr::parse) {
                        problem(e);
                    }
                } else if PLACES.contains(&key.as_str()) {
                    match value.as_str().ok_or(format!("`{key}` names somewhere in `form` or `ui`")).and_then(expr::parse_path) {
                        Ok(p) if !matches!(p.root.as_str(), "form" | "ui") => problem(format!("`{key}` writes to `form` or `ui`, not `{}`", p.root)),
                        Ok(_) => {}
                        Err(e) => problem(e),
                    }
                } else if key == "let" {
                    match value.as_object() {
                        Some(names) => {
                            for (name, value) in names {
                                if let Err(e) = value.as_str().ok_or(format!("`let` gives `{name}` a value by naming it")).and_then(expr::parse) {
                                    problem(e);
                                }
                            }
                        }
                        None => problem("`let` is an object of names and the values they stand for".into()),
                    }
                } else if !ACTIONS.contains(&key.as_str()) && key != "responsive" {
                    check_templates(value, problem);
                }
            }
        };
        check_props(&node.props, &mut problem);
        if let Some(rules) = node.props.get("responsive") {
            match rules.as_array() {
                Some(rules) => {
                    for rule in rules {
                        match rule.as_object() {
                            Some(r) if r.get("under").is_some_and(Value::is_number) => check_props(r, &mut problem),
                            _ => problem("a `responsive` rule is an object with `under` (a width) and the properties to use below it".into()),
                        }
                    }
                }
                None => problem("`responsive` is a list of rules".into()),
            }
        }
        if node.kind == "use" && !node.str("part").is_some_and(|p| self.parts.contains_key(p)) {
            found.push(Problem { place: place.clone(), text: format!("there's no part `{}`", node.str("part").unwrap_or_default()), fatal: true });
        }
        if !component.holds && !node.children.is_empty() {
            problem(format!("`{}` holds no children", node.kind));
        }
        found.extend(problems.into_iter().map(|text| Problem { place: place.clone(), text, fatal: false }));
        if let Some(click) = node.props.get("click") {
            match read_action(click, "") {
                Ok(steps) => self.check_steps(&steps, &format!("{place} click"), found),
                Err(e) => found.push(Problem { place: format!("{place} click"), text: e, fatal: false }),
            }
        }
        for (n, child) in node.children.iter().enumerate() {
            self.check_node(child, &format!("{place}[{n}]"), found);
        }
        if let Some(overlay) = &node.overlay {
            self.check_node(overlay, &format!("{place} overlay"), found);
        }
    }

    /// The names of the components it uses, for saying what a workflow needs.
    pub fn components(&self) -> Vec<String> {
        fn walk(node: &Node, into: &mut Vec<String>) {
            if !into.contains(&node.kind) {
                into.push(node.kind.clone());
            }
            node.children.iter().chain(node.overlay.as_deref()).for_each(|c| walk(c, into));
        }
        let mut names = Vec::new();
        self.parts.values().chain([&self.card]).chain(&self.row).for_each(|n| walk(n, &mut names));
        names.sort();
        names
    }
}

fn check_condition(condition: &str, place: &str, found: &mut Vec<Problem>) {
    if let Err(e) = expr::parse(condition) {
        found.push(Problem { place: place.to_string(), text: e, fatal: false });
    }
}

fn check_place(path: &str, problem: &mut dyn FnMut(String)) {
    match expr::parse_path(path) {
        Ok(p) if !matches!(p.root.as_str(), "form" | "ui") => problem(format!("`{path}`: only `form` and `ui` are written to")),
        Ok(_) => {}
        Err(e) => problem(e),
    }
}

/// Every string in `value` reads as a template.
fn check_templates(value: &Value, problem: &mut dyn FnMut(String)) {
    match value {
        Value::String(s) => {
            if let Err(e) = expr::parse_template(s) {
                problem(e);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| check_templates(i, problem)),
        Value::Object(map) => map.values().for_each(|v| check_templates(v, problem)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn manifest(card: Value) -> Manifest {
        Manifest::read(&json!({ "id": "test", "version": 3, "card": card, "actions": { "make": [{ "check": "form.prompt", "message": "Say what." }, { "call": { "path": "/v1/things", "body": { "prompt": "{form.prompt}" }, "spends": true } }] } })).unwrap()
    }

    #[test]
    fn a_manifest_reads_its_layout_and_actions() {
        let m = manifest(json!({ "type": "column", "gap": 8, "children": [{ "type": "text", "text": "Hello {form.name}" }, { "type": "button", "label": "Make", "click": "make" }] }));
        assert_eq!((m.id.as_str(), m.version.as_str(), m.schema), ("test", "3", 1));
        assert_eq!(m.card.children.len(), 2);
        assert_eq!(m.card.children[0].str("text"), Some("Hello {form.name}"));
        let Step::Call(call) = &m.actions["make"][1] else { panic!("the second step is the call") };
        assert_eq!((call.name.as_str(), call.method.as_str(), call.into.as_str(), call.spends), ("make", "POST", "data", true), "a call with a body posts, and is named for its action");
        assert!(m.check().is_empty(), "{:?}", m.check());
        assert!(m.supported());
        assert_eq!(m.components(), ["button", "column", "text"]);
    }

    #[test]
    fn a_component_this_app_lacks_makes_the_workflow_unsupported() {
        let m = manifest(json!({ "type": "column", "children": [{ "type": "hologram" }] }));
        let problems = m.check();
        assert!(problems.iter().any(|p| p.fatal && p.text.contains("hologram")), "{problems:?}");
        assert!(!m.supported());
        let newer = Manifest::read(&json!({ "id": "x", "schema": 99, "card": { "type": "column" } })).unwrap();
        assert!(!newer.supported(), "a later format isn't offered");
    }

    #[test]
    fn mistakes_are_found_and_placed_but_still_drawn() {
        let m = manifest(json!({ "type": "row", "children": [
            { "type": "text", "id": "title", "text": "{form.name", "colour": "muted" },
            { "type": "button", "label": "Go", "click": "launch", "disabled": "card.busy &&" },
            { "type": "input", "bind": "data.name" },
        ] }));
        let problems = m.check();
        let said = |needle: &str| problems.iter().any(|p| p.text.contains(needle));
        assert!(said("isn't closed"), "{problems:?}");
        assert!(said("`colour` isn't a property"));
        assert!(said("no action `launch`"));
        assert!(said("missing"), "the condition that stops short");
        assert!(said("writes to `form` or `ui`"));
        assert!(problems.iter().any(|p| p.place.contains("text#title")), "a problem says which node");
        assert!(m.supported(), "none of these stops the card being drawn");
    }

    /// The manifests in `cards/` are the ones the service starts from: each reads, checks
    /// clean and is one this app offers.
    #[test]
    fn the_example_manifests_check_clean() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cards");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") || name.ends_with(".scenario.json") {
                continue;
            }
            let m = Manifest::parse(&std::fs::read_to_string(entry.path()).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(m.check(), [], "{name}");
            assert!(m.supported(), "{name}");
            assert_eq!(format!("{}.json", m.id), name, "a manifest's file is named for its id");
            seen += 1;
        }
        assert!(seen >= 3, "the image, sound effect and 3D asset cards");
    }

    #[test]
    fn actions_read_in_their_three_forms() {
        assert_eq!(read_action(&json!("make"), "").unwrap(), [Step::Run("make".into())]);
        assert_eq!(read_action(&json!({ "toggle": "ui.open" }), "").unwrap(), [Step::Toggle("ui.open".into())]);
        let steps = read_action(&json!([{ "set": { "ui.step": "views" } }, { "close": true }]), "").unwrap();
        assert_eq!(steps, [Step::Set(vec![("ui.step".into(), json!("views"))]), Step::Close]);
        assert!(read_action(&json!({ "set": {}, "close": true }), "").unwrap_err().contains("one thing"));
        assert!(read_action(&json!({ "explode": true }), "").unwrap_err().contains("isn't something"));
        let Step::Call(call) = &read_action(&json!({ "call": { "path": "/v1/x" } }), "").unwrap()[0] else { panic!() };
        assert_eq!(call.name, "GET /v1/x", "a call outside a named action is known by what it asks");
    }

    #[test]
    fn responsive_rules_and_parts_are_checked() {
        let m = Manifest::read(&json!({
            "id": "x",
            "parts": { "head": { "type": "text", "text": "Head" } },
            "card": { "type": "row", "responsive": [{ "under": 500, "gap": 4, "sparkle": true }], "children": [{ "type": "use", "part": "head" }, { "type": "use", "part": "tail" }] },
        }))
        .unwrap();
        let problems = m.check();
        assert!(problems.iter().any(|p| p.text.contains("`sparkle`")), "{problems:?}");
        assert!(problems.iter().any(|p| p.fatal && p.text.contains("no part `tail`")));
    }
}
