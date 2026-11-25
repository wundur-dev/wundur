//! Settings > Account: the Wundur account ([`wundur_core::account`]): the plan and what's left
//! this month, the invite-only beta's access (asking for it), billing, signing out; the API's
//! address. Signing in (a provider in the browser, or a code sent by email) is the sign-in
//! dialog's ([`crate::sign_in_dialog`]); its state lives here.
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
use wundur_core::account::{self, Cancel, Error, Me, Provider, SignedIn};

use crate::settings_window::change;

/// What the Account page shows.
#[derive(Default)]
pub struct AccountUi {
    /// Signed in on this machine (`None`: not looked up yet).
    pub(crate) signed_in: Option<bool>,
    /// A sign-in waiting for the browser.
    pub(crate) waiting: Option<Cancel>,
    /// Signing in by email: the address a code went to (the code's field shows).
    pub(crate) code_sent_to: Option<String>,
    /// Each code sent gets a fresh field.
    pub(crate) code_round: usize,
    /// Sending a code, or checking one ("Sending a code…").
    pub(crate) email_busy: Option<&'static str>,
    /// Signed in on the key, but signing in again (the session lapsed).
    pub(crate) reauth: bool,
    /// The sign-in dialog is open.
    pub(crate) dialog: bool,
    /// Each opening of the dialog gets fresh fields.
    pub(crate) dialog_round: usize,
    /// The provider's page, while waiting for the browser (Open the page again).
    pub(crate) provider_url: Option<String>,
    /// What went wrong signing in with a provider (shown under its button).
    pub(crate) provider_error: Option<String>,
    /// When the last code was sent (Send again waits a little).
    pub(crate) code_sent_at: Option<Instant>,
    /// The dialog's email step (Continue with email), before a code is sent.
    pub(crate) email_step: bool,
    /// The address last typed, for the field when coming back to it.
    pub(crate) last_email: String,
    /// The code's length (`0`: [`crate::sign_in_dialog::CODE_LENGTH`]); a pasted code of
    /// another length sets it.
    pub(crate) code_len: usize,
    /// Signed in from the dialog: who, shown a moment before it closes.
    pub(crate) welcome: Option<String>,
    /// The account, as last fetched.
    me: Option<Me>,
    /// Fetching it.
    loading: bool,
    /// An action under way ("Opening checkout…").
    busy: Option<&'static str>,
    /// What went wrong last.
    pub(crate) error: Option<String>,
    last_fetch: Option<Instant>,
    /// A request for access on its way.
    requesting: bool,
    /// Signed in on this machine's key, but the session has lapsed: what needs it (billing, a
    /// new key) asks to sign in again; everything else goes on.
    session_lapsed: bool,
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
        let (me, key, session) = cx
            .background_spawn(async move {
                let a = account::shared();
                // The Keychain can't be read right now (locked after sleep): nothing is known,
                // and nothing is signed out for it.
                if let Some(e) = a.store_error() {
                    return (Err(Error::Store(e)), None, true);
                }
                if !a.is_signed_in() {
                    return (Ok(None), None, false);
                }
                // This machine's key first: it doesn't expire, so a lapsed session doesn't sign
                // anyone out.
                let mut me = a.me_any();
                if a.api_key().is_some() && matches!(&me, Err(Error::Api(e)) if e.status == 401) {
                    // The key was revoked elsewhere: forget it here; a session makes a new one.
                    a.forget_key();
                    me = if a.has_session() { a.me() } else { Err(Error::SignedOut) };
                }
                // Signed in but without a key (making it failed at sign-in): try again.
                let key = (me.is_ok() && a.api_key().is_none() && a.has_session()).then(|| a.create_machine_key());
                (me.map(Some), key, a.has_session())
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
                        s.session_lapsed = me.is_some() && !session;
                        changed = set_me(s, me);
                        s.error = match key {
                            Some(Err(e)) => Some(format!("Couldn't make this machine's key: {e}")),
                            _ => None,
                        };
                    });
                    if changed {
                        crate::hosted_store::catalog_changed(cx);
                        crate::card_store::changed(cx);
                    }
                }
                // Neither a session nor a key left: signed out (nothing to revoke).
                Err(Error::SignedOut) => {
                    change(cx, |s| s.account.key_id.clear());
                    update(cx, |s| {
                        *s = AccountUi { signed_in: Some(false), error: Some("Your sign-in has expired. Sign in again.".into()), last_fetch: s.last_fetch, ..Default::default() };
                    });
                    crate::hosted_store::catalog_changed(cx);
                    crate::card_store::changed(cx);
                }
                // Not known now: keep what was shown; the next refresh tries again.
                Err(Error::Store(e)) => update(cx, |s| {
                    s.loading = false;
                    s.error = Some(format!("Couldn't read the Keychain ({e}); trying again later."));
                }),
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
                        crate::card_store::changed(cx);
                    }
                    done(Ok(message), cx);
                }
                Err(Error::SignedOut) => {
                    session_needed(cx);
                    done(Err("Sign in again to ask for access (your sign-in has lapsed).".into()), cx);
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

/// Something needed the session and it has lapsed: say so (Sign in again), without signing out.
fn session_needed(cx: &mut App) {
    update(cx, |s| s.session_lapsed = true);
}

/// Whether the session has lapsed (the menu offers Sign in again).
pub fn session_lapsed(cx: &App) -> bool {
    cx.try_global::<AccountUi>().is_some_and(|s| s.session_lapsed)
}

/// Signing out, asked for here: revoke this machine's key, forget it and the tokens.
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
            crate::card_store::changed(cx);
        });
    })
    .detach();
}

