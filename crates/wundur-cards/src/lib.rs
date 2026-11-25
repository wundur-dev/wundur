//! Cards drawn from manifests (see `wundur_core::cards` and `docs/card-manifests.md`).
//!
//! A [`CardView`] holds one card: the manifest it's drawn from and its data. It draws the
//! manifest's layout with this crate's components, keeps what the person types and picks, and
//! runs the manifest's actions. What only its owner can do (ask the service, save files, open
//! something) it hands over as a [`CardEvent::Effect`]; the owner does it and, for a call or a
//! save, says how it went with [`CardView::finish`].
//!
//! The owner in Wundur is the chat; in the preview (`wundur-card-preview`) it is a scenario's
//! made-up answers. Both draw with this code, so a card looks and acts the same in each.

pub mod style;
pub mod theme;

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use wundur_core::cards::manifest::read_action;
use wundur_core::cards::{CardState, Effect, Failure, Locals, Manifest, Node, expr};

use style::Props;

/// What a card tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum CardEvent {
    /// Something for the owner to do.
    Effect(Effect),
    /// Its data changed (worth saving).
    Changed,
}

/// How the owner draws what it draws better than this crate can: Wundur's pictures are
/// decoded small and let go when off screen, and its spinner is the system's.
#[derive(Clone, Default)]
pub struct Hooks {
    pub picture: Option<DrawPicture>,
    pub spinner: Option<DrawSpinner>,
}

/// A picture file drawn to fill its box, fitted as asked.
pub type DrawPicture = Rc<dyn Fn(&Path, ObjectFit, &mut App) -> AnyElement>;
/// A spinner: a key (several can turn at once), its size in points and its colour.
pub type DrawSpinner = Rc<dyn Fn(&str, f32, Hsla, &mut App) -> AnyElement>;

enum Field {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

/// A text box and the place in `form` or `ui` it writes.
struct Bound {
    field: Field,
    number: bool,
    place: Vec<String>,
    /// The value the box shows: when the data differs (a reset, a restore), the box follows.
    shown: Value,
    _sub: Subscription,
}

pub struct CardView {
    manifest: Arc<Manifest>,
    state: CardState,
    /// Where a picture named without a folder is looked for.
    base: PathBuf,
    hooks: Hooks,
    bound: HashMap<String, Bound>,
    width: Rc<Cell<f32>>,
    /// Whether the layout asks how wide the card is (`card.width`, `responsive`).
    measures: bool,
    polling: bool,
}

impl EventEmitter<CardEvent> for CardView {}

/// The theme's colours a card draws with, copied once a frame.
#[derive(Clone, Copy)]
struct Palette {
    text: Hsla,
    muted: Hsla,
    accent: Hsla,
    border: Hsla,
    surface: Hsla,
    background: Hsla,
    success: Hsla,
    skeleton: Hsla,
}

fn palette(cx: &App) -> Palette {
    let t = cx.theme();
    Palette { text: t.foreground, muted: t.muted_foreground, accent: t.ring, border: t.border, surface: t.secondary, background: t.background, success: t.green, skeleton: t.skeleton }
}

fn measures(manifest: &Manifest) -> bool {
    fn asks(value: &Value) -> bool {
        match value {
            Value::String(s) => s.contains("card.width"),
            Value::Array(items) => items.iter().any(asks),
            Value::Object(map) => map.values().any(asks),
            _ => false,
        }
    }
    fn walk(node: &Node) -> bool {
        node.props.contains_key("responsive") || node.props.values().any(asks) || node.children.iter().chain(node.overlay.as_deref()).any(walk)
    }
    manifest.parts.values().chain([&manifest.card]).chain(&manifest.row).any(walk)
}

/// A node's element id: its own `id` (numbered inside a loop, so each is its own), else where
/// it is in the layout.
fn element_id(props: &Props, key: &str) -> SharedString {
    match props.get("id").and_then(Value::as_str) {
        Some(id) => {
            let loops: String = key.split('/').filter_map(|s| s.split_once(':').map(|(_, n)| format!(":{n}"))).collect();
            format!("{id}{loops}").into()
        }
        None => key.to_string().into(),
    }
}

fn capital(text: &str) -> String {
    let mut c = text.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn is_picture(path: &str) -> bool {
    matches!(Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "webp" | "gif"))
}

/// How deep a layout nests (a part that uses itself would never end).
const DEPTH: usize = 32;

impl CardView {
    /// A card for `manifest`, new. Pictures named without a folder are looked for in `base`.
    pub fn new(manifest: Arc<Manifest>, base: impl Into<PathBuf>, _: &mut Context<Self>) -> Self {
        Self { state: CardState::new(&manifest), measures: measures(&manifest), manifest, base: base.into(), hooks: Hooks::default(), bound: HashMap::new(), width: Rc::new(Cell::new(0.)), polling: false }
    }

