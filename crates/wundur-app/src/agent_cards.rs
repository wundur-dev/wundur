//! Cards the agent puts in front of the person (through `wundur-mcp`):
//!
//! - `ask_to_choose`: files it made, shown one of a few fixed ways with fixed buttons (see
//!   [`wundur_core::choices`]). The call waits for the answer (up to [`WAIT`]); an answer that
//!   comes later, or to a call that didn't wait, reaches the agent as a new message.
//! - `present_card`: one of the cards the person could open from the Create menu (a built-in
//!   one or a workflow the service offers), filled in as the agent asks, with its question on
//!   top. The person finishes it; what they made reaches the agent as a new message. The agent
//!   never defines a card, and only form values the card already has can be filled in. Spending
//!   stays the person's click on the card.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::*;
use serde_json::{Value, json};
use wundur_core::choices::{self, State};
use wundur_core::conversation::Item;

use crate::agent_session::AgentSession;
use crate::hooks::Reply;

/// How long a call that waits holds on for the answer before saying there's none yet.
const WAIT: Duration = Duration::from_secs(5 * 60);

/// A question waiting for the person.
struct Pending {
    session: WeakEntity<AgentSession>,
    /// The agent's call, while it still waits.
    reply: Option<Reply>,
}

#[derive(Default)]
struct Choices {
    next: u64,
    pending: HashMap<u64, Pending>,
}

impl Global for Choices {}

/// The agent's `ask_to_choose`: shown in its chat; answered on `reply` once the person picks
/// (or at once, when it doesn't wait).
pub fn ask_to_choose(session: Entity<AgentSession>, args: &Value, reply: Option<Reply>, cx: &mut App) {
    let cwd = session.read(cx).cwd().to_path_buf();
    let choice = match choices::parse(&cwd, args) {
        Ok(c) => c,
        Err(why) => return send(reply, &why, true),
    };
    let waits = choices::waits(args);
    let state = cx.default_global::<Choices>();
    state.next += 1;
    let id = state.next;
    session.update(cx, |s, cx| {
        s.chat.items.push(Item::Choice { id, choice });
        s.chat.revision += 1;
        cx.notify();
    });
    if !waits {
        cx.default_global::<Choices>().pending.insert(id, Pending { session: session.downgrade(), reply: None });
        return send(reply, &choices::not_yet(false), false);
    }
    cx.default_global::<Choices>().pending.insert(id, Pending { session: session.downgrade(), reply });
    // Past the wait: the call returns and the answer comes later as a message.
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(WAIT).await;
        cx.update(|cx| {
            if let Some(p) = cx.default_global::<Choices>().pending.get_mut(&id)
                && let Some(reply) = p.reply.take()
            {
                send(Some(reply), &choices::not_yet(true), false);
            }
        });
    })
    .detach();
}

/// The person's answer to question `id`: which files (by place) and their note.
pub fn answer(id: u64, picked: Vec<usize>, note: String, cx: &mut App) {
    let Some(p) = cx.default_global::<Choices>().pending.remove(&id) else { return };
    let Some(session) = p.session.upgrade() else { return };
    let text = session.update(cx, |s, cx| {
        let mut text = None;
        for item in s.chat.items.iter_mut().rev() {
            if let Item::Choice { id: i, choice } = item
                && *i == id
            {
                text = Some(choices::answer_text(choice, &picked, &note));
                choice.state = State::Answered { picked: picked.clone(), note: note.clone() };
                break;
            }
        }
        s.chat.revision += 1;
        cx.notify();
        text
    });
    let Some(text) = text else { return };
    match p.reply {
        Some(reply) => send(Some(reply), &text, false),
        None => session.update(cx, |s, cx| s.tell_agent(&choices::late_message(&text), cx)),
    }
}

fn send(reply: Option<Reply>, text: &str, error: bool) {
    if let Some(reply) = reply {
        reply.send(&json!({ "text": text, "error": error }).to_string());
    }
}

// ----- present_card -----

/// A card the agent asked to show: which, filled in with what, and its question.
#[derive(Clone, Debug)]
pub struct Present {
    pub card: String,
    pub fill: serde_json::Map<String, Value>,
    pub question: String,
}

/// What each built-in card can be filled in with.
const BUILT_IN_FILL: [(&str, &[&str]); 3] = [
    ("asset-3d", &["kind", "name", "description"]),
    ("image", &["prompt", "quality", "size", "transparent"]),
    ("sound-effect", &["prompt", "seconds", "loop"]),
];

/// The cards the agent can present here, with what each takes: one line each, for its answer
/// when it names one that isn't there.
fn offered(cx: &mut App) -> Vec<(String, String, Vec<String>)> {
    let listed = crate::card_store::offered(cx);
    let mut out: Vec<(String, String, Vec<String>)> = Vec::new();
    for (id, keys) in BUILT_IN_FILL {
        if !listed.iter().any(|l| l.id == id) {
            let label = match id {
                "asset-3d" => "3D asset",
                "image" => "Image",
                _ => "Sound effect",
            };
            out.push((id.to_string(), label.to_string(), keys.iter().map(|k| k.to_string()).collect()));
        }
    }
    for l in listed {
        let keys = crate::card_store::manifest(&l.id, cx).and_then(|m| m.form.as_object().map(|f| f.keys().cloned().collect())).unwrap_or_default();
        out.push((l.id.clone(), l.label.trim_end_matches('…').to_string(), keys));
    }
    out
}

/// The agent's `present_card`: checked here, then opened by the session's chat when it next
/// draws (it has the window the card's fields need).
pub fn present_card(session: Entity<AgentSession>, args: &Value, cx: &mut App) -> (String, bool) {
    if !wundur_core::account::shared().is_signed_in() {
        return ("The person isn't signed in to a Wundur account, so its cards can't be shown. Ask them to sign in (the Create menu offers it) if they want to use them.".into(), true);
    }
    let card = args.get("card").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let question = args.get("question").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let offered = offered(cx);
    let list = || offered.iter().map(|(id, label, keys)| format!("- {id} ({label}): fill {}", if keys.is_empty() { "nothing".into() } else { keys.join(", ") })).collect::<Vec<_>>().join("\n");
    let Some((_, label, keys)) = offered.iter().find(|(id, _, _)| *id == card) else {
        return (format!("No card \"{card}\". The cards here:\n{}", list()), true);
    };
    let fill = args.get("fill").and_then(Value::as_object).cloned().unwrap_or_default();
    let unknown: Vec<&String> = fill.keys().filter(|k| !keys.contains(k)).collect();
    if !unknown.is_empty() {
        let names = unknown.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ");
        return (format!("The {card} card has no {names} to fill in. It takes: {}.", if keys.is_empty() { "nothing".into() } else { keys.join(", ") }), true);
    }
    let label = label.clone();
    session.update(cx, |s, cx| {
        s.present.push(Present { card, fill, question });
        cx.notify();
    });
    (
        format!(
            "Shown to the person in the chat: the {label} card, filled in as you asked. They finish it and press its button themselves (that's where anything is spent). What they make comes to this session as a new message; carry on with other work or finish your turn, and don't present it again."
        ),
        false,
    )
}
