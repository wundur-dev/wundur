//! Undo last turn. A restore point ([`wundur_core::snapshots`]) is taken just before a message
//! goes to the agent and again when its turn ends; undoing puts back the files that changed
//! between the two, and, where the agent can continue a conversation without its last turn
//! (Claude Code in Chat), takes the turn out of the conversation too, so the agent no longer
//! remembers it. Elsewhere the files are put back and the agent is said to still remember it.
//!
//! Only sessions on this machine: over SSH there are no restore points.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AsyncApp, Context, WeakEntity};
use wundur_core::snapshots::{Changed, Store};

use super::{AgentSession, SessionEvent};

/// The longest a message waits for its restore point. Slower than that (a first snapshot of a
/// very large folder), it goes without one: that turn can't be undone, the next ones can.
const WAIT_AT_MOST: Duration = Duration::from_secs(3);

/// A session's restore points.
#[derive(Default)]
pub struct RestorePoints {
    /// The project's store, once opened (`Err`: it can't have one, and why).
    store: Option<Result<Arc<Store>, String>>,
    /// The latest snapshot: the end of the last turn, or the first one taken.
    last: Option<String>,
    /// Taken just before the turn that starts at this item.
    before: Option<(usize, String)>,
    /// The last turn's two points: where it starts in the chat, before, after.
    turn: Option<(usize, String, String)>,
    /// What undoing would do, being shown for the person to confirm.
    plan: Option<UndoPlan>,
    /// Working out a plan, or undoing.
    working: bool,
    /// Said once per session that undo isn't available here.
    said_unavailable: bool,
}

/// What undoing the last turn would do.
#[derive(Clone, Debug)]
pub struct UndoPlan {
    /// Where the turn starts in the chat.
    pub turn: usize,
    /// The files it changed, relative to the session's folder.
    pub files: Vec<Changed>,
    /// Of those, the ones changed again since (by the person or another session).
    pub conflicts: Vec<PathBuf>,
    /// The agent forgets the turn too (else only the files go back).
    pub forgets: bool,
}

type Then = Box<dyn FnOnce(&mut AgentSession, &mut Context<AgentSession>)>;

impl AgentSession {
    fn restore_store(&self) -> Option<Arc<Store>> {
        self.restore.store.as_ref()?.as_ref().ok().cloned()
    }

    fn restore_unavailable(&self) -> bool {
        self.host.is_remote() || matches!(self.restore.store, Some(Err(_)))
    }

