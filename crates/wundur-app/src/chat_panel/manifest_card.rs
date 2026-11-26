//! Cards drawn from manifests the service sends (see `crate::card_store`, `wundur_cards` and
//! `docs/card-manifests.md`). The card draws itself and runs its own actions; this is its
//! owner in the chat: it carries out what only the app can (asking the service, saving files
//! into the project, opening and playing them), keeps the card's record so it comes back with
//! its chat, and takes it out when it's closed.
//!
//! A card's record lives with the app, not in the project (`wundur_core::cards::remote`).
//! Spending is only ever the person's click on the card: nothing here asks on its own but the
//! manifest's refresh, which the service never charges for.

use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use serde_json::{Value, json};
use wundur_cards::{CardEvent, CardView, Hooks};
use wundur_core::cards::remote::{self, Record};
use wundur_core::cards::Effect;

use super::*;
use crate::card_store;

/// A card in this chat.
pub(super) struct CardHost {
    view: Entity<CardView>,
    /// The store's revision its manifest and account were taken at.
    seen: u64,
    _subs: [Subscription; 2],
}

/// One entry of the Create menu.
#[derive(Debug, PartialEq)]
pub(super) enum Entry {
    /// A card built into the app, by the id a workflow of the service's would replace it under.
    BuiltIn(&'static str),
    /// A workflow the service offers.
    Service(remote::Listed),
}

/// The cards built into the app, in the menu's order.
const BUILT_IN: [&str; 3] = ["asset-3d", "image", "sound-effect"];

/// The Create menu for a signed-in account: each built-in card, unless the service offers a
/// workflow of the same id (its manifest's card takes that place); then the service's other
/// workflows, in its order.
pub(super) fn create_menu(offered: Vec<remote::Listed>) -> Vec<Entry> {
    let mut menu: Vec<Entry> = BUILT_IN.iter().map(|id| match offered.iter().find(|l| l.id == *id) {
        Some(l) => Entry::Service(l.clone()),
        None => Entry::BuiltIn(id),
    }).collect();
    menu.extend(offered.into_iter().filter(|l| !BUILT_IN.contains(&l.id.as_str())).map(Entry::Service));
    menu
}

/// What a card saved, as the agent reads it: the paths in the project.
fn saved_names(saved: &Value) -> String {
    let names: Vec<String> = saved
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string).or_else(|| v.get("path").and_then(Value::as_str).map(str::to_string)))
        .collect();
    if names.is_empty() { saved.to_string() } else { names.join(", ") }
}

/// Whether a file is a picture the viewer shows.
fn is_picture(path: &Path) -> bool {
    matches!(path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "webp" | "gif"))
}

/// A path a card names, in the project: relative, and never above it.
fn in_project(project: &Path, target: &str) -> Option<PathBuf> {
    let path = Path::new(target);
    path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)).then(|| project.join(path))
}

thread_local! {
    /// Pictures at links that cards have asked for: the file each was fetched to, or nothing
    /// while it's being fetched (or when it couldn't be: asked again the next time the app runs).
    static FETCHED: RefCell<HashMap<String, Option<PathBuf>>> = RefCell::default();
}

/// A picture at one of the service's links: fetched once to a file of the app's, off the UI
/// thread, and drawn from there as any other picture is. A still block until it's here.
fn remote_picture(url: &str, fit: ObjectFit, cx: &mut App) -> AnyElement {
    match FETCHED.with_borrow(|f| f.get(url).cloned()) {
        Some(Some(path)) => return crate::thumbs::picture(&path, crate::thumbs::LARGE).size_full().object_fit(fit).into_any_element(),
        Some(None) => {}
        None => {
            FETCHED.with_borrow_mut(|f| f.insert(url.to_string(), None));
            let url = url.to_string();
            cx.spawn(async move |cx: &mut AsyncApp| {
                let link = url.clone();
                if let Ok(path) = cx.background_spawn(async move { remote::fetched(&link) }).await {
                    FETCHED.with_borrow_mut(|f| f.insert(url, Some(path)));
                    cx.update(|cx| cx.refresh_windows());
                }
            })
            .detach();
        }
    }
    div().size_full().bg(cx.theme().skeleton).into_any_element()
}

