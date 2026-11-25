use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, AppContext, Entity, TestAppContext, WindowBounds, WindowOptions, px, size};
use serde_json::{Value, json};
use wundur_core::cards::{Effect, Failure, Manifest};

use super::{CardEvent, CardView};

struct Shown {
    window: AnyWindowHandle,
    card: Entity<CardView>,
    effects: Rc<RefCell<Vec<Effect>>>,
}

fn show(manifest: Value, width: f32, cx: &mut TestAppContext) -> Shown {
    cx.update(gpui_kit::init);
    let manifest = Arc::new(Manifest::read(&manifest).unwrap());
    assert!(manifest.check().is_empty(), "{:?}", manifest.check());
    let effects = Rc::new(RefCell::new(Vec::new()));
    let seen = effects.clone();
    let (window, card) = cx.update(|cx| {
        let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds { origin: Default::default(), size: size(px(width), px(600.)) })), ..Default::default() };
        let (window, card) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|cx| CardView::new(manifest, "", cx))).unwrap();
        cx.subscribe(&card, move |_, event: &CardEvent, _| {
            if let CardEvent::Effect(effect) = event {
                seen.borrow_mut().push(effect.clone());
            }
        })
        .detach();
        (window.into(), card)
    });
    let shown = Shown { window, card, effects };
    shown.frame(cx);
    shown
}

impl Shown {
    fn frame(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
    }

    fn click(&self, id: &'static str, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.click(id, cx)).unwrap();
        cx.run_until_parked();
        self.frame(cx);
    }

    fn has(&self, id: &'static str, cx: &mut TestAppContext) -> bool {
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some()).unwrap()
    }

    fn at(&self, path: &str, cx: &mut TestAppContext) -> Value {
        cx.update(|cx| self.card.read(cx).state().scope(&[]).value_at(path))
    }
}

fn sound() -> Value {
    json!({
        "id": "sound",
        "form": { "prompt": "", "loop": false, "quality": "medium" },
        "ui": { "step": "edit" },
        "actions": {
            "make": [
                { "check": "form.prompt", "message": "Say what the sound is first." },
                { "call": { "path": "/v1/sounds", "body": { "prompt": "{form.prompt}", "loop": "{form.loop}" }, "spends": true, "working": "Making it" } },
                { "set": { "ui.step": "done" } },
            ],
        },
        "card": { "type": "column", "padding": 12, "gap": 8, "children": [
            { "type": "input", "id": "prompt", "bind": "form.prompt", "placeholder": "The sound", "when": "ui.step == 'edit'" },
            { "type": "segmented", "id": "quality", "bind": "form.quality", "options": ["low", "medium", "high"], "capitalize": true },
            { "type": "toggle", "id": "loop", "bind": "form.loop", "label": "Loop" },
            { "type": "text", "id": "error", "text": "{card.error}", "color": "danger", "when": "card.error" },
            { "type": "row", "gap": 8, "children": [
                { "type": "button", "id": "make", "label": "Make", "style": "primary", "click": "make", "loading": "card.busy", "when": "ui.step == 'edit'" },
                { "type": "button", "id": "again", "label": "Make another", "click": [{ "reset": "form" }, { "set": { "ui.step": "edit" } }], "when": "ui.step == 'done'" },
            ] },
        ] },
    })
}

#[gpui_kit::test]
async fn a_click_runs_its_action_and_the_owner_s_answer_finishes_it(cx: &mut TestAppContext) {
    let shown = show(sound(), 520., cx);
    assert!(!shown.has("error", cx));

    // Nothing typed: the check stops it, and the card says why.
    shown.click("make", cx);
    assert!(shown.effects.borrow().is_empty());
    assert!(shown.has("error", cx));
    assert_eq!(shown.at("card.error", cx), "Say what the sound is first.");

    // Typed into the box: the form has it, and Make asks the service.
    shown.click("prompt", cx);
    cx.update_window(shown.window, |_, window, cx| window.input("rain on a tin roof", cx)).unwrap();
    cx.run_until_parked();
    assert_eq!(shown.at("form.prompt", cx), "rain on a tin roof");
    shown.click("loop", cx);
    shown.click("make", cx);
    let effects = shown.effects.borrow().clone();
    let [Effect::Call { id, request }] = effects.as_slice() else { panic!("{effects:?}") };
    assert_eq!(request.body, Some(json!({ "prompt": "rain on a tin roof", "loop": true })));
    assert_eq!(shown.at("card.busy", cx), true);
    assert!(!shown.has("error", cx), "the next action clears the error");

    // The answer: the rest of the action runs, and the card moves on.
    let id = *id;
    cx.update(|cx| shown.card.update(cx, |card, cx| card.finish(id, Ok(json!({ "id": "snd_1" })), cx)));
    shown.frame(cx);
    assert_eq!(shown.at("ui.step", cx), "done");
    assert!(shown.has("again", cx) && !shown.has("make", cx) && !shown.has("prompt", cx));

    // Make another: the form is as it started, and so is its box.
    shown.click("again", cx);
    assert_eq!(shown.at("form.prompt", cx), "");
    assert_eq!(shown.at("form.loop", cx), false);
    assert!(shown.has("make", cx));
}

