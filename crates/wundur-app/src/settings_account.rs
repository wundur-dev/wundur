//! Settings > Account: signing in to a Wundur account ([`wundur_core::account`]), the plan and
//! what's left this month, the invite-only beta's access (asking for it), billing, signing out;
//! the API's address.
//!
//! The state is a global ([`AccountUi`]), so the page, the toolbar's account menu, the window
//! (which refreshes it when it becomes active again, so an approval shows without a restart)
//! and the control channel share it. Every network call runs in the background. The API decides
//! the access state and its words; this shows them as they come.

use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::account::{self, Cancel, Error, Me};

use crate::settings_window::change;

/// What the Account page shows.
#[derive(Default)]
pub struct AccountUi {
    /// Signed in on this machine (`None`: not looked up yet).
    signed_in: Option<bool>,
    /// A sign-in waiting for the browser.
    waiting: Option<Cancel>,
    /// The account, as last fetched.
    me: Option<Me>,
    /// Fetching it.
    loading: bool,
    /// An action under way ("Opening checkout…").
    busy: Option<&'static str>,
    /// What went wrong last.
    error: Option<String>,
    last_fetch: Option<Instant>,
    /// A request for access on its way.
    requesting: bool,
}

impl Global for AccountUi {}

/// Set up at start: the state, and the API's address from the settings.
pub fn init(cx: &mut App) {
    // An address saved before the API had its domain moves to it (kept: a development one).
    let mut config = cx.global::<crate::AppSettings>().0.borrow().account.clone();
    if config.migrate() {
        change(cx, |s| s.account.api_base.clear());
    }
    account::shared().set_api_base(&config.api_base);
    cx.set_global(AccountUi::default());
}

/// The account as last fetched (`None`: signed out, or not fetched yet).
pub fn me(cx: &App) -> Option<&Me> {
    cx.try_global::<AccountUi>().and_then(|s| s.me.as_ref())
}

/// Signed in (`None`: not looked up yet).
pub fn signed_in(cx: &App) -> Option<bool> {
    cx.try_global::<AccountUi>().and_then(|s| s.signed_in)
}

/// Waiting for the browser to come back from signing in.
pub fn waiting(cx: &App) -> bool {
    cx.try_global::<AccountUi>().is_some_and(|s| s.waiting.is_some())
}

/// Whether Request access is the way on (the API says the account may ask).
pub fn can_request(cx: &App) -> bool {
    me(cx).is_some_and(|m| m.access.can_request)
}

/// A request for access is on its way.
pub fn requesting(cx: &App) -> bool {
    cx.try_global::<AccountUi>().is_some_and(|s| s.requesting)
}

/// What went wrong last, for the menu.
pub fn error(cx: &App) -> Option<String> {
    cx.try_global::<AccountUi>().and_then(|s| s.error.clone())
}

/// Keep `me`; when its plan or access changed, the catalog (which differs by plan) is fetched
/// again.
fn set_me(s: &mut AccountUi, me: Option<Me>) -> bool {
    let changed = match (&s.me, &me) {
        (Some(a), Some(b)) => a.plan != b.plan || a.access.state != b.access.state,
        _ => false,
    };
    s.me = me;
    changed
}

fn update(cx: &mut App, f: impl FnOnce(&mut AccountUi)) {
    cx.update_global::<AccountUi, _>(|s, _| f(s));
    cx.refresh_windows();
}

/// Look up whether signed in and fetch the account (and make this machine's key if it has
/// none). In the background; once at a time.
pub fn refresh(cx: &mut App) {
    if cx.try_global::<AccountUi>().is_none_or(|s| s.loading || s.waiting.is_some()) {
        return;
    }
    update(cx, |s| {
        s.loading = true;
        s.last_fetch = Some(Instant::now());
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let (me, key) = cx
            .background_spawn(async move {
                let a = account::shared();
                if !a.is_signed_in() {
                    return (Ok(None), None);
                }
                let me = a.me().map(Some);
                // Signed in but without a key (making it failed at sign-in): try again.
                let key = (me.is_ok() && a.api_key().is_none()).then(|| a.create_machine_key());
                (me, key)
            })
            .await;
        cx.update(|cx| {
            if let Some(Ok(info)) = &key {
                let id = info.id.clone();
                change(cx, move |s| s.account.key_id = id);
            }
            match me {
                Ok(me) => {
                    let mut changed = false;
                    update(cx, |s| {
                        s.loading = false;
                        s.signed_in = Some(me.is_some());
                        changed = set_me(s, me);
                        s.error = match key {
                            Some(Err(e)) => Some(format!("Couldn't make this machine's key: {e}")),
                            _ => None,
                        };
                    });
                    if changed {
                        crate::hosted_store::catalog_changed(cx);
                    }
                }
                Err(Error::SignedOut) => {
                    update(cx, |s| s.loading = false);
                    signed_out(Some("Your sign-in has expired. Sign in again.".into()), cx);
                }
                Err(e) => update(cx, |s| {
                    s.loading = false;
                    s.signed_in = Some(account::shared().known_signed_in().unwrap_or(true));
                    s.error = Some(format!("Couldn't get your account: {e}"));
                }),
            }
        });
    })
    .detach();
}

