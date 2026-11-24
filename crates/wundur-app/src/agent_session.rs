//! One chat session: an agent (from its backend) and the conversation it feeds. No drawing here.
//!
//! The agent runs headless, behind the window, over its structured stream (see
//! [`wundur_core::agent::Agent`]): the chat is built from the events it sends, and messages,
//! permission answers and interrupts go back the same way. An agent's own terminal interface
//! is never run here; a terminal tab is there for a shell.

mod undo;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::summary;
use wundur_core::agent::{Agent, Backend, Event, Message, ModelOption, StartOptions};
use wundur_core::conversation::Conversation;
use wundur_core::host::Host;
use wundur_core::outbox::{Outbox, SendNow, Waiting};

use crate::hooks::{self, Registration};

/// Parts of a session read for a search: each part's items, by where the part starts.
pub type EarlierParts = HashMap<u64, Vec<wundur_core::conversation::Item>>;

/// What a session has used, as the window shows it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Spend {
    /// Dollars (`estimated`: from token counts and published prices).
    Cost { total: f64, turn: Option<f64>, estimated: bool },
    /// How full the context window is after the last turn.
    Context { percent: u32, tokens: u64, window: u64, fill: Fill },
}

/// Whether cost shows rather than the context, as the setting says and, on auto, for a session
/// billed per use (`per_use`).
pub fn shows_cost(per_use: Option<bool>, cx: &App) -> bool {
    match cx.global::<crate::AppSettings>().0.borrow().cost_display.as_str() {
        "always" => true,
        "never" => false,
        _ => per_use == Some(true),
    }
}

/// How close a session's context is to where its agent compacts it by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fill {
    Room,
    /// From 70% of the way to the compaction point.
    Filling,
    /// From 90%: the agent compacts soon.
    Nearly,
}

impl Fill {
    pub fn of(tokens: u64, compacts_at: u64) -> Self {
        match tokens * 100 / compacts_at.max(1) {
            90.. => Fill::Nearly,
            70.. => Fill::Filling,
            _ => Fill::Room,
        }
    }
}

/// How much of its context a session uses, after the last turn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextUse {
    pub tokens: u64,
    pub window: u64,
    /// Where the agent compacts by itself (a little short of the window).
    pub compacts_at: u64,
}

impl ContextUse {
    pub fn fill(&self) -> Fill {
        Fill::of(self.tokens, self.compacts_at)
    }
}

impl Spend {
    /// Short, for the session list.
    pub fn short(&self) -> String {
        match self {
            Spend::Cost { total, estimated, .. } => {
                let est = if *estimated { "~" } else { "" };
                if *total < 0.1 { format!("{est}${total:.3}") } else { format!("{est}${total:.2}") }
            }
            Spend::Context { percent, .. } => format!("{percent}%"),
        }
    }
}

/// The least time between two batches of the agent's messages (see `listen`).
const BATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// Something the rest of the window should react to.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// The agent wrote or edited this file.
    FileChanged(PathBuf),
    TurnEnded,
    /// A tool made a page to look at (a pack's model viewer): open it in the web panel.
    ShowPage(PathBuf),
    /// Show a 3D model in the model viewer, optionally playing an animation (part of its name).
    ShowModel(PathBuf, Option<String>),
    /// The last turn was undone and taken out of the conversation: the person's message, to
    /// put back in the message box.
    Undone(Option<String>),
}

/// The issue a write refused by the sandbox raises (Wundur's own, not the agent's).
pub(crate) const SANDBOX_ISSUE: &str = "wundur-sandbox";

pub struct AgentSession {
    host: Arc<dyn Host>,
    /// Opened again at start: its conversation shows, but its agent starts only when it is
    /// needed (a message), so reopening many sessions costs nothing.
    pub dormant: bool,
    /// Reads the journals of the workflows running (see `follow_workflows`).
    workflow_poll: Option<Task<()>>,
    pub cwd: PathBuf,
    pub chat: Conversation,
    pub backend: Arc<dyn Backend>,
    /// Every model the agent lists for this person (see [`Backend::list_models`]); `None` until
    /// asked ([`AgentSession::want_models`]).
    pub listed_models: Option<Vec<ModelOption>>,
    /// The agent, headless (its structured stream).
    agent: Box<dyn Agent>,
    /// `None` is the backend's default mode (it asks); otherwise its own mode name.
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    /// The effort level, for an agent that takes one apart from the model; `None`: its default.
    pub effort: Option<String>,
    /// Something happened since the user last looked at this session.
    pub unseen: bool,
    /// The next start forks the resumed session.
    fork_next: bool,
    /// The next start (with `fork_next`) leaves out the undone last turn.
    rewind_next: Option<wundur_core::agent::Rewind>,
    /// A message is being sent again (see `send_again`): a second ask meanwhile does nothing new.
    resending: bool,
    /// Restore points for undoing the last turn (see `undo.rs`).
    restore: undo::RestorePoints,
    /// Its agent may write anywhere; else only in its folder and `writable` (see
    /// [`wundur_core::sandbox`]). Kept with the session.
    pub writes_anywhere: bool,
    pub writable: Vec<PathBuf>,
    /// The agent last started with its writes kept to its folder.
    confined: bool,
    /// A reviewer: it reads and answers, its folder not writable (see `messaging`).
    pub read_only: bool,
    /// Messages from other sessions on their way to the agent: they go as turns, but show as
    /// the other session's card, not as the person's message.
    quiet: Vec<String>,
    /// Waiting for an agent process elsewhere (this pid) to exit before resuming here.
    pub waiting_for: Option<u32>,
    _reader: Option<Task<()>>,
    hooks: Option<Registration>,
    /// With `WUNDUR_PERF=1`: when the last message was sent, until its reply shows.
    sent_at: Option<std::time::Instant>,
    /// Billed per use (an API key) rather than by a plan; `None` until known.
    pub per_use: Option<bool>,
    /// A watch for the agent's sign-in runs (see `watch_sign_in`).
    sign_in_watching: bool,
    /// A watch for the agent being installed runs (see `watch_install`).
    install_watching: bool,
    /// Its conversation is being read (reopened from the last run): the chat shows its shape.
    pub loading_chat: bool,
    /// Messages sent while the conversation was still being read (see `loading_chat`): sent
    /// once it is there, so they follow it rather than being replaced by it.
    held_for_chat: Vec<(String, Vec<PathBuf>)>,
    /// The person's last message is set to go again at this time (Unix), when the usage
    /// limit resets.
    pub scheduled_retry: Option<u64>,
    /// Counts uses of the session: an idle timer set before the last one does nothing.
    idle_gen: u64,
    /// The current turn (where it starts in the chat) and the git working tree when it started,
    /// once read: what commands changed is found against it when the turn ends.
    git_turn: Option<(usize, Option<wundur_core::command_changes::Baseline>)>,
    /// Messages sent while the agent works, each sent when the turn before it ends.
    outbox: Outbox,
    /// The person stopped the turn: what waits stays until they send it, or send another.
    outbox_paused: bool,
    /// Turns whose summary was asked for (by key), so a turn's end seen twice asks once.
    summarized: HashSet<String>,
}