/// Start signing in with Apple. `open`: open the browser at the sign-in page (the control
/// channel's tests don't). The listener's port, for tests.
pub fn sign_in(open: bool, cx: &mut App) -> Result<u16, String> {
    sign_in_with(Provider::Apple, open, cx)
}

/// Sign in with `provider` in the browser; `open` as [`sign_in`].
pub fn sign_in_with(provider: Provider, open: bool, cx: &mut App) -> Result<u16, String> {
    if cx.global::<AccountUi>().waiting.is_some() {
        return Err("already waiting for the browser".into());
    }
    let sign_in = account::shared().begin_sign_in(provider).map_err(|e| format!("Couldn't start signing in: {e}"))?;
    let port = sign_in.port();
    if open {
        cx.open_url(&sign_in.url);
    }
    let cancel = sign_in.canceller();
    let url = sign_in.url.clone();
    update(cx, |s| {
        s.waiting = Some(cancel);
        s.provider_url = Some(url);
        s.provider_error = None;
        s.error = None;
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let done = cx.background_spawn(async move { account::shared().finish_sign_in(sign_in, account::SIGN_IN_TIMEOUT) }).await;
        cx.update(|cx| match done {
            Ok(signed) => finished(signed, cx),
            Err(Error::Cancelled) => update(cx, |s| {
                s.waiting = None;
                s.provider_url = None;
            }),
            Err(e) => update(cx, |s| {
                s.waiting = None;
                s.provider_url = None;
                s.provider_error = Some(match e {
                    Error::TimedOut => "The browser didn't come back within 5 minutes. Try again.".into(),
                    Error::Denied { error, .. } if error == "access_denied" => "Signing in was cancelled.".into(),
                    Error::Api(a) if a.code.starts_with("flow_state_") => "That sign-in took too long or was already used. Try again.".into(),
                    Error::Network(_) => "Can't reach the sign-in service. Check your connection and try again.".into(),
                    e => format!("Not signed in: {e}"),
                });
            }),
        });
    })
    .detach();
    Ok(port)
}

/// Open the provider's page again (the browser tab was closed).
pub fn reopen_provider_page(cx: &mut App) {
    if let Some(url) = cx.global::<AccountUi>().provider_url.clone() {
        cx.open_url(&url);
    }
}

/// Signed in (a provider or an email code): keep the key's id, show the account.
fn finished(signed: SignedIn, cx: &mut App) {
    let error = match &signed.key {
        Ok(Some(info)) => {
            let id = info.id.clone();
            change(cx, move |s| s.account.key_id = id);
            None
        }
        // The key already kept still works.
        Ok(None) => None,
        Err(e) => Some(format!("Signed in, but couldn't make this machine's key: {e}")),
    };
    let fetched = signed.me.is_some();
    let who = signed
        .me
        .as_ref()
        .and_then(|m| (!m.name.is_empty()).then(|| m.name.clone()).or_else(|| (!m.email.is_empty()).then(|| m.email.clone())))
        .or_else(|| account::shared().display_name())
        .or_else(|| cx.global::<AccountUi>().code_sent_to.clone());
    let in_dialog = cx.global::<AccountUi>().dialog;
    update(cx, |s| {
        s.waiting = None;
        s.provider_url = None;
        s.provider_error = None;
        s.code_sent_at = None;
        if s.dialog {
            s.welcome = Some(who.unwrap_or_default());
        }
        s.code_sent_to = None;
        s.email_busy = None;
        s.reauth = false;
        s.signed_in = Some(true);
        s.session_lapsed = false;
        s.me = signed.me;
        s.error = error;
    });
    // A different account (or the first): its own catalog.
    crate::hosted_store::catalog_changed(cx);
    crate::card_store::changed(cx);
    if !fetched {
        refresh(cx);
    }
    // A moment to see it worked, then the dialog goes.
    if in_dialog {
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(1400)).await;
            cx.update(|cx| {
                if cx.global::<AccountUi>().welcome.is_some() {
                    close_sign_in(cx);
                }
            });
        })
        .detach();
    }
}

