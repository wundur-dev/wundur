//! The toolbar's account button and its menu: Sign in and Create account when signed out;
//! when signed in, who, the beta access the API reports (its message as it comes), Request
//! access, this month's usage, Manage subscription (when the API allows it) and Sign out.
//!
//! Request access opens [`RequestDialog`] over the window: an optional note (what they'd like to
//! make, up to [`ACCESS_NOTE_MAX`] characters), then the API's answer in place of the button.
//! The state lives in [`crate::settings_account`]; this only shows it.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::account::{self, ACCESS_NOTE_MAX, Me, Usage};

use crate::settings_account as acct;

/// The widest the menu grows; long messages wrap inside it.
const MENU_WIDTH: f32 = 290.;

/// The toolbar's account button (below Settings), opening the menu.
pub fn button(cx: &App) -> impl IntoElement {
    let signed_in = acct::signed_in(cx) == Some(true);
    let tip = match acct::me(cx) {
        Some(me) if !me.email.is_empty() => format!("Account: {}", me.email),
        _ if signed_in => "Account".to_string(),
        _ => "Sign in to your Wundur account".to_string(),
    };
    let muted = cx.theme().muted_foreground;
    Button::new("tb-account")
        .ghost()
        .text_color(muted)
        // As a child, so the button keeps the toolbar's icon size (its own `icon` resizes it).
        .child(Icon::new(IconName::CircleUser).size(px(20.)))
        .w(px(36.))
        .h(px(36.))
        .rounded(px(10.))
        .tooltip(tip)
        // From the window's bottom left corner: the menu opens above and to the right.
        .dropdown_menu_with_anchor(Anchor::BottomLeft, |menu, _, cx| {
            // Opening it is a good moment to look again (an approval, this month's usage).
            cx.defer(acct::refresh_soon);
            build(menu, cx)
        })
}

/// A line of text in the menu that wraps (the menu's labels don't).
fn text(t: String, muted: bool) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let theme = cx.theme();
        div().max_w(px(MENU_WIDTH - 24.)).whitespace_normal().text_sm().text_color(if muted { theme.muted_foreground } else { theme.foreground }).child(t.clone())
    })
    .disabled(true)
}

/// "3 of 5 left".
fn left(u: Usage) -> String {
    format!("{} of {} left", u.left, u.limit)
}

fn build(menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let menu = menu.min_w(px(220.)).max_w(px(MENU_WIDTH));
    if acct::waiting(cx) {
        return menu
            .item(text("Waiting for the browser… Sign in on the page that opened, then come back.".into(), true))
            .separator()
            .item(PopupMenuItem::new("Cancel signing in").on_click(|_, _, cx| acct::cancel(cx)));
    }
    let signed_in = acct::signed_in(cx);
    if signed_in != Some(true) {
        let menu = match acct::error(cx) {
            Some(e) => menu.item(text(e, true)).separator(),
            None => menu,
        };
        return menu
            .item(PopupMenuItem::new("Sign in").disabled(signed_in.is_none()).on_click(|_, _, cx| acct::start_sign_in(false, cx)))
            .item(PopupMenuItem::new("Create account").disabled(signed_in.is_none()).on_click(|_, _, cx| acct::start_sign_in(true, cx)));
    }
    let Some(me) = acct::me(cx).cloned() else {
        let note = acct::error(cx).unwrap_or_else(|| "Getting your account…".into());
        return menu
            .item(text(note, true))
            .separator()
            .item(PopupMenuItem::new("Account settings…").on_click(|_, _, cx| crate::shell::open_settings_page("account", cx)))
            .item(PopupMenuItem::new("Sign out").on_click(|_, _, cx| acct::signed_out(None, cx)));
    };
    signed_in_menu(menu, &me, cx)
}