/// Refresh unless it was moments ago (the window became active again), or the first time.
pub fn refresh_soon(cx: &mut App) {
    let recent = cx.try_global::<AccountUi>().and_then(|s| s.last_fetch).is_some_and(|t| t.elapsed() < Duration::from_secs(5));
    // Not looked up yet: look (whether signed in at all is the key store's to say).
    let signed_in = cx.try_global::<AccountUi>().and_then(|s| s.signed_in).unwrap_or(true);
    if signed_in && !recent {
        refresh(cx);
    }
}

/// For the control channel's tests: show `json` (a `GET /v1/me` answer) as the signed-in
/// account, without the network.
pub fn show_me_json(json: &str, cx: &mut App) -> Result<(), String> {
    let me: Me = serde_json::from_str(json).map_err(|e| format!("not a /v1/me answer: {e}"))?;
    update(cx, |s| {
        s.signed_in = Some(true);
        s.me = Some(me);
        s.last_fetch = Some(Instant::now());
    });
    Ok(())
}

/// Ask for beta access, with an optional note (cut to [`account::ACCESS_NOTE_MAX`]
/// characters). `done` hears how it went: `Ok` with the API's message, or what went wrong.
pub fn request_access(note: String, done: impl FnOnce(Result<String, String>, &mut App) + 'static, cx: &mut App) {
    if requesting(cx) {
        return;
    }
    let note: String = note.trim().chars().take(account::ACCESS_NOTE_MAX).collect();
    update(cx, |s| {
        s.requesting = true;
        s.error = None;
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx.background_spawn(async move { account::shared().request_access(&note) }).await;
        cx.update(|cx| {
            update(cx, |s| s.requesting = false);
            match got {
                Ok(access) => {
                    let message = access.message.clone();
                    let mut changed = false;
                    update(cx, |s| {
                        if let Some(mut me) = s.me.clone() {
                            me.access = access;
                            changed = set_me(s, Some(me));
                        }
                    });
                    if changed {
                        crate::hosted_store::catalog_changed(cx);
                    }
                    done(Ok(message), cx);
                }
                Err(Error::SignedOut) => {
                    signed_out(Some("Your sign-in has expired. Sign in again.".into()), cx);
                    done(Err("Your sign-in has expired. Sign in again.".into()), cx);
                }
                // Not allowed now (declined, already in, paid): the account as it stands says why.
                Err(Error::Api(e)) if e.status == 409 => {
                    refresh(cx);
                    done(Err(if e.message.is_empty() { "Access can't be asked for on this account now.".into() } else { e.message }), cx);
                }
                Err(Error::Api(e)) if e.status == 422 => {
                    done(Err(if e.message.is_empty() { format!("The note is too long: {} characters at most.", account::ACCESS_NOTE_MAX) } else { e.message }), cx)
                }
                Err(e) => done(Err(format!("Couldn't ask for access: {e}")), cx),
            }
        });
    })
    .detach();
}

/// The session is gone (signed out here, or the server refused the refresh token): revoke this
/// machine's key, forget it and the tokens.
pub fn signed_out(note: Option<String>, cx: &mut App) {
    let key_id = cx.global::<crate::AppSettings>().0.borrow().account.key_id.clone();
    update(cx, |s| s.busy = Some("Signing out…"));
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_spawn(async move { account::shared().sign_out(Some(&key_id)) }).await;
        cx.update(|cx| {
            change(cx, |s| s.account.key_id.clear());
            update(cx, |s| {
                *s = AccountUi { signed_in: Some(false), error: note, last_fetch: s.last_fetch, ..Default::default() };
            });
            crate::hosted_store::catalog_changed(cx);
        });
    })
    .detach();
}

/// Start signing in. `open`: open the browser at the sign-in page (the control channel's tests
/// don't). The listener's port, for tests.
pub fn sign_in(open: bool, cx: &mut App) -> Result<u16, String> {
    sign_in_with(open, false, cx)
}

