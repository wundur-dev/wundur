//! The chat's side of the cards the agent shows the person (see `crate::agent_cards`): the
//! `ask_to_choose` card (files laid out one of a few fixed ways, with that way's buttons), and
//! the cards it presents from the Create menu, opened and filled in here, with the agent's
//! question on top, and what the person made told back to the agent.

use std::path::{Path, PathBuf};

use gpui_kit::component::Disableable;
use serde_json::Value;
use wundur_core::choices::{self, Choice, Show, State};

use super::*;
use crate::hosted_store;

/// A card the agent presented, by where it is in the chat.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Shown {
    Make(u64),
    Card(PathBuf),
    /// The new 3D asset form (one at a time)...
    AssetForm,
    /// ...and the asset it made.
    Asset(PathBuf),
}

/// What a presented card was for: its question.
#[derive(Clone, Debug)]
pub(super) struct Presented {
    pub question: String,
}

/// How high a picture is drawn, by how many share the row.
fn tile_height(show: Show) -> f32 {
    match show {
        Show::One => 320.,
        Show::Compare | Show::BeforeAfter => 220.,
        Show::Grid => 120.,
    }
}

impl ChatPanel {
    /// Before drawing: a note box for each question waiting, a still for each model shown, the
    /// cards the agent asked to show opened and filled in, and the presented 3D assets watched
    /// for their import.
    pub(super) fn ensure_agent_cards(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let waiting: Vec<(u64, Vec<PathBuf>)> = self
            .session
            .read(cx)
            .chat
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Choice { id, choice } if choice.state == State::Waiting => Some((*id, choice.files.clone())),
                _ => None,
            })
            .collect();
        for (id, files) in waiting {
            self.choice_notes.entry(id).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("A note for the agent (optional), e.g. but a bit brighter")));
            for f in files.into_iter().filter(|f| choices::is_model(f)) {
                self.want_still(f, cx);
            }
        }
        let asked = std::mem::take(&mut self.session.update(cx, |s, _| std::mem::take(&mut s.present)));
        for present in asked {
            self.open_presented(present, window, cx);
        }
        self.watch_presented_assets(cx);
    }

    /// A still of a model for its tile, drawn off the UI thread once (by `look_at_model`'s
    /// renderer, into the project's `.wundur/looks/`).
    fn want_still(&mut self, model: PathBuf, cx: &mut Context<Self>) {
        if self.model_stills.contains_key(&model) {
            return;
        }
        self.model_stills.insert(model.clone(), None);
        let cwd = self.session.read(cx).cwd().to_path_buf();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let m = model.clone();
            let look = cx
                .background_spawn(async move {
                    let name = format!("choice-{}", m.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
                    let args = serde_json::json!({ "models": m.to_string_lossy(), "views": "three-quarter", "size": 400, "name": name });
                    wundur_core::model_look::look(&cwd, &args)
                })
                .await;
            let _ = this.update(cx, |p, cx| {
                p.model_stills.insert(model, look.image.filter(|_| !look.failed));
                p.list.remeasure();
                cx.notify();
            });
        })
        .detach();
    }

    /// The question card: its files laid out the way asked, the way's buttons while it waits,
    /// and what the person picked once answered.
    pub(super) fn render_choice(&self, id: u64, choice: &Choice, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, accent) = (theme.foreground, theme.muted_foreground, theme.border, theme.cyan);
        let waiting = choice.state == State::Waiting;
        let selected: Vec<usize> = match &choice.state {
            State::Answered { picked, .. } => picked.clone(),
            _ => self.choice_picks.get(&id).cloned().unwrap_or_default(),
        };
        let answered = matches!(choice.state, State::Answered { .. });
        let selectable = waiting && matches!(choice.show, Show::Compare | Show::Grid);
        let height = tile_height(choice.show);
        let tiles = choice.files.iter().enumerate().map(|(ix, file)| {
            let picked = selected.contains(&ix);
            let label = match (choice.show, ix) {
                (Show::BeforeAfter, 0) => format!("Before · {}", file_label(file)),
                (Show::BeforeAfter, _) => format!("After · {}", file_label(file)),
                _ => file_label(file),
            };
            let picture: AnyElement = if choices::is_model(file) {
                match self.model_stills.get(file).cloned().flatten() {
                    Some(still) => crate::thumbs::picture(&still, crate::thumbs::TILE).h(px(height)).max_w_full().object_fit(ObjectFit::Contain).into_any_element(),
                    None => div().size_full().flex().items_center().justify_center().text_xs().text_color(muted).child("3D model").into_any_element(),
                }
            } else {
                let px_wanted = if choice.show == Show::One { crate::thumbs::LARGE } else { crate::thumbs::TILE };
                crate::thumbs::picture(file, px_wanted).h(px(height)).max_w_full().object_fit(ObjectFit::Contain).into_any_element()
            };
            let open = file.clone();
            let is_model = choices::is_model(file);
            let dim = answered && !picked;
            let way = choice.show;
            div()
                .id(SharedString::from(format!("choice-{id}-tile-{ix}")))
                .flex()
                .flex_col()
                .when(choice.show == Show::Grid, |d| d.w(px(height)))
                .when(choice.show != Show::Grid, |d| d.flex_1().min_w(px(120.)))
                .rounded_md()
                .border_2()
                .border_color(if picked { accent } else { border })
                .overflow_hidden()
                .when(dim, |d| d.opacity(0.45))
                .when(selectable, |d| d.cursor_pointer())
                .on_click(cx.listener(move |p, _, _, cx| {
                    if !selectable {
                        return;
                    }
                    let picks = p.choice_picks.entry(id).or_default();
                    if picks.contains(&ix) {
                        picks.retain(|i| *i != ix);
                    } else if way == Show::Compare {
                        *picks = vec![ix];
                    } else {
                        picks.push(ix);
                    }
                    cx.notify();
                }))
                .child(div().flex_none().h(px(height)).w_full().flex().items_center().justify_center().overflow_hidden().bg(cx.theme().muted.opacity(0.3)).child(picture))
                .child(
                    div()
                        .flex_none()
                        .h(px(22.))
                        .px_1p5()
                        .bg(cx.theme().background)
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_t_1()
                        .border_color(border)
                        .text_xs()
                        .child(div().flex_1().min_w_0().truncate().text_color(fg).child(label))
                        .child(
                            Button::new(SharedString::from(format!("choice-{id}-open-{ix}")))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Maximize)
                                .tooltip(if is_model { "Open in the model viewer" } else { "Open full size" })
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    if is_model {
                                        p.session.update(cx, |_, cx| cx.emit(crate::agent_session::SessionEvent::ShowModel(open.clone(), None)));
                                    } else {
                                        crate::image_view::show(open.clone(), cx);
                                    }
                                })),
                        ),
                )
        });
        let row = div().flex().gap_2().when(choice.show == Show::Grid, |d| d.flex_wrap()).children(tiles);
        let status = match &choice.state {
            State::Waiting => None,
            State::Withdrawn => Some("Not answered".to_string()),
            State::Answered { picked, note } => {
                let what = match (choice.show, picked.is_empty()) {
                    (Show::One, true) => "Rejected".to_string(),
                    (Show::One, false) => "Confirmed".to_string(),
                    _ => format!("Picked {}", picked.iter().filter_map(|&i| choice.files.get(i)).map(|f| file_label(f)).collect::<Vec<_>>().join(", ")),
                };
                Some(if note.trim().is_empty() { what } else { format!("{what} · \u{201c}{}\u{201d}", note.trim()) })
            }
        };
        let buttons = waiting.then(|| {
            let mut bar = div().flex().flex_wrap().items_center().gap_2();
            if let Some(note) = self.choice_notes.get(&id) {
                bar = bar.child(div().flex_1().min_w(px(180.)).child(Input::new(note).small()));
            }
            for (n, label) in choices::buttons(choice.show).iter().enumerate() {
                let label: &'static str = label;
                let ready = choices::picks(choice.show, label, &selected).is_some();
                let show = choice.show;
                let picks = selected.clone();
                bar = bar.child(
                    Button::new(SharedString::from(format!("choice-{id}-btn-{n}")))
                        .small()
                        .label(label)
                        .when(n == 0, |b| b.primary())
                        .when(n > 0, |b| b.outline())
                        .disabled(!ready)
                        .on_click(cx.listener(move |p, _, _, cx| {
                            let Some(picked) = choices::picks(show, label, &picks) else { return };
                            let note = p.choice_notes.remove(&id).map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                            p.choice_picks.remove(&id);
                            crate::agent_cards::answer(id, picked, note, cx);
                            p.list.remeasure();
                        })),
                );
            }
            bar
        });
        let hint = match choice.show {
            Show::Compare if waiting => Some("Click the one you want, then Pick."),
            Show::Grid if waiting => Some("Click the ones you want, then Pick these."),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(accent.opacity(0.04))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(div().flex_none().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(accent).child("choose"))
                    .child(div().flex_1().min_w_0().text_color(fg).child(choice.question.clone()))
                    .children(status.map(|s| div().flex_none().text_xs().text_color(muted).child(s))),
            )
            .child(row)
            .children(hint.map(|h| div().text_xs().text_color(muted).child(h)))
            .children(buttons)
            .into_any_element()
    }

    /// Tests: `choose <json>` asks as the agent's ask_to_choose would (not waiting);
    /// `present <json>` opens a card as present_card would (without its sign-in check);
    /// `answer <id> <button> [picks,…]` presses a question's button; `agent-cards` says where
    /// they all are.
    pub(super) fn agent_card_command(&mut self, cmd: &str, arg: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        match cmd {
            "choose" => {
                let Ok(mut args) = serde_json::from_str::<Value>(arg) else { return "choose <json>".into() };
                args["wait"] = Value::Bool(false);
                crate::agent_cards::ask_to_choose(self.session.clone(), &args, None, cx);
                self.agent_card_command("agent-cards", "", window, cx)
            }
            "present" => {
                let Ok(args) = serde_json::from_str::<Value>(arg) else { return "present <json>".into() };
                let present = crate::agent_cards::Present {
                    card: args["card"].as_str().unwrap_or_default().to_string(),
                    fill: args["fill"].as_object().cloned().unwrap_or_default(),
                    question: args["question"].as_str().unwrap_or_default().to_string(),
                };
                self.open_presented(present, window, cx);
                self.agent_card_command("agent-cards", "", window, cx)
            }
            "answer" => {
                let mut words = arg.split_whitespace();
                let (Some(id), Some(button)) = (words.next().and_then(|w| w.parse::<u64>().ok()), words.next()) else { return "answer <id> <button-number> [picks]".into() };
                let picks: Vec<usize> = words.next().map(|w| w.split(',').filter_map(|n| n.parse().ok()).collect()).unwrap_or_default();
                let show = self.session.read(cx).chat.items.iter().find_map(|i| match i {
                    Item::Choice { id: i, choice } if *i == id => Some(choice.show),
                    _ => None,
                });
                let Some(show) = show else { return format!("no question {id}") };
                let label = choices::buttons(show).get(button.parse::<usize>().unwrap_or(0)).copied().unwrap_or_default();
                let Some(picked) = choices::picks(show, label, &picks) else { return format!("{label}: not with those picks") };
                let note = self.choice_notes.remove(&id).map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                crate::agent_cards::answer(id, picked, note, cx);
                self.agent_card_command("agent-cards", "", window, cx)
            }
            _ => {
                let mut out: Vec<String> = self
                    .session
                    .read(cx)
                    .chat
                    .items
                    .iter()
                    .filter_map(|i| match i {
                        Item::Choice { id, choice } => Some(format!("choice {id} {} {:?} {:?}", choice.show.name(), choice.names, choice.state)),
                        _ => None,
                    })
                    .collect();
                for (k, p) in &self.presented {
                    out.push(format!("presented {k:?} asks {:?}", p.question));
                }
                for (id, c) in &self.makes {
                    out.push(format!("make {id}: {}", c.describe(cx)));
                }
                if let Some(f) = &self.hosted_form {
                    out.push(format!("asset form: kind {:?} name {:?} description {:?}", f.kind, f.name.read(cx).value(), f.description.read(cx).value()));
                }
                out.join("\n")
            }
        }
    }

    // ----- present_card -----

    /// Open a card the agent asked to show, filled in with what it gave.
    fn open_presented(&mut self, present: crate::agent_cards::Present, window: &mut Window, cx: &mut Context<Self>) {
        let crate::agent_cards::Present { card, fill, question } = present;
        let text = |k: &str| fill.get(k).and_then(Value::as_str).map(str::to_string);
        let shown = match card.as_str() {
            "image" | "sound-effect" if crate::card_store::manifest(&card, cx).is_none() => {
                let kind = if card == "image" { MakeKind::Image } else { MakeKind::Sound };
                self.start_make(kind, cx);
                self.ensure_make_fields(window, cx);
                let Some(id) = self.makes.keys().max().copied() else { return };
                self.fill_make(id, &fill, window, cx);
                Shown::Make(id)
            }
            "asset-3d" if crate::card_store::manifest(&card, cx).is_none() => {
                self.start_asset_form(cx);
                self.ensure_asset_fields(window, cx);
                let (catalog, _) = hosted_store::catalog(cx);
                if let Some(form) = &mut self.hosted_form {
                    if let Some(k) = text("kind").filter(|k| catalog.as_ref().is_some_and(|c| c.kind(k).is_some())) {
                        form.kind = k;
                    }
                    let (name, description) = (form.name.clone(), form.description.clone());
                    if let Some(n) = text("name") {
                        name.update(cx, |i, cx| i.set_value(n, window, cx));
                    }
                    if let Some(d) = text("description") {
                        description.update(cx, |i, cx| i.set_value(d, window, cx));
                    }
                } else if let Some(form) = &self.new_asset {
                    let (name, brief) = (form.name.clone(), form.brief.clone());
                    if let Some(n) = text("name") {
                        name.update(cx, |i, cx| i.set_value(n, window, cx));
                    }
                    if let Some(d) = text("description") {
                        brief.update(cx, |i, cx| i.set_value(d, window, cx));
                    }
                }
                Shown::AssetForm
            }
            workflow => {
                self.start_card(workflow, cx);
                let Some(dir) = self.session.read(cx).chat.items.iter().rev().find_map(|i| if let Item::Card(d) = i { Some(d.clone()) } else { None }) else { return };
                // Only what the card's own form has (checked when asked), over its defaults.
                if let (Some(mut record), Some(manifest)) = (wundur_core::cards::remote::load(&dir), crate::card_store::manifest(workflow, cx)) {
                    let mut form = manifest.form.clone();
                    if let Some(f) = form.as_object_mut() {
                        for (k, v) in &fill {
                            if f.contains_key(k) {
                                f.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    record.form = form;
                    let _ = wundur_core::cards::remote::store(&dir, &record);
                }
                Shown::Card(dir)
            }
        };
        self.presented.insert(shown, Presented { question });
        self.list.remeasure();
        cx.notify();
    }

    /// The agent's question over a card it presented, if it is one.
    pub(super) fn with_question(&self, shown: Shown, card: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let Some(p) = self.presented.get(&shown).filter(|p| !p.question.is_empty()) else { return card };
        let theme = cx.theme();
        let (fg, accent) = (theme.foreground, theme.cyan);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(div().flex_none().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(accent).child("agent asks"))
                    .child(div().flex_1().min_w_0().text_color(fg).child(p.question.clone())),
            )
            .child(card)
            .into_any_element()
    }

    /// What the person did with a card the agent presented, told to the agent (once).
    pub(super) fn report_presented(&mut self, shown: &Shown, what: String, cx: &mut Context<Self>) {
        let Some(p) = self.presented.remove(shown) else { return };
        let asked = if p.question.is_empty() { String::new() } else { format!(" (you asked: \"{}\")", p.question) };
        let text = format!("[Wundur: the card you presented{asked}.]\n\n{what}");
        self.session.update(cx, |s, cx| s.tell_agent(&text, cx));
    }

    /// The new asset form the agent presented became an asset (or was closed).
    pub(super) fn presented_form_closed(&mut self, made: Option<&Path>, cx: &mut Context<Self>) {
        let Some(p) = self.presented.get(&Shown::AssetForm).cloned() else { return };
        match made {
            Some(dir) => {
                self.presented.remove(&Shown::AssetForm);
                self.presented.insert(Shown::Asset(dir.to_path_buf()), p);
            }
            None => self.report_presented(&Shown::AssetForm, "The person closed the 3D asset form without making one.".into(), cx),
        }
    }

    /// Presented 3D assets that have been imported: told to the agent, with their files.
    fn watch_presented_assets(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = self.presented.keys().filter_map(|k| if let Shown::Asset(d) = k { Some(d.clone()) } else { None }).collect();
        let cwd = self.session.read(cx).cwd().to_path_buf();
        for dir in dirs {
            let Some(e) = hosted_store::entry(&dir, cx) else { continue };
            if e.record.imported.is_none() || e.record.files.is_empty() {
                continue;
            }
            let files: Vec<String> = e.record.files.iter().map(|f| show_path(&dir.join(f), &cwd)).collect();
            let what = format!(
                "The person made the 3D asset \"{}\" ({}) and imported it: {}. Check it with look_at_model before using it.",
                e.record.name,
                e.record.id,
                files.join(", ")
            );
            self.report_presented(&Shown::Asset(dir), what, cx);
        }
    }
}

/// A file's name, for its tile.
fn file_label(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// A path as the agent would name it: in the project when it is.
pub(super) fn show_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}
