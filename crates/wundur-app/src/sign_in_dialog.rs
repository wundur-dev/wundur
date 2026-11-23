//! The sign-in dialog, over the window: Continue with Apple (in the browser), or an email and
//! the code sent to it, typed into boxes. It opens from every way in (the account menu, Settings
//! > Account, a locked feature, Sign in again) through [`acct::show_sign_in`].
//!
//! Its steps follow the account state ([`acct::AccountUi`]), so whatever starts or finishes a
//! sign-in shows here: waiting for the browser, the code's boxes once a code is on its way, then
//! who signed in, a moment before it closes. Errors show under what they're about, in plain
//! words.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, OtpEvent, OtpInput, OtpState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, IconName, Sizable, Size};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::account::Provider;

use crate::settings_account::{self as acct, AccountUi};

/// The code's length, unless a pasted code says otherwise (Supabase's default; a project can set
/// 6 to 10).
pub const CODE_LENGTH: usize = 6;
/// The longest code a project can send.
const CODE_MAX: usize = 10;
/// How long Send again waits after a code was sent.
pub const RESEND_AFTER: Duration = Duration::from_secs(30);

const CARD_WIDTH: f32 = 420.;
const CONTROL_HEIGHT: f32 = 46.;
const RADIUS: f32 = 12.;

/// The terms and privacy pages, for the line under the buttons; off until they're published.
const LEGAL: Option<(&str, &str)> = None;

/// Whether `s` could be an email address: something, one `@`, a domain with a dot inside it, no
/// spaces. The sign-in service has the final say.
pub fn looks_like_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty()
        && !domain.contains('@')
        && !s.chars().any(char::is_whitespace)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
}

/// Seconds until Send again is on, `elapsed` after the code was sent (`None`: now).
pub fn resend_wait(elapsed: Duration) -> Option<u64> {
    let left = RESEND_AFTER.checked_sub(elapsed)?;
    if left.is_zero() { None } else { Some(left.as_secs() + u64::from(left.subsec_nanos() > 0)) }
}

/// The digits of a pasted code ("123 456", "Code: 123456"), when they make one (6 to 10).
pub fn pasted_code(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    (CODE_LENGTH..=CODE_MAX).contains(&digits.len()).then_some(digits)
}

/// The code's length now: the one learned from a paste, else [`CODE_LENGTH`].
pub fn code_length(learned: usize) -> usize {
    if (CODE_LENGTH..=CODE_MAX).contains(&learned) { learned } else { CODE_LENGTH }
}

/// The email field, kept while the dialog shows it.
struct EmailField {
    input: Entity<InputState>,
    _sub: Subscription,
}

/// The code's boxes, one set per code sent (and after a wrong one).
struct CodeField {
    otp: Entity<OtpState>,
    _sub: Subscription,
}

/// What the dialog draws, read from the account state first.
struct View {
    round: usize,
    reauth: bool,
    email_step: bool,
    waiting: bool,
    provider_error: Option<String>,
    code_sent_to: Option<String>,
    code_round: usize,
    code_len: usize,
    resend: Option<u64>,
    busy: Option<&'static str>,
    error: Option<String>,
    welcome: Option<String>,
    last_email: String,
    known: bool,
}

/// The dialog, when it's open.
pub fn render(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let s = cx.try_global::<AccountUi>().filter(|s| s.dialog)?;
    let view = View {
        round: s.dialog_round,
        reauth: s.reauth,
        email_step: s.email_step,
        waiting: s.waiting.is_some(),
        provider_error: s.provider_error.clone(),
        code_sent_to: s.code_sent_to.clone(),
        code_round: s.code_round,
        code_len: code_length(s.code_len),
        resend: s.code_sent_at.and_then(|t| resend_wait(t.elapsed())),
        busy: s.email_busy,
        error: s.error.clone(),
        welcome: s.welcome.clone(),
        last_email: s.last_email.clone(),
        known: s.signed_in.is_some(),
    };
    let body = if let Some(who) = view.welcome.clone() {
        welcome(who, cx)
    } else if view.waiting {
        waiting(cx)
    } else if let Some(to) = view.code_sent_to.clone() {
        code_step(&view, to, window, cx)
    } else if view.email_step {
        email_step(&view, window, cx)
    } else {
        start_step(&view, cx)
    };
    Some(frame(body, !view.waiting && view.welcome.is_none(), cx))
}

