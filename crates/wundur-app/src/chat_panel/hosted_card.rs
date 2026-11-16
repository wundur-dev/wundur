//! A hosted asset's card in the chat (see `hosted_store` and `wundur_core::asset_flow`), and the
//! form that starts one. Four steps, each the person's: Describe (a kind from the catalog and
//! its fields), Views (sets of views as versions, refined or tried again, one approved), Model
//! (an engine, free attempts, a preview, then Import, the only step that spends model credits)
//! and Rig (coming). The card is two columns that stack when the chat is narrow: the work on the
//! left, what can be done to it on the right, and the step's main action at the foot.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Disableable;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use serde_json::Value;
use wundur_core::asset_flow::{Asset, Catalog, Field, Kind, Model, Set};

use super::*;
use crate::hosted_store;

const STEPS: [&str; 4] = ["Describe", "Views", "Model", "Rig"];

/// The form for a new hosted asset: the kind picked, its name and description, and the kind's
/// fields (text and numbers in inputs, choices and switches as values).
pub(super) struct HostedForm {
    pub kind: String,
    pub name: Entity<InputState>,
    pub description: Entity<TextareaState>,
    pub texts: HashMap<String, Entity<InputState>>,
    pub choices: HashMap<String, String>,
    pub flags: HashMap<String, bool>,
    pub error: Option<String>,
    /// Whether "Style & generation options" (the kind's fields) is open.
    pub options_open: bool,
    /// Pictures for the look, shape or style (at most three), sent with it.
    pub references: Vec<PathBuf>,
}

/// A value for `field` as the form starts with it.
fn default_choice(field: &Field) -> String {
    match &field.default {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => field.options.first().cloned().unwrap_or_default(),
    }
}

/// A paste into the new-asset form's boxes: pictures (an image, or image files) become
/// references; anything else is the box's as usual. True when it took the paste.
fn reference_paste(item: &ClipboardItem, panel: &WeakEntity<ChatPanel>, window: &mut Window, cx: &mut App) -> bool {
    let mut pictures: Vec<PathBuf> = Vec::new();
    for entry in item.entries() {
        match entry {
            ClipboardEntry::Image(image) => {
                let ext = match image.format {
                    ImageFormat::Jpeg => "jpg",
                    ImageFormat::Webp => "webp",
                    ImageFormat::Png => "png",
                    // The service takes PNG, JPEG and WebP only.
                    _ => continue,
                };
                if let Some(dir) = wundur_core::images::session_dir(Some("references"))
                    && let Ok(path) = wundur_core::images::store(&dir, &image.bytes, ext)
                {
                    pictures.push(path);
                }
            }
            // Files: only pictures are references; other files stay the box's paste.
            ClipboardEntry::ExternalPaths(paths) => pictures.extend(
                paths.paths().iter().filter(|p| matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "webp"))).cloned(),
            ),
            _ => {}
        }
    }
    if pictures.is_empty() {
        return false;
    }
    let panel = panel.clone();
    window.defer(cx, move |_, cx| {
        let _ = panel.update(cx, |p, cx| p.take_references(pictures, cx));
    });
    true
}

/// "1 credit", "3 credits".
fn credits(n: u32) -> String {
    format!("{n} credit{}", if n == 1 { "" } else { "s" })
}

/// One card, laid out: the header, the steps, the two columns and the foot.
struct Frame {
    id: SharedString,
    title: String,
    subtitle: String,
    /// The step shown (1–4) and the furthest the asset has got.
    shown: u8,
    reached: u8,
    dir: Option<PathBuf>,
    intro: String,
    left: AnyElement,
    right: Option<AnyElement>,
    error: Option<AnyElement>,
    back: Option<AnyElement>,
    status: String,
    primary: Option<AnyElement>,
}

/// Where the asset has got: 2 views, 3 model, 4 imported.
fn reached(e: &hosted_store::Entry, kind: Option<&Kind>) -> u8 {
    match &e.asset {
        _ if e.record.imported.is_some() => 4,
        Some(a) if a.approved_set().is_some() && kind.is_none_or(|k| k.supports_3d) => 3,
        _ => 2,
    }
}

/// The set shown on the Views step: the one picked from the versions, else the latest.
fn shown_set<'a>(e: &hosted_store::Entry, asset: &'a Asset) -> Option<&'a Set> {
    e.shown_set.as_ref().and_then(|id| asset.sets.iter().find(|s| &s.id == id)).or_else(|| asset.latest_set())
}

/// A set's number among the asset's sets ("v2").
fn version(asset: &Asset, set: &str) -> usize {
    asset.sets.iter().position(|s| s.id == set).map_or(0, |n| n + 1)
}