/// Plain words for what the sign-in service said about an email code.
fn email_problem(e: &Error, sending: bool) -> String {
    match e {
        Error::Api(a) if a.status == 429 || a.code.starts_with("over_") => {
            if sending { "Too many codes asked for. Wait a minute, then try again.".into() } else { "Too many tries. Wait a minute, then try again.".into() }
        }
        Error::Api(a) if a.code == "otp_expired" || a.code == "otp_disabled" || (!sending && a.status == 403) => {
            "That code is wrong or has expired. Check it, or send a new one.".into()
        }
        Error::Api(a) if a.code == "email_address_invalid" || a.code == "validation_failed" => "That doesn't look like an email address.".into(),
        Error::Api(a) if a.code == "signup_disabled" || a.code == "email_provider_disabled" => "Signing up by email isn't open right now.".into(),
        Error::Network(_) => "Can't reach the sign-in service. Check your connection and try again.".into(),
        e if sending => format!("Couldn't send a code: {e}"),
        e => format!("Not signed in: {e}"),
    }
}

/// Email a sign-in code to `email`; the code's field shows when it's on its way.
pub fn send_code(email: String, cx: &mut App) {
    let email = email.trim().to_string();
    if cx.global::<AccountUi>().email_busy.is_some() {
        return;
    }
    if !crate::sign_in_dialog::looks_like_email(&email) {
        update(cx, |s| s.error = Some("That doesn't look like an email address.".into()));
        return;
    }
    update(cx, |s| {
        s.email_busy = Some("Sending a code…");
        s.last_email = email.clone();
        s.error = None;
    });
    // Send again counts down from here: the dialog redraws each second until it's on.
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let more = cx.update(|cx| {
                let s = cx.global::<AccountUi>();
                let more = (s.dialog && s.code_sent_to.is_some() && s.code_sent_at.is_some_and(|t| crate::sign_in_dialog::resend_wait(t.elapsed()).is_some())) || s.email_busy.is_some();
                cx.refresh_windows();
                more
            });
            if !more {
                break;
            }
        }
    })
    .detach();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let to = email.clone();
        let sent = cx.background_spawn(async move { account::shared().send_email_code(&to) }).await;
        cx.update(|cx| {
            update(cx, |s| {
                s.email_busy = None;
                match sent {
                    Ok(()) => {
                        s.code_sent_to = Some(email);
                        s.code_round += 1;
                        s.code_sent_at = Some(Instant::now());
                    }
                    Err(e) => s.error = Some(email_problem(&e, true)),
                }
            })
        });
    })
    .detach();
}

/// Sign in with the emailed `code`.
pub fn verify_code(code: String, cx: &mut App) {
    let Some(email) = cx.global::<AccountUi>().code_sent_to.clone() else { return };
    if cx.global::<AccountUi>().email_busy.is_some() {
        return;
    }
    if code.trim().is_empty() {
        update(cx, |s| s.error = Some("Type the code from the email.".into()));
        return;
    }
    update(cx, |s| {
        s.email_busy = Some("Signing in…");
        s.error = None;
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let done = cx.background_spawn(async move { account::shared().sign_in_with_email_code(&email, &code) }).await;
        cx.update(|cx| match done {
            Ok(signed) => finished(signed, cx),
            Err(e) => update(cx, |s| {
                s.email_busy = None;
                s.error = Some(email_problem(&e, false));
                // Empty boxes to type it again.
                s.code_round += 1;
            }),
        });
    })
    .detach();
}

/// Back to the email field (a different address, or the code never came).
pub fn change_email(cx: &mut App) {
    update(cx, |s| {
        s.email_step = true;
        s.code_sent_to = None;
        s.code_sent_at = None;
        s.error = None;
    });
}

