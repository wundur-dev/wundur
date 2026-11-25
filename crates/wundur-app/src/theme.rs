//! The look, light and dark: distinct surface tones so each region reads as itself (sidebar,
//! Claude, then the editor), clear dividers, an obvious active tab and quiet inactive ones. The
//! brand accent is the icon's periwinkle; crane yellow marks what wants attention. It follows
//! the system's appearance unless the setting (`appearance`: system, light or dark) says
//! otherwise, and changes live.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

// The colours themselves (`wundur_cards::theme::colors`) and the interface text's weight are
// shared with the card preview.
pub use wundur_cards::theme::UI_WEIGHT;

/// The side lists' text with the pressed-in look: a mid dark grey, so the white edge under it
/// reads (on near black it is lost).
const PRESSED_TEXT: u32 = 0x55555b;

thread_local! {
    /// The light lists' ground, while trying others (the control channel's `sidebar`).
    static SIDEBAR_TONE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Try `tone` as the light lists' ground (`None`: its own).
pub fn try_sidebar_tone(tone: Option<u32>, cx: &mut App) {
    SIDEBAR_TONE.with(|t| t.set(tone));
    apply(wants_dark(cx), cx);
    cx.refresh_windows();
}

/// The side lists' main text: a dark grey with the pressed-in look on a light theme (black
/// reads as printed on, not pressed in), else the theme's.
pub fn list_text(cx: &App) -> Hsla {
    if crate::prefs(cx).inset_text && !cx.theme().is_dark() { hex(PRESSED_TEXT) } else { cx.theme().foreground }
}

/// `text` pressed into the surface, letterpress style: a faint copy a pixel lower drawn
/// under it (light on a light theme, dark on a dark one), when the setting is on. `el` holds it
/// and sets its size, colour and truncation.
pub fn pressed(el: Div, text: impl Into<SharedString>, cx: &App) -> Div {
    let text = text.into();
    if !crate::prefs(cx).inset_text {
        return el.child(text);
    }
    let shadow = if cx.theme().is_dark() { gpui::black().opacity(0.45) } else { gpui::white() };
    el.relative()
        .child(div().absolute().top(px(1.)).left_0().right_0().truncate().text_color(shadow).child(text.clone()))
        .child(div().relative().truncate().child(text))
}

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

/// Whether the app should be dark now: the setting, else the system.
pub fn wants_dark(cx: &App) -> bool {
    let setting = cx.global::<crate::AppSettings>().0.borrow().appearance.clone();
    match setting.as_str() {
        "light" => false,
        "dark" => true,
        _ => matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark),
    }
}

/// Apply the light or dark theme to everything, the terminals' palette included.
pub fn apply(dark: bool, cx: &mut App) {
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    crate::fonts::apply(cx);
    crate::palette::set_dark(dark);
    wundur_cards::theme::colors(dark, cx);
}

/// Match the system (or the setting) if it changed; redraw everything when it does.
/// The ground of tab strips and title bars inside a panel: between the panel's white and the
/// grey the panels sit on, so a panel's edges stay its own.
pub fn strip_ground(cx: &App) -> Hsla {
    let t = cx.theme();
    crate::sheet_tabs::mix(t.background, t.sidebar, 0.55)
}

/// The ground of the tool panels (files, changes, skills, the agent): a much lighter grey than
/// the session list's, so the chat and files stand out white beside them.
pub fn tool_ground(cx: &App) -> Hsla {
    let t = cx.theme();
    if !t.is_dark()
        && let Some(tone) = SIDEBAR_TONE.with(|t| t.get())
    {
        return hex(tone);
    }
    crate::sheet_tabs::mix(t.background, t.sidebar, 0.45)
}

pub fn sync(cx: &mut App) {
    let dark = wants_dark(cx);
    if cx.theme().is_dark() != dark {
        apply(dark, cx);
        cx.refresh_windows();
    }
}

/// System, Light or Dark: save it, and set the window chrome to match.
pub fn set_appearance(choice: &str, cx: &mut App) {
    let settings = cx.global::<crate::AppSettings>().0.clone();
    settings.borrow_mut().appearance = choice.to_string();
    let _ = settings.borrow().save();
    cx.set_window_appearance(match choice {
        "light" => Some(WindowAppearance::Light),
        "dark" => Some(WindowAppearance::Dark),
        _ => None,
    });
    sync(cx);
    // `sync` compares with the theme; apply anyway so the chrome and theme agree.
    let dark = wants_dark(cx);
    apply(dark, cx);
    // The View menu's checks are read when the menu bar is built: rebuild it, or the check stays
    // on the old choice (the menu bar is only rebuilt by the settings window otherwise).
    cx.set_menus(crate::actions::menus(cx));
    cx.refresh_windows();
}

use gpui_kit::component::ActiveTheme;