fn signed_in_menu(mut menu: PopupMenu, me: &Me, cx: &mut App) -> PopupMenu {
    let name = (!me.name.is_empty()).then(|| me.name.clone()).or_else(|| account::shared().display_name());
    let email = me.email.clone();
    menu = menu.item(
        PopupMenuItem::element(move |_, cx| {
            let theme = cx.theme();
            let (fg, muted) = (theme.foreground, theme.muted_foreground);
            div()
                .flex()
                .flex_col()
                .max_w(px(MENU_WIDTH - 24.))
                .when_some(name.clone(), |d, n| d.child(div().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(n)))
                .child(div().truncate().text_sm().text_color(if name.is_some() { muted } else { fg }).child(if email.is_empty() { "Signed in".to_string() } else { email.clone() }))
        })
        .disabled(true),
    );
    if !me.access.message.is_empty() {
        menu = menu.separator().item(text(me.access.message.clone(), true));
    }
    if me.access.can_request {
        let sending = acct::requesting(cx);
        menu = menu.item(PopupMenuItem::new(if sending { "Asking for access…" } else { "Request access…" }).disabled(sending).on_click(|_, _, cx| crate::shell::open_access_request(cx)));
    }
    let a = &me.allowance;
    menu = menu.separator().label("This month").item(text(format!("Image sets: {}", left(a.images)), false));
    if let Some(u) = a.model_credits {
        menu = menu.item(text(format!("Model credits: {}", left(u)), false));
    }
    menu = menu.separator();
    if me.can_manage_billing {
        menu = menu.item(PopupMenuItem::new("Manage subscription").on_click(|_, _, cx| acct::open_from("Opening billing…", || account::shared().billing_portal(), cx)));
    }
    if let Some(e) = acct::error(cx) {
        menu = menu.item(text(e, true));
    }
    menu.item(PopupMenuItem::new("Account settings…").on_click(|_, _, cx| crate::shell::open_settings_page("account", cx)))
        .item(PopupMenuItem::new("Sign out").on_click(|_, _, cx| acct::signed_out(None, cx)))
}

// ----- Request access -----

/// Asking for beta access: the note, then the API's answer.
pub struct RequestDialog {
    input: Entity<TextareaState>,
    error: Option<String>,
    /// The API's message once asked (shown in place of the button).
    done: Option<String>,
    _sub: Subscription,
}

/// What the dialog asks of the window.
pub enum DialogEvent {
    Send,
}

impl RequestDialog {
    pub fn new<T: 'static>(window: &mut Window, cx: &mut Context<T>, on: impl Fn(&mut T, DialogEvent, &mut Window, &mut Context<T>) + 'static) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("What would you like to make? (optional)"));
        input.update(cx, |i, cx| i.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, move |t, _, ev: &InputEvent, window, cx| {
            // Cmd+Enter sends; Enter is a new line.
            if let InputEvent::PressEnter { secondary: true, .. } = ev {
                on(t, DialogEvent::Send, window, cx);
            }
            cx.notify();
        });
        Self { input, error: None, done: None, _sub: sub }
    }

    /// The note as typed.
    pub fn note(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn too_long(&self, cx: &App) -> bool {
        self.note(cx).trim().chars().count() > ACCESS_NOTE_MAX
    }

    pub fn answered(&mut self, got: Result<String, String>) {
        match got {
            Ok(message) => {
                self.error = None;
                self.done = Some(if message.is_empty() { "Asked. You'll hear back by email.".into() } else { message });
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The dialog, over the window. `close` and `send` are the window's.
    pub fn render(&self, close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, send: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, red) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.red);
        let count = self.note(cx).trim().chars().count();
        let sending = acct::requesting(cx);
        let message = acct::me(cx).map(|m| m.access.message.clone()).filter(|m| !m.is_empty());
        let mut card = div()
            .w(px(460.))
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_lg()
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Request access"));
        let close = std::rc::Rc::new(close);
        match &self.done {
            Some(answer) => {
                let c = close.clone();
                card = card.child(div().text_sm().line_height(rems(1.4)).text_color(fg).child(answer.clone())).child(
                    div().pt_1().flex().justify_end().child(Button::new("access-done").small().primary().label("Done").on_click(move |e, w, cx| c(e, w, cx))),
                );
            }
            None => {
                let c = close.clone();
                card = card
                    .when_some(message, |d, m| d.child(div().text_sm().line_height(rems(1.4)).text_color(muted).child(m)))
                    .child(Textarea::new(&self.input).small())
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_xs()
                            .text_color(if count > ACCESS_NOTE_MAX { red } else { muted })
                            .child("Cmd+Enter sends")
                            .child(format!("{count} / {ACCESS_NOTE_MAX}")),
                    )
                    .children(self.error.clone().map(|e| div().text_xs().text_color(red).child(e)))
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("access-cancel").small().ghost().label("Cancel").on_click(move |e, w, cx| c(e, w, cx)))
                            .child(
                                Button::new("access-send")
                                    .small()
                                    .primary()
                                    .label(if sending { "Asking…" } else { "Request access" })
                                    .loading(sending)
                                    .disabled(sending || self.too_long(cx))
                                    .on_click(send),
                            ),
                    );
            }
        }
        let c = close.clone();
        div()
            .id("access-request")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(90.))
            .bg(gpui_kit::black().opacity(0.25))
            .occlude()
            .capture_key_down(move |ev: &KeyDownEvent, w, cx| {
                if ev.keystroke.key == "escape" {
                    c(&ClickEvent::default(), w, cx);
                    cx.stop_propagation();
                }
            })
            .child(card)
            .into_any_element()
    }
}