/// [`sign_in`]; `create`: open on the sign-up page (Create account).
pub fn sign_in_with(open: bool, create: bool, cx: &mut App) -> Result<u16, String> {
    if cx.global::<AccountUi>().waiting.is_some() {
        return Err("already waiting for the browser".into());
    }
    let sign_in = account::shared().begin_sign_in_with(create).map_err(|e| format!("Couldn't start signing in: {e}"))?;
    let port = sign_in.port();
    if open {
        cx.open_url(&sign_in.url);
    }
    let cancel = sign_in.canceller();
    update(cx, |s| {
        s.waiting = Some(cancel);
        s.error = None;
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let done = cx.background_spawn(async move { account::shared().finish_sign_in(sign_in, account::SIGN_IN_TIMEOUT) }).await;
        cx.update(|cx| match done {
            Ok(signed) => {
                let error = match &signed.key {
                    Ok(info) => {
                        let id = info.id.clone();
                        change(cx, move |s| s.account.key_id = id);
                        None
                    }
                    Err(e) => Some(format!("Signed in, but couldn't make this machine's key: {e}")),
                };
                let fetched = signed.me.is_some();
                update(cx, |s| {
                    s.waiting = None;
                    s.signed_in = Some(true);
                    s.me = signed.me;
                    s.error = error;
                });
                // A different account (or the first): its own catalog.
                crate::hosted_store::catalog_changed(cx);
                if !fetched {
                    refresh(cx);
                }
            }
            Err(Error::Cancelled) => update(cx, |s| s.waiting = None),
            Err(e) => update(cx, |s| {
                s.waiting = None;
                s.error = Some(match e {
                    Error::TimedOut => "The browser didn't come back within 5 minutes. Try again.".into(),
                    e => format!("Not signed in: {e}"),
                });
            }),
        });
    })
    .detach();
    Ok(port)
}

/// Stop waiting for the browser.
pub fn cancel(cx: &mut App) {
    if let Some(c) = cx.global::<AccountUi>().waiting.clone() {
        c.cancel();
    }
}

/// Ask the API for an address (checkout, billing portal) and open it in the browser.
pub fn open_from(busy: &'static str, ask: fn() -> Result<String, Error>, cx: &mut App) {
    update(cx, |s| {
        s.busy = Some(busy);
        s.error = None;
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let url = cx.background_spawn(async move { ask() }).await;
        cx.update(|cx| {
            update(cx, |s| s.busy = None);
            match url {
                Ok(url) => cx.open_url(&url),
                Err(Error::SignedOut) => signed_out(Some("Your sign-in has expired. Sign in again.".into()), cx),
                // Checkout is closed in the beta: asking for access is the way on.
                Err(e) if wundur_core::asset_flow::needs_access(&e) => {
                    refresh(cx);
                    crate::shell::open_access_request(cx);
                }
                Err(e) => update(cx, |s| s.error = Some(format!("Couldn't open it: {e}"))),
            }
        });
    })
    .detach();
}

/// For the control channel: signed in or not, the plan; never tokens or the key.
pub fn describe(cx: &App) -> String {
    let Some(s) = cx.try_global::<AccountUi>() else { return "no account state".into() };
    let signed_in = match s.signed_in {
        None => "unknown",
        Some(true) => "yes",
        Some(false) => "no",
    };
    let mut out = format!("signed_in={signed_in} waiting={} loading={}", s.waiting.is_some(), s.loading);
    if let Some(me) = &s.me {
        out.push_str(&format!(" plan={} status={} images_left={}", me.plan, if me.status.is_empty() { "-" } else { &me.status }, me.allowance.images.left));
        out.push_str(&format!(" access={} can_request={}", if me.access.state.is_empty() { "-" } else { &me.access.state }, me.access.can_request));
    }
    out.push_str(&format!(" key={}", if account::shared().api_key().is_some() { "kept" } else { "none" }));
    out.push_str(&format!(" api={}", account::shared().api_base()));
    if let Some(e) = &s.error {
        out.push_str(&format!(" error={e:?}"));
    }
    out
}

/// Sign in (or create an account) from a button: the browser opens; a failure to start shows.
pub fn start_sign_in(create: bool, cx: &mut App) {
    if let Err(e) = sign_in_with(true, create, cx) {
        update(cx, |s| s.error = Some(e));
    }
}

/// "1 December 2026" from an ISO 8601 time.
fn day(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso).map(|t| t.format("%-d %B %Y").to_string()).unwrap_or_else(|_| iso.chars().take(10).collect())
}

