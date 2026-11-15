//! Undo last turn, in the message box: the button in its toolbar, and above the box the panel
//! that says what undoing would do, for the person to confirm (see `agent_session/undo.rs`).

use super::*;
use gpui_kit::component::Disableable as _;
use wundur_core::snapshots::Kind;

/// The most files the panel names; the rest are counted.
const NAMED: usize = 8;

impl ChatPanel {
    /// The toolbar's Undo turn button, when the last turn can be undone.
    pub(super) fn render_undo_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        if session.undo_working() {
            return Some(Button::new("undo-turn").xsmall().ghost().label("Undoing...").disabled(true).into_any_element());
        }
        if !session.can_undo() || session.undo_plan().is_some() {
            return None;
        }
        let weak = self.session.downgrade();
        Some(
            Button::new("undo-turn")
                .xsmall()
                .ghost()
                .label("Undo turn")
                .tooltip("Put back the files the last turn changed, and take the turn back")
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |s, cx| s.plan_undo(cx));
                })
                .into_any_element(),
        )
    }

    /// The confirmation, on top of the box: the files that go back, the ones changed again
    /// since, whether the agent forgets the turn, and what undo can't reach.
    pub(super) fn render_undo_plan(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        let plan = session.undo_plan()?.clone();
        let name = session.backend.name();
        let root = session.cwd.clone();
        let theme = cx.theme();
        let (border, muted, fg, field, warning, green) = (theme.border, theme.muted_foreground, theme.foreground, theme.secondary, theme.warning, theme.green);
        let line = |text: String, color: Hsla| div().px_3().py(px(1.)).text_xs().text_color(color).child(text);
        let files = match plan.files.len() {
            0 => "It changed no files.".to_string(),
            1 => "Puts back 1 file:".to_string(),
            n => format!("Puts back {n} files:"),
        };
        let mut panel = div()
            .w_full()
            .flex()
            .flex_col()
            .pt_1()
            // Tucked under the box's rounded top, as the waiting messages are.
            .pb(px(14.))
            .mb(px(-12.))
            .rounded_t(px(14.))
            .border_1()
            .border_b_0()
            .border_color(border)
            .bg(field.opacity(0.6))
            .child(div().px_3().py(px(2.)).text_sm().text_color(fg).child("Undo the last turn?"))
            .child(line(files, muted));
        for c in plan.files.iter().take(NAMED) {
            let (what, color) = match c.kind {
                Kind::Added => ("removed (new in the turn)", muted),
                Kind::Deleted => ("restored (deleted in the turn)", green),
                Kind::Modified => ("restored", muted),
            };
            let later = plan.conflicts.contains(&c.path);
            panel = panel.child(
                div()
                    .px_3()
                    .flex()
                    .gap_2()
                    .text_xs()
                    .child(div().flex_none().text_color(fg).child(c.path.display().to_string()))
                    .child(div().flex_none().text_color(color).child(what))
                    .when(later, |d| d.child(div().flex_none().text_color(warning).child("changed again since"))),
            );
        }
        if plan.files.len() > NAMED {
            panel = panel.child(line(format!("and {} more", plan.files.len() - NAMED), muted));
        }
        // Changed in the folder while the turn ran, but not by this agent as far as Wundur saw:
        // another session's work, the person's, or a command's. Left alone unless asked.
        if !plan.others.is_empty() {
            let n = plan.others.len();
            panel = panel.child(line(
                format!(
                    "{} also changed while the turn ran, not by {name} as far as Wundur saw (another session, you, or a command). Left as {}:",
                    if n == 1 { "1 other file".to_string() } else { format!("{n} other files") },
                    if n == 1 { "it is" } else { "they are" }
                ),
                muted,
            ));
            let names: Vec<String> = plan.others.iter().take(NAMED).map(|c| c.path.display().to_string()).collect();
            let more = if n > NAMED { format!(" and {} more", n - NAMED) } else { String::new() };
            panel = panel.child(line(format!("{}{more}", names.join(", ")), fg));
        }
        let conflicts = plan.conflicts.iter().filter(|p| plan.files.iter().any(|c| &c.path == *p)).count();
        if conflicts > 0 {
            let n = conflicts;
            panel = panel.child(line(
                format!(
                    "{} changed again after the turn (by you or another session). Undoing {} loses that.",
                    if n == 1 { "One file was".to_string() } else { format!("{n} files were") },
                    if n == 1 { "it" } else { "them" }
                ),
                warning,
            ));
        }
        panel = panel.child(line(
            if plan.forgets {
                format!("{name} forgets the turn, and your message comes back to the box.")
            } else {
                format!("{name} will still remember the turn (only Chat can take it out of the conversation).")
            },
            muted,
        ));
        panel = panel.child(line(
            format!("Undo can't reach outside {}: installed packages, pushes, or anything sent online.", root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "the folder".into())),
            muted,
        ));
        let weak = self.session.downgrade();
        let (w1, w2, w3, w4) = (weak.clone(), weak.clone(), weak.clone(), weak);
        let mut buttons = div().px_3().py_1().flex().gap_2();
        if conflicts == 0 {
            buttons = buttons.child(Button::new("undo-confirm").xsmall().primary().label("Undo").on_click(move |_, _, cx| {
                let _ = w1.update(cx, |s, cx| s.undo_turn(false, false, cx));
            }));
        } else {
            buttons = buttons
                .child(Button::new("undo-keep-later").xsmall().primary().label("Undo, keep later changes").on_click(move |_, _, cx| {
                    let _ = w2.update(cx, |s, cx| s.undo_turn(true, false, cx));
                }))
                .child(Button::new("undo-all").xsmall().label("Undo all").on_click(move |_, _, cx| {
                    let _ = w1.update(cx, |s, cx| s.undo_turn(false, false, cx));
                }));
        }
        if !plan.others.is_empty() {
            // Later changes are kept here too: these files are someone else's work.
            buttons = buttons.child(Button::new("undo-others").xsmall().label("Undo the other files too").on_click(move |_, _, cx| {
                let _ = w4.update(cx, |s, cx| s.undo_turn(true, true, cx));
            }));
        }
        buttons = buttons.child(Button::new("undo-cancel").xsmall().ghost().label("Cancel").on_click(move |_, _, cx| {
            let _ = w3.update(cx, |s, cx| s.cancel_undo(cx));
        }));
        Some(panel.child(buttons).into_any_element())
    }
}
