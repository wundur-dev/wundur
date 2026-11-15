//! Settings > Account: signing in to a Wundur account ([`wundur_core::account`]), the plan and
//! what's left this month, upgrading, billing, signing out; the API's address.
//!
//! The state is a global ([`AccountUi`]), so the page, the window (which refreshes it when it
//! becomes active again, after a checkout in the browser) and the control channel share it.
//! Every network call runs in the background.

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
}

impl Global for AccountUi {}

/// Set up at start: the state, and the API's address from the settings.
pub fn init(cx: &mut App) {
    let base = cx.global::<crate::AppSettings>().0.borrow().account.api_base.clone();
    account::shared().set_api_base(&base);
    cx.set_global(AccountUi::default());
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
                Ok(me) => update(cx, |s| {
                    s.loading = false;
                    s.signed_in = Some(me.is_some());
                    s.me = me;
                    s.error = match key {
                        Some(Err(e)) => Some(format!("Couldn't make this machine's key: {e}")),
                        _ => None,
                    };
                }),
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

/// Refresh unless it was moments ago (the window became active again).
pub fn refresh_soon(cx: &mut App) {
    let recent = cx.try_global::<AccountUi>().and_then(|s| s.last_fetch).is_some_and(|t| t.elapsed() < Duration::from_secs(5));
    let signed_in = cx.try_global::<AccountUi>().and_then(|s| s.signed_in).unwrap_or(false);
    if signed_in && !recent {
        refresh(cx);
    }
}

/// The session is gone (signed out here, or the server refused the refresh token): revoke this
/// machine's key, forget it and the tokens.
fn signed_out(note: Option<String>, cx: &mut App) {
    let key_id = cx.global::<crate::AppSettings>().0.borrow().account.key_id.clone();
    update(cx, |s| s.busy = Some("Signing out…"));
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_spawn(async move { account::shared().sign_out(Some(&key_id)) }).await;
        cx.update(|cx| {
            change(cx, |s| s.account.key_id.clear());
            update(cx, |s| {
                *s = AccountUi { signed_in: Some(false), error: note, last_fetch: s.last_fetch, ..Default::default() };
            });
        });
    })
    .detach();
}

/// Start signing in. `open`: open the browser at the sign-in page (the control channel's tests
/// don't). The listener's port, for tests.
pub fn sign_in(open: bool, cx: &mut App) -> Result<u16, String> {
    if cx.global::<AccountUi>().waiting.is_some() {
        return Err("already waiting for the browser".into());
    }
    let sign_in = account::shared().begin_sign_in().map_err(|e| format!("Couldn't start signing in: {e}"))?;
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
fn open_from(busy: &'static str, ask: fn() -> Result<String, Error>, cx: &mut App) {
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
    }
    out.push_str(&format!(" key={}", if account::shared().api_key().is_some() { "kept" } else { "none" }));
    out.push_str(&format!(" api={}", account::shared().api_base()));
    if let Some(e) = &s.error {
        out.push_str(&format!(" error={e:?}"));
    }
    out
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
            .child(div().flex().pt_1().child(Button::new("account-sign-in").small().primary().label("Sign in").disabled(state.signed_in.is_none()).on_click(|_, _, cx| {
                if let Err(e) = sign_in(true, cx) {
                    update(cx, |s| s.error = Some(e));
                }
            })))
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
                body = body
                    .child(row("Signed in as", if me.email.is_empty() { "your account".into() } else { me.email.clone() }))
                    .child(row("Plan", plan))
                    .child(div().pt_2().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Left this month"))
                    .child(row("Images", left(a.images, "images")))
                    .child(row("3D models", left(a.approved_models, "models")))
                    .child(row("Model attempts", left(a.attempts, "attempts")))
                    .child(row("Turn summaries", left(a.summaries, "summaries")))
                    .child(note("A model counts when you import it; each try at one is an attempt.".into()));
            }
        }
        let busy = state.busy.is_some();
        let mut buttons = div().flex().gap_2().pt_2();
        if state.me.as_ref().is_some_and(Me::is_free) {
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