    pub fn set_hooks(&mut self, hooks: Hooks) {
        self.hooks = hooks;
    }

    pub fn manifest(&self) -> &Arc<Manifest> {
        &self.manifest
    }

    /// Draw from another manifest (a newer layout), keeping the card's data.
    pub fn set_manifest(&mut self, manifest: Arc<Manifest>, cx: &mut Context<Self>) {
        self.measures = measures(&manifest);
        self.manifest = manifest;
        self.poll(cx);
        cx.notify();
    }

    pub fn state(&self) -> &CardState {
        &self.state
    }

    /// Change the card's data from outside (the catalog arrived, a card is restored).
    pub fn update_state(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut CardState)) {
        change(&mut self.state);
        self.poll(cx);
        cx.notify();
    }

    /// Run one of the manifest's actions by name, as a click on it would.
    pub fn run(&mut self, action: &str, cx: &mut Context<Self>) {
        self.act(json!(action), Locals::new(), cx);
    }

    /// A call or a save came back.
    pub fn finish(&mut self, id: u64, result: Result<Value, Failure>, cx: &mut Context<Self>) {
        let manifest = self.manifest.clone();
        let effects = self.state.finish(&manifest, id, result);
        self.settle(effects, cx);
    }

    fn act(&mut self, action: Value, locals: Locals, cx: &mut Context<Self>) {
        let Ok(steps) = read_action(&action, "") else { return };
        let manifest = self.manifest.clone();
        let effects = self.state.run(&manifest, "", &steps, locals);
        self.settle(effects, cx);
    }

    fn settle(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for effect in effects {
            cx.emit(CardEvent::Effect(effect));
        }
        self.poll(cx);
        cx.emit(CardEvent::Changed);
        cx.notify();
    }

    fn put(&mut self, place: &[String], value: Value, cx: &mut Context<Self>) {
        self.state.write(place, value);
        cx.emit(CardEvent::Changed);
        cx.notify();
    }