impl ChatPanel {
    /// Before drawing: the new-asset form's fields (made with the window; a kind's own fields
    /// once the catalog says what they are), and each hosted card's refine box.
    pub(super) fn ensure_hosted_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (wanted, dirs): (bool, Vec<PathBuf>) = {
            let items = &self.session.read(cx).chat.items;
            (
                items.iter().any(|i| matches!(i, Item::NewAsset)) && hosted_store::flow_on(),
                items.iter().filter_map(|i| if let Item::Asset(d) = i { Some(d.clone()) } else { None }).filter(|d| hosted_store::is_hosted(d)).collect(),
            )
        };
        for dir in dirs {
            self.refine_notes
                .entry(dir)
                .or_insert_with(|| cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 5).placeholder("What to change, e.g. keep the silhouette, make the hood darker")));
        }
        if !wanted {
            self.hosted_form = None;
            return;
        }
        if !hosted_store::signed_in() {
            return;
        }
        let (catalog, _) = hosted_store::catalog(cx);
        if self.hosted_form.is_none() {
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Goblin scout"));
            let description = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 6).placeholder("What it is and how it looks, e.g. a wiry goblin scout with olive-green skin, a worn leather hood and a wooden bow"));
            self.hosted_form = Some(HostedForm {
                kind: String::new(),
                name,
                description,
                texts: HashMap::new(),
                choices: HashMap::new(),
                flags: HashMap::new(),
                error: None,
                options_open: false,
                references: Vec::new(),
            });
            self.list.remeasure();
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

    // ----- the frame -----

    fn hosted_frame(&self, f: Frame, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, ground, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.secondary, theme.green);
        let header = div()
            .px_4()
            .pt_4()
            .pb_2()
            .flex()
            .items_center()
            .gap_3()
            .child(div().size(px(40.)).flex_none().rounded(px(10.)).bg(ground).flex().items_center().justify_center().child(Icon::new(Lucide::Box).text_color(muted)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(fg).truncate().child(f.title.clone()))
                    .child(div().text_sm().text_color(muted).truncate().child(f.subtitle.clone())),
            )
            .when_some(f.dir.clone(), |d, dir| {
                d.child(div().flex().items_center().gap_1().text_xs().text_color(muted).child(Icon::new(Lucide::CircleCheck).small().text_color(green)).child("Saved"))
                    .child(div().w(px(1.)).h(px(18.)).bg(border))
                    .child(
                        Button::new(SharedString::from(format!("{}-open", f.id)))
                            .xsmall()
                            .ghost()
                            .icon(Lucide::Maximize2)
                            .tooltip("Open the asset in a tab")
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenAsset(dir.clone())))),
                    )
            });
        let columns = div()
            .flex()
            .flex_wrap()
            .gap_4()
            .child(div().flex_grow(1.).flex_basis(px(300.)).min_w(px(240.)).child(f.left))
            .when_some(f.right, |d, r| d.child(div().flex_grow(1.).flex_basis(px(220.)).max_w(px(340.)).child(r)));
        let foot = div()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(border)
            .flex()
            .items_center()
            .gap_3()
            .when_some(f.back, |d, b| d.child(b).child(div().w(px(1.)).h(px(18.)).bg(border)))
            .child(div().flex_1().min_w_0().text_xs().text_color(muted).child(f.status))
            .children(f.primary);
        div()
            .id(f.id.clone())
            .flex()
            .flex_col()
            .rounded(px(14.))
            .border_1()
            .border_color(border.opacity(0.8))
            .bg(bg)
            .shadow_sm()
            .overflow_hidden()
            .child(header)
            .child(self.hosted_steps(&f.id, f.shown, f.reached, f.dir.as_deref(), cx))
            .child(
                div()
                    .px_4()
                    .pt_3()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(!f.intro.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(f.intro)))
                    .child(columns)
                    .children(f.error),
            )
            .child(foot)
            .into_any_element()
    }

    /// The four steps: done ones ticked (and clickable, to look back), the shown one marked.
    fn hosted_steps(&self, key: &str, shown: u8, reached: u8, dir: Option<&Path>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, ring, green, ground) = (theme.foreground, theme.muted_foreground, theme.border, theme.ring, theme.green, theme.secondary);
        let mut row = div().mx_4().flex().border_b_1().border_color(border);
        for (i, label) in STEPS.iter().enumerate() {
            let n = i as u8 + 1;
            let (current, done) = (n == shown, n < reached);
            let rig_soon = n == 4;
            let clickable = dir.is_some() && n >= 2 && n <= reached && !current && !rig_soon;
            let badge = div().size(px(22.)).flex_none().rounded_full().flex().items_center().justify_center().text_xs();
            let badge = if done && !current {
                badge.border_1().border_color(green).child(Icon::new(Lucide::Check).xsmall().text_color(green))
            } else if current {
                badge.bg(ring).text_color(gpui_kit::white()).child(n.to_string())
            } else {
                badge.bg(ground).text_color(muted).child(n.to_string())
            };
            let folder = dir.map(Path::to_path_buf);
            row = row.child(
                div()
                    .id(SharedString::from(format!("{key}-step-{n}")))
                    .flex_1()
                    .py_2()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_2()
                    .border_b_2()
                    .border_color(if current { ring } else { gpui_kit::transparent_black() })
                    .child(badge)
                    .child(div().text_sm().when(current, |d| d.font_weight(FontWeight::SEMIBOLD)).text_color(if current { fg } else { muted }).child(*label))
                    .when(rig_soon, |d| d.child(div().text_xs().text_color(muted).child("soon")))
                    .when(clickable, |d| {
                        d.cursor_pointer().on_click(move |_, _, cx| {
                            if let Some(dir) = &folder {
                                hosted_store::show_step(dir, if n == reached { None } else { Some(n) }, cx);
                            }
                        })
                    }),
            );
        }
        row.into_any_element()
    }

    fn hosted_heading(&self, text: &str, cx: &mut Context<Self>) -> Div {
        div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().foreground).child(text.to_string())
    }

    fn hosted_error(&self, id: &SharedString, text: String, upgrade: bool, cx: &mut Context<Self>) -> AnyElement {
        let danger = cx.theme().danger;
        // On a paid plan the way on is the reset, not an upgrade.
        let resets = upgrade.then(|| hosted_store::resets_on(cx)).flatten();
        let text = match &resets {
            Some(day) => format!("{text} It resets on {day}."),
            None => text,
        };
        div()
            .px_3()
            .py_2()
            .rounded(px(8.))
            .bg(danger.opacity(0.08))
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().text_xs().text_color(danger).child(text))
            .when(upgrade && resets.is_none(), |d| {
                d.child(Button::new(SharedString::from(format!("{id}-upgrade"))).xsmall().outline().label("Upgrade").on_click(|_, _, cx| crate::shell::open_settings_page("account", cx)))
            })
            .into_any_element()
    }

    /// Placeholders while a set is on its way.
    fn hosted_waiting(&self, what: &str, views: u32, height: f32, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut row = div().flex().flex_wrap().gap_2();
        for _ in 0..views {
            row = row.child(self.asset_making(height, None, cx));
        }
        div().flex().flex_col().gap_2().child(row).child(div().text_xs().text_color(muted).child(format!("{what}…"))).into_any_element()
    }

    /// A set's views, from the asset's folder, labelled; a click shows one large.
    fn hosted_view_row(&self, dir: &Path, files: &[String], labels: &[String], height: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, ground, border) = (theme.muted_foreground, theme.secondary, theme.border);
        let mut row = div().flex().flex_wrap().gap_2();
        for (n, file) in files.iter().enumerate() {
            let path = dir.join(file);
            let full = path.clone();
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border)
                    .overflow_hidden()
                    .child(div().px_2().py_1().text_xs().font_weight(FontWeight::MEDIUM).text_color(muted).child(labels.get(n).cloned().unwrap_or_default()))
                    .child(
                        div()
                            .id(SharedString::from(format!("hosted-view-{}", path.display())))
                            .h(px(height))
                            .w(px(height * 0.62))
                            .bg(ground)
                            .cursor_pointer()
                            .child(img(path).size_full().object_fit(ObjectFit::Contain))
                            .on_click(move |_, _, cx| crate::image_view::show(full.clone(), cx)),
                    ),
            );
        }
        row.into_any_element()
    }

    // ----- step 1: describe -----

    /// The form for a new hosted asset.
    pub(super) fn render_hosted_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, border, ring, fg, ground) = (theme.muted_foreground, theme.danger, theme.border, theme.ring, theme.foreground, theme.secondary);
        let cancel = Button::new("new-asset-cancel").small().ghost().label("Cancel").on_click(cx.listener(|p, _, _, cx| p.close_asset_form(cx, None))).into_any_element();
        let frame = |left: AnyElement, right: Option<AnyElement>, error: Option<AnyElement>, status: &str, primary: Option<AnyElement>, cancel: AnyElement| Frame {
            id: "new-asset".into(),
            title: "New game asset".into(),
            subtitle: "Start with a description".into(),
            shown: 1,
            reached: 1,
            dir: None,
            intro: String::new(),
            left,
            right,
            error,
            back: Some(cancel),
            status: status.into(),
            primary,
        };
        if !hosted_store::signed_in() {
            let left = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().text_color(fg).child("Game assets are made on your Wundur account."))
                .child(div().text_xs().text_color(muted).child("Sign in (it's free: 5 images and 2 model credits a month) to describe an asset, review its views and build a 3D model."))
                .into_any_element();
            let sign_in = Button::new("new-asset-signin").small().primary().label("Sign in").on_click(|_, _, cx| crate::shell::open_settings_page("account", cx)).into_any_element();
            return self.hosted_frame(frame(left, None, None, "", Some(sign_in), cancel), cx);
        }
        let (catalog, _) = hosted_store::catalog(cx);
        let Some(form) = &self.hosted_form else { return div().into_any_element() };
        let Some(catalog) = catalog else {
            let left = match hosted_store::catalog_error(cx) {
                Some(e) => div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().text_xs().text_color(danger).child(e))
                    .child(Button::new("hosted-catalog-retry").small().ghost().label("Try again").on_click(|_, _, cx| hosted_store::reload_catalog(cx)))
                    .into_any_element(),
                None => div().text_xs().text_color(muted).child("Asking Wundur what it can make…").into_any_element(),
            };
            return self.hosted_frame(frame(left, None, None, "", None, cancel), cx);
        };
        let label = |t: &str| div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(t.to_string());
        let me = cx.entity().downgrade();
        // The kinds, as one segmented control.
        let mut kinds = div().flex().rounded(px(8.)).border_1().border_color(border).overflow_hidden();
        for (n, k) in catalog.kinds.iter().enumerate() {
            let (on, id) = (form.kind == k.id, k.id.clone());
            kinds = kinds.child(
                div()
                    .id(SharedString::from(format!("hosted-kind-{}", k.id)))
                    .flex_1()
                    .py(px(6.))
                    .flex()
                    .justify_center()
                    .text_sm()
                    .cursor_pointer()
                    .when(n > 0, |d| d.border_l_1().border_color(border))
                    .when(on, |d| d.bg(ring.opacity(0.12)).text_color(ring).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.text_color(fg).hover(|d| d.bg(ground)))
                    .child(k.label.clone())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(f) = &mut p.hosted_form {
                            f.kind = id.clone();
                        }
                        cx.notify();
                    })),
            );
        }
        let kind = catalog.kind(&form.kind);
        let mut options = div().flex().flex_col().gap_2();
        let has_fields = kind.is_some_and(|k| !k.fields.is_empty());
        if form.options_open
            && let Some(kind) = kind
        {
            for f in &kind.fields {
                options = options.child(self.hosted_field(form, f, cx));
            }
        }
        let open = form.options_open;
        let left = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("Name"))
            .child(Input::new(&form.name).on_paste({
                let me = me.clone();
                move |item, window, cx| reference_paste(item, &me, window, cx)
            }))
            .child(div().h(px(4.)))
            .child(label("Asset type"))
            .child(kinds)
            .children(kind.filter(|k| !k.description.is_empty()).map(|k| div().text_xs().text_color(muted).child(k.description.clone())))
            .child(div().h(px(4.)))
            .child(label("Describe your asset"))
            .child(Textarea::new(&form.description).on_paste({
                let me = me.clone();
                move |item, window, cx| reference_paste(item, &me, window, cx)
            }))
            .when(has_fields, |d| {
                d.child(
                    div()
                        .id("hosted-options")
                        .mt_1()
                        .px_3()
                        .py_2()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(border)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .id("hosted-options-toggle")
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).small().text_color(muted))
                                .child(div().text_sm().text_color(fg).child("Style & generation options"))
                                .on_click(cx.listener(|p, _, _, cx| {
                                    if let Some(f) = &mut p.hosted_form {
                                        f.options_open = !f.options_open;
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(open, |d| d.child(options)),
                )
            })
            .into_any_element();
        let three_d = kind.is_none_or(|k| k.supports_3d);
        let step = |icon: Lucide, text: &str| div().flex().items_center().gap_3().child(Icon::new(icon).small().text_color(muted)).child(div().text_sm().text_color(fg).child(text.to_string()));
        let right = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.hosted_form_references(form, cx))
            .child(
        div()
            .p_3()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(ground.opacity(0.5))
            .flex()
            .flex_col()
            .gap_2()
            .child(label("What happens next"))
            .child(step(Lucide::Image, "Make a set of views (one image)"))
            .child(step(Lucide::Eye, "Review them, refine or try again"))
            .child(if three_d { step(Lucide::Box, "Build a model when you're ready") } else { step(Lucide::Box, "3D isn't available for this kind yet") }),
            )
            .into_any_element();
        let error = form.error.clone().map(|e| div().text_xs().text_color(danger).child(e).into_any_element());
        let primary = Button::new("new-asset-create").small().primary().icon(Lucide::Sparkles).label("Make the views").on_click(cx.listener(|p, _, _, cx| p.create_hosted(cx))).into_any_element();
        let card = self.hosted_frame(frame(left, Some(right), error, "You can rename it later", Some(primary), cancel), cx);
        // Pictures dropped anywhere on the form are references.
        div()
            .id("hosted-form-drop")
            .rounded(px(14.))
            .border_2()
            .border_color(gpui_kit::transparent_black())
            .drag_over::<ExternalPaths>(move |d, _, _, _| d.border_color(ring))
            .on_drop(cx.listener(|p, paths: &ExternalPaths, _, cx| p.take_references(paths.paths().to_vec(), cx)))
            .child(card)
            .into_any_element()
    }

    /// The form's reference pictures: each as a thumbnail (with a way to drop it), and a tile
    /// to add more, up to three.
    fn hosted_form_references(&self, form: &HostedForm, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, ground, bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.secondary, theme.background);
        let mut tiles = div().flex().flex_wrap().gap_2();
        for (n, path) in form.references.iter().enumerate() {
            tiles = tiles.child(
                div()
                    .size(px(76.))
                    .relative()
                    .rounded(px(8.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(border)
                    .bg(ground)
                    .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                    .child(
                        div().absolute().top(px(3.)).right(px(3.)).rounded_full().bg(bg.opacity(0.9)).child(
                            Button::new(SharedString::from(format!("hosted-ref-remove-{n}")))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Remove")
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    if let Some(f) = &mut p.hosted_form
                                        && n < f.references.len()
                                    {
                                        f.references.remove(n);
                                    }
                                    p.list.remeasure();
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }
        if form.references.len() < wundur_core::asset_flow::REFERENCES {
            tiles = tiles.child(
                div()
                    .id("hosted-ref-add")
                    .size(px(76.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(border)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(ground))
                    .child(Icon::new(IconName::Plus).text_color(muted))
                    .child(div().text_xs().text_color(muted).child("Add"))
                    .on_click(cx.listener(|p, _, _, cx| p.pick_references(cx))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Reference images"))
            .child(div().text_xs().text_color(muted).child("Optional · up to 3 pictures for the look, shape or art style. Drop them on the form or paste one into a box. They can't be changed later."))
            .child(div().pt_1().child(tiles))
            .into_any_element()
    }

    /// The system's picker for reference pictures; the ones chosen join the form's (three at most).
    fn pick_references(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |p, cx| p.take_references(paths, cx));
        })
        .detach();
    }

    /// Pictures for the form's references, picked, dropped or pasted: PNG, JPEG and WebP join
    /// (three at most); anything else says why it didn't.
    pub(super) fn take_references(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(f) = &mut self.hosted_form else { return };
        for path in paths {
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
                f.error = Some("References are PNG, JPEG or WebP pictures.".into());
                continue;
            }
            if f.references.contains(&path) {
                continue;
            }
            if f.references.len() >= wundur_core::asset_flow::REFERENCES {
                f.error = Some(format!("At most {} reference pictures.", wundur_core::asset_flow::REFERENCES));
                break;
            }
            f.references.push(path);
            f.error = None;
        }
        self.list.remeasure();
        cx.notify();
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
                        form.options_open = true;
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
        let references = form.references.clone();
        match hosted_store::create(&project, &kind.id, &name, &description, fields, references, origin, cx) {
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

    // ----- the card -----

    /// A hosted asset's card: the step it's at (or the one the person went back to).
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
        let reached = reached(&e, kind.as_ref());
        let shown = e.step.filter(|s| *s >= 2 && *s <= reached).unwrap_or(reached);
        let error = e.error.clone().map(|(text, upgrade)| self.hosted_error(&id("error"), text, upgrade, cx));
        let mut frame = Frame {
            id: id("card"),
            title: e.record.name.clone(),
            subtitle: kind_label.clone(),
            shown,
            reached,
            dir: Some(dir.to_path_buf()),
            intro: String::new(),
            left: div().into_any_element(),
            right: None,
            error,
            back: None,
            status: String::new(),
            primary: None,
        };
        match shown {
            4 => self.hosted_rig(dir, &e, &kind_label, &id, &mut frame, cx),
            3 => self.hosted_model(dir, &e, catalog.as_ref(), kind.as_ref(), plan.as_deref(), &id, &mut frame, cx),
            _ => self.hosted_views(dir, &e, kind.as_ref(), &kind_label, reached, &id, &mut frame, cx),
        }
        self.hosted_frame(frame, cx)
    }

    // ----- step 2: views -----

    #[allow(clippy::too_many_arguments)]
    fn hosted_views(&self, dir: &Path, e: &hosted_store::Entry, kind: Option<&Kind>, kind_label: &str, reached: u8, id: &dyn Fn(&str) -> SharedString, frame: &mut Frame, cx: &mut Context<Self>) {
        let theme = cx.theme();
        let (muted, fg, danger, border, ring, ground) = (theme.muted_foreground, theme.foreground, theme.danger, theme.border, theme.ring, theme.secondary);
        let views = kind.map_or(3, |k| k.views.max(1));
        let three_d = kind.is_none_or(|k| k.supports_3d);
        frame.intro = "Check the silhouette, outfit and scale before building the model.".into();
        let Some(asset) = &e.asset else {
            frame.left = self.hosted_waiting(e.working.unwrap_or("Making the views"), views, 200., cx);
            frame.status = "One image for the whole set".into();
            return;
        };
        let Some(set) = shown_set(e, asset).cloned() else {
            frame.left = self.hosted_waiting(e.working.unwrap_or("Making the views"), views, 200., cx);
            return;
        };
        let v = version(asset, &set.id);
        frame.subtitle = format!("{kind_label} · Views v{v}");
        let generating = asset.sets.iter().any(Set::is_generating);
        let busy = e.working.is_some() || generating;
        let saved = e.record.views.get(&set.id).cloned();
        // The set shown, large.
        let main: AnyElement = if set.is_generating() || (set.is_ready() && saved.is_none()) {
            self.hosted_waiting(e.working.unwrap_or("Making the views"), (set.views.len() as u32).max(views), 180., cx)
        } else if set.is_ready() {
            let labels: Vec<String> = set.views.iter().map(|v| v.label.clone()).collect();
            self.hosted_view_row(dir, saved.as_deref().unwrap_or_default(), &labels, 180., cx)
        } else {
            div().text_xs().text_color(danger).child(set.error.as_ref().map_or("These views didn't come out (not counted); try another direction.".to_string(), |er| er.message.clone())).into_any_element()
        };
        // The versions: every set, the shown one marked.
        let mut strip = div().flex().flex_wrap().items_end().gap_2();
        for (n, s) in asset.sets.iter().enumerate() {
            let on = s.id == set.id;
            let thumb = e.record.views.get(&s.id).and_then(|f| f.first()).map(|f| dir.join(f));
            let (folder, sid) = (dir.to_path_buf(), s.id.clone());
            let caption = format!("v{}{}", n + 1, if s.approved { " · Approved" } else if on { " · Shown" } else { "" });
            strip = strip.child(
                div()
                    .id(SharedString::from(format!("hosted-version-{}-{}", s.id, dir.display())))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(move |_, _, cx| hosted_store::show_set(&folder, Some(sid.clone()), cx))
                    .child(
                        div()
                            .size(px(56.))
                            .rounded(px(8.))
                            .overflow_hidden()
                            .bg(ground)
                            .border_2()
                            .border_color(if on { ring } else { border.opacity(0.0) })
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match (&thumb, s.status.as_str()) {
                                (Some(p), _) => img(p.clone()).size_full().object_fit(ObjectFit::Cover).into_any_element(),
                                (None, "failed") => div().text_xs().text_color(danger).child("failed").into_any_element(),
                                _ => gpui_kit::component::spinner::Spinner::new().color(muted).into_any_element(),
                            }),
                    )
                    .child(div().text_xs().text_color(if on { ring } else { muted }).child(caption)),
            );
        }
        frame.left = div().flex().flex_col().gap_3().child(main).child(strip).into_any_element();
        // Refine, try again, and the brief.
        let mut right = div().flex().flex_col().gap_2().child(self.hosted_heading(&format!("Refine v{v}"), cx));
        if let Some(note) = self.refine_notes.get(dir) {
            let (d1, n) = (dir.to_path_buf(), note.clone());
            right = right.child(Textarea::new(note)).child(
                Button::new(id("change"))
                    .small()
                    .outline()
                    .w_full()
                    .icon(Lucide::Sparkles)
                    .label("Make a revision")
                    .disabled(busy || !set.is_ready())
                    .on_click(move |_, window, cx| {
                        let text = n.read(cx).value().trim().to_string();
                        if !text.is_empty() {
                            n.update(cx, |i, cx| i.set_value("", window, cx));
                            hosted_store::change(&d1, &text, cx);
                        }
                    }),
            );
        }
        let d2 = dir.to_path_buf();
        right = right
            .child(div().text_xs().text_color(muted).child("A new version from this one; earlier versions are kept. One image."))
            .child(Button::new(id("retry")).small().outline().w_full().icon(Lucide::RefreshCw).label("Try another direction").disabled(busy).on_click(move |_, _, cx| hosted_store::retry(&d2, cx)))
            .child(div().h(px(1.)).bg(border).my_1())
            .child(self.hosted_heading("Asset brief", cx))
            .child(div().text_sm().text_color(fg).child(e.record.description.clone()))
            .when(!e.record.references.is_empty(), |d| {
                let mut row = div().flex().items_center().gap_2().child(div().text_xs().text_color(muted).child("References"));
                for f in &e.record.references {
                    let (path, full) = (dir.join(f), dir.join(f));
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("hosted-ref-{}", path.display())))
                            .size(px(40.))
                            .rounded(px(6.))
                            .overflow_hidden()
                            .bg(ground)
                            .cursor_pointer()
                            .child(img(path).size_full().object_fit(ObjectFit::Cover))
                            .on_click(move |_, _, cx| crate::image_view::show(full.clone(), cx)),
                    );
                }
                d.child(row)
            })
            .when(!e.record.fields.is_empty(), |d| {
                let summary = e.record.fields.iter().map(|(k, v)| format!("{k}: {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect::<Vec<_>>().join(" · ");
                d.child(div().text_xs().text_color(muted).child(summary))
            })
            .child(
                div()
                    .mt_1()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(Lucide::Layers).small().text_color(muted))
                    .child(div().text_xs().text_color(muted).child(format!("{} version{} saved", asset.sets.len(), if asset.sets.len() == 1 { "" } else { "s" }))),
            );
        frame.right = Some(right.into_any_element());
        // The foot: approve the shown set.
        let approved = asset.approved_set().map(|s| s.id.clone());
        if reached >= 3 {
            let d = dir.to_path_buf();
            frame.back = Some(Button::new(id("to-model")).small().ghost().label(if reached == 4 { "Back to the import" } else { "Back to the model" }).on_click(move |_, _, cx| hosted_store::show_step(&d, None, cx)).into_any_element());
        }
        frame.status = match (&approved, set.attempts_left) {
            (Some(a), _) if *a == set.id => format!("Views v{v} approved"),
            (Some(a), _) => format!("Views v{} approved; approving v{v} models from it instead", version(asset, a)),
            (None, _) => "Views not yet approved".into(),
        };
        // Looking back at the approved set: nothing to do here but go back.
        if three_d && reached >= 3 && approved.as_deref() == Some(set.id.as_str()) {
            return;
        }
        if !three_d && approved.as_deref() == Some(set.id.as_str()) {
            frame.status = format!("Approved and saved in the asset's folder. 3D isn't available for {} yet.", kind_label.to_lowercase());
            return;
        }
        let (d3, set_id) = (dir.to_path_buf(), set.id.clone());
        let label = if three_d { "Use these views & build model →" } else { "Approve these views" };
        frame.primary = Some(
            Button::new(id("approve"))
                .small()
                .primary()
                .label(label)
                .disabled(busy || !set.is_ready())
                .on_click(move |_, _, cx| hosted_store::approve(&d3, &set_id, cx))
                .into_any_element(),
        );
    }

    // ----- step 3: model -----

    #[allow(clippy::too_many_arguments)]
    fn hosted_model(
        &self,
        dir: &Path,
        e: &hosted_store::Entry,
        catalog: Option<&Catalog>,
        kind: Option<&Kind>,
        plan: Option<&str>,
        id: &dyn Fn(&str) -> SharedString,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) {
        let theme = cx.theme();
        let (muted, fg, ring, border, yellow, danger, ground, green) = (theme.muted_foreground, theme.foreground, theme.ring, theme.border, theme.yellow, theme.danger, theme.secondary, theme.green);
        let Some(asset) = &e.asset else { return };
        let Some(set) = asset.approved_set().cloned() else { return };
        let v = version(asset, &set.id);
        let labels: Vec<String> = set.views.iter().map(|v| v.label.clone()).collect();
        let files = e.record.views.get(&set.id).cloned().unwrap_or_default();
        let attempts: Vec<Model> = asset.models.iter().filter(|m| m.set == set.id).cloned().collect();
        let attempt = attempts.last().cloned();
        let attempts_left = set.attempts_left.or_else(|| attempt.as_ref().and_then(|m| m.attempts_left));
        // Being made: asked for (and not listed yet), or listed as queued or running.
        let building = e.building.is_some() || matches!(e.working, Some("Starting the model" | "Rebuilding")) || attempt.as_ref().is_some_and(Model::is_running);
        let busy = e.working.is_some() || building;
        let imported = e.record.imported.clone();
        frame.intro = "Inspect the model before importing it into your project.".into();
        frame.subtitle = match &attempt {
            Some(_) => format!("{} · Model v{}", frame.subtitle, attempts.len()),
            None => format!("{} · Model", frame.subtitle),
        };
        let engines = match (catalog, kind) {
            (Some(c), Some(k)) => c.engines_for(k).into_iter().cloned().collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let picked = e.engine.clone().or_else(|| kind.map(|k| k.default_engine.clone())).unwrap_or_default();
        let picked_engine = engines.iter().find(|en| en.id == picked).cloned();
        let plan = plan.unwrap_or("free");
        // The stage: what's being built, or the finished attempt and its preview.
        let stage = div().min_h(px(240.)).rounded(px(12.)).bg(ground).flex().flex_col().items_center().justify_center().gap_2().p_4();
        let stage = match &attempt {
            // Being made: the views it's made from, dimmed, under what's happening.
            _ if building => {
                let running = attempt.as_ref().filter(|m| m.is_running());
                let engine = running.map_or(picked.clone(), |m| m.engine.clone());
                let seconds = catalog.and_then(|c| c.engine(&engine)).map_or(60, |en| en.seconds);
                let about = if seconds >= 120 { format!("usually about {} minutes", seconds / 60) } else { "usually about a minute".to_string() };
                let progress = running.and_then(|m| m.progress).map(|p| format!(" · {}%", (p * 100.0).round() as u32)).unwrap_or_default();
                let bg = cx.theme().background;
                stage.relative().child(div().opacity(0.3).child(self.hosted_view_row(dir, &files, &labels, 120., cx))).child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.))
                        .bg(bg.opacity(0.75))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(gpui_kit::component::spinner::Spinner::new().large().color(fg))
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(format!("Building the model{progress}")))
                        .child(div().text_xs().text_color(muted).child(format!("{}, {about}. It carries on if you close the card.", if running.is_some() { "Running" } else { "Starting" }))),
                )
            }
            Some(m) if m.succeeded() => {
                let (d1, mid) = (dir.to_path_buf(), m.id.clone());
                let me = cx.entity().downgrade();
                stage
                    .child(Icon::new(Lucide::Box).large().text_color(muted))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(format!("Model v{} is ready to inspect", attempts.len())))
                    .child(Button::new(id("preview")).small().outline().icon(Lucide::Eye).label("Open 3D preview").on_click(move |_, _, cx| {
                        let me = me.clone();
                        hosted_store::preview(&d1, &mid, cx, move |page, cx| {
                            let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(wundur_core::web::file_url(&page))));
                        });
                    }))
                    .child(div().text_xs().text_color(muted).child("Opens beside the chat: drag to orbit, scroll to zoom. Check the hands, feet and silhouette from every angle."))
            }
            Some(m) if m.status == "failed" => stage.child(div().text_sm().text_color(danger).child(m.error.clone().map_or("That model didn't come out (not counted).".into(), |er| er.message))).child(div().text_xs().text_color(muted).child("Build it again, or pick another engine.")),
            _ => stage.child(self.hosted_view_row(dir, &files, &labels, 120., cx)).child(div().text_xs().text_color(muted).child("These views go to the engine; pick one and build.")),
        };
        // Built from, and the attempts so far.
        let mut thumbs = div().flex().items_center().gap_2().child(div().text_xs().text_color(muted).child(format!("Built from Views v{v}")));
        for f in files.iter().take(4) {
            thumbs = thumbs.child(div().size(px(40.)).rounded(px(6.)).overflow_hidden().bg(ground).child(img(dir.join(f)).size_full().object_fit(ObjectFit::Cover)));
        }
        let history = (attempts.len() > 1).then(|| {
            let line = attempts
                .iter()
                .enumerate()
                .map(|(n, m)| {
                    let engine = catalog.and_then(|c| c.engine(&m.engine)).map_or(m.engine.clone(), |en| en.label.clone());
                    let state = if imported.as_deref() == Some(m.id.as_str()) { "imported" } else { m.status.as_str() };
                    format!("v{} {engine} · {state}", n + 1)
                })
                .collect::<Vec<_>>()
                .join("   ");
            div().text_xs().text_color(muted).child(line)
        });
        frame.left = div().flex().flex_col().gap_3().child(stage).child(thumbs).children(history).into_any_element();
        // The engines, and what the finished attempt would cost.
        let mut right = div().flex().flex_col().gap_2();
        if let Some(m) = attempt.as_ref().filter(|m| m.succeeded()) {
            let cost = m.credits.or_else(|| catalog.and_then(|c| c.engine(&m.engine)).map(|en| en.credits)).unwrap_or(1);
            let engine = catalog.and_then(|c| c.engine(&m.engine)).map_or(m.engine.clone(), |en| en.label.clone());
            let row = |k: &str, val: String| div().flex().justify_between().py_1().child(div().text_xs().text_color(muted).child(k.to_string())).child(div().text_xs().text_color(fg).child(val));
            right = right
                .child(div().flex().items_center().gap_2().child(Icon::new(Lucide::Box).small().text_color(fg)).child(self.hosted_heading("Model ready for review", cx)))
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(border)
                        .child(row("Engine", engine))
                        .child(row("Import", credits(cost)))
                        .child(row("Attempts left", attempts_left.map_or("—".into(), |n| n.to_string())))
                        .child(row("Rig", "Not yet".into())),
                );
        }
        right = right.child(self.hosted_heading("Engine", cx));
        for en in &engines {
            let (on, allowed, folder, eid) = (en.id == picked, en.on_plan(plan), dir.to_path_buf(), en.id.clone());
            right = right.child(
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
                    .when(allowed && !building, |d| d.cursor_pointer().on_click(move |_, _, cx| hosted_store::pick_engine(&folder, &eid, cx)))
                    .when(!allowed, |d| d.opacity(0.55))
                    .child(div().text_sm().text_color(fg).child(en.label.clone()))
                    .children((!en.license.label.is_empty()).then(|| div().text_xs().text_color(yellow).child(en.license.label.clone())))
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(muted).child(if allowed { credits(en.credits) } else { format!("{} · Monthly plan", credits(en.credits)) })),
            );
        }
        let help_key = format!("hosted-help:{}", dir.display());
        let open = self.toggled.contains(&help_key);
        let toggle = help_key.clone();
        right = right.child(
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
                right = right.child(div().text_xs().text_color(muted).child(format!("{}: {}{best}", en.label, en.help)));
            }
        }
        let left_n = attempts_left.unwrap_or(1);
        if attempt.as_ref().is_some_and(|m| m.succeeded() || m.status == "failed") {
            let d = dir.to_path_buf();
            let last = attempt.as_ref().map(|m| m.id.clone());
            right = right.child(div().h(px(4.))).child(
                Button::new(id("rebuild"))
                    .small()
                    .outline()
                    .w_full()
                    .icon(Lucide::RefreshCw)
                    .label("Rebuild model")
                    .disabled(busy || left_n == 0)
                    .on_click(move |_, _, cx| hosted_store::model(&d, last.clone(), cx)),
            );
            right = right.child(div().text_xs().text_color(muted).child(if left_n == 0 {
                "This set's attempts are used: make a change or a new set of views.".to_string()
            } else {
                format!("Free · makes another version on the engine picked ({left_n} left)")
            }));
            if let Some(m) = attempt.as_ref().filter(|m| m.succeeded() && imported.as_deref() != Some(m.id.as_str())) {
                let (d, mid) = (dir.to_path_buf(), m.id.clone());
                right = right.child(div().flex().child(Button::new(id("discard")).xsmall().ghost().label("Discard this one").disabled(busy).on_click(move |_, _, cx| hosted_store::reject(&d, &mid, cx))));
            }
        }
        frame.right = Some(if building { div().opacity(0.45).child(right).into_any_element() } else { right.into_any_element() });
        let d = dir.to_path_buf();
        frame.back = Some(Button::new(id("to-views")).small().ghost().label("Back to views").on_click(move |_, _, cx| hosted_store::show_step(&d, Some(2), cx)).into_any_element());
        frame.status = "Free to build; Import spends credits.".into();
        if let Some(m) = imported.as_ref().and_then(|i| attempts.iter().find(|m| &m.id == i)) {
            let _ = m;
            frame.status = "Imported into the project.".into();
            let d = dir.to_path_buf();
            frame.primary = Some(Button::new(id("to-rig")).small().primary().label("Continue →").on_click(move |_, _, cx| hosted_store::show_step(&d, None, cx)).into_any_element());
            let _ = green;
            return;
        }
        frame.primary = Some(match &attempt {
            // A rebuild on its way: the earlier attempt isn't the one to import now.
            _ if building => Button::new(id("building")).small().primary().label("Building…").loading(true).disabled(true).into_any_element(),
            Some(m) if m.succeeded() => {
                let cost = m.credits.or_else(|| catalog.and_then(|c| c.engine(&m.engine)).map(|en| en.credits)).unwrap_or(1);
                let (d, mid) = (dir.to_path_buf(), m.id.clone());
                Button::new(id("import")).small().primary().icon(Lucide::FolderOpen).label(format!("Import · {} →", credits(cost))).disabled(busy).on_click(move |_, _, cx| hosted_store::import(&d, &mid, cx)).into_any_element()
            }
            _ => {
                let d = dir.to_path_buf();
                let blocked = picked_engine.as_ref().is_some_and(|en| !en.on_plan(plan));
                Button::new(id("model"))
                    .small()
                    .primary()
                    .icon(Lucide::Sparkles)
                    .label("Build model")
                    .disabled(busy || engines.is_empty() || left_n == 0 || blocked)
                    .on_click(move |_, _, cx| hosted_store::model(&d, None, cx))
                    .into_any_element()
            }
        });
    }

    // ----- step 4: rig (and what was imported) -----

    fn hosted_rig(&self, dir: &Path, e: &hosted_store::Entry, kind_label: &str, id: &dyn Fn(&str) -> SharedString, frame: &mut Frame, cx: &mut Context<Self>) {
        let theme = cx.theme();
        let (muted, green, fg, border) = (theme.muted_foreground, theme.green, theme.foreground, theme.border);
        frame.subtitle = format!("{kind_label} · Imported");
        frame.intro = "The model is in your project.".into();
        let model = e.record.files.iter().find(|f| f.ends_with(".glb")).cloned();
        let shown_path = |f: &str| dir.join(f).strip_prefix(&e.project).map(|p| p.display().to_string()).unwrap_or_else(|_| f.to_string());
        frame.left = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().items_center().gap_2().child(Icon::new(Lucide::CircleCheck).small().text_color(green)).child(div().text_sm().text_color(fg).child("Imported")))
            .children(e.record.files.iter().map(|f| div().px_3().py_1().rounded(px(6.)).border_1().border_color(border).text_xs().text_color(muted).child(shown_path(f))))
            .into_any_element();
        frame.right = Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().flex().items_center().gap_2().child(Icon::new(Lucide::Bone).small().text_color(muted)).child(self.hosted_heading("Rig", cx)))
                .child(div().text_xs().text_color(muted).child("Automatic rigging on Wundur's server is on its way. The model works unrigged in the meantime."))
                .into_any_element(),
        );
        let d = dir.to_path_buf();
        frame.back = Some(Button::new(id("to-model")).small().ghost().label("Back to model").on_click(move |_, _, cx| hosted_store::show_step(&d, Some(3), cx)).into_any_element());
        frame.status = model.as_deref().map(shown_path).unwrap_or_default();
        if let Some(file) = model {
            let me = cx.entity().downgrade();
            let (project, name, path) = (e.project.clone(), e.record.name.clone(), dir.join(&file));
            frame.primary = Some(
                Button::new(id("open-model"))
                    .small()
                    .primary()
                    .icon(Lucide::Box)
                    .label("Open model")
                    .on_click(move |_, _, cx| {
                        let me = me.clone();
                        crate::asset_store::viewer_page(&project, &path, &name, cx, move |page, cx| {
                            let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(wundur_core::web::file_url(&page))));
                        });
                    })
                    .into_any_element(),
            );
        }
    }


    /// Test commands for this chat's latest hosted asset: `new <kind>|<name>|<description>`,
    /// `approve`, `change <text>`, `retry`, `engine <id>`, `model`, `reroll`, `import`, `state`; and
    /// `form` (the empty form), `ref <path>` (a reference picture on it), `open <dir>` (a card for an asset folder), `step <n>` (show a step).
    pub fn hosted_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        if cmd == "form" {
            self.start_asset_form(cx);
            self.ensure_hosted_form(window, cx);
            let items = self.session.read(cx).chat.items.len();
            return format!("form open: {items} items, form {}, signed in {}, flow {}", self.hosted_form.is_some(), hosted_store::signed_in(), hosted_store::flow_on());
        }
        if cmd == "ref" {
            self.take_references(vec![PathBuf::from(arg)], cx);
            return format!("{} references", self.hosted_form.as_ref().map_or(0, |f| f.references.len()));
        }
        if cmd == "open" {
            let dir = PathBuf::from(arg);
            self.session.update(cx, |s, cx| {
                s.chat.items.push(Item::Asset(dir));
                s.chat.revision += 1;
                cx.notify();
            });
            return "card open".into();
        }
        if cmd == "step" {
            if let Some(dir) = self.session.read(cx).chat.items.iter().rev().find_map(|i| if let Item::Asset(d) = i { Some(d.clone()) } else { None }) {
                hosted_store::show_step(&dir, arg.parse().ok(), cx);
            }
            return format!("step {arg}");
        }
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
