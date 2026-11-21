//! Updating Wundur Studio itself ([`wundur_core::update`]): a check shortly after starting and
//! every few hours, the download and its checks in the background, then a button in the toolbar.
//! Nothing installs until the person agrees: Restart quits the usual way (which asks first while
//! a session is working, and can be cancelled), and the update goes in as the app ends; the
//! new version opens with the sessions that were open.
//!
//! Only an installed app checks: a development build, or one run from a disk image, doesn't.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;
use wundur_core::update::{self, Ready};

/// How long after starting the first check waits (the window settles first).
const FIRST_CHECK: Duration = Duration::from_secs(20);
/// How often it checks after that.
const EVERY: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Default)]
struct Updater {
    /// The installed bundle to replace (`None`: not an installed app; no checks).
    bundle: Option<PathBuf>,
    /// Why updates can't be installed here, when it's the person's to fix (move to
    /// Applications).
    blocked: Option<String>,
    ready: Option<Ready>,
    checking: bool,
    /// The person said Restart: install as the app ends.
    agreed: bool,
}

impl Global for Updater {}

/// This build's version.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Start checking (an installed app only), unless `WUNDUR_NO_UPDATES=1`.
pub fn init(cx: &mut App) {
    let mut state = Updater::default();
    let off = wundur_core::env_var("NO_UPDATES").is_ok_and(|v| v != "0");
    if update::platform().is_some() && !off {
        match update::installed_bundle() {
            Ok(app) => state.bundle = Some(app),
            // A development build: not something to update.
            Err(why) if why.contains("development") => {}
            Err(why) => state.blocked = Some(why),
        }
    }
    let check = state.bundle.is_some();
    cx.set_global(state);
    if check {
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(FIRST_CHECK).await;
            loop {
                cx.update(check_now);
                cx.background_executor().timer(EVERY).await;
            }
        })
        .detach();
    }
}

/// Check now (and download and stage what's newer), in the background; once at a time.
pub fn check_now(cx: &mut App) {
    let Some(s) = cx.try_global::<Updater>() else { return };
    // Already one waiting to install: the next check is after it's in.
    if s.checking || s.bundle.is_none() || s.ready.is_some() {
        return;
    }
    cx.global_mut::<Updater>().checking = true;
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx.background_spawn(async move { update::fetch(current()) }).await;
        cx.update(|cx| {
            let s = cx.global_mut::<Updater>();
            s.checking = false;
            match got {
                Ok(Some(ready)) => {
                    s.ready = Some(ready);
                    cx.refresh_windows();
                }
                Ok(None) => {}
                // Quietly: the next check tries again.
                Err(e) => eprintln!("wundur: update: {e}"),
            }
        });
    })
    .detach();
}

/// Why this copy can't update itself, when the person can fix it (move it to Applications).
pub fn blocked(cx: &App) -> Option<String> {
    cx.try_global::<Updater>().and_then(|s| s.blocked.clone())
}

/// The version ready to install, if any.
pub fn ready_version(cx: &App) -> Option<String> {
    cx.try_global::<Updater>().and_then(|s| s.ready.as_ref()).map(|r| r.version.clone())
}

/// Ask whether to restart now to install; Restart quits the usual way.
pub fn offer(window: AnyWindowHandle, cx: &mut App) {
    let Some(ready) = cx.try_global::<Updater>().and_then(|s| s.ready.clone()) else { return };
    cx.spawn(async move |cx: &mut AsyncApp| {
        use gpui_kit::PromptButton;
        let answers = vec![PromptButton::Ok("Restart to update".into()), PromptButton::Other("Release notes".into()), PromptButton::Cancel("Later".into())];
        let message = format!("Wundur Studio {} is ready", ready.version);
        let detail = format!(
            "You have {}. Restarting installs the new version; your open sessions come back. If a session is still working, you'll be asked first.",
            current()
        );
        let answer = crate::closing::ask(window, PromptLevel::Info, message, detail, answers, cx).await;
        cx.update(|cx| match answer {
            Some(0) => {
                cx.global_mut::<Updater>().agreed = true;
                crate::request_quit(cx);
            }
            Some(1) if !ready.notes.is_empty() => cx.open_url(&ready.notes),
            _ => {}
        });
    })
    .detach();
}

/// As the app ends: when the person agreed, start the helper that installs the update and
/// opens it. Called from [`crate::quit_now`].
pub fn on_quit(cx: &mut App) {
    let Some(s) = cx.try_global::<Updater>() else { return };
    let (Some(ready), Some(app), true) = (s.ready.clone(), s.bundle.clone(), s.agreed) else { return };
    if let Err(e) = update::install_on_exit(&ready, &app) {
        eprintln!("wundur: update not installed: {e}");
    }
}

/// For the control channel: what the updater knows.
pub fn describe(cx: &App) -> String {
    let Some(s) = cx.try_global::<Updater>() else { return "no updater".into() };
    format!(
        "current={} bundle={} blocked={:?} checking={} ready={} agreed={}",
        current(),
        s.bundle.as_ref().map_or("-".into(), |b| b.display().to_string()),
        s.blocked,
        s.checking,
        s.ready.as_ref().map_or("-", |r| r.version.as_str()),
        s.agreed
    )
}
