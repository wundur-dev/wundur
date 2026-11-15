//! A hosted asset's card in the chat (see `hosted_store` and `wundur_core::asset_flow`), and the
//! form that starts one when the person is signed in to a Wundur account. Each step is the
//! person's: describe it (a kind from the catalog and its fields), approve a set of views or ask
//! for a change, pick an engine and look at the preview, reroll, then Import, which is the only
//! step that spends model credits.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use gpui_kit::component::Disableable;
use gpui_kit::component::input::{Input, InputState};
use serde_json::Value;
use wundur_core::asset_flow::{Catalog, Field, Kind, Model};

use super::*;
use crate::hosted_store;

/// The form for a new hosted asset: the kind picked, its name and description, and the kind's
/// fields (text and numbers in inputs, choices and switches as values).
pub(super) struct HostedForm {
    pub kind: String,
    pub name: Entity<InputState>,
    pub description: Entity<InputState>,
    pub texts: HashMap<String, Entity<InputState>>,
    pub choices: HashMap<String, String>,
    pub flags: HashMap<String, bool>,
    pub error: Option<String>,
}

/// A value for `field` as the form starts with it.
fn default_choice(field: &Field) -> String {
    match &field.default {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => field.options.first().cloned().unwrap_or_default(),
    }
}

impl ChatPanel {
    /// Before drawing: the new-asset form's fields, for a person signed in (made with the
    /// window; a kind's own fields once the catalog says what they are).
    pub(super) fn ensure_hosted_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.session.read(cx).chat.items.iter().any(|i| matches!(i, Item::NewAsset)) && hosted_store::signed_in();
        if !wanted {
            self.hosted_form = None;
            return;
        }
        let (catalog, _) = hosted_store::catalog(cx);
        if self.hosted_form.is_none() {
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name, e.g. Goblin scout"));
            let description = cx.new(|cx| InputState::new(window, cx).placeholder("What it is, e.g. a wiry goblin in leather armour with a short bow"));
            self.hosted_form = Some(HostedForm {
                kind: String::new(),
                name,
                description,
                texts: HashMap::new(),
                choices: HashMap::new(),
                flags: HashMap::new(),
                error: None,
            });
        }
        let Some(catalog) = catalog else { return };
        let Some(form) = &mut self.hosted_form else { return };
        if form.kind.is_empty()
            && let Some(k) = catalog.kinds.first()
        {
            form.kind = k.id.clone();
        }
        let Some(kind) = catalog.kind(&form.kind).cloned() else { return };
        for f in &kind.fields {
            match f.kind.as_str() {
                "enum" => {
                    form.choices.entry(f.id.clone()).or_insert_with(|| default_choice(f));
                }
                "bool" => {
                    form.flags.entry(f.id.clone()).or_insert(matches!(f.default, Some(Value::Bool(true))));
                }
                _ => {
                    if !form.texts.contains_key(&f.id) {
                        let (label, start) = (f.label.clone(), f.default.as_ref().filter(|v| !v.is_null()).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())));
                        let input = cx.new(|cx| {
                            let mut i = InputState::new(window, cx).placeholder(label);
                            if let Some(v) = start {
                                i.set_value(v, window, cx);
                            }
                            i
                        });
                        form.texts.insert(f.id.clone(), input);
                    }
                }
            }
        }
    }

    /// The form for a new hosted asset.
    pub(super) fn render_hosted_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let Some(form) = &self.hosted_form else { return div().into_any_element() };
        let (catalog, _) = hosted_store::catalog(cx);
        let Some(catalog) = catalog else {
            let body = match hosted_store::catalog_error(cx) {
                Some(e) => div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().text_xs().text_color(danger).child(e))
                    .child(Button::new("hosted-catalog-retry").small().ghost().label("Try again").on_click(|_, _, cx| hosted_store::reload_catalog(cx)))
                    .into_any_element(),
                None => div().text_xs().text_color(muted).child("Asking Wundur what it can make…").into_any_element(),
            };
            return self.asset_frame("new-asset".into(), "New asset".into(), None, None, None, body, cx);
        };
        let mut kinds = div().flex().flex_wrap().gap_1();
        for k in &catalog.kinds {
            let (on, id) = (form.kind == k.id, k.id.clone());
            kinds = kinds.child(
                Button::new(SharedString::from(format!("hosted-kind-{}", k.id)))
                    .xsmall()
                    .label(k.label.clone())
                    .tooltip(SharedString::from(k.description.clone()))
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.ghost())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(f) = &mut p.hosted_form {
                            f.kind = id.clone();
                        }
                        cx.notify();
                    })),
            );
        }
        let kind = catalog.kind(&form.kind);
        let mut fields = div().flex().flex_col().gap_2();
        if let Some(kind) = kind {
            for f in &kind.fields {
                fields = fields.child(self.hosted_field(form, f, cx));
            }
        }
        let note = kind.map(|k| if k.supports_3d { "Uses one image; a 3D model comes after, from your model credits." } else { "Uses one image. 3D models aren't available for this kind yet." });
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(kinds)
            .children(kind.filter(|k| !k.description.is_empty()).map(|k| div().text_xs().text_color(muted).child(k.description.clone())))
            .child(Input::new(&form.name))
            .child(Input::new(&form.description))
            .child(fields)
            .children(form.error.clone().map(|e| div().text_xs().text_color(danger).child(e)))
            .child(
                div()
                    .pt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(note.map(|n| div().text_xs().text_color(muted).child(n)))
                    .child(div().flex_1())
                    .child(Button::new("new-asset-cancel").small().ghost().label("Cancel").on_click(cx.listener(|p, _, _, cx| p.close_asset_form(cx, None))))
                    .child(Button::new("new-asset-create").small().primary().label("Make the views").on_click(cx.listener(|p, _, _, cx| p.create_hosted(cx)))),
            )
            .into_any_element();
        self.asset_frame("new-asset".into(), "New asset".into(), None, None, None, body, cx)
    }

    /// One field of the form: its label and its control.
    fn hosted_field(&self, form: &HostedForm, f: &Field, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let control: AnyElement = match f.kind.as_str() {
            "enum" => {
                let current = form.choices.get(&f.id).cloned().unwrap_or_default();
                let mut row = div().flex().flex_wrap().gap_1();
                for o in &f.options {
                    let (on, field, value) = (*o == current, f.id.clone(), o.clone());
                    row = row.child(
                        Button::new(SharedString::from(format!("hosted-field-{}-{o}", f.id)))
                            .xsmall()
                            .label(o.clone())
                            .when(on, |b| b.outline())
                            .when(!on, |b| b.ghost())
                            .on_click(cx.listener(move |p, _, _, cx| {
                                if let Some(form) = &mut p.hosted_form {
                                    form.choices.insert(field.clone(), value.clone());
                                }
                                cx.notify();
                            })),
                    );
                }
                row.into_any_element()
            }
            "bool" => {
                let (on, field) = (form.flags.get(&f.id).copied().unwrap_or(false), f.id.clone());
                Button::new(SharedString::from(format!("hosted-field-{}", f.id)))
                    .xsmall()
                    .label(if on { "Yes" } else { "No" })
                    .when(on, |b| b.outline())
                    .when(!on, |b| b.ghost())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(form) = &mut p.hosted_form {
                            let v = form.flags.entry(field.clone()).or_insert(false);
                            *v = !*v;
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            _ => match form.texts.get(&f.id) {
                Some(input) => Input::new(input).into_any_element(),
                None => div().into_any_element(),
            },
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().w(px(110.)).flex_none().text_xs().text_color(muted).child(f.label.clone()))
            .child(div().flex_1().min_w_0().child(control))
            .when(!f.help.is_empty(), |d| d.child(div().text_xs().text_color(muted).child(f.help.clone())))
            .into_any_element()
    }

    /// Start the hosted asset the form describes: its folder, then its first set of views.
    fn create_hosted(&mut self, cx: &mut Context<Self>) {
        let (catalog, _) = hosted_store::catalog(cx);
        let Some(form) = &self.hosted_form else { return };
        let Some(kind) = catalog.as_ref().and_then(|c| c.kind(&form.kind)).cloned() else { return };
        let (name, description) = (form.name.read(cx).value().to_string(), form.description.read(cx).value().to_string());
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        for f in &kind.fields {
            let value = match f.kind.as_str() {
                "enum" => form.choices.get(&f.id).map(|v| Value::String(v.clone())),
                "bool" => form.flags.get(&f.id).map(|v| Value::Bool(*v)),
                "number" => form.texts.get(&f.id).map(|i| i.read(cx).value().trim().to_string()).filter(|t| !t.is_empty()).and_then(|t| t.parse::<f64>().ok()).map(|n| serde_json::json!(n)),
                _ => form.texts.get(&f.id).map(|i| i.read(cx).value().trim().to_string()).filter(|t| !t.is_empty()).map(Value::String),
            };
            match value {
                Some(v) => {
                    fields.insert(f.id.clone(), v);
                }
                None if f.required => {
                    if let Some(form) = &mut self.hosted_form {
                        form.error = Some(format!("{} is needed.", f.label));
                    }
                    cx.notify();
                    return;
                }
                None => {}
            }
        }
        let (project, origin) = {
            let s = self.session.read(cx);
            let chat = &s.chat;
            let turn = chat.items.iter().rposition(|i| matches!(i, Item::User(_))).and_then(|from| wundur_core::summary::TurnText::key_at(chat, from));
            (s.cwd().to_path_buf(), chat.session_id.clone().map(|session| wundur_core::assets::Origin { session, turn }))
        };
        match hosted_store::create(&project, &kind.id, &name, &description, fields, origin, cx) {
            Ok(dir) => {
                self.hosted_form = None;
                self.close_asset_form(cx, Some(dir));
            }
            Err(e) => {
                if let Some(f) = &mut self.hosted_form {
                    f.error = Some(e);
                }
                cx.notify();
            }
        }
    }

    /// A hosted asset's card: where it has got, and the person's next step.
    pub(super) fn render_hosted_card(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let key = dir.to_string_lossy().to_string();
        let id = |what: &str| SharedString::from(format!("hosted-{what}-{key}"));
        let Some(e) = hosted_store::entry(dir, cx) else {
            let muted = cx.theme().muted_foreground;
            let body = div().text_xs().text_color(muted).child(dir.display().to_string()).into_any_element();
            return self.asset_frame(id("card"), "Asset not found".into(), None, None, None, body, cx);
        };
        let (catalog, plan) = hosted_store::catalog(cx);
        let kind = catalog.as_ref().and_then(|c| c.kind(&e.record.kind)).cloned();
        let kind_label = kind.as_ref().map(|k| k.label.clone()).unwrap_or_else(|| e.record.kind.clone());
        let mut body = div().flex().flex_col().gap_3();
        if let Some((text, upgrade)) = e.error.clone() {
            body = body.child(self.hosted_error(&id("error"), text, upgrade, cx));
        }
        body = body.child(match (&e.asset, e.record.imported.is_some()) {
            (_, true) => self.hosted_imported(dir, &e, &id, cx),
            (None, false) => self.hosted_waiting(e.working.unwrap_or("Loading"), kind.as_ref().map_or(3, |k| k.views.max(1)), cx),
            (Some(asset), false) => match asset.approved_set() {
                None => self.hosted_views(dir, &e, kind.as_ref(), &id, cx),
                Some(_) if kind.as_ref().is_some_and(|k| !k.supports_3d) => self.hosted_no_3d(dir, &e, kind.as_ref(), cx),
                Some(_) => self.hosted_model(dir, &e, catalog.as_ref(), kind.as_ref(), plan.as_deref(), &id, cx),
            },
        });
        self.asset_frame(id("card"), e.record.name.clone(), Some(kind_label), None, None, body.into_any_element(), cx)
    }

    fn hosted_error(&self, id: &SharedString, text: String, upgrade: bool, cx: &mut Context<Self>) -> AnyElement {
        let danger = cx.theme().danger;
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().text_xs().text_color(danger).child(text))
            .when(upgrade, |d| {
                d.child(Button::new(SharedString::from(format!("{id}-upgrade"))).xsmall().outline().label("Upgrade").on_click(|_, _, cx| crate::shell::open_settings_page("account", cx)))
            })
            .into_any_element()
    }

    /// Placeholders while the first answer or a set is on its way.
    fn hosted_waiting(&self, what: &str, views: u32, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut row = div().flex().gap_2();
        for _ in 0..views {
            row = row.child(self.asset_making(96., None, cx));
        }
        div().flex().flex_col().gap_2().child(row).child(div().text_xs().text_color(muted).child(format!("{what}…"))).into_any_element()
    }

    /// A set's views, from the asset's folder, labelled.
    fn hosted_view_row(&self, dir: &Path, files: &[String], labels: &[String], height: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, ground) = (theme.muted_foreground, theme.secondary);
        let mut row = div().flex().flex_wrap().gap_2();
        for (n, file) in files.iter().enumerate() {
            let path = dir.join(file);
            let full = path.clone();
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id(SharedString::from(format!("hosted-view-{}", path.display())))
                            .h(px(height))
                            .w(px(height * 0.6))
                            .rounded(px(10.))
                            .overflow_hidden()
                            .bg(ground)
                            .cursor_pointer()
                            .child(img(path).size_full().object_fit(ObjectFit::Contain))
                            .on_click(move |_, _, cx| crate::image_view::show(full.clone(), cx)),
                    )
                    .child(div().text_xs().text_color(muted).child(labels.get(n).cloned().unwrap_or_default())),
            );
        }
        row.into_any_element()
    }

    /// The views step: the latest set, with Approve, a change to the whole set, or a fresh start.
    fn hosted_views(&self, dir: &Path, e: &hosted_store::Entry, kind: Option<&Kind>, id: &dyn Fn(&str) -> SharedString, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(asset) = &e.asset else { return div().into_any_element() };
        let Some(set) = asset.latest_set().cloned() else {
            return self.hosted_waiting(e.working.unwrap_or("Making the views"), kind.map_or(3, |k| k.views.max(1)), cx);
        };
        let busy = e.working.is_some() || set.is_generating();
        let mut col = div().flex().flex_col().gap_2();
        if set.is_generating() || (set.is_ready() && !e.record.views.contains_key(&set.id)) {
            return self.hosted_waiting(e.working.unwrap_or("Making the views"), (set.views.len() as u32).max(kind.map_or(3, |k| k.views.max(1))), cx);
        }
        if set.is_ready() {
            let labels: Vec<String> = set.views.iter().map(|v| v.label.clone()).collect();
            col = col.child(self.hosted_view_row(dir, &e.record.views[&set.id], &labels, 150., cx));
        } else {
            col = col.child(div().text_xs().text_color(muted).child(set.error.as_ref().map_or("These views didn't come out; start again.".to_string(), |er| er.message.clone())));
        }
        if let Some(note) = self.asset_notes.get(dir) {
            let (d1, n) = (dir.to_path_buf(), note.clone());
            let change = move |window: &mut Window, cx: &mut App| {
                let text = n.read(cx).value().trim().to_string();
                if !text.is_empty() {
                    n.update(cx, |i, cx| i.set_value("", window, cx));
                    hosted_store::change(&d1, &text, cx);
                }
            };
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(note)))
                    .child(Button::new(id("change")).small().outline().label("Change").disabled(busy || !set.is_ready()).on_click(move |_, window, cx| change(window, cx))),
            );
        }
        let (d2, d3, set_id) = (dir.to_path_buf(), dir.to_path_buf(), set.id.clone());
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(Button::new(id("retry")).small().ghost().icon(gpui_kit::assets::IconName::RefreshCw).label("Start again").disabled(busy).on_click(move |_, _, cx| hosted_store::retry(&d2, cx)))
                .child(div().text_xs().text_color(muted).child("A change or a fresh start uses one image."))
                .child(div().flex_1())
                .child(Button::new(id("approve")).small().primary().label("Approve these").disabled(busy || !set.is_ready()).on_click(move |_, _, cx| hosted_store::approve(&d3, &set_id, cx))),
        );
        col.into_any_element()
    }

    /// A kind with no 3D engines yet: its approved views are what it makes.
    fn hosted_no_3d(&self, dir: &Path, e: &hosted_store::Entry, kind: Option<&Kind>, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(set) = e.asset.as_ref().and_then(|a| a.approved_set()).cloned() else { return div().into_any_element() };
        let labels: Vec<String> = set.views.iter().map(|v| v.label.clone()).collect();
        let files = e.record.views.get(&set.id).cloned().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.hosted_view_row(dir, &files, &labels, 150., cx))
            .child(div().text_xs().text_color(muted).child(format!(
                "Saved in the asset's folder. 3D models aren't available for {} yet; they'll appear here when they are.",
                kind.map_or("this kind".to_string(), |k| k.label.to_lowercase())
            )))
            .into_any_element()
    }

    /// The model step: the engine picker, then the attempt (being made, or its preview with
    /// Import, Reroll and Discard).
    #[allow(clippy::too_many_arguments)]
    fn hosted_model(
        &self,
        dir: &Path,
        e: &hosted_store::Entry,
        catalog: Option<&Catalog>,
        kind: Option<&Kind>,
        plan: Option<&str>,
        id: &dyn Fn(&str) -> SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, fg, ring, border, yellow, danger) = (theme.muted_foreground, theme.foreground, theme.ring, theme.border, theme.yellow, theme.danger);
        let Some(asset) = &e.asset else { return div().into_any_element() };
        let Some(set) = asset.approved_set().cloned() else { return div().into_any_element() };
        let labels: Vec<String> = set.views.iter().map(|v| v.label.clone()).collect();
        let files = e.record.views.get(&set.id).cloned().unwrap_or_default();
        let attempt: Option<Model> = asset.models.iter().rfind(|m| m.set == set.id).cloned();
        let attempts_left = attempt.as_ref().and_then(|m| m.attempts_left).or(set.attempts_left);
        let busy = e.working.is_some() || attempt.as_ref().is_some_and(Model::is_running);
        let mut col = div().flex().flex_col().gap_3().child(self.hosted_view_row(dir, &files, &labels, 90., cx));
        let engines = match (catalog, kind) {
            (Some(c), Some(k)) => c.engines_for(k).into_iter().cloned().collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let picked = e.engine.clone().or_else(|| kind.map(|k| k.default_engine.clone())).unwrap_or_default();
        let plan = plan.unwrap_or("free");
        let picker = {
            let mut list = div().flex().flex_col().gap_1();
            for en in &engines {
                let (on, allowed, folder, eid) = (en.id == picked, en.on_plan(plan), dir.to_path_buf(), en.id.clone());
                let credits = format!("{} credit{}", en.credits, if en.credits == 1 { "" } else { "s" });
                list = list.child(
                    div()
                        .id(SharedString::from(format!("hosted-engine-{}-{}", en.id, dir.display())))
                        .px_2()
                        .py(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(if on { ring } else { border })
                        .when(on, |d| d.bg(ring.opacity(0.06)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(allowed, |d| d.cursor_pointer().on_click(move |_, _, cx| hosted_store::pick_engine(&folder, &eid, cx)))
                        .when(!allowed, |d| d.opacity(0.55))
                        .child(div().text_sm().text_color(fg).child(en.label.clone()))
                        .children((!en.license.label.is_empty()).then(|| div().text_xs().text_color(yellow).child(en.license.label.clone())))
                        .child(div().flex_1())
                        .child(div().text_xs().text_color(muted).child(if allowed { credits } else { format!("{credits} · Monthly plan") })),
                );
            }
            let help_key = format!("hosted-help:{}", dir.display());
            let open = self.toggled.contains(&help_key);
            let toggle = help_key.clone();
            list = list.child(
                div()
                    .id(SharedString::from(help_key.clone()))
                    .text_xs()
                    .text_color(muted)
                    .cursor_pointer()
                    .child(if open { "Which should I use? ▾" } else { "Which should I use? ▸" })
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.toggled.remove(&toggle) {
                            p.toggled.insert(toggle.clone());
                        }
                        cx.notify();
                    })),
            );
            if open {
                for en in &engines {
                    let best = if en.best_for.is_empty() { String::new() } else { format!(" Best for {}.", en.best_for.join(", ")) };
                    list = list.child(div().text_xs().text_color(muted).child(format!("{}: {}{best}", en.label, en.help)));
                }
            }
            list
        };
        match attempt {
            Some(m) if m.is_running() => {
                let seconds = catalog.and_then(|c| c.engine(&m.engine)).map_or(60, |en| en.seconds);
                let about = if seconds >= 120 { format!("usually about {} minutes", seconds / 60) } else { "usually about a minute".to_string() };
                let progress = m.progress.map(|p| format!(" · {}%", (p * 100.0).round() as u32)).unwrap_or_default();
                col = col.child(div().flex().items_center().gap_2().child(gpui_kit::component::spinner::Spinner::new().color(muted)).child(div().text_xs().text_color(muted).child(format!("Making the model, {about}{progress}…"))));
            }
            Some(m) if m.succeeded() => {
                let credits = catalog.and_then(|c| c.engine(&m.engine)).map_or(1, |en| en.credits);
                let (d1, d2, d3, d4, mid1, mid2, mid3, mid4) = (dir.to_path_buf(), dir.to_path_buf(), dir.to_path_buf(), dir.to_path_buf(), m.id.clone(), m.id.clone(), m.id.clone(), m.id.clone());
                let me = cx.entity().downgrade();
                let rerolls = attempts_left.unwrap_or(0);
                col = col
                    .child(div().text_sm().text_color(fg).child("The model is ready to look at."))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Button::new(id("preview")).small().outline().icon(gpui_kit::assets::IconName::Box).label("Open 3D preview").on_click(move |_, _, cx| {
                                let me = me.clone();
                                hosted_store::preview(&d1, &mid1, cx, move |page, cx| {
                                    let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(wundur_core::web::file_url(&page))));
                                });
                            }))
                            .child(div().flex_1())
                            .child(Button::new(id("discard")).small().ghost().label("Discard").disabled(busy).on_click(move |_, _, cx| hosted_store::reject(&d2, &mid2, cx)))
                            .child(
                                Button::new(id("reroll"))
                                    .small()
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::RefreshCw)
                                    .label(format!("Reroll ({rerolls} left)"))
                                    .disabled(busy || rerolls == 0)
                                    .on_click(move |_, _, cx| hosted_store::model(&d3, Some(mid3.clone()), cx)),
                            )
                            .child(
                                Button::new(id("import"))
                                    .small()
                                    .primary()
                                    .label(format!("Import ({credits} credit{})", if credits == 1 { "" } else { "s" }))
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| hosted_store::import(&d4, &mid4, cx)),
                            ),
                    )
                    .child(div().text_xs().text_color(muted).child(if rerolls == 0 {
                        "No rerolls left for these views: change the views to try again.".to_string()
                    } else {
                        "Rerolls are free; pick another engine below to reroll on it. Credits are spent only on Import.".to_string()
                    }))
                    .child(picker);
            }
            failed => {
                if let Some(m) = failed.filter(|m| m.status == "failed") {
                    col = col.child(div().text_xs().text_color(danger).child(m.error.map_or("That model didn't come out (not counted).".into(), |er| er.message)));
                }
                let d = dir.to_path_buf();
                col = col.child(picker).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_xs().text_color(muted).child("Previews are free; credits are spent only when you import."))
                        .child(div().flex_1())
                        .child(Button::new(id("model")).small().primary().icon(gpui_kit::assets::IconName::Sparkles).label("Make the model").disabled(busy || engines.is_empty()).on_click(move |_, _, cx| hosted_store::model(&d, None, cx))),
                );
            }
        }
        col.into_any_element()
    }

    /// Imported: the files in the project, and what can come next.
    fn hosted_imported(&self, dir: &Path, e: &hosted_store::Entry, id: &dyn Fn(&str) -> SharedString, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, green, fg) = (theme.muted_foreground, theme.green, theme.foreground);
        let model = e.record.files.iter().find(|f| f.ends_with(".glb")).cloned();
        let me = cx.entity().downgrade();
        let (project, name) = (e.project.clone(), e.record.name.clone());
        let folder: PathBuf = dir.to_path_buf();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().items_center().gap_2().child(Icon::new(IconName::Check).small().text_color(green)).child(div().text_sm().text_color(fg).child("Imported into the project.")))
            .children(e.record.files.iter().map(|f| div().text_xs().text_color(muted).child(f.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(model.map(|file| {
                        let path = folder.join(&file);
                        Button::new(id("open-model")).small().outline().icon(gpui_kit::assets::IconName::Box).label("Open model").on_click(move |_, _, cx| {
                            let me = me.clone();
                            crate::asset_store::viewer_page(&project, &path, &name, cx, move |page, cx| {
                                let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(wundur_core::web::file_url(&page))));
                            });
                        })
                    }))
                    .child(div().flex_1()),
            )
            .child(div().text_xs().text_color(muted).child("Rigging comes next: automatic on Wundur's server, or by hand with Mesh2Motion."))
            .into_any_element()
    }

    /// Test commands for this chat's latest hosted asset: `new <kind>|<name>|<description>`,
    /// `approve`, `change <text>`, `retry`, `engine <id>`, `model`, `reroll`, `import`, `state`.
    pub fn hosted_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        if cmd == "new" {
            self.start_asset_form(cx);
            self.ensure_hosted_form(window, cx);
            let mut parts = arg.splitn(3, '|');
            let (kind, name, description) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
            if let Some(f) = &mut self.hosted_form {
                f.kind = kind.to_string();
                let (n, d) = (f.name.clone(), f.description.clone());
                n.update(cx, |i, cx| i.set_value(name.to_string(), window, cx));
                d.update(cx, |i, cx| i.set_value(description.to_string(), window, cx));
            } else {
                return "no form (signed out?)".into();
            }
            self.ensure_hosted_form(window, cx);
            self.create_hosted(cx);
        }
        let Some(dir) = self.session.read(cx).chat.items.iter().rev().find_map(|i| match i {
            Item::Asset(d) if hosted_store::is_hosted(d) => Some(d.clone()),
            _ => None,
        }) else {
            return format!("no hosted asset in this chat{}", self.hosted_form.as_ref().and_then(|f| f.error.clone()).map(|e| format!(" ({e})")).unwrap_or_default());
        };
        let e = hosted_store::entry(&dir, cx);
        let asset = e.as_ref().and_then(|e| e.asset.clone());
        let last_model = asset.as_ref().and_then(|a| a.models.last().map(|m| m.id.clone()));
        match cmd {
            "new" | "state" => {}
            "approve" => {
                if let Some(set) = asset.as_ref().and_then(|a| a.latest_set()).map(|s| s.id.clone()) {
                    hosted_store::approve(&dir, &set, cx);
                }
            }
            "change" => hosted_store::change(&dir, arg, cx),
            "retry" => hosted_store::retry(&dir, cx),
            "engine" => hosted_store::pick_engine(&dir, arg, cx),
            "model" => hosted_store::model(&dir, None, cx),
            "reroll" => hosted_store::model(&dir, last_model.clone(), cx),
            "import" => {
                if let Some(m) = &last_model {
                    hosted_store::import(&dir, m, cx);
                }
            }
            _ => return format!("unknown hosted command: {cmd}"),
        }
        let Some(e) = hosted_store::entry(&dir, cx) else { return "gone".into() };
        let a = e.asset.as_ref();
        format!(
            "{}: id {:?}, sets {:?}, approved {:?}, models {:?}, saved views {}, imported {:?}, working {:?}, error {:?}",
            e.record.name,
            e.record.id,
            a.map(|a| a.sets.iter().map(|s| format!("{}:{}", s.id, s.status)).collect::<Vec<_>>()),
            a.and_then(|a| a.approved_set()).map(|s| s.id.clone()),
            a.map(|a| a.models.iter().map(|m| format!("{}:{}:{}", m.id, m.engine, m.status)).collect::<Vec<_>>()),
            e.record.views.len(),
            e.record.imported,
            e.working,
            e.error.map(|x| x.0),
        )
    }
}