/// The backdrop and the card, with its close button.
fn frame(body: AnyElement, closable: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (bg, border, muted) = (theme.background, theme.border, theme.muted_foreground);
    let card = div()
        .id("sign-in-card")
        .relative()
        .w(px(CARD_WIDTH))
        .px(px(32.))
        .pt(px(36.))
        .pb(px(28.))
        .rounded(px(16.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .shadow_2xl()
        // Clicks inside stay inside (the backdrop's click closes).
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(body)
        .when(closable, |d| {
            d.child(
                div().absolute().top(px(12.)).right(px(12.)).child(
                    Button::new("sign-in-close").ghost().small().icon(Icon::new(IconName::Close).text_color(muted)).tooltip("Close").on_click(|_, _, cx| acct::close_sign_in(cx)),
                ),
            )
        });
    div()
        .id("sign-in-dialog")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_kit::black().opacity(0.35))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            if closable {
                acct::close_sign_in(cx);
            }
        })
        .capture_key_down(|ev: &KeyDownEvent, _, cx| {
            if ev.keystroke.key == "escape" {
                acct::close_sign_in(cx);
                cx.stop_propagation();
            }
        })
        .child(card)
        .into_any_element()
}

/// The heading: the app's mark, a title and a line under it.
fn heading(title: &str, line: AnyElement, cx: &App) -> Div {
    let fg = cx.theme().foreground;
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .text_center()
        .child(img(mark()).size(px(56.)).rounded(px(13.)).mb(px(14.)))
        .child(div().text_2xl().font_weight(FontWeight::BOLD).text_color(fg).child(title.to_string()))
        .child(line)
}

/// The app's mark, for the heading.
fn mark() -> std::sync::Arc<Image> {
    static MARK: std::sync::OnceLock<std::sync::Arc<Image>> = std::sync::OnceLock::new();
    MARK.get_or_init(|| std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../assets/icon/wundur-mark-128.png").to_vec()))).clone()
}

fn muted_line(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div().text_sm().line_height(rems(1.4)).text_color(cx.theme().muted_foreground).child(text.into()).into_any_element()
}

fn error_line(text: Option<String>, cx: &App) -> Option<AnyElement> {
    let red = cx.theme().danger;
    text.map(|t| {
        div()
            .flex()
            .items_start()
            .gap(px(6.))
            .text_sm()
            .line_height(rems(1.35))
            .text_color(red)
            .child(Icon::new(IconName::CircleX).size(px(14.)).mt(px(2.)).text_color(red))
            .child(div().flex_1().min_w_0().whitespace_normal().child(t))
            .into_any_element()
    })
}

/// A full-width filled button (gpui-kit's custom buttons fade their colour): `bg` on `fg`,
/// `hover` under the pointer; a spinner while `busy`; faded and inert when `disabled`.
fn solid_button(
    id: impl Into<ElementId>,
    content: impl IntoElement,
    (bg, fg, hover): (Hsla, Hsla, Hsla),
    busy: bool,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let inert = disabled || busy;
    div()
        .id(id.into())
        .w_full()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .rounded(px(RADIUS))
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .when(disabled && !busy, |d| d.opacity(0.45))
        .when(!inert, |d| d.cursor_pointer().hover(move |d| d.bg(hover)).on_click(on_click))
        .when(busy, |d| d.child(spinner(fg, 15.)))
        .child(content)
}

/// A turning loader.
fn spinner(color: Hsla, size: f32) -> impl IntoElement {
    Icon::new(IconName::LoaderCircle).size(px(size)).text_color(color).with_animation(
        SharedString::from(format!("sign-in-spinner-{size}")),
        Animation::new(Duration::from_secs(1)).repeat(),
        |icon, t| icon.transform(Transformation::rotate(percentage(t))),
    )
}

/// The accent colour, white on it, and its hover.
fn accent(cx: &App) -> (Hsla, Hsla, Hsla) {
    let blue = cx.theme().blue;
    (blue, gpui_kit::white(), blue.opacity(0.86))
}

/// A full-width button in the accent colour.
fn accent_button(
    id: &'static str,
    label: &str,
    busy: bool,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    solid_button(id, label.to_string(), accent(cx), busy, disabled, on_click)
}

/// A small link in the accent colour.
fn text_link(id: &'static str, label: &str, disabled: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let link = cx.theme().blue;
    div()
        .id(id)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(link)
        .child(label.to_string())
        .when(disabled, |d| d.opacity(0.45))
        .when(!disabled, |d| d.cursor_pointer().hover(|d| d.opacity(0.8)).on_click(on_click))
        .into_any_element()
}

/// "← All sign-in options", back to the start.
fn back_link(cx: &App) -> AnyElement {
    let link = cx.theme().blue;
    div()
        .id("sign-in-back")
        .flex()
        .items_center()
        .gap(px(6.))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(link)
        .cursor_pointer()
        .hover(|d| d.opacity(0.8))
        .child(Icon::new(IconName::ArrowLeft).size(px(14.)).text_color(link))
        .child("All sign-in options")
        .on_click(|_, _, cx| acct::email_step(false, cx))
        .into_any_element()
}

/// The line about the terms, when they're published.
fn legal(cx: &App) -> Option<AnyElement> {
    let (terms, privacy) = LEGAL?;
    let theme = cx.theme();
    let (muted, link) = (theme.muted_foreground, theme.blue);
    let page = |id: &'static str, label: &'static str, url: &'static str| {
        div().id(id).text_color(link).cursor_pointer().child(label).on_click(move |_, _, cx| cx.open_url(url))
    };
    Some(
        div()
            .pt(px(22.))
            .flex()
            .flex_wrap()
            .justify_center()
            .gap(px(3.))
            .text_xs()
            .text_color(muted)
            .child("By continuing, you agree to our")
            .child(page("sign-in-terms", "Terms", terms))
            .child("and")
            .child(page("sign-in-privacy", "Privacy Policy", privacy))
            .child(".")
            .into_any_element(),
    )
}