impl EventEmitter<SessionEvent> for AgentSession {}

impl AgentSession {
    /// Apply a fix for a known tool failure (see `Issue`) in this session's folder.
    pub fn apply_fix(&mut self, kind: &str, fix: &str, cx: &mut Context<Self>) -> std::io::Result<String> {
        if kind == SANDBOX_ISSUE {
            return Ok(match fix.strip_prefix("allow:") {
                Some(folder) => {
                    self.set_writable(PathBuf::from(folder), true, cx);
                    format!("Writes allowed in {folder} for this session.")
                }
                None => {
                    self.set_writes_anywhere(true, cx);
                    "Writes allowed anywhere for this session.".to_string()
                }
            });
        }
        self.backend.apply_fix(self.host.as_ref(), &self.cwd, kind, fix)
    }

    pub fn new(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf) -> Self {
        Self {
            agent: backend.new_agent(),
            backend,
            host,
            dormant: false,
            workflow_poll: None,
            listed_models: None,
            chat: Conversation::new(cwd.to_string_lossy().to_string()),
            cwd,
            permission_mode: wundur_core::env_var("PERMISSION_MODE").ok().filter(|m| !m.is_empty()),
            model: wundur_core::env_var("MODEL").ok().filter(|m| !m.is_empty()),
            effort: None,
            unseen: false,
            fork_next: false,
            rewind_next: None,
            resending: false,
            restore: Default::default(),
            writes_anywhere: false,
            writable: Vec::new(),
            confined: false,
            read_only: false,
            quiet: Vec::new(),
            waiting_for: None,
            _reader: None,
            hooks: None,
            sent_at: None,
            per_use: None,
            sign_in_watching: false,
            install_watching: false,
            loading_chat: false,
            held_for_chat: Vec::new(),
            scheduled_retry: None,
            idle_gen: 0,
            git_turn: None,
            outbox: Outbox::default(),
            outbox_paused: false,
            summarized: HashSet::new(),
        }
    }

    /// A session continuing an earlier one, its history already rebuilt from the file.
    /// `fork`: continue in a copy (the original runs elsewhere).
    pub fn resumed(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf, chat: Conversation, fork: bool) -> Self {
        let mut s = Self::new(host, backend, cwd);
        s.chat = chat;
        s.attach_summaries();
        s.chat.process_ended();
        s.fork_next = fork;
        s.chat.push_notice(if fork {
            "Forked: this continues a copy of the conversation here; the original keeps running."
        } else {
            "Resumed: the next message continues this conversation."
        });
        s
    }

    /// The conversation read for a session opened before it was ready (see `loading_chat`):
    /// it takes it as [`AgentSession::resumed`] does.
    pub fn take_chat(&mut self, chat: Conversation, cx: &mut Context<Self>) {
        let revision = self.chat.revision.max(chat.revision) + 1;
        self.chat = chat;
        self.attach_summaries();
        self.chat.process_ended();
        self.chat.push_notice("Resumed: the next message continues this conversation.");
        self.chat.revision = revision;
        self.chat_read(cx);
    }

    /// The conversation is as read as it will be (there, or it could not be read): what was
    /// sent meanwhile goes now.
    pub fn chat_read(&mut self, cx: &mut Context<Self>) {
        self.loading_chat = false;
        for (text, images) in std::mem::take(&mut self.held_for_chat) {
            self.send_with(&text, images, cx);
        }
        cx.notify();
    }