    /// Take a restore point before the turn that starts at item `turn`, then `then` (send the
    /// message): as soon as it's taken, or after [`WAIT_AT_MOST`] without it.
    pub(super) fn before_turn(&mut self, turn: usize, then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        if self.restore_unavailable() {
            then(self, cx);
            return;
        }
        let then: Rc<RefCell<Option<Then>>> = Rc::new(RefCell::new(Some(Box::new(then))));
        let (store, cwd) = (self.restore_store(), self.cwd.clone());
        let late = then.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(WAIT_AT_MOST).await;
            if let Some(f) = late.borrow_mut().take() {
                let _ = this.update(cx, |s, cx| f(s, cx));
            }
        })
        .detach();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let taken = cx
                .background_executor()
                .spawn(async move {
                    let store = match store {
                        Some(s) => s,
                        None => Arc::new(Store::open(&cwd).map_err(|e| format!("{e:#}"))?),
                    };
                    let id = store.snapshot().map_err(|e| format!("{e:#}"))?;
                    Ok::<_, String>((store, id))
                })
                .await;
            let in_time = then.borrow_mut().take();
            let _ = this.update(cx, |s, cx| {
                match taken {
                    Ok((store, id)) => {
                        s.restore.store = Some(Ok(store));
                        if in_time.is_some() {
                            s.restore.before = Some((turn, id.clone()));
                        }
                        s.restore.last = Some(id);
                    }
                    Err(e) => s.restore_failed(e),
                }
                if let Some(f) = in_time {
                    f(s, cx);
                }
            });
        })
        .detach();
    }

    fn restore_failed(&mut self, why: String) {
        eprintln!("wundur: restore points for {}: {why}", self.cwd.display());
        // A folder too big or too broad stays without; another failure may pass next time.
        if why.contains("too many files") || why.contains("too much") {
            self.restore.store = Some(Err(why));
            if !std::mem::replace(&mut self.restore.said_unavailable, true) {
                self.chat.push_notice("This folder is too big for restore points, so turns here can't be undone.");
            }
        }
    }

    /// The turn ended: take its after point, so it can be undone.
    pub(super) fn after_turn(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.restore_store() else { return };
        let turn = self.chat.turn_start();
        // Before it: the point taken when it was sent, else (a message typed in the agent's
        // own terminal) the end of the turn before.
        let before = match self.restore.before.take() {
            Some((t, id)) if t == turn => Some(id),
            _ => self.restore.last.clone(),
        };
        self.restore.turn = None;
        let name = self.chat.session_id.clone().unwrap_or_else(|| "new".into());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let after = cx
                .background_executor()
                .spawn(async move {
                    let id = store.snapshot().ok()?;
                    let _ = store.keep(&format!("{name}/after"), &id);
                    if let Some(b) = &before {
                        let _ = store.keep(&format!("{name}/before"), b);
                    }
                    Some((before, id))
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                let Some((before, after)) = after else { return };
                s.restore.last = Some(after.clone());
                // Still the last turn, and finished.
                if let Some(before) = before
                    && s.chat.turn_start() == turn
                    && !s.chat.busy
                {
                    s.restore.turn = Some((turn, before, after));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// What undoing the last turn would do, while it is shown for the person to confirm.
    pub fn undo_plan(&self) -> Option<&UndoPlan> {
        self.restore.plan.as_ref()
    }

    /// Working out what to undo, or undoing.
    pub fn undo_working(&self) -> bool {
        self.restore.working
    }

    /// Whether the last turn can be undone now.
    pub fn can_undo(&self) -> bool {
        !self.chat.busy && !self.restore.working && self.restore.turn.as_ref().is_some_and(|(t, _, _)| *t == self.chat.turn_start())
    }

    /// Work out what undoing the last turn would do, for the person to confirm.
    pub fn plan_undo(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some((turn, before, after))) = (self.restore_store(), self.restore.turn.clone()) else { return };
        if !self.can_undo() {
            return;
        }
        self.restore.working = true;
        // The agent forgets the turn too in Chat, when it can continue a conversation without
        // it (its own terminal interface keeps the whole conversation).
        let ask = (!self.interactive()).then(|| self.chat.session_id.clone()).flatten();
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let plan = cx
                .background_executor()
                .spawn(async move {
                    let files = store.changes(&before, &after).map_err(|e| format!("{e:#}"))?;
                    let conflicts = store.changed_since(&after, &files).map_err(|e| format!("{e:#}"))?;
                    let forgets = ask.is_some_and(|id| backend.rewind_point(host.as_ref(), &cwd, &id).is_ok());
                    Ok::<_, String>(UndoPlan { turn, files, conflicts, forgets })
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.restore.working = false;
                match plan {
                    Ok(plan) => s.restore.plan = Some(plan),
                    Err(e) => s.chat.push_error(format!("Could not work out what to undo: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cancel_undo(&mut self, cx: &mut Context<Self>) {
        self.restore.plan = None;
        cx.notify();
    }

    /// Undo the last turn as planned. `keep_later`: leave the files changed again since alone.
    pub fn undo_turn(&mut self, keep_later: bool, cx: &mut Context<Self>) {
        let Some(plan) = self.restore.plan.take() else { return };
        let (Some(store), Some((turn, before, _))) = (self.restore_store(), self.restore.turn.clone()) else { return };
        if turn != plan.turn || !self.can_undo() {
            cx.notify();
            return;
        }
        let files: Vec<Changed> =
            plan.files.iter().filter(|c| !(keep_later && plan.conflicts.contains(&c.path))).cloned().collect();
        self.restore.working = true;
        let (host, backend, cwd, id) = (self.host.clone(), self.backend.clone(), self.cwd.clone(), self.chat.session_id.clone());
        let forgets = plan.forgets;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let restored_files = files.clone();
            let done = cx
                .background_executor()
                .spawn(async move {
                    store.restore(&before, &restored_files).map_err(|e| format!("{e:#}"))?;
                    let rewind = match (&id, forgets) {
                        (Some(id), true) => Some(backend.rewind_point(host.as_ref(), &cwd, id)),
                        _ => None,
                    };
                    Ok::<_, String>(rewind)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.restore.working = false;
                s.restore.turn = None;
                s.restore.last = None;
                let rewind = match done {
                    Ok(r) => r,
                    Err(e) => {
                        s.chat.push_error(format!("Could not undo the turn: {e}"));
                        cx.notify();
                        return;
                    }
                };
                for c in &files {
                    cx.emit(SessionEvent::FileChanged(s.cwd.join(&c.path)));
                }
                let n = match files.len() {
                    0 => "no files to put back".to_string(),
                    1 => "1 file put back".to_string(),
                    n => format!("{n} files put back"),
                };
                match rewind {
                    Some(Ok(point)) => {
                        // The agent starts again on the conversation without the turn: a copy
                        // cut before it (its id comes with the start), or none for a first turn.
                        s.stop(cx);
                        match point {
                            Some(p) => {
                                s.rewind_next = Some(p);
                                s.fork_next = true;
                            }
                            None => s.chat.session_id = None,
                        }
                        let text = s.chat.drop_last_turn();
                        s.chat.push_notice(format!("Undid the last turn ({n}). {} won't remember it.", s.backend.name()));
                        cx.emit(SessionEvent::Undone(text));
                    }
                    Some(Err(why)) => s.chat.push_notice(format!(
                        "Undid the last turn's files ({n}). {} still remembers the turn ({why}).",
                        s.backend.name()
                    )),
                    None => s.chat.push_notice(format!(
                        "Undid the last turn's files ({n}). {} still remembers the turn: tell it, if that matters.",
                        s.backend.name()
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Claude refused to continue without the turn: it continues the whole conversation.
    pub(super) fn rewind_refused(&mut self, line: &str) -> bool {
        if !line.contains("--resume-drops-turn") && !line.contains("--resume-session-at") {
            return false;
        }
        self.rewind_next = None;
        self.fork_next = false;
        self.chat.push_error(format!(
            "{} couldn't leave out the undone turn, so it still remembers it. The files stay undone; send the message again.",
            self.backend.name()
        ));
        true
    }
}
