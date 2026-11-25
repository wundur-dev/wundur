//! The workflows the person's Wundur account offers, and their manifests (see
//! `wundur_core::cards::remote` and `docs/card-manifests.md`): what the Create menu lists
//! beyond the cards built into the app, and what a manifest card is drawn from.
//!
//! The list as last fetched is read from disk first, so the menu is whole before the service
//! answers; it is asked for again when first wanted, every [`AGE`] after, and when the account
//! changes. Signed out there is no list: every workflow needs the account.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::*;
use serde_json::{Value, json};
use wundur_core::cards::Manifest;
use wundur_core::cards::remote::{self, Listed};

/// How old the list may be before it's asked for again.
const AGE: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
struct Store {
    offered: Vec<(Listed, Arc<Manifest>)>,
    /// Manifests of workflows no longer offered, for cards of them still in a chat.
    others: HashMap<String, Option<Arc<Manifest>>>,
    /// `account` as a card reads it.
    account: Value,
    read_kept: bool,
    loading: bool,
    asked: Option<Instant>,
    revision: u64,
}

impl Global for Store {}

/// Goes up when the list, a manifest or the account changes.
pub fn revision(cx: &App) -> u64 {
    cx.try_global::<Store>().map_or(0, |s| s.revision)
}

fn look(cx: &mut App) {
    let signed_in = crate::hosted_store::signed_in();
    let store = cx.default_global::<Store>();
    if !signed_in {
        if !store.offered.is_empty() || store.account.get("signed_in") != Some(&json!(false)) {
            store.offered.clear();
            store.account = json!({ "signed_in": false });
            store.asked = None;
            store.revision += 1;
        }
        return;
    }
    if !store.read_kept {
        store.read_kept = true;
        store.account = json!({ "signed_in": true });
        store.offered = remote::kept_list().into_iter().filter_map(|l| remote::kept_manifest(&l.id, Some(&l.version)).filter(Manifest::supported).map(|m| (l, Arc::new(m)))).collect();
        store.revision += 1;
    }
    if store.loading || store.asked.is_some_and(|at| at.elapsed() < AGE) {
        return;
    }
    store.loading = true;
    store.asked = Some(Instant::now());
    cx.spawn(async move |cx: &mut AsyncApp| {
        let (fetched, account) = cx.background_spawn(async move { (remote::fetch(), remote::account_scope()) }).await;
        cx.update(|cx| {
            let store = cx.default_global::<Store>();
            store.loading = false;
            store.account = account;
            // A failure keeps the list there was: the menu doesn't empty because a request did.
            if let Ok(offered) = fetched {
                store.offered = offered.into_iter().map(|(l, m)| (l, Arc::new(m))).collect();
                store.others.clear();
            }
            store.revision += 1;
            cx.refresh_windows();
        });
    })
    .detach();
}

/// The workflows to offer, in the menu's order. Empty while signed out.
pub fn offered(cx: &mut App) -> Vec<Listed> {
    look(cx);
    cx.global::<Store>().offered.iter().map(|(l, _)| l.clone()).collect()
}

/// The manifest to draw a card of `workflow` with: the one offered now, else the last one
/// kept (a workflow turned off still draws the cards people have).
pub fn manifest(workflow: &str, cx: &mut App) -> Option<Arc<Manifest>> {
    look(cx);
    let store = cx.global_mut::<Store>();
    if let Some((_, m)) = store.offered.iter().find(|(l, _)| l.id == workflow) {
        return Some(m.clone());
    }
    store.others.entry(workflow.to_string()).or_insert_with(|| remote::kept_manifest(workflow, None).filter(Manifest::supported).map(Arc::new)).clone()
}

/// `account` as a card reads it: what `GET /v1/me` says, with `signed_in`.
pub fn account(cx: &mut App) -> Value {
    look(cx);
    cx.global::<Store>().account.clone()
}

/// The account changed (signed in or out, a plan, beta access): ask again.
pub fn changed(cx: &mut App) {
    if !crate::hosted_store::signed_in() {
        remote::forget();
    }
    let store = cx.default_global::<Store>();
    store.asked = None;
    store.read_kept = false;
    store.revision += 1;
    look(cx);
    cx.refresh_windows();
}

/// Ask the service again now (a card just spent something: what's left changed).
pub fn refresh(cx: &mut App) {
    cx.default_global::<Store>().asked = None;
    look(cx);
}