/// The start: every way to sign in.
fn start_step(v: &View, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (border, muted) = (theme.border, theme.muted_foreground);
    let (title, line) = if v.reauth {
        ("Sign in again", "Your sign-in has lapsed. Sign in to manage billing and make keys; everything else keeps working.")
    } else {
        ("Welcome to Wundur", "Sign in to pick up where you left off. New here? Signing in creates your account.")
    };
    let mut providers = div().flex().flex_col().gap(px(12.));
    for (i, provider) in Provider::ALL.iter().copied().enumerate() {
        providers = providers.child(provider_button(i, provider, !v.known, cx));
    }
    div()
        .flex()
        .flex_col()
        .child(heading(title, muted_line(line, cx), cx))
        .child(div().h(px(30.)))
        .child(providers)
        .children(error_line(v.provider_error.clone(), cx).map(|e| div().pt(px(10.)).child(e)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .py(px(20.))
                .child(div().flex_1().h(px(1.)).bg(border))
                .child(div().text_sm().text_color(muted).child("or"))
                .child(div().flex_1().h(px(1.)).bg(border)),
        )
        .child(solid_button(
            "sign-in-email",
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(Icon::new(gpui_kit::assets::IconName::Mail).size(px(17.)).text_color(gpui_kit::white()))
                .child("Continue with email"),
            accent(cx),
            false,
            !v.known,
            |_, _, cx| acct::email_step(true, cx),
        ))
        .children(legal(cx))
        .into_any_element()
}

/// The email step: the address a code goes to.
fn email_step(v: &View, window: &mut Window, cx: &mut App) -> AnyElement {
    let key = SharedString::from(format!("sign-in-email-{}", v.round));
    let last = v.last_email.clone();
    let field = window.use_keyed_state(key, cx, move |window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com").default_value(last));
        input.update(cx, |i, cx| i.focus(window, cx));
        let _sub = cx.subscribe(&input, |_, input, ev: &InputEvent, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let value = input.read(cx).value().to_string();
                if looks_like_email(&value) {
                    acct::send_code(value, cx);
                }
            }
            // The button follows the text.
            InputEvent::Change => cx.refresh_windows(),
            _ => {}
        });
        EmailField { input, _sub }
    });
    let input = field.read(cx).input.clone();
    let ok = looks_like_email(&input.read(cx).value());
    let sending = v.busy.is_some();
    let fg = cx.theme().foreground;
    let submit = input.clone();
    div()
        .flex()
        .flex_col()
        .child(heading("Sign in with email", muted_line("We'll send you a code to sign in. No password needed.", cx), cx))
        .child(div().pt(px(26.)).pb(px(18.)).child(back_link(cx)))
        .child(div().pb(px(8.)).text_sm().font_weight(FontWeight::MEDIUM).text_color(fg).child("Email address"))
        .child(Input::new(&input).large().disabled(sending))
        .children(error_line(v.error.clone(), cx).map(|e| div().pt(px(10.)).child(e)))
        .child(
            div().pt(px(18.)).child(
                accent_button(
                    "sign-in-send",
                    if sending { "Sending a code…" } else { "Send code" },
                    sending,
                    !ok || !v.known,
                    move |_, _, cx| {
                        let value = submit.read(cx).value().to_string();
                        acct::send_code(value, cx);
                    },
                    cx,
                ),
            ),
        )
        .children(legal(cx))
        .into_any_element()
}