/// How the app draws a card's pictures and spinners: pictures decoded small and let go when
/// off screen, the spinner the system's (one drawn by GPUI asks for a frame on every step,
/// for as long as something is being made).
fn hooks() -> Hooks {
    let spinners: Rc<RefCell<HashMap<String, crate::native_spinner::NativeSpinner>>> = Default::default();
    Hooks {
        picture: Some(Rc::new(|path, fit, _| crate::thumbs::picture(path, crate::thumbs::LARGE).size_full().object_fit(fit).into_any_element())),
        remote_picture: Some(Rc::new(remote_picture)),
        spinner: Some(Rc::new(move |key, size, color, _| {
            if !crate::native_spinner::NativeSpinner::available() {
                return Icon::new(IconName::LoaderCircle).size(px(size)).text_color(color).into_any_element();
            }
            let mut kept = spinners.borrow_mut();
            kept.retain(|k, s| k == key || s.showing());
            let native = kept.entry(key.to_string()).or_default().clone();
            canvas(|_, _, _| {}, move |bounds, _, window, _| native.paint(bounds, color, window)).size_full().into_any_element()
        })),
    }
}

impl ChatPanel {
    /// Open a new card of `workflow` at the end of the chat.
    pub fn start_card(&mut self, workflow: &str, cx: &mut Context<Self>) {
        let Some(manifest) = card_store::manifest(workflow, cx) else { return };
        let Some(dir) = remote::new_folder(workflow) else { return };
        let (project, origin) = {
            let s = self.session.read(cx);
            let chat = &s.chat;
            let turn = chat.items.iter().rposition(|i| matches!(i, Item::User(_))).and_then(|from| wundur_core::summary::TurnText::key_at(chat, from));
            // Made while the agent is replying: the turn's key isn't settled yet, so the card
            // is told its place when the turn ends (see `claim_cards`).
            let session = chat.session_id.clone().filter(|_| !chat.busy);
            (s.cwd().to_path_buf(), session.map(|session| wundur_core::assets::Origin { session, turn }))
        };
        let claimed = origin.is_some();
        let record = Record { workflow: workflow.to_string(), version: manifest.version.clone(), project, form: Value::Null, ui: Value::Null, data: Value::Null, origin };
        if remote::store(&dir, &record).is_err() {
            return;
        }
        if claimed {
            self.claimed_cards.insert(dir.clone());
        }
        self.session.update(cx, |s, cx| {
            s.chat.items.push(Item::Card(dir));
            s.chat.revision += 1;
            cx.notify();
        });
    }