/// The dialog's email step (`true`), or back to every way to sign in.
pub fn email_step(on: bool, cx: &mut App) {
    update(cx, |s| {
        s.email_step = on;
        s.code_sent_to = None;
        s.code_sent_at = None;
        s.error = None;
        s.provider_error = None;
    });
}

/// Open the sign-in dialog over the window: from the menu, Settings, a locked feature, Sign in
/// again.
pub fn show_sign_in(cx: &mut App) {
    if cx.try_global::<AccountUi>().is_none_or(|s| s.dialog) {
        return;
    }
    let again = signed_in(cx) == Some(true);
    update(cx, |s| {
        s.dialog = true;
        s.dialog_round += 1;
        s.reauth = again;
        s.email_step = false;
        s.welcome = None;
        s.error = None;
        s.provider_error = None;
    });
    crate::shell::activate_window(cx);
}

/// Close the sign-in dialog; a sign-in waiting for the browser stops.
pub fn close_sign_in(cx: &mut App) {
    cancel(cx);
    update(cx, |s| {
        s.dialog = false;
        s.reauth = false;
        s.welcome = None;
        s.code_sent_to = None;
        s.code_sent_at = None;
        s.provider_error = None;
        if s.signed_in != Some(true) {
            s.error = None;
        }
    });
}

/// For the control channel: show a step of the dialog without the network, to look at it:
/// `start`, `email`, `email-error`, `apple-error`, `code`, `code-error`, `welcome`.
pub fn dialog_look(step: &str, cx: &mut App) {
    show_sign_in(cx);
    update(cx, |s| {
        s.dialog = true;
        s.signed_in.get_or_insert(false);
        s.error = None;
        s.provider_error = None;
        s.code_sent_to = None;
        s.welcome = None;
        s.email_step = false;
        match step {
            "email" => s.email_step = true,
            "email-error" => {
                s.email_step = true;
                s.error = Some("That doesn't look like an email address.".into());
            }
            "apple-error" => s.provider_error = Some("Signing in was cancelled.".into()),
            "code" | "code-error" => {
                s.code_sent_to = Some("name@example.com".into());
                s.code_sent_at = Some(Instant::now());
                s.code_round += 1;
                if step == "code-error" {
                    s.error = Some("That code is wrong or has expired. Check it, or send a new one.".into());
                }
            }
            "welcome" => s.welcome = Some("name@example.com".into()),
            _ => {}
        }
    });
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
                Err(Error::SignedOut) => {
                    session_needed(cx);
                    update(cx, |s| s.error = Some("Sign in again to do this (your sign-in has lapsed).".into()));
                }
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
    out.push_str(&format!(" session_lapsed={}", s.session_lapsed));
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
    let (fg, muted, danger, mono) = (theme.foreground, theme.muted_foreground, theme.danger, theme.mono_font_family.clone());
    let Some(state) = cx.try_global::<AccountUi>() else { return div().into_any_element() };
    let note = |t: String| div().text_sm().text_color(muted).child(t);
    let mut body = div().w_full().flex().flex_col().gap_2();

    if state.signed_in != Some(true) {
        body = body
            .child(note("A Wundur account gives you images, 3D models and turn summaries without your own keys. The free plan has a small monthly allowance to try them.".into()))
            .child(
                div().flex().pt_1().child(
                    Button::new("account-sign-in")
                        .small()
                        .primary()
                        .label(if state.waiting.is_some() { "Signing in…" } else { "Sign in or create an account…" })
                        .disabled(state.signed_in.is_none() || state.dialog)
                        .on_click(|_, _, cx| show_sign_in(cx)),
                ),
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
                    .when(!me.account_id.is_empty(), |d| {
                        let id = me.account_id.clone();
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().w(px(150.)).flex_none().text_sm().text_color(muted).child("Account ID"))
                                .child(div().min_w_0().truncate().font_family(mono.clone()).text_sm().text_color(fg).child(id.clone()))
                                .child(
                                    Button::new("account-id-copy")
                                        .icon(gpui_kit::assets::IconName::Copy)
                                        .xsmall()
                                        .ghost()
                                        .tooltip("Copy the account ID")
                                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))),
                                ),
                        )
                    })
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
        if state.session_lapsed {
            buttons = buttons.child(Button::new("account-sign-in-again").small().outline().label("Sign in again").disabled(busy).on_click(|_, _, cx| show_sign_in(cx)));
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