/// A provider's button, in its own colours: Apple's is black (white in a dark window).
fn provider_button(i: usize, provider: Provider, disabled: bool, cx: &App) -> AnyElement {
    let dark = cx.theme().is_dark();
    let colours = if dark {
        (gpui_kit::white(), gpui_kit::black(), hsla(0., 0., 0.86, 1.))
    } else {
        (gpui_kit::black(), gpui_kit::white(), hsla(0., 0., 0.2, 1.))
    };
    let glyph = match provider {
        // Apple's logo is in the system font, at U+F8FF.
        Provider::Apple => "\u{f8ff}",
    };
    solid_button(
        ("sign-in-provider", i),
        div()
            .flex()
            .items_center()
            .gap(px(7.))
            .child(div().font_family(crate::fonts::SYSTEM_SANS).text_size(px(18.)).mt(px(-2.)).child(glyph))
            .child(format!("Continue with {}", provider.label())),
        colours,
        false,
        disabled,
        move |_, _, cx| {
            if let Err(e) = acct::sign_in_with(provider, true, cx) {
                cx.update_global::<AccountUi, _>(|s, _| s.provider_error = Some(e));
                cx.refresh_windows();
            }
        },
    )
    .into_any_element()
}

/// Waiting for the browser.
fn waiting(cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    div()
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .child(heading("Finish in your browser", muted_line("We opened Apple's sign-in page. Sign in there, then come back here.", cx), cx))
        .child(div().py(px(26.)).child(spinner(muted, 28.)))
        .child(
            div()
                .w_full()
                .flex()
                .gap(px(10.))
                .child(Button::new("sign-in-cancel").outline().flex_1().h(px(CONTROL_HEIGHT)).rounded(px(RADIUS)).label("Cancel").on_click(|_, _, cx| acct::cancel(cx)))
                .child(div().flex_1().child(accent_button("sign-in-reopen", "Open the page again", false, false, |_, _, cx| acct::reopen_provider_page(cx), cx))),
        )
        .into_any_element()
}