    /// Before drawing: each card in the chat has its view (made from its record), follows the
    /// newest manifest and account, and knows which chat it's in.
    pub(super) fn ensure_cards(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = self.session.read(cx).chat.items.iter().filter_map(|i| if let Item::Card(d) = i { Some(d.clone()) } else { None }).collect();
        if dirs.is_empty() {
            if !self.cards.is_empty() {
                self.cards.clear();
            }
            return;
        }
        self.cards.retain(|dir, _| dirs.contains(dir));
        self.claim_cards(cx);
        let revision = card_store::revision(cx);
        let catalog = crate::hosted_store::catalog(cx).0.and_then(|c| serde_json::to_value(&*c).ok()).unwrap_or(Value::Null);
        for dir in dirs {
            if let Some(host) = self.cards.get_mut(&dir) {
                // The list was fetched again: a newer layout over the card's data, and the
                // account as it is now.
                if host.seen != revision {
                    host.seen = revision;
                    let (view, account, catalog) = (host.view.clone(), card_store::account(cx), catalog.clone());
                    let manifest = remote::load(&dir).and_then(|r| card_store::manifest(&r.workflow, cx));
                    view.update(cx, |card, cx| {
                        if let Some(m) = manifest.filter(|m| m.version != card.manifest().version) {
                            card.set_manifest(m, cx);
                        }
                        card.update_state(cx, |s| {
                            s.put("account", account);
                            s.put("catalog", catalog);
                        });
                    });
                }
                continue;
            }
            let Some(record) = remote::load(&dir) else { continue };
            let Some(manifest) = card_store::manifest(&record.workflow, cx) else { continue };
            let (account, catalog) = (card_store::account(cx), catalog.clone());
            let view = cx.new(|cx| {
                let mut card = CardView::new(manifest, record.project.clone(), cx);
                card.set_hooks(hooks());
                card
            });
            // What the record kept, over what the manifest starts with (a new card kept nothing).
            let kept: serde_json::Map<String, Value> = [("form", &record.form), ("ui", &record.ui), ("data", &record.data)].into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.to_string(), v.clone())).collect();
            view.update(cx, |card, cx| {
                card.update_state(cx, |s| {
                    s.restore(&Value::Object(kept));
                    s.put("account", account);
                    s.put("catalog", catalog);
                });
                // Read back from its record: what it holds may be from before a restart.
                card.catch_up(cx);
            });
            let folder = dir.clone();
            let events = cx.subscribe(&view, move |panel, _, event: &CardEvent, cx| panel.card_event(&folder, event, cx));
            // Its height changes with what it shows.
            let drawn = cx.observe(&view, |panel, _, cx| {
                panel.list.remeasure();
                cx.notify();
            });
            self.cards.insert(dir, CardHost { view, seen: revision, _subs: [events, drawn] });
            self.list.remeasure();
        }
    }

    /// Tell each card which chat it's in, once the chat has an id (as `claim_assets` does for
    /// an asset's card): one made before wouldn't come back when the chat is read in again.
    fn claim_cards(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(PathBuf, wundur_core::assets::Origin)> = {
            let s = self.session.read(cx);
            let chat = &s.chat;
            let Some(id) = chat.session_id.clone() else { return };
            if s.host().is_remote() {
                return;
            }
            let last_turn = chat.items.iter().rposition(|i| matches!(i, Item::User(_)));
            chat.items
                .iter()
                .enumerate()
                .filter_map(|(ix, i)| match i {
                    Item::Card(d) if !self.claimed_cards.contains(d) => Some((ix, d)),
                    _ => None,
                })
                .filter(|(ix, _)| !(chat.busy && last_turn.is_some_and(|t| t < *ix)))
                .map(|(ix, d)| (d.clone(), wundur_core::assets::Origin { session: id.clone(), turn: chat.turn_key_before(ix) }))
                .collect()
        };
        for (dir, origin) in wanted {
            if let Some(mut record) = remote::load(&dir).filter(|r| r.origin.is_none()) {
                record.origin = Some(origin);
                let _ = remote::store(&dir, &record);
            }
            self.claimed_cards.insert(dir);
        }
    }

    pub(super) fn render_card(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        match self.cards.get(dir) {
            Some(host) => host.view.clone().into_any_element(),
            // Its workflow's manifest isn't here (not fetched yet, or gone with nothing kept).
            None => div().text_xs().text_color(cx.theme().muted_foreground).child("This card can't be drawn right now.").into_any_element(),
        }
    }

    fn card_event(&mut self, dir: &Path, event: &CardEvent, cx: &mut Context<Self>) {
        let Some(view) = self.cards.get(dir).map(|h| h.view.clone()) else { return };
        let effect = match event {
            CardEvent::Changed => {
                // Keep what the person and the service put in it.
                if let Some(mut record) = remote::load(dir) {
                    let card = view.read(cx);
                    let root = &card.state().root;
                    (record.form, record.ui, record.data, record.version) = (root["form"].clone(), root["ui"].clone(), root["data"].clone(), card.manifest().version.clone());
                    let _ = remote::store(dir, &record);
                }
                return;
            }
            CardEvent::Effect(effect) => effect.clone(),
        };
        let project = remote::load(dir).map(|r| r.project).unwrap_or_else(|| self.session.read(cx).cwd().to_path_buf());
        match effect {
            Effect::Call { id, request } => {
                let spent = request.spends;
                cx.spawn(async move |_, cx: &mut AsyncApp| {
                    let answer = cx.background_spawn(async move { remote::perform(&request) }).await;
                    view.update(cx, |card, cx| card.finish(id, answer, cx));
                    // What's left this month changed.
                    if spent {
                        cx.update(card_store::refresh);
                    }
                })
                .detach();
            }
            Effect::Save { id, files, folder, .. } => {
                let dir = dir.to_path_buf();
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let saved = cx.background_spawn(async move { remote::save(&files, &folder, &project) }).await;
                    // Presented by the agent: it's told what the person saved.
                    if let Ok(list) = &saved {
                        let what = format!("The person saved: {}.", saved_names(list));
                        let _ = this.update(cx, |p, cx| p.report_presented(&choice_card::Shown::Card(dir), what, cx));
                    }
                    view.update(cx, |card, cx| card.finish(id, saved, cx));
                })
                .detach();
            }
            Effect::Open(target) => self.card_file(&project, target, false, cx),
            Effect::Play(target) => self.card_file(&project, target, true, cx),
            Effect::Close => {
                self.report_presented(&choice_card::Shown::Card(dir.to_path_buf()), "The person closed the card.".into(), cx);
                self.close_card(dir, cx)
            }
            Effect::App(what) => match what.as_str() {
                "sign_in" => crate::settings_account::show_sign_in(cx),
                "upgrade" => crate::shell::open_settings_page("account", cx),
                "request_access" => crate::shell::open_access_request(cx),
                _ => {}
            },
        }
    }

    /// Open or play what a card names: a file in the project, or one of the service's links
    /// (fetched to a file of its own first).
    fn card_file(&mut self, project: &Path, target: String, play: bool, cx: &mut Context<Self>) {
        let show = move |panel: &mut Self, path: PathBuf, cx: &mut Context<Self>| {
            if play {
                play_sound(&path, cx);
            } else if is_picture(&path) {
                crate::image_view::show(path, cx);
            } else {
                cx.emit(ChatPanelEvent::OpenFile(path, None));
            }
            let _ = panel;
        };
        if target.starts_with("https://") || target.starts_with("http://") {
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let fetched = cx.background_spawn(async move { remote::fetched(&target) }).await;
                if let Ok(path) = fetched {
                    let _ = this.update(cx, |panel, cx| show(panel, path, cx));
                }
            })
            .detach();
        } else if let Some(path) = in_project(project, &target).filter(|p| p.is_file()) {
            show(self, path, cx);
        }
    }

    fn close_card(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.cards.remove(dir);
        self.claimed_cards.remove(dir);
        remote::discard(dir);
        self.session.update(cx, |s, cx| {
            s.chat.items.retain(|i| !matches!(i, Item::Card(d) if d == dir));
            s.chat.revision += 1;
            cx.notify();
        });
    }

    /// Test commands for this chat's latest manifest card: `open <workflow>` (a new card),
    /// `set <place> <json>` (a value in `form` or `ui`, as typing or picking would), `do
    /// <action>` (one of the manifest's actions, as its button would: this can spend), `state`
    /// (its data), `list` (the workflows offered).
    pub(super) fn card_command(&mut self, line: &str, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        if cmd == "list" {
            return card_store::offered(cx).iter().map(|l| format!("{} {} \"{}\"", l.id, l.version, l.label)).collect::<Vec<_>>().join("\n");
        }
        if cmd == "open" {
            self.start_card(arg.trim(), cx);
            self.ensure_cards(cx);
        }
        let latest = self.session.read(cx).chat.items.iter().rev().find_map(|i| if let Item::Card(d) = i { Some(d.clone()) } else { None });
        let Some(view) = latest.and_then(|d| self.cards.get(&d)).map(|h| h.view.clone()) else { return "no card".into() };
        match cmd {
            "set" => {
                let (place, value) = arg.split_once(' ').unwrap_or((arg, "null"));
                let place: Vec<String> = place.split('.').map(str::to_string).collect();
                if !matches!(place.first().map(String::as_str), Some("form" | "ui")) {
                    return "set form.<name>|ui.<name> <json>".into();
                }
                let value = serde_json::from_str(value).unwrap_or_else(|_| json!(value));
                view.update(cx, |card, cx| card.update_state(cx, |s| s.write(&place, value)));
            }
            "do" => view.update(cx, |card, cx| card.run(arg.trim(), cx)),
            "open" | "state" => {}
            _ => return "card open <workflow>|set <place> <json>|do <action>|state|list".into(),
        }
        let card = view.read(cx);
        let root = &card.state().root;
        json!({ "workflow": card.manifest().id, "version": card.manifest().version, "form": root["form"], "ui": root["ui"], "data": root["data"], "card": root["card"] }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn the_service_s_workflows_replace_built_in_cards_of_the_same_id_and_the_rest_follow() {
        let listed = |id: &str| remote::Listed { id: id.into(), label: format!("{id}…"), ..Default::default() };
        assert_eq!(create_menu(Vec::new()), [Entry::BuiltIn("asset-3d"), Entry::BuiltIn("image"), Entry::BuiltIn("sound-effect")], "nothing offered: the app's own");
        assert_eq!(
            create_menu(vec![listed("image"), listed("iso-building")]),
            [Entry::BuiltIn("asset-3d"), Entry::Service(listed("image")), Entry::BuiltIn("sound-effect"), Entry::Service(listed("iso-building"))]
        );
        assert_eq!(
            create_menu(vec![listed("iso-building"), listed("sound-effect"), listed("asset-3d"), listed("image")]),
            [Entry::Service(listed("asset-3d")), Entry::Service(listed("image")), Entry::Service(listed("sound-effect")), Entry::Service(listed("iso-building"))]
        );
    }

    #[::core::prelude::v1::test]
    fn a_card_opens_only_files_inside_the_project() {
        let project = Path::new("/projects/game");
        assert_eq!(in_project(project, "assets/generated/coin.png"), Some(project.join("assets/generated/coin.png")));
        assert_eq!(in_project(project, "../other/secret.txt"), None);
        assert_eq!(in_project(project, "/etc/passwd"), None);
        assert!(is_picture(Path::new("a/coin.PNG")) && !is_picture(Path::new("a/model.glb")));
    }
}