    /// Read an earlier part of this session's file (off the UI thread).
    pub fn earlier_loader(&self) -> Option<impl FnOnce(u64, u64) -> std::io::Result<Vec<wundur_core::conversation::Item>> + Send + 'static> {
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        let id = self.chat.session_id.clone()?;
        Some(move |from, to| backend.load_segment(host.as_ref(), &cwd, &id, from, to))
    }

    /// Read several parts of the session not in the list (for a search): each part's items by
    /// where it starts; a part that cannot be read is left out.
    pub fn earlier_loader_many(&self) -> Option<impl FnOnce(Vec<(u64, u64)>) -> EarlierParts + Send + 'static> {
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        let id = self.chat.session_id.clone()?;
        Some(move |parts: Vec<(u64, u64)>| {
            parts
                .into_iter()
                .filter_map(|(from, to)| Some((from, backend.load_segment(host.as_ref(), &cwd, &id, from, to).ok()?)))
                .collect()
        })
    }

    /// Messages sent while the agent works, in the order they go.
    pub fn waiting(&self) -> impl Iterator<Item = &Waiting> {
        self.outbox.items()
    }

    /// Whether what waits stays until the person sends it (they stopped the turn).
    pub fn waiting_paused(&self) -> bool {
        self.outbox_paused && !self.outbox.is_empty()
    }

    /// Whether a message sent now goes into the running turn: the agent reads it as it works.
    pub fn takes_mid_turn(&self) -> bool {
        self.agent.takes_mid_turn()
    }

    /// Send waiting message `id` now: into the running turn when the agent takes it, else it
    /// goes first once the turn has been stopped.
    pub fn send_waiting_now(&mut self, id: u64, cx: &mut Context<Self>) {
        self.outbox_paused = false;
        if self.outbox.editing() == Some(id) {
            self.outbox.set_editing(None);
        }
        match SendNow::decide(self.chat.busy, self.takes_mid_turn()) {
            SendNow::Now if self.chat.busy => {
                if let Some(w) = self.outbox.remove(id) {
                    self.deliver(&w.text, w.images, cx);
                }
            }
            SendNow::Now => {
                self.outbox.to_front(id);
                self.send_waiting(cx);
            }
            SendNow::StopFirst => {
                if self.outbox.to_front(id) {
                    self.stop_turn(cx);
                }
            }
        }
        cx.notify();
    }

    /// The waiting message being changed, if one is.
    pub fn editing_waiting(&self) -> Option<u64> {
        self.outbox.editing()
    }

    /// Start changing waiting message `id`: it stays in its place and doesn't go until the
    /// change is saved or given up. False when it has gone already.
    pub fn start_editing_waiting(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let held = self.outbox.set_editing(Some(id));
        cx.notify();
        held
    }

    /// Finish changing waiting message `id`: its new text (`None` keeps it as it was). Then
    /// what waits goes as usual (the turn may have ended meanwhile).
    pub fn finish_editing_waiting(&mut self, id: u64, text: Option<String>, cx: &mut Context<Self>) {
        if let Some(text) = text {
            self.outbox.edit(id, text);
        }
        if self.outbox.editing() == Some(id) {
            self.outbox.set_editing(None);
        }
        self.send_waiting(cx);
        cx.notify();
    }

    /// Drop waiting message `id`.
    pub fn remove_waiting(&mut self, id: u64, cx: &mut Context<Self>) {
        self.outbox.remove(id);
        cx.notify();
    }

    /// Send what waits while the agent is free: the first message, which starts a turn; the
    /// next waits for that turn to end.
    fn send_waiting(&mut self, cx: &mut Context<Self>) {
        while !self.chat.busy && !self.outbox_paused && self.waiting_for.is_none() {
            let Some(w) = self.outbox.next(self.chat.busy) else { break };
            self.deliver(&w.text, w.images, cx);
            cx.notify();
        }
    }

    /// Set the session up as it opens; its agent starts with the first message.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        // How it is billed decides whether cost or context shows (settings files: off the UI
        // thread).
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let per_use = cx.background_spawn(async move { backend.billed_per_use(host.as_ref(), &cwd) }).await;
            let _ = this.update(cx, |s, cx| {
                s.per_use = per_use;
                cx.notify();
            });
        })
        .detach();
        // Always a mode of its own, passed to the agent as it starts.
        if self.permission_mode.is_none() {
            self.permission_mode = Some(cx.global::<crate::AppSettings>().0.borrow().mode_for(self.chat.session_id.as_deref()));
        }
    }

    /// The process elsewhere has exited: this session's own can start.
    pub fn takeover_done(&mut self, cx: &mut Context<Self>) {
        self.waiting_for = None;
        self.send_waiting(cx);
        cx.notify();
    }

    /// Cost when it is billed per use (or the setting says always), else how full the context
    /// is. `None` before there is anything to show.
    pub fn spend(&self, cx: &App) -> Option<Spend> {
        self.spend_as(shows_cost(self.per_use, cx))
    }

    /// What it used, as cost (`cost`) or as how full its context is.
    pub fn spend_as(&self, cost: bool) -> Option<Spend> {
        if cost {
            return (self.chat.total_cost_usd > 0.0).then_some(Spend::Cost {
                total: self.chat.total_cost_usd,
                turn: self.chat.last_turn_cost_usd,
                estimated: self.chat.cost_estimated,
            });
        }
        let used = self.context_use()?;
        let ContextUse { tokens, window, .. } = used;
        Some(Spend::Context { percent: ((tokens * 100) / window.max(1)) as u32, tokens, window, fill: used.fill() })
    }

    /// How much of its context the session uses: the window as the agent reported it, else
    /// as Wundur knows the model. `None` before a turn has ended, or when the agent says nothing.
    pub fn context_use(&self) -> Option<ContextUse> {
        let tokens = self.chat.last_turn_tokens?.0;
        let window = match self.chat.context_window {
            Some(w) => w,
            None => {
                let model = self.chat.model.clone().or_else(|| self.model.clone())?;
                self.backend.context_window(&model)?
            }
        };
        (tokens > 0 && window > 0).then(|| ContextUse { tokens, window, compacts_at: self.backend.compacts_at(window) })
    }

    /// Ask the agent to compact its context now, when it can be asked.
    pub fn compact(&mut self, cx: &mut Context<Self>) {
        if let Some(command) = self.backend.compact_command() {
            self.send(command, cx);
        }
    }

    /// Where the agent may write: its folder (and its own state, caches, temporary folders),
    /// when Settings keeps agents' writes there and this machine can; else `None` (anywhere).
    /// An agent on another machine runs unconfined.
    fn write_policy(&self, cx: &App) -> Option<wundur_core::sandbox::WritePolicy> {
        if self.host.is_remote() {
            return None;
        }
        // A reviewer writes nowhere in its folder, whatever Settings says.
        if self.read_only {
            let mut extra = wundur_core::mcp::writes(&self.mcp_servers(cx));
            extra.retain(|f| !f.starts_with(&self.cwd));
            return Some(wundur_core::sandbox::read_only_policy(&self.cwd, &self.backend.state_paths(), &extra));
        }
        if !crate::prefs(cx).sandbox_writes || self.writes_anywhere {
            return None;
        }
        // Its tool servers run inside it: where they write is allowed too; and the folders
        // its project allows every session.
        let mut extra = self.writable.clone();
        extra.extend(project_writable(&self.cwd, cx));
        extra.extend(wundur_core::mcp::writes(&self.mcp_servers(cx)));
        Some(wundur_core::sandbox::policy_for(&self.cwd, &self.backend.state_paths(), &extra))
    }

    /// The tool servers set up in Settings for this session's folder (none over SSH: they
    /// run where the agent does).
    fn mcp_servers(&self, cx: &App) -> Vec<wundur_core::mcp::Server> {
        if self.host.is_remote() {
            return Vec::new();
        }
        let s = cx.global::<crate::AppSettings>().0.borrow();
        wundur_core::mcp::servers_for(&s.mcp_servers, &s.folder_mcp, &self.cwd)
    }

    /// The tool servers file for the agent: the window's own (the messaging tools) and those
    /// set up in Settings; on this machine only.
    fn mcp_config(&self, cx: &App) -> Option<PathBuf> {
        if self.host.is_remote() {
            return None;
        }
        self.hooks.as_ref().and_then(|h| h.write_mcp(&self.mcp_servers(cx)))
    }

    /// A message from the session `from`: shown as its card, and sent to the agent as a turn
    /// (after the one it is in), marked as from that session, with how to answer.
    pub fn receive_message(&mut self, from: &str, text: &str, cx: &mut Context<Self>) {
        self.chat.apply(Event::AgentMessage { from: format!("From {from}"), text: text.to_string() });
        let framed = format!(
            "[A message from the Wundur session \"{from}\", another agent (not the person):]\n\n{text}\n\n[If it needs an answer, use the wundur send_message tool with to: \"{from}\".]"
        );
        self.quiet.push(framed.clone());
        self.send(&framed, cx);
        cx.notify();
    }

    /// A message the agent sent another session (`to`), as its card in this chat.
    pub fn note_outgoing(&mut self, id: u64, to: &str, text: &str, state: wundur_core::conversation::SendState, cx: &mut Context<Self>) {
        self.chat.items.push(wundur_core::conversation::Item::Outgoing { id, to: to.to_string(), text: text.to_string(), state });
        self.chat.revision += 1;
        cx.notify();
    }

    /// Where message `id` is now (allowed, denied, failed).
    pub fn set_outgoing(&mut self, id: u64, state: wundur_core::conversation::SendState, cx: &mut Context<Self>) {
        for item in self.chat.items.iter_mut().rev() {
            if let wundur_core::conversation::Item::Outgoing { id: i, state: s, .. } = item
                && *i == id
            {
                *s = state;
                break;
            }
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// A run of one of the person's tools the agent asked for, as its card (see `tool_runs`).
    pub fn note_tool_run(&mut self, id: u64, tool: &str, command: &str, state: wundur_core::conversation::SendState, cx: &mut Context<Self>) {
        self.chat.items.push(wundur_core::conversation::Item::ToolRun { id, tool: tool.to_string(), command: command.to_string(), state });
        self.chat.revision += 1;
        cx.notify();
    }

    /// Where tool run `id` is now (allowed, denied).
    pub fn set_tool_run(&mut self, id: u64, state: wundur_core::conversation::SendState, cx: &mut Context<Self>) {
        for item in self.chat.items.iter_mut().rev() {
            if let wundur_core::conversation::Item::ToolRun { id: i, state: s, .. } = item
                && *i == id
            {
                *s = state;
                break;
            }
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// Where the sign-in card is now (the last one).
    fn set_sign_in(&mut self, state: wundur_core::conversation::SignIn, cx: &mut Context<Self>) {
        if let Some(wundur_core::conversation::Item::SignIn(s)) =
            self.chat.items.iter_mut().rev().find(|i| matches!(i, wundur_core::conversation::Item::SignIn(_)))
        {
            *s = state;
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// Start the agent's own sign-in: the command line to run in a terminal (the chat runs
    /// it), while Wundur asks the agent every few seconds whether it is signed in yet.
    pub fn start_sign_in(&mut self, cx: &mut Context<Self>) -> Option<String> {
        use wundur_core::conversation::{Item, SignIn};
        let spec = self.backend.sign_in()?;
        let line = std::iter::once(&spec.program).chain(&spec.args).map(|w| wundur_core::host::shell_quote(w)).collect::<Vec<_>>().join(" ");
        // From the menu, before anything was refused: a card of its own to follow it on.
        let open = self.chat.items.iter().rev().find_map(|i| match i {
            Item::SignIn(s) => Some(*s != SignIn::Done),
            _ => None,
        });
        if open != Some(true) {
            self.chat.items.push(Item::SignIn(SignIn::Needed));
        }
        self.set_sign_in(SignIn::Waiting, cx);
        self.watch_sign_in(cx);
        Some(line)
    }

    /// While the sign-in card shows, ask the agent whether it is signed in (its status
    /// command): every few seconds once its sign-in runs, less often while the card only waits,
    /// for up to an hour. Signed in, however it was done, the card says so and the agent starts
    /// afresh with its next message (a running one keeps the sign-in it started with).
    pub fn watch_sign_in(&mut self, cx: &mut Context<Self>) {
        use wundur_core::conversation::{Item, SignIn};
        if self.sign_in_watching {
            return;
        }
        self.sign_in_watching = true;
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3600);
            while std::time::Instant::now() < until {
                let (backend, host) = (backend.clone(), host.clone());
                let signed = cx.background_spawn(async move { backend.signed_in(host.as_ref()) }).await;
                let card = this
                    .update(cx, |s, cx| {
                        let card = s.chat.items.iter().rev().find_map(|i| match i {
                            Item::SignIn(st) => Some(*st),
                            _ => None,
                        });
                        if signed == Some(true) && card.is_some_and(|c| c != SignIn::Done) {
                            s.stop(cx);
                            s.set_sign_in(SignIn::Done, cx);
                            return None;
                        }
                        card.filter(|c| *c != SignIn::Done)
                    })
                    .ok()
                    .flatten();
                let Some(card) = card else { break };
                let pause = if card == SignIn::Waiting { 3 } else { 15 };
                cx.background_executor().timer(std::time::Duration::from_secs(pause)).await;
            }
            let _ = this.update(cx, |s, cx| {
                s.sign_in_watching = false;
                let waiting = s.chat.items.iter().rev().find_map(|i| match i {
                    Item::SignIn(st) => Some(*st == SignIn::Waiting),
                    _ => None,
                });
                if waiting == Some(true) {
                    s.set_sign_in(SignIn::Needed, cx);
                }
            });
        })
        .detach();
    }

    // ----- installing the agent -----

    fn install_card(&self) -> Option<wundur_core::conversation::SignIn> {
        self.chat.items.iter().rev().find_map(|i| match i {
            wundur_core::conversation::Item::Install(st) => Some(*st),
            _ => None,
        })
    }

    fn set_install(&mut self, state: wundur_core::conversation::SignIn, cx: &mut Context<Self>) {
        use wundur_core::conversation::Item;
        if let Some(Item::Install(s)) = self.chat.items.iter_mut().rev().find(|i| matches!(i, Item::Install(_))) {
            *s = state;
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// The agent is not on this machine: the install card (one at a time), watched until it is.
    pub fn show_install(&mut self, cx: &mut Context<Self>) {
        use wundur_core::conversation::{Item, SignIn};
        if self.install_card().is_none_or(|c| c == SignIn::Done) {
            self.chat.items.push(Item::Install(SignIn::Needed));
            self.chat.revision += 1;
        }
        self.watch_install(cx);
        cx.notify();
    }

    /// Start the agent's own installer: the command line to run in a terminal (the chat runs
    /// it), while Wundur looks for the agent every few seconds.
    pub fn start_install(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let install = self.backend.install()?;
        if self.install_card().is_none() {
            self.chat.items.push(wundur_core::conversation::Item::Install(wundur_core::conversation::SignIn::Needed));
        }
        self.set_install(wundur_core::conversation::SignIn::Waiting, cx);
        self.watch_install(cx);
        Some(install.command)
    }

    /// While the install card is open, look for the agent: every few seconds once its installer
    /// runs, less often while the card only waits, for up to an hour. Found (however it was
    /// installed), the card says so, the agent starts afresh with the next message, and its
    /// sign-in is asked.
    fn watch_install(&mut self, cx: &mut Context<Self>) {
        use wundur_core::conversation::SignIn;
        if self.install_watching {
            return;
        }
        self.install_watching = true;
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3600);
            while std::time::Instant::now() < until {
                let (b, h) = (backend.clone(), host.clone());
                let installed = cx.background_spawn(async move { b.installed(h.as_ref()) }).await;
                let card = this
                    .update(cx, |s, cx| {
                        let card = s.install_card().filter(|c| *c != SignIn::Done);
                        if installed != Some(false) && card.is_some() {
                            s.stop(cx);
                            s.set_install(SignIn::Done, cx);
                            s.install_watching = false;
                            s.check_sign_in(cx);
                            return None;
                        }
                        card
                    })
                    .ok()
                    .flatten();
                let Some(card) = card else { break };
                let pause = if card == SignIn::Waiting { 3 } else { 10 };
                cx.background_executor().timer(std::time::Duration::from_secs(pause)).await;
            }
            let _ = this.update(cx, |s, cx| {
                s.install_watching = false;
                if s.install_card() == Some(SignIn::Waiting) {
                    s.set_install(SignIn::Needed, cx);
                }
            });
        })
        .detach();
    }

    /// Ask the agent (its status command, off the window's thread) whether it is signed in;
    /// signed out, the chat shows the sign-in card at once.
    /// Not installed, the install card shows instead (and the sign-in is asked once it is).
    pub fn check_sign_in(&mut self, cx: &mut Context<Self>) {
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (b, h) = (backend.clone(), host.clone());
            if cx.background_spawn(async move { b.installed(h.as_ref()) }).await == Some(false) {
                let _ = this.update(cx, |s, cx| s.show_install(cx));
                return;
            }
            let signed = cx.background_spawn(async move { backend.signed_in(host.as_ref()) }).await;
            if signed == Some(false) {
                let _ = this.update(cx, |s, cx| {
                    s.chat.apply(wundur_core::agent::Event::SignedOut);
                    s.chat.revision += 1;
                    s.watch_sign_in(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Send the person's last message again at `at` (Unix time: when the usage limit resets),
    /// a little after it, unless sent or cancelled before.
    pub fn retry_at(&mut self, at: u64, cx: &mut Context<Self>) {
        self.scheduled_retry = Some(at);
        cx.notify();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(at);
        let wait = std::time::Duration::from_secs(at.saturating_sub(now) + 20);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |s, cx| {
                if s.scheduled_retry == Some(at) {
                    s.send_again(cx);
                }
            });
        })
        .detach();
    }

    /// No send at the limit's reset after all.
    pub fn cancel_retry(&mut self, cx: &mut Context<Self>) {
        self.scheduled_retry = None;
        cx.notify();
    }

    /// Send the person's last message again (after signing in, or when a usage limit resets).
    ///
    /// The attempt that failed is still in the agent's record of the conversation: sent again
    /// on top of it, the agent read the message twice (three times after two failures) and the
    /// chat showed it as many. Where the agent can continue without a turn, the failed attempt
    /// is taken out first, so the message is there once. Not when the turn got as far as using
    /// a tool: that work is kept, and the message follows it as before.
    pub fn send_again(&mut self, cx: &mut Context<Self>) {
        use wundur_core::conversation::Item;
        // Already on its way (a second click): once is the point.
        if self.resending {
            return;
        }
        let Some(at) = self.chat.items.iter().rposition(|i| matches!(i, Item::User(_))) else { return };
        let Item::User(text) = self.chat.items[at].clone() else { return };
        let images = match at.checked_sub(1).map(|b| &self.chat.items[b]) {
            Some(Item::Images(paths)) => paths.clone(),
            _ => Vec::new(),
        };
        self.chat.refused_signed_out = false;
        self.scheduled_retry = None;
        let did_nothing = !self.chat.items[at..].iter().any(|i| matches!(i, Item::Tool(_)));
        let id = self.chat.session_id.clone().filter(|_| did_nothing && !self.chat.busy);
        let Some(id) = id else {
            self.send(&text, cx);
            return;
        };
        self.resending = true;
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let point = cx.background_spawn(async move { backend.rewind_point(host.as_ref(), &cwd, &id) }).await;
            let _ = this.update(cx, |s, cx| {
                s.resending = false;
                // Something came meanwhile (another message, a turn): it goes as it would have.
                let same = s.chat.items.iter().rposition(|i| matches!(i, Item::User(_))) == Some(at) && !s.chat.busy;
                match point {
                    Ok(point) if same => {
                        // The agent starts again on the conversation without the attempt: a
                        // copy cut before it, or none when it was the first message.
                        s.stop(cx);
                        match point {
                            Some(p) => {
                                s.rewind_next = Some(p);
                                s.fork_next = true;
                            }
                            None => s.chat.session_id = None,
                        }
                        s.chat.drop_last_turn();
                        s.send_with(&text, images, cx);
                    }
                    // This agent can't leave a turn out: the message follows the attempt.
                    _ => s.send(&text, cx),
                }
            });
        })
        .detach();
    }

    /// Whether it runs on another machine.
    pub fn host_is_remote(&self) -> bool {
        self.host.is_remote()
    }

    /// The folder it works in.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Make it a reviewer: its folder not writable, the agent asking before it acts.
    pub fn make_reviewer(&mut self) {
        self.read_only = true;
        self.permission_mode = Some("default".into());
    }

    /// Whether this session's agent can be kept to its folder at all (Settings has it on, this
    /// machine has a sandbox, the agent runs here).
    pub fn can_confine(&self, cx: &App) -> bool {
        crate::prefs(cx).sandbox_writes
            && !self.host.is_remote()
            && wundur_core::sandbox::system().support() == wundur_core::sandbox::Support::Available
    }

    /// Let the agent write anywhere, or keep it to its folder again; it restarts to apply.
    pub fn set_writes_anywhere(&mut self, anywhere: bool, cx: &mut Context<Self>) {
        if self.writes_anywhere == anywhere {
            return;
        }
        self.writes_anywhere = anywhere;
        let said = if anywhere { "Writes allowed anywhere" } else { "Writes kept to this folder" };
        self.restart_with(said.into(), cx);
    }

    /// Let the agent write in `folder` too (or, `allow` false, no longer); it restarts to apply.
    /// Allow (or stop allowing) writes in `folder` for every session in this session's project;
    /// this one restarts with it (the others take it when they next start).
    pub fn set_project_writable(&mut self, folder: PathBuf, allow: bool, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        let mut changed = false;
        crate::settings_window::change(cx, |s| {
            let Some(root) = wundur_core::settings::project_of(&s.projects, &cwd).map(|p| p.root.clone()) else { return };
            let Some(p) = s.projects.iter_mut().find(|p| p.root == root) else { return };
            let has = p.writable.contains(&folder);
            if has != allow {
                if allow {
                    p.writable.push(folder.clone());
                } else {
                    p.writable.retain(|f| *f != folder);
                }
                changed = true;
            }
        });
        if !changed {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| folder.display().to_string());
        let said = if allow { format!("Writes allowed in {name} for the whole project") } else { format!("Writes no longer allowed in {name} for the project") };
        self.restart_with(said, cx);
    }

    pub fn set_writable(&mut self, folder: PathBuf, allow: bool, cx: &mut Context<Self>) {
        let has = self.writable.contains(&folder);
        if has == allow {
            return;
        }
        if allow {
            self.writable.push(folder.clone());
        } else {
            self.writable.retain(|f| *f != folder);
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| folder.display().to_string());
        let said = if allow { format!("Writes allowed in {name}") } else { format!("Writes no longer allowed in {name}") };
        self.restart_with(said, cx);
    }

    /// A write the sandbox refused: the card that says so, with ways to allow it.
    fn sandbox_issue(&self, path: Option<PathBuf>) -> wundur_core::agent::Issue {
        use wundur_core::agent::{Issue, IssueFix};
        let here = self.cwd.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut fixes = Vec::new();
        // The project the refused path is in (its repository, else its folder).
        let project = path.as_deref().and_then(|p| {
            let dir = p.ancestors().skip(1).find(|d| d.is_dir())?;
            Some(crate::shell::repo_of(dir).map(|(top, _)| top).unwrap_or_else(|| dir.to_path_buf()))
        });
        if let Some(project) = project {
            let name = project.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| project.display().to_string());
            fixes.push(IssueFix {
                id: format!("allow:{}", project.display()),
                label: format!("Allow writes in {name}"),
                preview: format!("The agent restarts and may also write inside {}, for this session.", project.display()),
            });
        }
        fixes.push(IssueFix {
            id: "anywhere".into(),
            label: "Allow writes anywhere".into(),
            preview: "The agent restarts with no limit on where it writes, for this session. The ⋯ menu over the chat keeps it to its folder again.".into(),
        });
        let what = path.map(|p| format!(" to {}", p.display())).unwrap_or_default();
        Issue {
            kind: SANDBOX_ISSUE.into(),
            title: "Kept out of a folder".into(),
            detail: format!(
                "This session's agent writes only inside {here} (and its own settings, caches and temporary folders). A write{what} was refused."
            ),
            fixes,
        }
    }

    /// Ask the agent for its models, once, off the UI thread (the model menu then lists them).
    pub fn want_models(&mut self, cx: &mut Context<Self>) {
        if self.listed_models.is_some() {
            return;
        }
        self.listed_models = Some(Vec::new());
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let models = cx.background_spawn(async move { backend.list_models(host.as_ref()) }).await;
            let _ = this.update(cx, |s, cx| {
                s.listed_models = Some(models);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn set_model(&mut self, model: &str, cx: &mut Context<Self>) {
        self.model = Some(model.to_string());
        self.restart_with(format!("Model: {model}"), cx);
    }

    /// Switch the effort level (`None`: the agent's default), as the model is switched.
    pub fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        if self.effort == effort {
            return;
        }
        let name = effort.clone().unwrap_or_else(|| "default".into());
        self.effort = effort;
        self.restart_with(format!("Effort: {name}"), cx);
    }

    /// What the agent starts with changed: it ends, and starts with it on the next message.
    /// `said` goes in the chat.
    fn restart_with(&mut self, said: String, cx: &mut Context<Self>) {
        self.stop(cx);
        self.chat.push_notice(format!("{said}. Applies from the next message."));
        cx.notify();
    }

    /// Keep the session's mode for when it opens again.
    fn remember_mode(&self, mode: Option<&str>, cx: &mut Context<Self>) {
        if let (Some(id), Some(mode)) = (&self.chat.session_id, mode) {
            let settings = cx.global::<crate::AppSettings>().0.clone();
            settings.borrow_mut().set_mode(id, mode);
            let _ = settings.borrow().save();
        }
    }

    /// Switch the permission mode, kept for when the session opens again.
    pub fn set_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        let mode = Some(mode.unwrap_or_else(|| "default".into()));
        self.remember_mode(mode.as_deref(), cx);
        self.set_permission_mode(mode, cx);
    }

    // ----- the agent -----

    fn start(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        // Registered for its tool server (Wundur's own tools).
        if self.hooks.is_none() && !self.host.is_remote() {
            self.hooks = hooks::register(cx.entity().downgrade(), cx);
        }
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: self.chat.session_id.clone(),
            fork: std::mem::take(&mut self.fork_next),
            rewind: self.rewind_next.take(),
            sandbox: self.write_policy(cx),
            mcp_config: self.mcp_config(cx),
            env: wundur_core::mcp::secret_env(&self.mcp_servers(cx)),
            instructions: session_instructions(cx),
        };
        self.confined = options.sandbox.is_some();
        let (tx, rx) = async_channel::unbounded::<Message>();
        self.agent.start(
            self.host.clone(),
            &options,
            Arc::new(move |m| {
                let _ = tx.send_blocking(m);
            }),
        )?;
        self._reader = Some(Self::listen(rx, cx));
        Ok(())
    }

    /// Apply what arrives on `rx`, a batch per wake-up.
    fn listen(rx: async_channel::Receiver<Message>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let ok = this.update(cx, |session, cx| {
                    for m in batch {
                        session.handle(m, cx);
                    }
                    session.send_waiting(cx);
                    session.unseen = true;
                    session.follow_workflows(cx);
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
                // What arrives in the next tenth of a second is taken as one batch: a reply can
                // stream in dozens of pieces a second, and each change of the session draws the
                // window (the chat, its row, the working line) again. The first piece after a
                // quiet spell still shows at once.
                cx.background_executor().timer(BATCH_EVERY).await;
            }
        })
    }

    /// While a workflow runs (Claude Code's Workflow tool, shown as a background agent): every
    /// few seconds its journal is read, off the UI thread, and its card says how far it is
    /// ("9 of 13 agents done"). Claude Code reports a workflow's agents nowhere else Wundur reads.
    fn follow_workflows(&mut self, cx: &mut Context<Self>) {
        if self.workflow_poll.is_some() || self.host.is_remote() || self.running_workflows().is_empty() {
            return;
        }
        self.workflow_poll = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut shown: HashMap<String, (usize, usize)> = HashMap::new();
            loop {
                let Ok(running) = this.update(cx, |s, _| s.running_workflows()) else { break };
                if running.is_empty() {
                    let _ = this.update(cx, |s, _| s.workflow_poll = None);
                    break;
                }
                let read = cx
                    .background_spawn(async move {
                        running
                            .into_iter()
                            .map(|(id, dir)| {
                                let journal = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
                                (id, wundur_core::workflow::progress(&journal))
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let changed: Vec<_> = read.into_iter().filter(|(id, p)| shown.get(id) != Some(p)).collect();
                if !changed.is_empty() {
                    for (id, p) in &changed {
                        shown.insert(id.clone(), *p);
                    }
                    let _ = this.update(cx, |s, cx| {
                        for (id, (done, started)) in changed {
                            let what = if started == 0 { "starting".to_string() } else { format!("{done} of {started} agents done") };
                            s.chat.apply(Event::SubagentProgress { id, current_tool: Some(what), tokens: None, tool_uses: None, duration_ms: None });
                        }
                        cx.notify();
                    });
                }
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            }
        }));
    }

    /// The workflows running, with where each keeps its journal.
    fn running_workflows(&self) -> Vec<(String, PathBuf)> {
        self.chat
            .running_agents()
            .filter(|a| a.kind == "workflow")
            .filter_map(|a| {
                let output = self.chat.items.iter().find_map(|i| match i {
                    wundur_core::conversation::Item::Tool(t) if t.id == a.tool_use_id => t.result.clone(),
                    _ => None,
                })?;
                Some((a.tool_use_id.clone(), wundur_core::workflow::dir(&output)?))
            })
            .collect()
    }

    fn handle(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Event(event) => {
                if matches!(event, Event::Text { .. } | Event::TextDelta(_) | Event::ToolCall { .. })
                    && let Some(at) = self.sent_at.take()
                {
                    eprintln!("wundur: reply shows {:.0} ms after Send", at.elapsed().as_secs_f64() * 1000.0);
                }
                // A write the sandbox refused: said on the tool's card, with ways to allow it.
                let refused = match &event {
                    Event::ToolResult { id, output, .. } if self.confined => wundur_core::sandbox::refused(output).map(|p| (id.clone(), p)),
                    _ => None,
                };
                let signed_out = matches!(event, Event::SignedOut);
                let applied = self.chat.apply(event);
                if signed_out {
                    self.watch_sign_in(cx);
                }
                if let Some((id, path)) = refused {
                    let issue = self.sandbox_issue(path);
                    self.chat.apply(Event::ToolIssue { id, issue });
                }
                if let Some(path) = applied.file_changed {
                    cx.emit(SessionEvent::FileChanged(path));
                }
                self.watch_turn(cx);
                if applied.turn_ended {
                    self.after_turn(cx);
                    self.find_command_changes(cx);
                    self.idle_later(cx);
                    cx.emit(SessionEvent::TurnEnded);
                    self.summarize_turn(cx);
                    crate::parent_mode::check_turn(&self.chat, self.chat.turn_start(), cx);
                }
            }
            Message::Log(line) => {
                if !self.rewind_refused(&line) && !line.trim().is_empty() {
                    self.chat.push_notice(format!("{}: {line}", self.backend.name()));
                }
            }
            Message::Closed => {
                self.agent.stop();
                if self.chat.busy {
                    self.chat.push_error(format!("{} stopped before the turn finished.", self.backend.name()));
                }
                self.chat.process_ended();
            }
        }
    }

    /// When a turn has just begun, read the git working tree (in the background) to find later
    /// what commands changed in it (here or over SSH). Not for turns read back from a file.
    fn watch_turn(&mut self, cx: &mut Context<Self>) {
        let turn = self.chat.turn_start();
        if !self.chat.busy
            || !crate::prefs(cx).command_changes
            || !matches!(self.chat.items.get(turn), Some(wundur_core::conversation::Item::User(_)))
            || self.git_turn.as_ref().is_some_and(|(t, _)| *t == turn)
        {
            return;
        }
        self.git_turn = Some((turn, None));
        let (host, cwd) = (self.host.clone(), self.cwd.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let base =
                cx.background_executor().spawn(async move { wundur_core::command_changes::baseline(host.as_ref(), &cwd) }).await;
            let _ = this.update(cx, |s, _| {
                if let Some((t, b)) = &mut s.git_turn
                    && *t == turn
                {
                    *b = base;
                }
            });
        })
        .detach();
    }

    /// The turn ended: add what commands changed in it to its card.
    fn find_command_changes(&mut self, cx: &mut Context<Self>) {
        let turn = self.chat.turn_start();
        let Some((_, Some(base))) = self.git_turn.take_if(|(t, b)| *t == turn && b.is_some()) else { return };
        let (host, skip) = (self.host.clone(), self.chat.edited_in_turn(turn));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let found =
                cx.background_executor().spawn(async move { wundur_core::command_changes::since(host.as_ref(), &base, &skip) }).await;
            let _ = this.update(cx, |s, cx| {
                s.chat.add_command_changes(turn, found);
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask for the summary of the turn that just ended, when summaries are set up (see
    /// [`wundur_core::summary`]). A moment later, so the files commands changed are found and
    /// a followed session file has caught up; off the UI thread; kept for when the session is
    /// opened again.
    fn summarize_turn(&mut self, cx: &mut Context<Self>) {
        let config = cx.global::<crate::AppSettings>().0.borrow().summaries.clone();
        if !config.ready() {
            return;
        }
        let from = self.chat.turn_start();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let asked = this.update(cx, |s, cx| {
                let turn = summary::TurnText::of(&s.chat, from, &config)?;
                let key = turn.key();
                if !s.summarized.insert(key.clone()) {
                    return None;
                }
                s.chat.set_summary(&key, summary::State::Pending);
                cx.notify();
                Some((turn, key, s.chat.session_id.clone()))
            });
            let Ok(Some((turn, key, session))) = asked else { return };
            let result = cx.background_executor().spawn(async move { summary::summarize(&config, &turn) }).await;
            let _ = this.update(cx, |s, cx| {
                let state = match result {
                    // Skipped quietly (from the account: too long, or the hourly limit): no card.
                    Ok(None) => {
                        s.chat.drop_summary(&key);
                        cx.notify();
                        return;
                    }
                    Ok(Some(done)) => {
                        if let Some(id) = &session {
                            summary::keep(id, &key, &done);
                        }
                        summary::State::Ready(done)
                    }
                    Err(e) => {
                        eprintln!("wundur: turn summary: {e:#}");
                        summary::State::Failed(wundur_core::conversation::one_line(&format!("{e:#}"), 160))
                    }
                };
                s.chat.set_summary(&key, state);
                cx.notify();
            });
        })
        .detach();
    }

    /// Show the kept summaries of this session's turns, and the cards of the game assets
    /// started in its chat.
    pub fn attach_summaries(&mut self) {
        if let Some(id) = self.chat.session_id.clone() {
            self.chat.attach_summaries(&summary::cached(&id));
            if !self.host.is_remote() {
                self.chat.attach_assets(&crate::asset_store::started_in(&self.cwd, &id));
                self.chat.attach_assets(&crate::hosted_store::started_in(&self.cwd, &id));
            }
        }
    }

    /// The person has this session in front of them: the last turn's summary is no longer news
    /// (kept so, for the next run). Whether it changed anything.
    pub fn see_summary(&mut self) -> bool {
        let Some((key, seen)) = self.chat.see_last_summary() else { return false };
        if let Some(id) = &self.chat.session_id {
            summary::keep(id, &key, &seen);
        }
        true
    }

    // ----- what the window asks -----

    /// Send a user turn, starting the agent when needed.
    /// Where the session's folder is (this machine, or over SSH).
    pub fn host(&self) -> Arc<dyn Host> {
        self.host.clone()
    }

    pub fn send(&mut self, text: &str, cx: &mut Context<Self>) {
        self.send_with(text, Vec::new(), cx);
    }

    /// Send a user turn with images (files). Pastes marked in `text` (see
    /// [`wundur_core::paste`]) show folded in the chat and go without their markers. While the
    /// agent works it waits its turn (see [`wundur_core::outbox`]), as it does behind others
    /// waiting; it shows in the chat when it goes.
    pub fn send_with(&mut self, text: &str, images: Vec<PathBuf>, cx: &mut Context<Self>) {
        // Its conversation is still being read (the moment after a start): sent once it is.
        if self.loading_chat {
            self.held_for_chat.push((text.to_string(), images));
            return;
        }
        // In use: an idle timer set before does nothing.
        self.idle_gen += 1;
        self.outbox_paused = false;
        if (self.chat.busy || !self.outbox.is_empty())
            && !(wundur_core::paste::without_markers(text).trim().is_empty() && images.is_empty())
        {
            self.outbox.push(text, images);
            self.send_waiting(cx);
            cx.notify();
            return;
        }
        self.deliver(text, images, cx);
    }

    /// Send a user turn now, starting the agent when needed.
    fn deliver(&mut self, text: &str, images: Vec<PathBuf>, cx: &mut Context<Self>) {
        let shown = text.trim();
        let sent = wundur_core::paste::without_markers(shown);
        let text = sent.trim();
        if text.is_empty() && images.is_empty() {
            return;
        }
        // A message from another session shows as its card (already there), not as the
        // person's message.
        let quiet = match self.quiet.iter().position(|q| q.trim() == shown) {
            Some(i) => {
                self.quiet.remove(i);
                true
            }
            None => false,
        };
        if let Some(pid) = self.waiting_for {
            self.chat.push_error(format!(
                "This conversation is still open in another terminal (process {pid}). Quit it there first; it resumes here by itself."
            ));
            cx.notify();
            return;
        }
        if crate::perf::enabled() {
            self.sent_at = Some(std::time::Instant::now());
        }
        if !self.agent.running()
            && let Err(e) = self.start(cx)
        {
            self.chat.push_error(e.to_string());
            // Most likely not installed (any more): the card says how to.
            self.check_sign_in(cx);
            cx.notify();
            return;
        }
        if !text.is_empty() {
            if quiet {
                self.chat.begin_turn();
            } else {
                self.chat.push_user(shown);
            }
        } else if !images.is_empty() {
            // Images alone start a turn too: what waits goes after it.
            self.chat.busy = true;
            self.chat.status = Some("sending".into());
        }
        let images = if self.agent.takes_images() {
            images
        } else {
            if !images.is_empty() {
                self.chat.push_notice(format!("{} cannot take images here; sent the text only.", self.backend.name()));
            }
            Vec::new()
        };
        if !images.is_empty() {
            self.chat.apply(Event::UserImages(images.clone()));
        }
        self.watch_turn(cx);
        let (turn, text) = (self.chat.turn_start(), self.with_undo_note(text, "\n\n"));
        self.before_turn(
            turn,
            move |s, cx| {
                let sent = if images.is_empty() { s.agent.send(&text) } else { s.agent.send_with_images(&text, &images) };
                if let Err(e) = sent {
                    s.chat.push_error(format!("Could not send to {}: {e}", s.backend.name()));
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }

    pub fn answer(&mut self, request_id: &str, allow: bool, cx: &mut Context<Self>) {
        if let Some(input) = self.chat.answer_permission(request_id, allow) {
            let _ = self.agent.answer_permission(request_id, allow, &input);
        }
        cx.notify();
    }

    /// Answer a question the agent asked (its request to run the asking tool): allowed, with
    /// the chosen labels for each question added to the tool's input.
    pub fn answer_question(&mut self, request_id: &str, answers: Vec<(String, Vec<String>)>, cx: &mut Context<Self>) {
        let shown = answers.iter().map(|(_, a)| a.join(", ")).collect::<Vec<_>>().join("; ");
        let Some(input) = self.chat.answer_question(request_id, shown) else { return };
        let input = wundur_core::questions::answered(&input, &answers);
        let _ = self.agent.answer_permission(request_id, true, &input);
        cx.notify();
    }

    /// Answer the oldest open permission prompt (for the control channel).
    pub fn answer_first(&mut self, allow: bool, cx: &mut Context<Self>) {
        let id = self.chat.pending_permissions().next().map(|p| p.request_id.clone());
        if let Some(id) = id {
            self.answer(&id, allow, cx);
        }
    }

    /// The person stops the turn: what waits stays until they send it, or send another.
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.chat.busy && !self.outbox.is_empty() {
            self.outbox_paused = true;
        }
        self.stop_turn(cx);
    }

    /// Stop the current turn, keeping the agent.
    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        // The turn ending in an error now is this stop, not a failure.
        if self.chat.busy {
            self.chat.stop_requested = true;
        }
        let _ = self.agent.interrupt();
        cx.notify();
    }

    /// Switch permission mode: the agent ends, and starts in it on the next message.
    pub fn set_permission_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        if self.permission_mode == mode {
            return;
        }
        self.permission_mode = mode;
        self.stop(cx);
        self.chat.push_notice(format!(
            "Permission mode: {}. Applies from the next message.",
            mode_label(self.backend.as_ref(), self.permission_mode.as_deref())
        ));
        cx.notify();
    }

    /// Set the idle timer: unused for the minutes in Settings, the agent's process ends (the
    /// conversation stays; the next message starts it again). Any use since sets it again.
    fn idle_later(&mut self, cx: &mut Context<Self>) {
        self.idle_gen += 1;
        let minutes = crate::prefs(cx).idle_minutes;
        if minutes == 0 {
            return;
        }
        let generation = self.idle_gen;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(u64::from(minutes) * 60)).await;
            let _ = this.update(cx, |s, cx| {
                if s.idle_gen != generation {
                    return;
                }
                if s.can_rest() {
                    s.end_agent();
                    s.dormant = true;
                    cx.notify();
                } else if s.agent.running() && !s.chat.busy && s.chat.background_work() {
                    // Background work still runs (a subagent, a shell command): look again later.
                    s.idle_later(cx);
                }
            });
        })
        .detach();
    }

    /// Whether the agent may end for being idle: running, between turns, with
    /// nothing waiting to be sent, and nothing running in the background
    /// (ending the process would end its subagents and background commands with it).
    fn can_rest(&self) -> bool {
        self.agent.running()
            && !self.chat.busy
            && self.outbox.is_empty()
            && !self.chat.background_work()
    }

    /// Whether the agent's process runs.
    pub fn agent_running(&self) -> bool {
        self.agent.running()
    }

    /// End the agent's process; the conversation stays.
    fn end_agent(&mut self) {
        if self.agent.running() {
            self.agent.stop();
            self._reader = None;
            self.chat.process_ended();
        }
    }

    /// End the session's agent.
    pub fn stop(&mut self, _cx: &mut App) {
        self.release();
    }

    /// Let go of the agent (all a drop can do, with no app at hand).
    fn release(&mut self) {
        self.agent.stop();
        self._reader = None;
        self.chat.process_ended();
    }
}