    /// Ask the manifest's refresh for as long as its condition holds.
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.polling || !self.state.refreshes(&self.manifest) {
            return;
        }
        let every = self.manifest.refresh.as_ref().map_or(2.5, |r| r.every);
        self.polling = true;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_secs_f64(every)).await;
                let more = this.update(cx, |card, cx| {
                    let manifest = card.manifest.clone();
                    if !card.state.refreshes(&manifest) {
                        card.polling = false;
                        return false;
                    }
                    if let Some(effect) = card.state.refresh(&manifest) {
                        cx.emit(CardEvent::Effect(effect));
                    }
                    true
                });
                if !more.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    // ----- reading a node -----

    /// A node's properties as they are at the card's width: its `responsive` rules that hold
    /// (each `under` a width) laid over its own, later rules over earlier.
    fn props<'a>(&self, node: &'a Node) -> Cow<'a, Props> {
        let Some(Value::Array(rules)) = node.props.get("responsive") else { return Cow::Borrowed(&node.props) };
        let width = self.state.root["card"]["width"].as_f64().unwrap_or(0.);
        let mut props = node.props.clone();
        // Before the card has been measured nothing is known to be narrow.
        for rule in rules.iter().filter_map(Value::as_object).filter(|r| width > 0. && r.get("under").and_then(Value::as_f64).is_some_and(|u| width < u)) {
            props.extend(rule.iter().filter(|(k, _)| *k != "under").map(|(k, v)| (k.clone(), v.clone())));
        }
        Cow::Owned(props)
    }

    fn holds(&self, props: &Props, key: &str, locals: &Locals) -> bool {
        match props.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(condition)) => self.state.scope(locals).test(condition),
            _ => false,
        }
    }

    fn text(&self, props: &Props, key: &str, locals: &Locals) -> String {
        props.get(key).map(|v| expr::text_of(&self.state.scope(locals).fill(v))).unwrap_or_default()
    }

    fn place(&self, props: &Props, locals: &Locals) -> Option<Vec<String>> {
        let place = self.state.scope(locals).place(props.get("bind")?.as_str()?).ok()?;
        matches!(place[0].as_str(), "form" | "ui").then_some(place)
    }

    fn file(&self, src: &str) -> PathBuf {
        let path = Path::new(src);
        if path.is_absolute() { path.to_path_buf() } else { self.base.join(path) }
    }

    fn picture(&self, src: &str, fit: ObjectFit, p: Palette, cx: &mut App) -> AnyElement {
        if src.is_empty() {
            return div().size_full().bg(p.skeleton).into_any_element();
        }
        if src.starts_with("https://") || src.starts_with("http://") {
            return img(SharedString::from(src.to_string())).size_full().object_fit(fit).into_any_element();
        }
        let path = self.file(src);
        match &self.hooks.picture {
            Some(draw) => draw(&path, fit, cx),
            None => img(path).size_full().object_fit(fit).with_fallback(move || div().size_full().bg(p.skeleton).into_any_element()).into_any_element(),
        }
    }

    fn spinner(&self, key: &str, size: f32, color: Hsla, cx: &mut App) -> AnyElement {
        match &self.hooks.spinner {
            Some(draw) => draw(key, size, color, cx),
            None => Icon::new(gpui_kit::component::IconName::LoaderCircle)
                .size(px(size))
                .text_color(color)
                .with_animation(SharedString::from(format!("spin-{key}")), Animation::new(Duration::from_secs(1)).repeat(), |icon, t| icon.transform(Transformation::rotate(percentage(t))))
                .into_any_element(),
        }
    }

    // ----- drawing -----

    /// A node as elements: none when its `when` doesn't hold, one for each item of its `each`.
    fn nodes(&mut self, node: &Node, key: &str, locals: &mut Locals, depth: usize, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if depth > DEPTH {
            return Vec::new();
        }
        let Some(each) = node.str("each") else { return self.one(node, key, locals, depth, window, cx) };
        let items = match self.state.scope(locals).value_at(each) {
            Value::Array(items) => items,
            Value::Object(map) => map.into_iter().map(|(key, value)| json!({ "key": key, "value": value })).collect(),
            _ => Vec::new(),
        };
        let name = node.str("as").unwrap_or("item").to_string();
        let mut out = Vec::new();
        for (n, item) in items.into_iter().enumerate() {
            locals.push((name.clone(), item));
            locals.push((format!("{name}_index"), json!(n)));
            locals.push((format!("{name}_number"), json!(n + 1)));
            out.extend(self.one(node, &format!("{key}:{n}"), locals, depth, window, cx));
            locals.truncate(locals.len() - 3);
        }
        out
    }

    /// A node with the names its `let` gives in reach, for it and everything in it.
    fn one(&mut self, node: &Node, key: &str, locals: &mut Locals, depth: usize, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(Value::Object(names)) = node.props.get("let") else { return self.draw(node, key, locals, depth, window, cx) };
        let before = locals.len();
        for (name, value) in names {
            // Each can use the ones before it.
            let value = value.as_str().map_or(Value::Null, |v| self.state.scope(locals).value_at(v));
            locals.push((name.clone(), value));
        }
        let drawn = self.draw(node, key, locals, depth, window, cx);
        locals.truncate(before);
        drawn
    }

    fn draw(&mut self, node: &Node, key: &str, locals: &mut Locals, depth: usize, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let props = self.props(node);
        if props.contains_key("when") && !self.holds(&props, "when", locals) {
            return Vec::new();
        }
        // Each kind is drawn by its own function, so that a layout nested deep costs little
        // stack for each level (one function for them all overflowed a test thread's).
        match node.kind.as_str() {
            "row" | "column" | "grid" => vec![self.container(node, &props, key, locals, depth, window, cx)],
            "use" => {
                let manifest = self.manifest.clone();
                let Some(part) = node.str("part").and_then(|name| manifest.parts.get(name)) else { return Vec::new() };
                self.nodes(part, &format!("{key}/{}", node.str("part").unwrap_or_default()), locals, depth + 1, window, cx)
            }
            _ => self.leaf(node, &props, key, locals, window, cx),
        }
    }

    /// A row, column or grid, with what it holds.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn container(&mut self, node: &Node, props: &Props, key: &str, locals: &mut Locals, depth: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let id = element_id(props, key);
        // A node the manifest names can be found by that name (by tests, and by automation).
        let named = props.contains_key("id");
        let tip = props.contains_key("tooltip").then(|| self.text(props, "tooltip", locals)).filter(|t| !t.is_empty());
        let disabled = self.holds(props, "disabled", locals);
        let click = props.get("click").cloned();
        let mut children = Vec::new();
        for (n, child) in node.children.iter().enumerate() {
            children.extend(self.nodes(child, &format!("{key}/{n}"), locals, depth + 1, window, cx));
        }
        let overlay = match node.overlay.as_deref() {
            Some(o) => self.nodes(o, &format!("{key}/overlay"), locals, depth + 1, window, cx).into_iter().next(),
            None => None,
        };
        let column = node.kind == "column";
        let mut d = div().min_w_0();
        d = match node.kind.as_str() {
            "row" => d.flex().flex_row(),
            "column" => d.flex().flex_col(),
            _ => d.grid().grid_cols(style::num(props, "columns").map_or(2, |c| c.max(1.) as u16)),
        };
        d = style::boxed(style::sized(d, props), props, cx);
        if style::flag(props, "divide") {
            for (n, child) in children.into_iter().enumerate() {
                let line = div().min_w_0().when(n > 0, |l| if column { l.border_t_1() } else { l.border_l_1() }).border_color(p.border);
                d = d.child(line.child(child));
            }
        } else {
            d = d.children(children);
        }
        let selected = self.holds(props, "selected", locals);
        if selected {
            d = d.border_color(p.accent).bg(p.accent.opacity(0.08));
        }
        if disabled {
            d = d.opacity(0.55);
        }
        if let Some(overlay) = overlay {
            d = d.relative().child(
                div().id(SharedString::from(format!("{id}-overlay"))).absolute().inset_0().bg(p.background.opacity(0.8)).block_mouse_except_scroll().flex().flex_col().items_center().justify_center().child(overlay),
            );
        }
        match (click.filter(|_| !disabled), tip) {
            (None, None) if !named => d.into_any_element(),
            (click, tip) => {
                let mut d = d.id(id.clone());
                if let Some(action) = click {
                    let locals = locals.clone();
                    d = d.cursor_pointer().when(!selected, |d| d.hover(move |d| d.bg(p.surface.opacity(0.6)))).on_click(cx.listener(move |card, _, _, cx| card.act(action.clone(), locals.clone(), cx)));
                }
                if let Some(tip) = tip {
                    d = d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(SharedString::from(tip.clone())).build(w, cx));
                }
                d.test_support().into_any_element()
            }
        }
    }

    /// Any other component: none holds children.
    #[inline(never)]
    fn leaf(&mut self, node: &Node, props: &Props, key: &str, locals: &mut Locals, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = palette(cx);
        let id = element_id(props, key);
        // A node the manifest names can be found by that name (by tests, and by automation).
        let named = props.contains_key("id");
        let tip = props.contains_key("tooltip").then(|| self.text(props, "tooltip", locals)).filter(|t| !t.is_empty());
        let disabled = self.holds(props, "disabled", locals);
        let click = props.get("click").cloned();
        // A leaf that isn't a plain box: given its size and tooltip by a box around it.
        let wrap = |element: AnyElement, tip: Option<String>| -> AnyElement {
            if !style::has_sizing(props) && tip.is_none() {
                return element;
            }
            let wrapper = style::sized(div().min_w_0(), props).child(element);
            match tip {
                Some(tip) => wrapper.id(SharedString::from(format!("{id}-tip"))).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(SharedString::from(tip.clone())).build(w, cx)).into_any_element(),
                None => wrapper.into_any_element(),
            }
        };
        let element: AnyElement = match node.kind.as_str() {
            "text" => {
                let mut d = style::text_size(div().min_w_0(), props.get("size")).text_color(props.get("color").and_then(Value::as_str).and_then(|c| style::color(c, cx)).unwrap_or(p.text));
                if let Some(w) = style::weight(props.get("weight").and_then(Value::as_str)) {
                    d = d.font_weight(w);
                }
                if style::flag(props, "truncate") {
                    d = d.truncate();
                }
                match props.get("align").and_then(Value::as_str) {
                    Some("center") => d = d.text_center(),
                    Some("end") => d = d.text_right(),
                    _ => {}
                }
                if style::flag(props, "underline") {
                    d = d.underline();
                }
                let d = style::sized(d, props).child(self.text(props, "text", locals));
                match tip {
                    Some(tip) => d.id(id.clone()).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(SharedString::from(tip.clone())).build(w, cx)).test_support().into_any_element(),
                    None if named => d.id(id.clone()).test_support().into_any_element(),
                    None => d.into_any_element(),
                }
            }
            "icon" => {
                let icon = style::icon(&self.text(props, "name", locals)).unwrap_or(Lucide::Box);
                let color = props.get("color").and_then(Value::as_str).and_then(|c| style::color(c, cx)).unwrap_or(p.muted);
                wrap(Icon::new(icon).size(px(style::icon_size(props.get("size")))).flex_none().text_color(color).into_any_element(), tip)
            }
            "image" => {
                let fit = match props.get("fit").and_then(Value::as_str) {
                    Some("cover") => ObjectFit::Cover,
                    Some("fill") => ObjectFit::Fill,
                    _ => ObjectFit::Contain,
                };
                let picture = self.picture(&self.text(props, "src", locals), fit, p, cx);
                let d = style::boxed(style::sized(div().overflow_hidden(), props), props, cx).child(picture);
                match click.filter(|_| !disabled) {
                    Some(action) => {
                        let locals = locals.clone();
                        d.id(id.clone()).cursor_pointer().on_click(cx.listener(move |card, _, _, cx| card.act(action.clone(), locals.clone(), cx))).test_support().into_any_element()
                    }
                    None => d.into_any_element(),
                }
            }
            "spinner" => {
                let size = match props.get("size") {
                    Some(Value::String(s)) if s == "sm" => 16.,
                    Some(Value::Number(n)) => n.as_f64().unwrap_or(24.) as f32,
                    _ => 24.,
                };
                let color = props.get("color").and_then(Value::as_str).and_then(|c| style::color(c, cx)).unwrap_or(p.muted);
                wrap(div().size(px(size)).flex_none().child(self.spinner(&id, size, color, cx)).into_any_element(), tip)
            }
            "placeholder" => {
                let d = div().rounded(px(style::num(props, "radius").unwrap_or(12.))).bg(p.skeleton).when_some(style::num(props, "aspect"), |d, a| d.aspect_ratio(a));
                if style::has_sizing(props) { style::sized(d, props).into_any_element() } else { d.size_full().into_any_element() }
            }
            "divider" => {
                let color = props.get("color").and_then(Value::as_str).and_then(|c| style::color(c, cx)).unwrap_or(p.border);
                let d = if style::flag(props, "vertical") { div().w(px(1.)).flex_none().self_stretch() } else { div().h(px(1.)).flex_none().w_full() };
                style::sized(d.bg(color), props).into_any_element()
            }
            "spacer" => style::sized(div().flex_1(), props).into_any_element(),
            "button" => {
                // No label: the icon alone.
                let label = self.text(props, "label", locals);
                let mut b = Button::new(id.clone());
                if !label.is_empty() {
                    b = b.label(label);
                }
                if let Some(icon) = props.contains_key("icon").then(|| style::icon(&self.text(props, "icon", locals))).flatten() {
                    b = b.icon(icon);
                }
                b = match props.get("style").and_then(Value::as_str) {
                    Some("primary") => b.primary(),
                    Some("ghost") => b.ghost(),
                    Some("danger") => b.danger(),
                    _ => b.outline(),
                };
                b = match props.get("size").and_then(Value::as_str) {
                    Some("xs") => b.xsmall(),
                    Some("md") => b,
                    Some("lg") => b.large(),
                    _ => b.small(),
                };
                if let Some(icon) = props.contains_key("icon_right").then(|| style::icon(&self.text(props, "icon_right", locals))).flatten() {
                    b = b.child(Icon::new(icon).small());
                }
                let loading = self.holds(props, "loading", locals);
                b = b.loading(loading).disabled(disabled || loading);
                if let Some(tip) = tip.clone() {
                    b = b.tooltip(tip);
                }
                if let Some(action) = click {
                    let locals = locals.clone();
                    b = b.on_click(cx.listener(move |card, _, _, cx| card.act(action.clone(), locals.clone(), cx)));
                }
                wrap(b.into_any_element(), None)
            }
            "link" => {
                let icon = props.contains_key("icon").then(|| style::icon(&self.text(props, "icon", locals))).flatten();
                let mut d = style::text_size(div().id(id.clone()).flex().items_center().gap_1(), props.get("size")).text_color(if disabled { p.muted } else { p.accent });
                if let Some(icon) = icon {
                    d = d.child(Icon::new(icon).xsmall().text_color(p.muted));
                }
                d = d.child(self.text(props, "text", locals));
                if let Some(action) = click.filter(|_| !disabled) {
                    let locals = locals.clone();
                    d = d.cursor_pointer().hover(|d| d.underline()).on_click(cx.listener(move |card, _, _, cx| card.act(action.clone(), locals.clone(), cx)));
                }
 wrap(d.test_support().into_any_element(), tip)
            }
            "input" | "textarea" => match self.field(node, props, locals, window, cx) {
                Some(Field::Line(input)) => {
                    let small = props.get("size").and_then(Value::as_str) == Some("sm");
                    wrap(Input::new(&input).id(id.clone()).when(small, |i| i.small()).disabled(disabled).into_any_element(), tip)
                }
                Some(Field::Area(area)) => wrap(div().id(id.clone()).child(Textarea::new(&area).disabled(disabled)).test_support().into_any_element(), tip),
                None => return Vec::new(),
            },
            "segmented" => {
                let Some(place) = self.place(props, locals) else { return Vec::new() };
                let listed = match props.get("options") {
                    Some(Value::String(named)) => self.state.scope(locals).value_at(named),
                    Some(list) => list.clone(),
                    None => Value::Null,
                };
                let capitalize = style::flag(props, "capitalize");
                let options: Vec<(Value, String)> = listed
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| match o {
                        Value::Object(map) => (map.get("id").cloned().unwrap_or(Value::Null), map.get("label").map(expr::text_of).unwrap_or_default()),
                        other => (other.clone(), if capitalize { capital(&expr::text_of(other)) } else { expr::text_of(other) }),
                    })
                    .collect();
                // Nothing picked yet: the one the manifest says stands (the service's default).
                let current = Some(expr::read(&self.state.root, &place).clone()).filter(|v| !v.is_null() && v != "").unwrap_or_else(|| props.get("default").map_or(Value::Null, |d| self.state.scope(locals).fill(d)));
                if props.get("style").and_then(Value::as_str) == Some("chips") {
                    let mut chips = div().flex().flex_wrap().items_center().gap_1();
                    for (n, (value, label)) in options.into_iter().enumerate() {
                        let (on, place) = (value == current, place.clone());
                        let chip = Button::new(SharedString::from(format!("{id}-{n}"))).xsmall().label(label);
                        chips = chips.child((if on { chip.outline() } else { chip.ghost() }).disabled(disabled).on_click(cx.listener(move |card, _, _, cx| card.put(&place, value.clone(), cx))));
                    }
                    return vec![wrap(chips.into_any_element(), tip)];
                }
                let mut row = div().flex().w_full().rounded(px(8.)).border_1().border_color(p.border).overflow_hidden().when(disabled, |d| d.opacity(0.55));
                for (n, (value, label)) in options.into_iter().enumerate() {
                    let on = value == current;
                    let place = place.clone();
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("{id}-{n}")))
                            .flex_1()
                            .min_w_0()
                            .px_3()
                            .py(px(4.))
                            .when(n > 0, |d| d.border_l_1().border_color(p.border))
                            .flex()
                            .justify_center()
                            .text_sm()
                            .when(on, |d| d.bg(p.accent.opacity(0.16)).text_color(p.accent).font_weight(FontWeight::SEMIBOLD))
                            .when(!on, |d| d.text_color(p.text))
                            .child(div().truncate().child(label))
                            .when(!disabled, |d| d.cursor_pointer().when(!on, |d| d.hover(move |d| d.bg(p.surface))).on_click(cx.listener(move |card, _, _, cx| card.put(&place, value.clone(), cx))))
                            .test_support(),
                    );
                }
                wrap(row.into_any_element(), tip)
            }
            "toggle" => {
                let Some(place) = self.place(props, locals) else { return Vec::new() };
                // What it writes when on and when off: true and false, unless the manifest says.
                let (yes, no) = (props.get("on").cloned().unwrap_or(json!(true)), props.get("off").cloned().unwrap_or(json!(false)));
                let on = if props.contains_key("on") { expr::read(&self.state.root, &place) == &yes } else { expr::truthy(expr::read(&self.state.root, &place)) };
                let mut b = Button::new(id.clone()).label(self.text(props, "label", locals));
                if let Some(icon) = props.contains_key("icon").then(|| style::icon(&self.text(props, "icon", locals))).flatten() {
                    b = b.icon(icon);
                }
                b = if on { b.outline() } else { b.ghost() };
                b = match props.get("size").and_then(Value::as_str) {
                    Some("sm") => b.small(),
                    Some("md") => b,
                    _ => b.xsmall(),
                };
                if let Some(tip) = tip.clone() {
                    b = b.tooltip(tip);
                }
                b = b.disabled(disabled).on_click(cx.listener(move |card, _, _, cx| card.put(&place, if on { no.clone() } else { yes.clone() }, cx)));
                wrap(b.into_any_element(), None)
            }
            "files" => {
                let Some(place) = self.place(props, locals) else { return Vec::new() };
                wrap(self.files(&id, props, place, locals, p, cx), tip)
            }
            "steps" => {
                let Some(place) = self.place(props, locals) else { return Vec::new() };
                wrap(self.steps(&id, props, place, locals, p, cx), tip)
            }
            // A component this app doesn't have: nothing is drawn for it (and the manifest
            // isn't offered: see `Manifest::supported`).
            _ => return Vec::new(),
        };
        vec![element]
    }

    /// The text box for an `input` or `textarea` node: made the first time it's drawn, and
    /// made to show the data whenever something else changed it.
    fn field(&mut self, node: &Node, props: &Props, locals: &Locals, window: &mut Window, cx: &mut Context<Self>) -> Option<Field> {
        let place = self.place(props, locals)?;
        let key = place.join(".");
        let now = expr::read(&self.state.root, &place).clone();
        if !self.bound.contains_key(&key) {
            let placeholder = self.text(props, "placeholder", locals);
            let (start, changed) = (expr::text_of(&now), key.clone());
            let (field, sub) = if node.kind == "textarea" {
                let rows = props.get("rows").and_then(Value::as_array).map(|r| (r.first().and_then(Value::as_u64).unwrap_or(2) as usize, r.get(1).and_then(Value::as_u64).unwrap_or(6) as usize)).unwrap_or((2, 6));
                let area = cx.new(|cx| {
                    let mut state = TextareaState::new(window, cx).auto_grow(rows.0.max(1), rows.1.max(rows.0).max(1)).placeholder(placeholder);
                    if !start.is_empty() {
                        state.set_value(start, window, cx);
                    }
                    state
                });
                let sub = cx.subscribe_in(&area, window, move |card, area, event: &InputEvent, _, cx| {
                    if let InputEvent::Change = event {
                        let text = area.read(cx).value().to_string();
                        card.typed(&changed, text, cx);
                    }
                });
                (Field::Area(area), sub)
            } else {
                let input = cx.new(|cx| {
                    let mut state = InputState::new(window, cx).placeholder(placeholder);
                    if !start.is_empty() {
                        state.set_value(start, window, cx);
                    }
                    state
                });
                let sub = cx.subscribe_in(&input, window, move |card, input, event: &InputEvent, _, cx| {
                    if let InputEvent::Change = event {
                        let text = input.read(cx).value().to_string();
                        card.typed(&changed, text, cx);
                    }
                });
                (Field::Line(input), sub)
            };
            self.bound.insert(key.clone(), Bound { field, number: style::flag(props, "number"), place, shown: now.clone(), _sub: sub });
        }
        let bound = self.bound.get_mut(&key)?;
        if bound.shown != now {
            bound.shown = now.clone();
            let text = expr::text_of(&now);
            match &bound.field {
                Field::Line(input) => input.update(cx, |i, cx| i.set_value(text, window, cx)),
                Field::Area(area) => area.update(cx, |a, cx| a.set_value(text, window, cx)),
            }
        }
        Some(match &bound.field {
            Field::Line(input) => Field::Line(input.clone()),
            Field::Area(area) => Field::Area(area.clone()),
        })
    }

    fn typed(&mut self, key: &str, text: String, cx: &mut Context<Self>) {
        let Some(bound) = self.bound.get_mut(key) else { return };
        let value = if bound.number { text.trim().parse::<f64>().ok().map_or(Value::Null, expr::number) } else { Value::String(text) };
        if bound.shown == value {
            return;
        }
        bound.shown = value.clone();
        let place = bound.place.clone();
        self.put(&place, value, cx);
    }

    /// Files picked from disk (reference pictures): a button to add them, each one small with
    /// a way to drop it, and how many of the most there are.
    fn files(&mut self, id: &SharedString, props: &Props, place: Vec<String>, locals: &Locals, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let most = style::num(props, "max").map_or(3, |m| m.max(1.) as usize);
        let accept: Vec<String> = props.get("accept").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_lowercase).collect()).unwrap_or_default();
        let picked: Vec<String> = expr::read(&self.state.root, &place).as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        let label = Some(self.text(props, "label", locals)).filter(|l| !l.is_empty()).unwrap_or_else(|| "Add files".into());
        let mut line = div().flex().flex_wrap().items_center().gap_2();
        if picked.len() < most {
            let (place, picked) = (place.clone(), picked.clone());
            line = line.child(Button::new(SharedString::from(format!("{id}-add"))).small().outline().icon(Lucide::Paperclip).label(label).on_click(cx.listener(move |_, _, _, cx| {
                let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
                let (place, accept, mut picked) = (place.clone(), accept.clone(), picked.clone());
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let Ok(Ok(Some(paths))) = paths.await else { return };
                    for path in paths {
                        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                        let path = path.to_string_lossy().to_string();
                        if (accept.is_empty() || accept.contains(&ext)) && !picked.contains(&path) && picked.len() < most {
                            picked.push(path);
                        }
                    }
                    let _ = this.update(cx, |card, cx| card.put(&place, json!(picked), cx));
                })
                .detach();
            })));
        }
        for (n, path) in picked.iter().enumerate() {
            let (place, mut rest) = (place.clone(), picked.clone());
            rest.remove(n);
            let thumb = if is_picture(path) { self.picture(path, ObjectFit::Cover, p, cx) } else { div().size_full().flex().items_center().justify_center().child(Icon::new(Lucide::FileText).small().text_color(p.muted)).into_any_element() };
            line = line.child(
                div().size(px(36.)).relative().rounded(px(6.)).overflow_hidden().border_1().border_color(p.border).bg(p.surface).child(thumb).child(
                    div().absolute().top(px(1.)).right(px(1.)).rounded_full().bg(p.background.opacity(0.9)).child(
                        Button::new(SharedString::from(format!("{id}-remove-{n}")))
                            .xsmall()
                            .ghost()
                            .icon(gpui_kit::component::IconName::Close)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |card, _, _, cx| card.put(&place, json!(rest), cx))),
                    ),
                ),
            );
        }
        line.child(div().text_sm().text_color(p.muted).child(format!("{}/{most}", picked.len()))).into_any_element()
    }

    /// The card's steps as tabs: the one shown underlined, the ones done ticked and, with
    /// every one reached so far, a click away.
    fn steps(&mut self, id: &SharedString, props: &Props, place: Vec<String>, locals: &Locals, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.state.scope(locals);
        let listed = match props.get("items") {
            Some(Value::String(named)) => scope.value_at(named),
            Some(list) => list.clone(),
            None => Value::Null,
        };
        let items: Vec<(String, String)> = listed.as_array().into_iter().flatten().map(|i| (i.get("id").map(expr::text_of).unwrap_or_default(), i.get("label").map(expr::text_of).unwrap_or_default())).collect();
        // Nothing picked: the step the manifest says (where the card has got to).
        let shown = Some(expr::text_of(expr::read(&self.state.root, &place))).filter(|s| !s.is_empty()).unwrap_or_else(|| props.get("default").map(|d| expr::text_of(&scope.fill(d))).unwrap_or_default());
        let position = |id: &str| items.iter().position(|(i, _)| i == id);
        // How far the card has got: a step's id, or how many are done; else the one shown.
        let reached = match props.get("reached").and_then(Value::as_str).map(|r| scope.value_at(r)) {
            Some(Value::String(step)) => position(&step).unwrap_or(0),
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
            _ => position(&shown).unwrap_or(0),
        };
        let mut row = div().flex().border_b_1().border_color(p.border);
        for (n, (step, label)) in items.iter().enumerate() {
            let (current, done) = (*step == shown, n < reached);
            let badge = div().size(px(20.)).flex_none().rounded_full().flex().items_center().justify_center().text_xs();
            let badge = if done && !current {
                badge.border_1().border_color(p.success).child(Icon::new(Lucide::Check).xsmall().text_color(p.success))
            } else if current {
                badge.bg(p.accent).text_color(gpui_kit::white()).child((n + 1).to_string())
            } else {
                badge.border_1().border_color(p.muted.opacity(0.7)).text_color(p.muted).child((n + 1).to_string())
            };
            let (place, step) = (place.clone(), step.clone());
            row = row.child(
                div()
                    .id(SharedString::from(format!("{id}-{step}")))
                    .flex_1()
                    .min_w_0()
                    .py_2()
                    // The underline sits on the row's own line.
                    .mb(px(-1.))
                    .border_b_2()
                    .border_color(if current { p.accent } else { gpui_kit::transparent_black() })
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(badge)
                    .child(div().truncate().text_sm().when(current, |d| d.font_weight(FontWeight::SEMIBOLD)).text_color(if current { p.text } else { p.muted }).child(label.clone()))
                    .when(n <= reached && !current, |d| d.cursor_pointer().hover(move |d| d.bg(p.border.opacity(0.3))).on_click(cx.listener(move |card, _, _, cx| card.put(&place, json!(step), cx))))
                    .test_support(),
            );
        }
        row.into_any_element()
    }
}

impl Render for CardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let manifest = self.manifest.clone();
        let root = manifest.row.as_ref().filter(|_| self.state.scope(&[]).test("ui.folded")).unwrap_or(&manifest.card);
        let mut locals = Locals::new();
        let body = self.nodes(root, "card", &mut locals, 0, window, cx);
        let p = palette(cx);
        let card = div().id(ElementId::from(("card", cx.entity_id()))).relative().flex().flex_col().rounded(px(14.)).border_1().border_color(p.border.opacity(0.8)).bg(p.background).shadow_sm().overflow_hidden().children(body);
        if !self.measures {
            return card;
        }
        // The card's width, for `card.width` and `responsive`: known once it's laid out, so a
        // change is drawn on the frame after.
        let (known, me) = (self.width.clone(), cx.entity().downgrade());
        card.child(
            canvas(
                move |bounds, window, cx| {
                    let width = f32::from(bounds.size.width);
                    if (known.get() - width).abs() >= 1. {
                        known.set(width);
                        let me = me.clone();
                        window.defer(cx, move |_, cx| {
                            let _ = me.update(cx, |card, cx| {
                                card.state.set_width(width);
                                cx.notify();
                            });
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }
}

#[cfg(test)]
mod tests;
