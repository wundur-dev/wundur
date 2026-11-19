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
use wundur_core::asset_flow::{self as flow, Asset, Catalog, Field, Kind, Model, Set};

use super::*;
use crate::hosted_store;

const STEPS: [&str; 3] = ["Brief", "Views", "Model"];

/// What Cancel job does, since the service can't stop a job yet.
const CANCEL_TIP: &str = "Stop waiting on it. What's started still finishes on Wundur (the views' image is already used; a model costs nothing unless imported). Open it again to see the result.";

/// The name box's greyed text before there's a description to name it from.
const NAME_PLACEHOLDER: &str = "Named from the description";

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
    /// The name shown greyed in the name box, from the description (sent only if typed).
    pub suggested: String,
    /// Rename was clicked: the name box is shown (it is too once a name is typed).
    pub renaming: bool,
}

/// How wide the form's part names are ("Describe it"), so the parts line up.
const FORM_LABEL: f32 = 92.;

/// How wide a setting's name is on the form, so the choices and help line up.
const FIELD_LABEL: f32 = 96.;

/// An icon for a kind on the form; a kind the app doesn't know yet gets a box.
fn kind_icon(id: &str) -> Lucide {
    match id {
        "humanoid" => Lucide::PersonStanding,
        "creature" => Lucide::PawPrint,
        "prop" => Lucide::Package,
        "place" => Lucide::Mountain,
        _ => Lucide::Box,
    }
}

/// A triangle count as shown: "8k", "30k", "100k" (or the number, under a thousand).
fn triangles(n: u32) -> String {
    if n >= 1000 { format!("{}k", (n as f64 / 1000.0).round() as u32) } else { n.to_string() }
}