/// The page.
pub fn render(_window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, danger) = (theme.foreground, theme.muted_foreground, theme.danger);
    let Some(state) = cx.try_global::<AccountUi>() else { return div().into_any_element() };
    let note = |t: String| div().text_sm().text_color(muted).child(t);
    let mut body = div().w_full().flex().flex_col().gap_2();

    if let Some(_waiting) = &state.waiting {
        body = body
            .child(div().font_weight(FontWeight::MEDIUM).text_color(fg).child("Waiting for the browser…"))
            .child(note("Sign in on the page that opened in your browser, then come back here.".into()))
            .child(div().flex().child(Button::new("account-cancel").small().outline().label("Cancel").on_click(|_, _, cx| cancel(cx))));
    } else if state.signed_in != Some(true) {
        body = body
            .child(note(
                "A Wundur account gives you images, 3D models and turn summaries without your own keys. The free plan has a small monthly allowance to try them.".into(),
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_1()
                    .child(Button::new("account-sign-in").small().primary().label("Sign in").disabled(state.signed_in.is_none()).on_click(|_, _, cx| start_sign_in(false, cx)))
                    .child(Button::new("account-create").small().outline().label("Create account").disabled(state.signed_in.is_none()).on_click(|_, _, cx| start_sign_in(true, cx))),
            )
            .child(note("You must be 13 or older to create an account.".into()));
    } else {
        match &state.me {
            None => {
                body = body.child(note(if state.loading { "Getting your account…".into() } else { "Signed in.".into() }));
            }
            Some(me) => {
                let row = |label: &str, value: String| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(150.)).flex_none().text_sm().text_color(muted).child(label.to_string()))
                        .child(div().flex_1().min_w_0().text_sm().text_color(fg).child(value))
                };
                let mut plan = me.plan_name();
                if let Some(status) = me.status_note() {
                    plan.push_str(&format!(" ({status})"));
                }
                if let Some(end) = me.period_end.as_deref().filter(|e| !e.is_empty()) {
                    let verb = if matches!(me.status.as_str(), "canceled" | "cancelled") { "ends" } else { "renews" };
                    plan.push_str(&format!(", {verb} {}", day(end)));
                }
                let left = |u: account::Usage, what: &str| format!("{} of {} {what} left", u.left, u.limit);
                let a = &me.allowance;
                let name = (!me.name.is_empty()).then(|| me.name.clone()).or_else(|| account::shared().display_name());
                body = body
                    .when_some(name, |d, n| d.child(row("Name", n)))
                    .child(row("Signed in as", if me.email.is_empty() { "your account".into() } else { me.email.clone() }))
                    .child(row("Plan", plan))
                    .when(!me.access.message.is_empty(), |d| d.child(row("Access", me.access.message.clone())))
                    .child(div().pt_2().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Left this month"))
                    .child(row("Images", left(a.images, "images")))
                    // Credit buckets when the API has them; the older counts until then.
                    .when_some(a.model_credits, |d, u| d.child(row("3D models", left(u, "model credits"))))
                    .when(a.model_credits.is_none(), |d| {
                        d.child(row("3D models", left(a.approved_models, "models"))).child(row("Model attempts", left(a.attempts, "attempts")))
                    })
                    .when_some(a.sound_seconds, |d, u| d.child(row("Sound effects", left(u, "seconds"))))
                    .child(row("Turn summaries", left(a.summaries, "summaries")))
                    .child(note("A model counts when you import it; each try at one is an attempt.".into()));
            }
        }
        let busy = state.busy.is_some();
        let mut buttons = div().flex().gap_2().pt_2();
        if state.me.as_ref().is_some_and(|m| m.access.can_request) {
            buttons = buttons.child(
                Button::new("account-request").small().primary().label("Request access…").disabled(busy || state.requesting).on_click(|_, _, cx| crate::shell::open_access_request(cx)),
            );
        }
        if state.me.as_ref().is_some_and(Me::offers_upgrade) {
            buttons = buttons.child(Button::new("account-upgrade").small().primary().label("Upgrade").disabled(busy).on_click(|_, _, cx| {
                open_from("Opening checkout…", || account::shared().checkout("monthly"), cx)
            }));
        }
        if state.me.as_ref().is_some_and(|m| m.can_manage_billing) {
            buttons = buttons.child(Button::new("account-billing").small().outline().label("Manage billing").disabled(busy).on_click(|_, _, cx| {
                open_from("Opening billing…", || account::shared().billing_portal(), cx)
            }));
        }
        buttons = buttons.child(Button::new("account-sign-out").small().ghost().label("Sign out").disabled(busy).on_click(|_, _, cx| signed_out(None, cx)));
        body = body.child(buttons);
    }
    if let Some(busy) = state.busy {
        body = body.child(note(busy.into()));
    }
    if let Some(e) = &state.error {
        body = body.child(div().text_sm().text_color(danger).child(e.clone()));
    }
    body.into_any_element()
}