/// Step two: the code sent to `to`.
fn code_step(v: &View, to: String, window: &mut Window, cx: &mut App) -> AnyElement {
    let key = SharedString::from(format!("sign-in-code-{}-{}-{}", v.round, v.code_round, v.code_len));
    let len = v.code_len;
    let field = window.use_keyed_state(key, cx, move |window, cx| {
        let otp = cx.new(|cx| OtpState::new(len, window, cx));
        otp.update(cx, |o, cx| o.focus(window, cx));
        let _sub = cx.subscribe(&otp, |_, otp, ev: &OtpEvent, cx| {
            if *ev == OtpEvent::Complete {
                let code = otp.read(cx).value().to_string();
                acct::verify_code(code, cx);
            }
            cx.refresh_windows();
        });
        CodeField { otp, _sub }
    });
    let otp = field.read(cx).otp.clone();
    let checking = v.busy.is_some();
    let theme = cx.theme();
    let (fg, muted) = (theme.foreground, theme.muted_foreground);
    let line = div()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap(px(4.))
        .text_sm()
        .line_height(rems(1.4))
        .text_color(muted)
        .child("We sent a code to")
        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(to.clone()))
        .into_any_element();
    let cell = if len > 8 { 30. } else if len > 6 { 36. } else { 44. };
    let paste = otp.clone();
    let typed = otp.read(cx).value().to_string();
    let submit = otp.clone();
    let again = to.clone();
    let resend = match v.resend {
        Some(secs) => div().text_sm().text_color(muted).child(format!("Send a new code in {secs}s")).into_any_element(),
        None => text_link("sign-in-resend", "Send a new code", checking, move |_, _, cx| acct::send_code(again.clone(), cx), cx),
    };
    div()
        .flex()
        .flex_col()
        .child(heading("Check your email", line, cx))
        .child(div().pt(px(26.)).child(back_link(cx)))
        .child(
            div()
                .id("sign-in-code")
                .pt(px(22.))
                .flex()
                .justify_center()
                // Pasting a code fills the boxes and signs in.
                .capture_key_down(move |ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.modifiers.platform && ev.keystroke.key == "v" {
                        let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
                        if let Some(code) = pasted_code(&text) {
                            if code.len() == paste.read(cx).len() {
                                paste.update(cx, |o, cx| o.set_value(code.clone(), window, cx));
                            } else {
                                // Another length than expected: boxes for it, from now on.
                                let n = code.len();
                                cx.update_global::<AccountUi, _>(|s, _| s.code_len = n);
                            }
                            acct::verify_code(code, cx);
                        }
                        cx.stop_propagation();
                    }
                })
                .child(OtpInput::new(&otp).groups(1).with_size(Size::Size(px(cell))).disabled(checking)),
        )
        .children(error_line(v.error.clone(), cx).map(|e| div().pt(px(14.)).child(e)))
        .child(
            div().pt(px(22.)).child(
                accent_button(
                    "sign-in-verify",
                    if checking { "Signing in…" } else { "Sign in" },
                    checking,
                    typed.len() < len,
                    move |_, _, cx| {
                        let code = submit.read(cx).value().to_string();
                        acct::verify_code(code, cx);
                    },
                    cx,
                ),
            ),
        )
        .child(
            div()
                .pt(px(16.))
                .flex()
                .items_center()
                .justify_between()
                .child(resend)
                .child(text_link("sign-in-other-email", "Use a different email", checking, |_, _, cx| acct::change_email(cx), cx)),
        )
        .child(div().pt(px(14.)).text_center().text_xs().line_height(rems(1.4)).text_color(muted).child("Not there? Check your spam folder."))
        .into_any_element()
}

/// Signed in: who, for a moment.
fn welcome(who: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let green = theme.success;
    let line = if who.is_empty() { "Welcome to Wundur.".to_string() } else { format!("Welcome, {who}.") };
    div()
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .gap(px(6.))
        .py(px(16.))
        .child(div().size(px(56.)).mb(px(10.)).rounded_full().bg(green.opacity(0.14)).flex().items_center().justify_center().child(Icon::new(IconName::Check).size(px(28.)).text_color(green)))
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(theme.foreground).child("You're signed in"))
        .child(muted_line(line, cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{code_length, looks_like_email, pasted_code, resend_wait};

    #[test]
    fn an_email_needs_a_name_an_at_and_a_domain() {
        for ok in ["a@b.co", "name.surname+tag@example.com", "  me@wundur.ai  "] {
            assert!(looks_like_email(ok), "{ok}");
        }
        for no in ["", "me", "me@", "@wundur.ai", "me@wundur", "me@.ai", "me@wundur.", "me@@wundur.ai", "me @wundur.ai", "me@wundur..ai"] {
            assert!(!looks_like_email(no), "{no}");
        }
    }

    #[test]
    fn send_again_waits_thirty_seconds() {
        assert_eq!(resend_wait(Duration::ZERO), Some(30));
        assert_eq!(resend_wait(Duration::from_millis(100)), Some(30));
        assert_eq!(resend_wait(Duration::from_secs(29)), Some(1));
        assert_eq!(resend_wait(Duration::from_millis(29_500)), Some(1));
        assert_eq!(resend_wait(Duration::from_secs(30)), None);
        assert_eq!(resend_wait(Duration::from_secs(90)), None);
    }

    #[test]
    fn a_pasted_code_is_its_digits_when_they_make_one() {
        assert_eq!(pasted_code("123456").as_deref(), Some("123456"));
        assert_eq!(pasted_code("123 456\n").as_deref(), Some("123456"));
        assert_eq!(pasted_code("Your code: 12345678").as_deref(), Some("12345678"));
        assert_eq!(pasted_code("12345"), None);
        assert_eq!(pasted_code("12345678901"), None);
        assert_eq!(pasted_code("hello"), None);
    }

    #[test]
    fn the_code_is_six_long_unless_a_paste_said_otherwise() {
        assert_eq!(code_length(0), 6);
        assert_eq!(code_length(8), 8);
        assert_eq!(code_length(10), 10);
        assert_eq!(code_length(4), 6);
        assert_eq!(code_length(11), 6);
    }
}