/// What a model attempt was made with, as one line: "Wundur (preview) · Light · Faceted"
/// (the engine's, detail's and finish's names from the catalog, else their ids).
fn model_line(m: &Model, catalog: Option<&Catalog>) -> String {
    let engine = catalog.and_then(|c| c.engine(&m.engine)).map_or(m.engine.clone(), |en| en.label.clone());
    let detail = m.detail.as_ref().map(|d| catalog.and_then(|c| c.detail(d)).map_or(capitalised(d), |d| d.label.clone()));
    [Some(engine), detail, m.finish.as_deref().map(capitalised)].into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// A catalog value as shown: its first letter capitalised ("low poly" → "Low poly").
fn capitalised(text: &str) -> String {
    let mut c = text.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
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

/// The width of two view tiles side by side (a tile is 0.62 of its picture's height, plus its
/// border) and the gap between: four views wrap into two rows of two.
fn two_wide(height: f32) -> f32 {
    // Two points to spare: rounding never pushes a tile onto a third row.
    2. * (height * 0.62 + 2.) + 8. + 2.
}

/// [`two_wide`] for the placeholders (`asset_making` draws squares).
fn two_wide_placeholders(height: f32) -> f32 {
    2. * height + 8. + 2.
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
    /// Laid over the whole card (a model being made): it greys the card and takes its clicks.
    overlay: Option<AnyElement>,
    /// The form's close button, at the header's end.
    close: Option<AnyElement>,
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
            let name = cx.new(|cx| InputState::new(window, cx).placeholder(NAME_PLACEHOLDER));
            let description = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("e.g. A tiger raptor scout in a tactical vest. Include its silhouette, materials and defining details."));
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
                suggested: String::new(),
                renaming: false,
            });
            self.list.remeasure();
        }
        // The name box suggests what the service will call it, from the description.
        if let Some(form) = &mut self.hosted_form {
            let suggested = wundur_core::asset_flow::suggested_name(form.description.read(cx).value().as_ref());
            if suggested != form.suggested {
                let shown = if suggested.is_empty() { NAME_PLACEHOLDER.to_string() } else { suggested.clone() };
                form.suggested = suggested;
                form.name.update(cx, |i, cx| i.set_placeholder(shown, window, cx));
            }
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
                    let choice = form.choices.entry(f.id.clone()).or_insert_with(|| default_choice(f));
                    // The look: one the form offers, if the catalog's default isn't.
                    if f.id == flow::STYLE_FIELD {
                        let looks = flow::looks(f);
                        if !looks.contains(choice)
                            && let Some(first) = looks.first()
                        {
                            *choice = first.clone();
                        }
                    }
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
        let (fg, muted, border, bg, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.green);
        // One band: what it is, the steps, and what can be done to the card.
        let busy = f.dir.as_deref().and_then(|d| hosted_store::entry(d, cx)).is_some_and(|e| e.cancellable());
        let header = div()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(border)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_4()
            .gap_y_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(Icon::new(Lucide::Box).text_color(muted))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).truncate().child(f.title.clone()))
                            .when(!f.subtitle.is_empty(), |d| d.child(div().text_xs().text_color(muted).truncate().child(f.subtitle.clone()))),
                    ),
            )
            .child(div().flex_1().flex().justify_center().child(self.hosted_steps(&f.id, f.shown, f.reached, f.dir.as_deref(), cx)))
            .when_some(f.dir.clone(), |d, dir| {
                let (fold, cancel) = (dir.clone(), dir.clone());
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex().items_center().gap_1().text_xs().text_color(muted).child(Icon::new(Lucide::CircleCheck).small().text_color(green)).child("Saved"))
                        .when(busy, |d| {
                            d.child(
                                Button::new(SharedString::from(format!("{}-cancel", f.id)))
                                    .xsmall()
                                    .ghost()
                                    .label("Cancel job")
                                    .tooltip(CANCEL_TIP)
                                    .on_click(move |_, _, cx| hosted_store::cancel(&cancel, cx)),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("{}-open", f.id)))
                                .xsmall()
                                .ghost()
                                .icon(Lucide::Maximize2)
                                .tooltip("Open the asset in a tab")
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenAsset(dir.clone())))),
                        )
                        .child(
                            Button::new(SharedString::from(format!("{}-fold", f.id)))
                                .xsmall()
                                .ghost()
                                .icon(Lucide::ChevronUp)
                                .tooltip("Fold to one line")
                                .on_click(move |_, _, cx| hosted_store::fold(&fold, true, cx)),
                        ),
                )
            })
            .children(f.close);
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
        let overlay = f.overlay;
        div()
            .id(f.id.clone())
            .relative()
            .flex()
            .flex_col()
            .rounded(px(14.))
            .border_1()
            .border_color(border.opacity(0.8))
            .bg(bg)
            .shadow_sm()
            .overflow_hidden()
            .child(header)
            // The overlay covers the work, not the header: folding and Cancel job stay at hand.
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
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
                    .children(overlay),
            )
            .into_any_element()
    }

    /// What's laid over a card while its model is being made: the card greyed, the app's
    /// spinner and what's happening in the middle; scrolling the chat still works.
    fn hosted_busy_overlay(&self, title: String, detail: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, bg, border) = (theme.foreground, theme.muted_foreground, theme.background, theme.border);
        let spinner = if crate::native_spinner::NativeSpinner::available() {
            let native = self.card_spinner.clone();
            div().size(px(24.)).child(canvas(|_, _, _| {}, move |b, _, window, _| native.paint(b, muted, window)).size_full()).into_any_element()
        } else {
            Icon::new(IconName::LoaderCircle).text_color(muted).into_any_element()
        };
        div()
            .id("hosted-busy")
            .absolute()
            .inset_0()
            .bg(bg.opacity(0.8))
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            // On a panel of its own, narrow enough to read: a long note (a GPU waking up) would
            // otherwise run across the card's faded contents.
            .child(
                div()
                    .max_w(px(420.))
                    .mx_4()
                    .px_4()
                    .py_3()
                    .rounded(px(12.))
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .shadow_sm()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(spinner)
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(title))
                    .child(div().text_xs().text_center().text_color(muted).child(detail)),
            )
            .into_any_element()
    }

    /// The steps in a row: done ones ticked (and clickable, to look back), the shown one marked.
    /// An imported asset (step 4) has them all ticked.
    fn hosted_steps(&self, key: &str, shown: u8, reached: u8, dir: Option<&Path>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, ring, green, ground) = (theme.foreground, theme.muted_foreground, theme.ring, theme.green, theme.secondary);
        let mut row = div().flex().flex_wrap().items_center().gap_x_3().gap_y_1();
        for (i, label) in STEPS.iter().enumerate() {
            let n = i as u8 + 1;
            let (current, done) = (n == shown, n < reached);
            let clickable = dir.is_some() && n >= 2 && n <= reached && !current;
            let badge = div().size(px(20.)).flex_none().rounded_full().flex().items_center().justify_center().text_xs();
            let badge = if done && !current {
                badge.border_1().border_color(green).child(Icon::new(Lucide::Check).xsmall().text_color(green))
            } else if current {
                badge.bg(ring).text_color(gpui_kit::white()).child(n.to_string())
            } else {
                badge.bg(ground).text_color(muted).child(n.to_string())
            };
            if i > 0 {
                row = row.child(Icon::new(Lucide::ArrowRight).xsmall().text_color(muted));
            }
            let folder = dir.map(Path::to_path_buf);
            row = row.child(
                div()
                    .id(SharedString::from(format!("{key}-step-{n}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(badge)
                    .child(div().text_sm().when(current, |d| d.font_weight(FontWeight::SEMIBOLD)).text_color(if current { fg } else { muted }).child(*label))
                    .when(clickable, |d| {
                        d.cursor_pointer().on_click(move |_, _, cx| {
                            if let Some(dir) = &folder {
                                // Model is the last step shown: an imported asset's page is its own.
                                let own = n == reached || (n == 3 && reached == 4);
                                hosted_store::show_step(dir, if own { None } else { Some(n) }, cx);
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
            // Wraps: a long message stays inside the card.
            .child(div().flex_1().min_w_0().text_xs().text_color(danger).child(text))
            .when(upgrade && resets.is_none(), |d| {
                d.child(Button::new(SharedString::from(format!("{id}-upgrade"))).xsmall().outline().label("Upgrade").on_click(|_, _, cx| crate::shell::open_settings_page("account", cx)))
            })
            .into_any_element()
    }

    /// Placeholders while a set is on its way (four as two rows of two).
    fn hosted_waiting(&self, what: &str, views: u32, height: f32, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let height = if views == 4 { height * 0.85 } else { height };
        let mut row = div().flex().flex_wrap().gap_2().when(views == 4, |d| d.w(px(two_wide_placeholders(height))));
        for _ in 0..views {
            row = row.child(self.asset_making(height, None, cx));
        }
        div().flex().flex_col().gap_2().child(row).child(div().text_xs().text_color(muted).child(format!("{what}…"))).into_any_element()
    }

    /// A set's views, from the asset's folder, labelled; a click shows one large.
    fn hosted_view_row(&self, dir: &Path, files: &[String], labels: &[String], height: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, ground, border) = (theme.muted_foreground, theme.secondary, theme.border);
        // Four views (a character's or creature's) as two rows of two, as they're drawn.
        let four = files.len() == 4;
        let height = if four { height * 0.85 } else { height };
        let mut row = div().flex().flex_wrap().gap_2().when(four, |d| d.w(px(two_wide(height))));
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
        let cancel = Button::new("new-asset-cancel").small().ghost().icon(IconName::Close).tooltip("Close").on_click(cx.listener(|p, _, _, cx| p.close_asset_form(cx, None))).into_any_element();
        let frame = |left: AnyElement, right: Option<AnyElement>, error: Option<AnyElement>, status: &str, primary: Option<AnyElement>, cancel: AnyElement| Frame {
            id: "new-asset".into(),
            title: "New asset".into(),
            subtitle: String::new(),
            shown: 1,
            reached: 1,
            dir: None,
            intro: String::new(),
            left,
            right,
            error,
            back: None,
            status: status.into(),
            primary,
            overlay: None,
            close: Some(cancel),
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
        let me = cx.entity().downgrade();
        // Each part on a row: its name in a column on the left, beside it when there's room.
        let row = |label: &str, control: AnyElement| {
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_x_3()
                .gap_y_1()
                .child(div().w(px(FORM_LABEL)).flex_none().pt(px(6.)).text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(label.to_string()))
                .child(div().flex_1().min_w(px(260.)).child(control))
        };
        // What it is: the kinds, each a tile with its icon.
        let mut kinds = div().flex().flex_wrap().gap_2();
        for k in &catalog.kinds {
            let (on, id) = (form.kind == k.id, k.id.clone());
            kinds = kinds.child(
                div()
                    .id(SharedString::from(format!("hosted-kind-{}", k.id)))
                    .px_3()
                    .py(px(5.))
                    .rounded(px(8.))
                    .border_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .when(on, |d| d.border_color(ring).bg(ring.opacity(0.10)).text_color(ring).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.border_color(border).text_color(fg).hover(|d| d.bg(ground)))
                    .child(Icon::new(kind_icon(&k.id)).small().text_color(if on { ring } else { muted }))
                    .child(k.label.clone())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(f) = &mut p.hosted_form {
                            f.kind = id.clone();
                        }
                        p.list.remeasure();
                        cx.notify();
                    })),
            );
        }
        let kind = catalog.kind(&form.kind);
        // The brief, with its reference pictures under it.
        let describe = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Textarea::new(&form.description).on_paste({
                let me = me.clone();
                move |item, window, cx| reference_paste(item, &me, window, cx)
            }))
            .child(self.hosted_form_references(form, cx));
        // The look as two picture cards; the kind's other fields are its settings.
        let style = kind.and_then(|k| k.fields.iter().find(|f| f.id == flow::STYLE_FIELD && f.kind == "enum")).cloned();
        let look = style.map(|f| {
            let label = if f.label.eq_ignore_ascii_case("style") { "Look".to_string() } else { f.label.clone() };
            row(&label, self.hosted_style(form, &f, cx))
        });
        // The name: the one the description gives, unless the person renames it.
        let typed = !form.name.read(cx).value().trim().is_empty();
        let name = if form.renaming || typed {
            row(
                "Name",
                div()
                    .max_w(px(380.))
                    .child(Input::new(&form.name).on_paste({
                        let me = me.clone();
                        move |item, window, cx| reference_paste(item, &me, window, cx)
                    }))
                    .into_any_element(),
            )
        } else {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .child(div().text_color(muted).child("Name:"))
                .child(if form.suggested.is_empty() { div().text_color(muted).child(NAME_PLACEHOLDER) } else { div().text_color(fg).font_weight(FontWeight::MEDIUM).child(form.suggested.clone()) })
                .child(
                    div()
                        .id("hosted-rename")
                        .text_color(ring)
                        .cursor_pointer()
                        .hover(|d| d.underline())
                        .child("Rename")
                        .on_click(cx.listener(|p, _, window, cx| {
                            if let Some(f) = &mut p.hosted_form {
                                f.renaming = true;
                                let (name, suggested) = (f.name.clone(), f.suggested.clone());
                                name.update(cx, |i, cx| {
                                    if !suggested.is_empty() {
                                        i.set_value(suggested, window, cx);
                                    }
                                    i.focus(window, cx);
                                });
                            }
                            p.list.remeasure();
                            cx.notify();
                        })),
                )
        };
        let rest: Vec<Field> = kind.map(|k| k.fields.iter().filter(|f| f.id != flow::STYLE_FIELD).cloned().collect()).unwrap_or_default();
        let has_choices = rest.iter().any(|f| matches!(f.kind.as_str(), "enum" | "bool"));
        let open = form.options_open;
        let settings = if has_choices {
            let summary = flow::settings_summary(&rest, &form.choices.iter().map(|(k, v)| (k.clone(), v.clone())).collect(), &form.flags.iter().map(|(k, v)| (k.clone(), *v)).collect());
            let mut fields = div().flex().flex_col().gap_3().pt_3().pl(px(22.));
            if open {
                for f in &rest {
                    fields = fields.child(self.hosted_field(form, f, cx));
                }
            }
            Some(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("hosted-options-toggle")
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).small().text_color(muted))
                            .child(div().flex_none().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("More options"))
                            .when(!summary.is_empty(), |d| d.child(div().min_w_0().truncate().text_sm().text_color(muted).child(summary)))
                            .on_click(cx.listener(|p, _, _, cx| {
                                if let Some(f) = &mut p.hosted_form {
                                    f.options_open = !f.options_open;
                                }
                                p.list.remeasure();
                                cx.notify();
                            })),
                    )
                    .when(open, |d| d.child(fields))
                    .into_any_element(),
            )
        } else if !rest.is_empty() {
            // Only free text (props, places): no settings to fold away.
            let mut fields = div().flex().flex_col().gap_3();
            for f in &rest {
                fields = fields.child(self.hosted_field(form, f, cx));
            }
            Some(fields.into_any_element())
        } else {
            None
        };
        let left = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(row("Type", kinds.into_any_element()))
            .child(row("Describe it", describe.into_any_element()))
            .children(look)
            .child(name)
            .children(settings)
            .into_any_element();
        let error = form.error.clone().map(|e| div().text_xs().text_color(danger).child(e).into_any_element());
        let cost = match hosted_store::images_left(cx) {
            Some(n) => format!("Uses 1 image ({n} left this month) · References lock when you generate."),
            None => "Uses 1 image · References lock when you generate.".to_string(),
        };
        let primary = Button::new("new-asset-create").small().primary().icon(Lucide::Sparkles).label("Make views").on_click(cx.listener(|p, _, _, cx| p.create_hosted(cx))).into_any_element();
        let f = frame(left, None, error, &cost, Some(primary), cancel);
        let card = self.hosted_frame(f, cx);
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

    /// The look as picture cards (each option's picture from the catalog, its kind's icon for
    /// one without), a mark on the one picked. Only the looks the form offers ([`flow::looks`]).
    fn hosted_style(&self, form: &HostedForm, f: &Field, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, ring, border, ground) = (theme.foreground, theme.muted_foreground, theme.ring, theme.border, theme.secondary);
        let current = form.choices.get(&f.id).cloned().unwrap_or_default();
        let mut cards = div().flex().flex_wrap().gap_3();
        for o in flow::looks(f) {
            let picture = f.option_images.get(&o).and_then(|path| hosted_store::style_image(path, cx));
            let (on, field, value) = (o == current, f.id.clone(), o.clone());
            let thumb = div().size(px(52.)).flex_none().rounded(px(8.)).overflow_hidden().bg(ground).flex().items_center().justify_center();
            let thumb = match picture {
                Some(path) => thumb.child(img(path).size_full().object_fit(ObjectFit::Cover)),
                None => thumb.child(Icon::new(kind_icon(&form.kind)).text_color(muted)),
            };
            let mark = div().size(px(20.)).flex_none().rounded_full().flex().items_center().justify_center();
            let mark = if on { mark.bg(ring).child(Icon::new(Lucide::Check).xsmall().text_color(gpui_kit::white())) } else { mark.border_1().border_color(muted.opacity(0.6)) };
            cards = cards.child(
                div()
                    .id(SharedString::from(format!("hosted-style-{o}")))
                    .flex_1()
                    .min_w(px(170.))
                    .max_w(px(280.))
                    .p_1p5()
                    .pr_3()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if on { ring } else { border })
                    .when(on, |d| d.bg(ring.opacity(0.06)))
                    .when(!on, |d| d.hover(|d| d.bg(ground.opacity(0.5))))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .child(thumb)
                    .child(div().flex_1().min_w_0().truncate().text_sm().text_color(fg).when(on, |d| d.font_weight(FontWeight::SEMIBOLD)).child(capitalised(&o)))
                    .child(mark)
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(form) = &mut p.hosted_form {
                            form.choices.insert(field.clone(), value.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(cards)
            .when(!f.help.is_empty(), |d| d.child(div().text_xs().text_color(muted).child(f.help.clone())))
            .into_any_element()
    }

    /// The form's reference pictures, on one line under the description: Add images (up to
    /// three), a thumbnail for each (with a way to drop it), and what they're for.
    fn hosted_form_references(&self, form: &HostedForm, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, ground, bg) = (theme.muted_foreground, theme.border, theme.secondary, theme.background);
        let most = wundur_core::asset_flow::REFERENCES;
        let mut line = div().flex().flex_wrap().items_center().gap_2();
        if form.references.len() < most {
            line = line.child(
                Button::new("hosted-ref-add")
                    .xsmall()
                    .outline()
                    .icon(Lucide::Paperclip)
                    .label("Add images")
                    .tooltip("Pictures for the design: colours, costume, shapes. Drop or paste them here too.")
                    .on_click(cx.listener(|p, _, _, cx| p.pick_references(cx))),
            );
        }
        for (n, path) in form.references.iter().enumerate() {
            line = line.child(
                div()
                    .size(px(44.))
                    .relative()
                    .rounded(px(6.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(border)
                    .bg(ground)
                    .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                    .child(
                        div().absolute().top(px(1.)).right(px(1.)).rounded_full().bg(bg.opacity(0.9)).child(
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
        line.child(div().text_xs().text_color(muted).child(format!("{}/{most} · for the design, never the pose", form.references.len()))).into_any_element()
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
                            .label(capitalised(o))
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
        // The choices beside the field's name, and its help on a line of its own under them (it
        // explains the trade-offs, so it's shown, not tucked behind an icon).
        let fg = cx.theme().foreground;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(FIELD_LABEL)).flex_none().text_sm().text_color(fg).child(f.label.clone()))
                    .child(div().flex_1().min_w_0().child(control)),
            )
            .when(!f.help.is_empty(), |d| d.child(div().pl(px(FIELD_LABEL + 12.)).text_xs().text_color(muted).child(f.help.clone())))
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
        if e.folded() {
            return self.hosted_row(dir, &e, &id, cx);
        }
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
            overlay: None,
            close: None,
        };
        match shown {
            4 => self.hosted_rig(dir, &e, &kind_label, &id, &mut frame, cx),
            3 => self.hosted_model(dir, &e, catalog.as_ref(), kind.as_ref(), plan.as_deref(), &id, &mut frame, cx),
            _ => self.hosted_views(dir, &e, kind.as_ref(), &kind_label, reached, &id, &mut frame, cx),
        }
        self.hosted_frame(frame, cx)
    }

    /// A folded card: one line with a picture of it, its name, where it's at, Open, and Cancel
    /// job while something is being made.
    fn hosted_row(&self, dir: &Path, e: &hosted_store::Entry, id: &dyn Fn(&str) -> SharedString, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, ground, green, danger) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.secondary, theme.green, theme.danger);
        // Its picture: the approved views' first, else the latest saved set's, else a reference.
        let asset = e.asset.as_ref();
        let set = asset.and_then(|a| a.approved_set().or_else(|| a.sets.iter().rev().find(|s| e.record.views.contains_key(&s.id))));
        let picture = set.and_then(|s| e.record.views.get(&s.id)).and_then(|f| f.first()).or_else(|| e.record.references.first()).map(|f| dir.join(f));
        let thumb = div().size(px(40.)).flex_none().rounded(px(8.)).overflow_hidden().bg(ground).flex().items_center().justify_center();
        let thumb = match picture {
            Some(path) => thumb.child(img(path).size_full().object_fit(ObjectFit::Cover)),
            None => thumb.child(Icon::new(kind_icon(&e.record.kind)).text_color(muted)),
        };
        let busy = e.busy();
        let (icon, text): (AnyElement, String) = if e.record.cancelled {
            (Icon::new(Lucide::CircleSlash).small().text_color(muted).into_any_element(), "Cancelled".into())
        } else if e.error.is_some() {
            (Icon::new(Lucide::CircleAlert).small().text_color(danger).into_any_element(), "Something went wrong".into())
        } else if busy {
            let text = match (e.working, asset) {
                (Some(w), _) => format!("{w}…"),
                (None, Some(a)) if a.sets.iter().any(Set::is_generating) => "Generating views…".into(),
                _ => "Building the model…".into(),
            };
            (gpui_kit::component::spinner::Spinner::new().color(muted).into_any_element(), text)
        } else if e.record.imported.is_some() {
            (Icon::new(Lucide::CircleCheck).small().text_color(green).into_any_element(), "Imported".into())
        } else {
            let text = match asset {
                Some(a) if a.approved_set().is_some() && a.models.iter().any(Model::succeeded) => "Model ready to import",
                Some(a) if a.approved_set().is_some() => "Views approved",
                Some(a) if a.latest_set().is_some_and(Set::is_ready) => "Views ready to review",
                _ => "",
            };
            (div().into_any_element(), text.into())
        };
        let (open, cancel) = (dir.to_path_buf(), dir.to_path_buf());
        div()
            .id(id("row"))
            .px_4()
            .py_2()
            .rounded(px(14.))
            .border_1()
            .border_color(border.opacity(0.8))
            .bg(bg)
            .shadow_sm()
            .flex()
            .items_center()
            .gap_3()
            .child(thumb)
            .child(div().flex_none().max_w(px(240.)).truncate().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(e.record.name.clone()))
            .child(div().flex_1().min_w_0().flex().items_center().gap_2().child(icon).child(div().min_w_0().truncate().text_sm().text_color(muted).child(text)))
            .child(Button::new(id("row-open")).small().outline().label("Open").tooltip("Open the card").on_click(move |_, _, cx| hosted_store::fold(&open, false, cx)))
            .when(e.cancellable(), |d| d.child(Button::new(id("row-cancel")).small().ghost().label("Cancel job").tooltip(CANCEL_TIP).on_click(move |_, _, cx| hosted_store::cancel(&cancel, cx))))
            .into_any_element()
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
            // Being made: the views it's made from (the card's overlay says what's happening).
            _ if building => {
                let running = attempt.as_ref().filter(|m| m.is_running());
                let engine = running.map_or(picked.clone(), |m| m.engine.clone());
                let seconds = catalog.and_then(|c| c.engine(&engine)).map_or(60, |en| en.seconds);
                let about = if seconds >= 120 { format!("usually about {} minutes", seconds / 60) } else { "usually about a minute".to_string() };
                let progress = running.and_then(|m| m.progress).map(|p| format!(" · {}%", (p * 100.0).round() as u32)).unwrap_or_default();
                // Why it waits, while the service says (a sleeping GPU waking up): without it a
                // queued model looks stuck. It starts by itself; the store keeps asking.
                let note = running.filter(|m| m.status == "queued").and_then(|m| m.note.clone()).filter(|n| !n.trim().is_empty());
                let detail = match note {
                    Some(n) => format!("{n} It starts by itself, and carries on if you close the card."),
                    None => format!("{}, {about}. It carries on if you close the card.", if running.is_some() { "Running" } else { "Starting" }),
                };
                frame.overlay = Some(self.hosted_busy_overlay(format!("Building the model{progress}"), detail, cx));
                stage.child(self.hosted_view_row(dir, &files, &labels, 120., cx))
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
                            let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenFile(page, None)));
                        });
                    }))
                    .child(div().text_xs().text_color(muted).child("Opens beside the chat: drag to orbit, scroll to zoom. Check the hands, feet and silhouette from every angle."))
            }
            Some(m) if m.status == "failed" => stage.child(div().text_sm().text_color(danger).child(m.error.clone().map_or("That model didn't come out (not counted).".into(), |er| er.message))).child(div().text_xs().text_color(muted).child("Build it again, or pick another engine.")),
            _ => stage.child(self.hosted_view_row(dir, &files, &labels, 120., cx)).child(div().text_xs().text_color(muted).child("These views go to the engine; pick one and build.")),
        };
        // Built from, and the attempts so far.
        // The views the latest attempt was made from (the service says which), else the set's.
        let used: Vec<(String, String)> = match attempt.as_ref().filter(|m| !m.input_image_ids.is_empty()) {
            Some(m) => m
                .input_image_ids
                .iter()
                .filter_map(|id| set.views.iter().position(|v| &v.image_id == id))
                .filter_map(|n| Some((files.get(n)?.clone(), set.views[n].label.clone())))
                .collect(),
            None => files.iter().cloned().zip(labels.iter().cloned()).collect(),
        };
        let from = if attempt.as_ref().is_some_and(|m| !m.input_image_ids.is_empty()) && !used.is_empty() {
            format!("Built from {} (v{v})", used.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join(", "))
        } else {
            format!("Built from Views v{v}")
        };
        let mut thumbs = div().flex().items_center().gap_2().child(div().text_xs().text_color(muted).child(from));
        for (f, label) in used.iter().take(4) {
            let full = dir.join(f);
            thumbs = thumbs.child(
                div()
                    .id(SharedString::from(format!("hosted-used-{}", full.display())))
                    .size(px(40.))
                    .rounded(px(6.))
                    .overflow_hidden()
                    .bg(ground)
                    .cursor_pointer()
                    .tooltip({
                        let label = label.clone();
                        move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                    })
                    .child(img(full.clone()).size_full().object_fit(ObjectFit::Cover))
                    .on_click(move |_, _, cx| crate::image_view::show(full.clone(), cx)),
            );
        }
        let history = (attempts.len() > 1).then(|| {
            let line = attempts
                .iter()
                .enumerate()
                .map(|(n, m)| {
                    let engine = model_line(m, catalog);
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
            let engine = model_line(m, catalog);
            let row = |k: &str, val: String| div().flex().justify_between().gap_2().py_1().child(div().flex_none().text_xs().text_color(muted).child(k.to_string())).child(div().text_xs().text_right().text_color(fg).child(val));
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
                        .children(m.face_limit.map(|n| row("Triangles", format!("up to {}", triangles(n)))))
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
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().text_color(fg).child(en.label.clone()))
                            .child(div().text_xs().text_color(muted).child(match (en.multiview, set.views.len()) {
                                // Which ones is the service's choice; "Built from" says after.
                                (_, 0 | 1) => "Uses its view".to_string(),
                                (true, _) => "Uses several views".to_string(),
                                (false, _) => "Uses one view".to_string(),
                            }))
                            // Its own line, wrapping: a preview's terms are long and must be read.
                            .children((!en.license.label.is_empty()).then(|| div().text_xs().text_color(yellow).child(en.license.label.clone()))),
                    )
                    .child(div().flex_none().text_xs().text_color(muted).child(if allowed { credits(en.credits) } else { format!("{} · Monthly plan", credits(en.credits)) })),
            );
        }
        // Detail (always, when the catalog has levels) and finish (only for an engine with some).
        let (_, detail, finish) = hosted_store::model_choice(e, catalog);
        if let Some(c) = catalog.filter(|c| !c.details.is_empty()) {
            right = right.child(div().h(px(4.))).child(self.hosted_heading("Detail", cx));
            let mut chips = div().flex().flex_wrap().gap_1();
            for d in &c.details {
                let (on, folder, did) = (detail.as_deref() == Some(d.id.as_str()), dir.to_path_buf(), d.id.clone());
                chips = chips.child(
                    Button::new(SharedString::from(format!("hosted-detail-{}-{}", d.id, dir.display())))
                        .xsmall()
                        .label(if d.triangles > 0 { format!("{} · {}", d.label, triangles(d.triangles)) } else { d.label.clone() })
                        .when(on, |b| b.outline())
                        .when(!on, |b| b.ghost())
                        .disabled(building)
                        .on_click(move |_, _, cx| hosted_store::pick_detail(&folder, &did, cx)),
                );
            }
            right = right.child(chips);
            if let Some(d) = detail.as_deref().and_then(|d| c.detail(d)).filter(|d| !d.help.is_empty()) {
                right = right.child(div().text_xs().text_color(muted).child(d.help.clone()));
            }
        }
        if let Some(en) = picked_engine.as_ref().filter(|en| !en.finishes.is_empty()) {
            right = right.child(div().h(px(4.))).child(self.hosted_heading("Finish", cx));
            let mut chips = div().flex().flex_wrap().gap_1();
            for f in &en.finishes {
                let (on, folder, fid) = (finish.as_deref() == Some(f.as_str()), dir.to_path_buf(), f.clone());
                chips = chips.child(
                    Button::new(SharedString::from(format!("hosted-finish-{f}-{}", dir.display())))
                        .xsmall()
                        .label(capitalised(f))
                        .when(on, |b| b.outline())
                        .when(!on, |b| b.ghost())
                        .disabled(building)
                        .on_click(move |_, _, cx| hosted_store::pick_finish(&folder, &fid, cx)),
                );
            }
            right = right.child(chips);
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
        frame.right = Some(right.into_any_element());
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
            let path = dir.join(&file);
            frame.primary = Some(
                Button::new(id("open-model"))
                    .small()
                    .primary()
                    .icon(Lucide::Box)
                    .label("Open model")
                    .on_click(move |_, _, cx| {
                        let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenFile(path.clone(), None)));
                    })
                    .into_any_element(),
            );
        }
    }


    /// Test commands for this chat's latest hosted asset: `new <kind>|<name>|<description>`,
    /// `approve`, `change <text>`, `retry`, `engine <id>`, `model`, `reroll`, `import`, `state`; and
    /// `form` (the empty form), `options` (its settings folded or not), `ref <path>` (a reference picture on it), `open <dir>` (a card for an asset folder), `step <n>` (show a step).
    pub fn hosted_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        if cmd == "options" {
            if let Some(f) = &mut self.hosted_form {
                f.options_open = !f.options_open;
            }
            self.list.remeasure();
            cx.notify();
            return format!("options open: {}", self.hosted_form.as_ref().is_some_and(|f| f.options_open));
        }
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

#[cfg(test)]
mod tests {
    use super::{Catalog, Model, model_line, triangles};

    #[test]
    fn triangle_counts_read_in_thousands() {
        assert_eq!((triangles(8000), triangles(30000), triangles(100_000), triangles(640)), ("8k".into(), "30k".into(), "100k".into(), "640".into()));
    }

    #[test]
    fn a_model_line_names_its_engine_detail_and_finish() {
        let catalog: Catalog = serde_json::from_value(serde_json::json!({
            "engines": [{ "id": "wundur", "label": "Wundur (preview)", "finishes": ["smooth", "faceted"] }],
            "details": [{ "id": "light", "label": "Light", "triangles": 8000 }]
        }))
        .unwrap();
        let m = Model { engine: "wundur".into(), detail: Some("light".into()), finish: Some("faceted".into()), ..Default::default() };
        assert_eq!(model_line(&m, Some(&catalog)), "Wundur (preview) · Light · Faceted");
        let older = Model { engine: "tripo".into(), ..Default::default() };
        assert_eq!(model_line(&older, None), "tripo", "a model from before details: just its engine");
    }
}