#[gpui_kit::test]
async fn a_refused_call_is_shown_and_the_card_stays(cx: &mut TestAppContext) {
    let shown = show(sound(), 520., cx);
    cx.update(|cx| shown.card.update(cx, |card, cx| card.update_state(cx, |s| s.write(&["form".into(), "prompt".into()], json!("rain")))));
    shown.frame(cx);
    shown.click("make", cx);
    let id = match shown.effects.borrow().as_slice() {
        [Effect::Call { id, .. }] => *id,
        other => panic!("{other:?}"),
    };
    cx.update(|cx| shown.card.update(cx, |card, cx| card.finish(id, Err(Failure { message: "Out of seconds.".into(), code: "quota".into() }), cx)));
    shown.frame(cx);
    assert_eq!(shown.at("card.error", cx), "Out of seconds.");
    assert_eq!(shown.at("ui.step", cx), "edit");
    assert!(shown.has("make", cx));
}

#[gpui_kit::test]
async fn choices_write_what_they_re_bound_to(cx: &mut TestAppContext) {
    let shown = show(sound(), 520., cx);
    assert_eq!(shown.at("form.quality", cx), "medium");
    shown.click("quality-2", cx);
    assert_eq!(shown.at("form.quality", cx), "high");
    shown.click("loop", cx);
    assert_eq!(shown.at("form.loop", cx), true);
    shown.click("loop", cx);
    assert_eq!(shown.at("form.loop", cx), false);
}

#[gpui_kit::test]
async fn a_node_repeats_for_each_item_and_its_click_knows_which(cx: &mut TestAppContext) {
    let shown = show(
        json!({
            "id": "versions",
            "ui": { "set": "s1", "step": "views" },
            "actions": { "show": [{ "set": { "ui.set": "{set.id}" } }] },
            "parts": { "tile": { "type": "column", "id": "tile", "click": "show", "selected": "ui.set == set.id", "border": true, "padding": 6, "children": [{ "type": "text", "text": "{set.label}" }] } },
            "card": { "type": "column", "children": [
                { "type": "steps", "id": "steps", "bind": "ui.step", "items": [{ "id": "brief", "label": "Brief" }, { "id": "views", "label": "Views" }, { "id": "model", "label": "3D model" }], "reached": "data.reached" },
                { "type": "row", "gap": 8, "each": "data.sets", "as": "set", "children": [{ "type": "use", "part": "tile" }] },
                { "type": "text", "id": "none", "text": "No versions yet", "when": "!data.sets" },
            ] },
        }),
        520.,
        cx,
    );
    assert!(shown.has("none", cx));
    assert!(!shown.has("tile:0", cx));
    cx.update(|cx| shown.card.update(cx, |card, cx| card.update_state(cx, |s| s.put("data", json!({ "reached": "views", "sets": [{ "id": "s1", "label": "v1" }, { "id": "s2", "label": "v2" }] })))));
    shown.frame(cx);
    assert!(!shown.has("none", cx));
    assert!(shown.has("tile:0", cx) && shown.has("tile:1", cx) && !shown.has("tile:2", cx));
    shown.click("tile:1", cx);
    assert_eq!(shown.at("ui.set", cx), "s2", "the click knew its own item");

    // The steps: one reached is a click away; one not reached isn't.
    shown.click("steps-brief", cx);
    assert_eq!(shown.at("ui.step", cx), "brief");
    shown.click("steps-model", cx);
    assert_eq!(shown.at("ui.step", cx), "brief", "not reached yet");
}

#[gpui_kit::test]
async fn responsive_rules_follow_the_card_s_width(cx: &mut TestAppContext) {
    let manifest = json!({
        "id": "wide",
        "card": { "type": "column", "children": [
            { "type": "text", "id": "wide", "text": "Wide", "when": "card.width >= 500" },
            { "type": "text", "id": "narrow", "text": "Narrow", "when": "card.width < 500" },
        ] },
    });
    let wide = show(manifest.clone(), 700., cx);
    wide.frame(cx);
    assert!(wide.has("wide", cx) && !wide.has("narrow", cx));
    let narrow = show(manifest, 360., cx);
    narrow.frame(cx);
    assert!(narrow.has("narrow", cx) && !narrow.has("wide", cx));
}

/// A layout nested as deep as a real card's (the 3D asset's is about a dozen boxes deep) is
/// drawn on a thread with a small stack, as a test's is.
#[gpui_kit::test]
async fn a_deeply_nested_layout_is_drawn(cx: &mut TestAppContext) {
    let mut node = json!({ "type": "text", "id": "leaf", "text": "At the bottom" });
    for n in 0..20 {
        node = json!({ "type": if n % 2 == 0 { "column" } else { "row" }, "padding": 1, "let": { "depth": "ui.depth" }, "children": [node] });
    }
    let shown = show(json!({ "id": "deep", "ui": { "depth": 20 }, "card": node }), 520., cx);
    assert!(shown.has("leaf", cx));
}