/// What a permission mode is called, among `backend`'s modes; one it does not have (another
/// agent's) counts as its first, its default.
pub fn mode_label(backend: &dyn Backend, mode: Option<&str>) -> &'static str {
    let modes = backend.permission_modes();
    match mode {
        Some("bypassPermissions") if !modes.iter().any(|(m, _)| *m == "bypassPermissions") => "Bypass permissions",
        _ => modes
            .iter()
            .find(|(name, _)| Some(*name) == mode)
            .or(modes.first())
            .map_or("Default", |(_, label)| *label),
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod fill_tests {
    use super::Fill;

    /// Measured against where the agent compacts, not the whole window.
    #[test]
    fn the_context_turns_amber_then_red_on_the_way_to_compaction() {
        let at = 167_000; // a 200k window, compacting 33k short of it
        assert_eq!(Fill::of(100_000, at), Fill::Room);
        assert_eq!(Fill::of(117_000, at), Fill::Filling);
        assert_eq!(Fill::of(151_000, at), Fill::Nearly);
        assert_eq!(Fill::of(190_000, at), Fill::Nearly);
        assert_eq!(Fill::of(5, 0), Fill::Nearly);
    }
}

/// What a session's agent is told beside its own instructions (for an agent that takes them;
/// Claude Code's --append-system-prompt): that Wundur Studio's tools are there and when to use
/// them, in case its tool server is late or ignored, then parent mode's, when it is on.
fn session_instructions(cx: &App) -> Option<String> {
    let wundur = "You are running inside Wundur Studio. When the person has to do something themselves (sign up, accept an agreement, add a secret or DNS record, test on a device, decide something), hand it to them as task cards with the wundur add_tasks tool instead of listing the steps in your reply, one card per step, and never with secret values. Use the wundur notify tool when long work finishes or needs them. For commands the person should run themselves, write run after the language on the code block's fence (```sh run): Wundur shows a Run button on it, and nothing runs until they press it. Leave run off code you only show and off commands you run yourself.";
    Some(match crate::parent_mode::instructions(cx) {
        Some(parent) => format!("{wundur}\n\n{parent}"),
        None => wundur.to_string(),
    })
}

/// The folders the project holding `folder` lets its sessions write to (none outside a project).
pub fn project_writable(folder: &std::path::Path, cx: &App) -> Vec<PathBuf> {
    let settings = cx.global::<crate::AppSettings>().0.borrow();
    wundur_core::settings::project_of(&settings.projects, folder).map(|p| p.writable.clone()).unwrap_or_default()
}

/// The project holding `folder`, by name (for the menu's words).
pub fn project_name(folder: &std::path::Path, cx: &App) -> Option<String> {
    let settings = cx.global::<crate::AppSettings>().0.borrow();
    wundur_core::settings::project_of(&settings.projects, folder).map(|p| p.name.clone())
}
