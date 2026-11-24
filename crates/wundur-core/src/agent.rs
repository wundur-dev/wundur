//! The agent behind a session, in neutral terms. The window renders [`Event`]s and talks to an
//! [`Agent`] (start, send, answer a permission prompt, interrupt, stop) made by a [`Backend`],
//! whose [`Capabilities`] say which optional features it has (skills, earlier sessions,
//! subagents, permission prompts, models). Backends are found in a [`Registry`].
//!
//! A backend maps its own wire format onto these events inside its own module (Claude Code's
//! `stream-json` in `backends::claude`); nothing outside a backend knows its format.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::conversation::Conversation;
use crate::diff::FileDiff;
use crate::host::Host;
use crate::skills::Catalog;

/// A known way a tool call fails for no good reason (the agent then has to try again), with
/// fixes in the agent's own terms that keep it from happening again.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    /// Which known issue (the backend's name for it, for [`Backend::apply_fix`]).
    pub kind: String,
    pub title: String,
    /// What happened, in a sentence or two.
    pub detail: String,
    pub fixes: Vec<IssueFix>,
}

/// One way to fix an [`Issue`]: what it is called and exactly what it would write where.
#[derive(Clone, Debug, PartialEq)]
pub struct IssueFix {
    pub id: String,
    pub label: String,
    pub preview: String,
}

/// One thing that happened in a conversation.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The agent process is up.
    Started { session_id: Option<String>, model: Option<String>, permission_mode: Option<String>, cwd: Option<String> },
    /// A short progress note ("thinking", "requesting").
    Status(String),
    /// A new block of the agent's text begins (streamed in [`Event::TextDelta`]s).
    TextStart,
    /// More of the text being streamed.
    TextDelta(String),
    /// A complete block of text (replaces the streamed one). `parent`: a subagent's.
    Text { text: String, parent: Option<String> },
    /// The person's message (from a history being replayed).
    UserText(String),
    /// A tool call, or more of its details. `diff`: what it will change, when known.
    ToolCall { id: String, name: String, summary: String, input: Value, diff: Option<FileDiff>, parent: Option<String> },
    /// A tool's result. `diff` and `file_changed`: what it changed.
    ToolResult { id: String, output: String, is_error: bool, diff: Option<FileDiff>, file_changed: Option<PathBuf>, parent: Option<String> },
    /// Images a tool call returned (a Read of an image file), as files in the session's image
    /// folder: shown with the call.
    ToolImages { id: String, images: Vec<PathBuf> },
    /// The model's context window, in tokens, as the agent reported it.
    ContextWindow(u64),
    /// The agent asks before doing something.
    Permission { request_id: String, tool: String, summary: String, description: Option<String>, input: Value },
    /// A subagent began (`id` is the tool call that started it).
    SubagentStarted { id: String, description: String, kind: String },
    /// A subagent's progress.
    SubagentProgress { id: String, current_tool: Option<String>, tokens: Option<u64>, tool_uses: Option<u64>, duration_ms: Option<u64> },
    /// A subagent runs in the background: its tool call returns at once (`agent_id`: the id its
    /// report will come back under), and it ends with [`Event::SubagentEnded`] later.
    SubagentBackground { id: String, agent_id: Option<String> },
    /// Work the agent runs in the background that is not a subagent (a shell command, a
    /// monitor), by its task id: started (`running`) or ended. The agent's process must not be
    /// ended for being idle while any runs.
    BackgroundTask { id: String, running: bool },
    /// A message from another agent: a background subagent's report, or a teammate's.
    AgentMessage { from: String, text: String },
    /// What a running tool has printed so far (its latest part), while it runs.
    ToolOutput { id: String, text: String },
    /// A tool call failed in a known way (see [`Issue`]).
    ToolIssue { id: String, issue: Issue },
    /// The person stopped the turn: it is over, with no answer to come.
    Interrupted,
    /// A background subagent was sent another message and runs again (`agent_id`: the id it
    /// was launched under).
    SubagentResumed { agent_id: String },
    /// A subagent finished. `id` is the tool call that started it, or, when only that is
    /// known, the id it was launched under.
    SubagentEnded { id: String, failed: bool, summary: Option<String>, tokens: Option<u64>, duration_ms: Option<u64> },
    /// A turn ended, with what it used. `cost_total_usd` is the process total so far.
    TurnEnded {
        error: Option<String>,
        cost_total_usd: Option<f64>,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: Option<u64>,
        session_id: Option<String>,
    },
    /// A quiet note (warnings the agent printed).
    Notice(String),
    /// The agent is not signed in (or its sign-in ran out), as it says in its own terms (a
    /// code, never its words, which may be in any language).
    SignedOut,
    /// The account's usage limit: `reached` (messages are refused until `resets_at`, Unix
    /// time) or not any more.
    Limit { reached: bool, resets_at: Option<u64> },
    /// Spend worked out from token counts and published prices (the agent reported none).
    CostEstimate { usd: f64 },
    /// Images the person sent with their last message (stored files).
    UserImages(Vec<PathBuf>),
}

/// A model an agent offers, as the agent itself lists it: one id may be one variant of a
/// model (Cursor's `gpt-5.3-codex-high-fast` is `gpt-5.3-codex` at high effort, fast), which
/// the window offers as the model plus its variant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelOption {
    /// What to ask for (`--model <id>`).
    pub id: String,
    /// What the agent calls it.
    pub label: String,
    /// The family it is grouped under in the menu ("Claude", "GPT"); empty: on its own.
    pub family: String,
    /// The model this id is a variant of (the id itself when it is not one).
    pub base: String,
    /// What the base model is called (its label without the variant's words).
    pub base_label: String,
    /// The effort this variant runs at, when it names one.
    pub effort: Option<String>,
    /// A variant that thinks first.
    pub thinking: bool,
    /// A faster variant.
    pub fast: bool,
}

/// Effort levels from least to most, the names agents use.
pub const EFFORT_ORDER: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// How far apart two efforts are (no effort counts as between medium and high).
fn effort_rank(effort: Option<&str>) -> i32 {
    match effort.and_then(|e| EFFORT_ORDER.iter().position(|o| *o == e)) {
        Some(i) => 2 * i as i32,
        None => 7,
    }
}

/// The variant of model `base` closest to what is wanted: the same effort, thinking and speed
/// when there is one; else the nearest effort (the higher of two as near), keeping thinking
/// and speed where it can.
pub fn pick_variant<'a>(models: &'a [ModelOption], base: &str, effort: Option<&str>, thinking: bool, fast: bool) -> Option<&'a ModelOption> {
    models.iter().filter(|m| m.base == base).min_by_key(|m| {
        let rank = effort_rank(m.effort.as_deref());
        let off = (rank - effort_rank(effort)).abs() * 4 + i32::from(m.thinking != thinking) * 2 + i32::from(m.fast != fast);
        (off, -rank)
    })
}

/// What an agent's reader threads report.
#[derive(Clone, Debug)]
pub enum Message {
    Event(Event),
    /// A line the agent wrote to its error output.
    Log(String),
    /// The agent process ended.
    Closed,
}

/// Where every message from the agent goes (called on a reader thread).
pub type Sink = Arc<dyn Fn(Message) + Send + Sync>;

/// What a session asks of its agent.
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// `None`: the backend's own default (ask before acting).
    pub permission_mode: Option<String>,
    /// Continue this earlier conversation.
    pub resume: Option<String>,
    /// Continue it in a copy (the original keeps running elsewhere).
    pub fork: bool,
    /// The effort level, for an agent that takes one apart from the model
    /// ([`Backend::efforts`]).
    pub effort: Option<String>,
    /// Where it may write ([`crate::sandbox`]); `None`: anywhere.
    pub sandbox: Option<crate::sandbox::WritePolicy>,
    /// A file naming tool servers (MCP) for it, beside its own (the window's messaging tools,
    /// the servers set up in Settings).
    pub mcp_config: Option<PathBuf>,
    /// Environment it starts with: the tool servers' secrets ([`crate::mcp::secret_env`]).
    pub env: Vec<(String, String)>,
    /// Instructions added to the agent's own (parent mode's), for an agent that takes them.
    pub instructions: Option<String>,
    /// With `resume` and `fork`: continue a copy that ends before the last turn (undoing it),
    /// from [`Backend::rewind_point`].
    pub rewind: Option<Rewind>,
}

/// Where to cut a conversation to undo its last turn: the agent's ids for its messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewind {
    /// The last message kept (the reply before the turn).
    pub at: String,
    /// The person's message that began the turn, dropped with everything after it.
    pub drops: String,
}

/// A running conversation with an agent.
pub trait Agent: Send {
    fn running(&self) -> bool;
    /// Start the agent on `host`. Messages arrive through `sink`.
    fn start(&mut self, host: Arc<dyn Host>, options: &StartOptions, sink: Sink) -> anyhow::Result<()>;
    fn send(&mut self, text: &str) -> std::io::Result<()>;
    /// Send a user turn with images (files on this machine). Unsupported by default: then
    /// [`Agent::takes_images`] is false and only the text goes.
    fn send_with_images(&mut self, text: &str, _images: &[std::path::PathBuf]) -> std::io::Result<()> {
        self.send(text)
    }
    /// Whether [`Agent::send_with_images`] sends the images.
    fn takes_images(&self) -> bool {
        false
    }
    /// Whether a message sent while a turn runs goes into that turn (the agent reads it as it
    /// works). When not, the window keeps it until the turn ends, or stops the turn to send it
    /// at once (see [`crate::outbox`]).
    fn takes_mid_turn(&self) -> bool {
        false
    }
    /// Answer a permission prompt; `input` is what to run with when allowed.
    fn answer_permission(&mut self, request_id: &str, allow: bool, input: &Value) -> std::io::Result<()>;
    /// Stop the current turn, keeping the process.
    fn interrupt(&mut self) -> std::io::Result<()>;
    fn stop(&mut self);
}

/// The optional features a backend has; the window hides what is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Skills (and similar files) it reads from the project and the user's folder.
    pub skills: bool,
    /// Earlier sessions of a folder it can list and resume.
    pub earlier_sessions: bool,
    /// Resuming a session as a copy.
    pub fork: bool,
    /// Subagents it reports.
    pub subagents: bool,
    /// Asking before acting, answered from the window.
    pub permission_prompts: bool,
    /// A permission mode that accepts edits without asking (its name).
    pub accept_edits_mode: Option<String>,
    /// Models it can be asked to use (empty: its default only).
    pub models: Vec<String>,
}

/// An earlier session of a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    /// The first thing the person asked.
    pub title: String,
    /// ISO time of the last message.
    pub last_activity: Option<String>,
    pub messages: usize,
    /// Seconds since the epoch, when known.
    pub modified: Option<u64>,
}

/// A session that is, or may be, open in an agent process outside this window. Such a
/// session must not be continued here as it is: two processes would write one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSession {
    pub id: String,
    /// The process, when one was found.
    pub pid: Option<u32>,
    pub reason: LiveReason,
}

/// How a live session was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveReason {
    /// A process names the session on its command line.
    CommandLine,
    /// A process works in the folder (which of its sessions it writes is a guess).
    Folder,
    /// No process found, but the session file changed a moment ago.
    RecentWrite,
}

/// An agent process running on this machine, outside this window or in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub pid: u32,
    /// The folder it works in.
    pub cwd: PathBuf,
    /// The session it named on its command line, when it did.
    pub session: Option<String>,
    /// When it started, as `HH:MM` local time.
    pub since: Option<String>,
}

/// A kind of agent. Optional features have defaults that do nothing; [`Backend::capabilities`]
/// says which are real.
pub trait Backend: Send + Sync {
    /// A stable id, saved with sessions ("claude-code").
    fn id(&self) -> &'static str;
    /// Shown in the window ("Claude Code").
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn new_agent(&self) -> Box<dyn Agent>;
    /// Where the agent keeps its own state on this machine (its settings, its conversations):
    /// written even when its writes are kept to the session's folder ([`crate::sandbox`]). A
    /// folder, or a file (with its siblings sharing the name, for writes renamed into place).
    fn state_paths(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// The program the agent runs as (`claude`): looked for to tell whether it is installed.
    fn program(&self) -> Option<String> {
        None
    }

    /// The agent's own installer, for when its program is not on the machine.
    fn install(&self) -> Option<Install> {
        None
    }

    /// Whether the agent is installed on `host`; `None`: not known (it names no program, or the
    /// host can't tell), taken as installed.
    fn installed(&self, host: &dyn Host) -> Option<bool> {
        host.has_program(&self.program()?)
    }

    /// The agent's own sign-in, run in a terminal (it may open a browser, or ask for a code).
    fn sign_in(&self) -> Option<crate::host::ProgramSpec> {
        None
    }

    /// Whether the agent is signed in, by its own status command (`None`: it cannot say).
    fn signed_in(&self, _host: &dyn Host) -> Option<bool> {
        None
    }

    /// Skills and similar files relevant to `project`.
    fn skills(&self, _host: &dyn Host, _project: &Path) -> Catalog {
        Catalog::default()
    }
    /// The folders this agent reads skills from for `scope`, the one a skill is installed into
    /// first (see [`crate::skill_install`]); empty when it has no skills.
    fn skill_folders(&self, _host: &dyn Host, _project: &Path, _scope: crate::skill_install::Scope) -> Vec<PathBuf> {
        Vec::new()
    }
    /// Create a project skill from a template; returns the file.
    fn create_skill(&self, _host: &dyn Host, _project: &Path, _name: &str) -> std::io::Result<PathBuf> {
        Err(std::io::Error::other("this agent has no skills"))
    }
    /// Earlier sessions of `project`, newest first.
    fn earlier_sessions(&self, _host: &dyn Host, _project: &Path, _max: usize) -> Vec<SessionInfo> {
        Vec::new()
    }
    /// How many sessions `project` has and when the newest changed (cheap: no file contents).
    fn session_count(&self, _host: &dyn Host, _project: &Path) -> (usize, Option<u64>) {
        (0, None)
    }
    /// A session's history as a chat. Long histories may leave parts as
    /// [`crate::conversation::Item::Earlier`], read with [`Backend::load_segment`].
    fn load_session(&self, _host: &dyn Host, _project: &Path, _id: &str) -> std::io::Result<Conversation> {
        Err(std::io::Error::other("this agent cannot resume sessions"))
    }
    /// When session `id` was last written, for "12m ago" in the list: its file's time.
    fn session_modified(&self, _host: &dyn Host, _project: &Path, _id: &str) -> Option<std::time::SystemTime> {
        None
    }
    /// Where to cut session `id` to undo its last turn, for an agent that can continue a
    /// conversation without it ([`StartOptions::rewind`]). `Ok(None)`: the turn is the first,
    /// so nothing is kept. `Err`: this agent can't forget a turn.
    fn rewind_point(&self, _host: &dyn Host, _project: &Path, _id: &str) -> Result<Option<Rewind>, String> {
        Err("this agent can't forget a turn".into())
    }
    /// The items of an [`crate::conversation::Item::Earlier`] part of session `id`.
    fn load_segment(&self, _host: &dyn Host, _project: &Path, _id: &str, _from: u64, _to: u64) -> std::io::Result<Vec<crate::conversation::Item>> {
        Err(std::io::Error::other("this agent cannot read earlier parts"))
    }
    /// The agent's own `/` commands worth offering in the message box (name, what it does).
    fn builtin_commands(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
    /// The command that switches the model (`/model <name>`), when there is one.
    fn model_command(&self, _model: &str) -> Option<String> {
        None
    }
    /// The effort levels the agent takes apart from the model (Claude Code's `--effort`), from
    /// least to most; empty when it takes none (Cursor's are part of its model ids).
    fn efforts(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// Every model the agent offers this person, asked of the agent itself (slow: run off the
    /// window's thread). Empty when it cannot list them: then [`Capabilities::models`].
    fn list_models(&self, _host: &dyn Host) -> Vec<ModelOption> {
        Vec::new()
    }
    /// The permission modes the window offers, as `(mode name, what it is called)`; the first
    /// is the default. Claude Code's by default.
    fn permission_modes(&self) -> Vec<(&'static str, &'static str)> {
        vec![("default", "Ask before acting"), ("auto", "Auto"), ("acceptEdits", "Accept edits"), ("plan", "Plan")]
    }
    /// Whether `host`'s agent is billed per use (an API key) rather than by a subscription
    /// plan, as far as can be told without reading any credential. `None`: cannot tell.
    fn billed_per_use(&self, _host: &dyn Host, _project: &Path) -> Option<bool> {
        None
    }
    /// The context window of `model`, in tokens, when known.
    fn context_window(&self, _model: &str) -> Option<u64> {
        None
    }
    /// Where in a `window`-token context the agent compacts by itself, in tokens.
    fn compacts_at(&self, window: u64) -> u64 {
        window
    }
    /// The message that asks it to compact now, when it has one.
    fn compact_command(&self) -> Option<&'static str> {
        None
    }
    /// Every agent process of this kind on this machine except this window's own (children of
    /// `own_pid`). Read-only: nothing is signalled. Empty for a remote host.
    fn running(&self, _host: &dyn Host, _own_pid: u32) -> Vec<Running> {
        Vec::new()
    }
    /// Sessions of `project` that are, or may be, open in an agent process outside this window
    /// (`own_pid`'s children and the sessions in `own` are this window's). Read-only: nothing
    /// is signalled.
    fn live_sessions(&self, _host: &dyn Host, _project: &Path, _sessions: &[SessionInfo], _own_pid: u32, _own: &[String]) -> Vec<LiveSession> {
        Vec::new()
    }
    /// Whether session `id` of `project` is, or may be, open outside this window, checked now
    /// (just before it would be continued here).
    fn session_live(&self, _host: &dyn Host, _project: &Path, _id: &str, _own_pid: u32) -> Option<LiveSession> {
        None
    }
    /// The folder `old` was renamed to `new`: move what the agent keeps for it (its sessions,
    /// its notes) so they belong to the new name. Nothing to move by default.
    fn folder_renamed(&self, _host: &dyn Host, _old: &Path, _new: &Path) -> std::io::Result<()> {
        Ok(())
    }
    /// Apply fix `fix` of the known issue `kind` (see [`Issue`]) for `project`; what was done,
    /// in a sentence.
    fn apply_fix(&self, _host: &dyn Host, _project: &Path, _kind: &str, _fix: &str) -> std::io::Result<String> {
        Err(std::io::Error::other("this agent has no fixes"))
    }
}

/// How to install an agent that is not on the machine: its own installer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    /// The command line that installs it, to run in a terminal.
    pub command: String,
    /// The agent's install page (other ways to install it, help when it fails).
    pub page: String,
}

/// The backends a window can start sessions with.
#[derive(Clone, Default)]
pub struct Registry {
    backends: Vec<Arc<dyn Backend>>,
}

impl Registry {
    pub fn new(backends: Vec<Arc<dyn Backend>>) -> Self {
        Self { backends }
    }

    pub fn all(&self) -> &[Arc<dyn Backend>] {
        &self.backends
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Backend>> {
        self.backends.iter().find(|b| b.id() == id).cloned()
    }

    /// The first backend: the default for new sessions.
    pub fn default_backend(&self) -> Option<Arc<dyn Backend>> {
        self.backends.first().cloned()
    }

    /// The first backend installed on `host` (or that can't tell), else the first: the default
    /// for a new session there.
    pub fn default_installed(&self, host: &dyn Host) -> Option<Arc<dyn Backend>> {
        self.backends.iter().find(|b| b.installed(host) != Some(false)).cloned().or_else(|| self.default_backend())
    }
}

#[cfg(test)]
mod variant_tests {
    use super::*;

    #[test]
    fn the_nearest_variant_is_picked() {
        let m = |id: &str, effort: Option<&str>, thinking: bool, fast: bool| ModelOption {
            id: id.into(),
            base: "m".into(),
            effort: effort.map(str::to_string),
            thinking,
            fast,
            ..Default::default()
        };
        let models = [
            m("m-low", Some("low"), false, false),
            m("m-high", Some("high"), false, false),
            m("m-high-fast", Some("high"), false, true),
            m("m-thinking-max", Some("max"), true, false),
        ];
        let pick = |e: Option<&str>, t, f| pick_variant(&models, "m", e, t, f).map(|m| m.id.as_str());
        assert_eq!(pick(Some("high"), false, true), Some("m-high-fast"));
        // No medium: low and high are as near; the higher.
        assert_eq!(pick(Some("medium"), false, false), Some("m-high"));
        // Effort first: xhigh is nearest to high, thinking or not.
        assert_eq!(pick(Some("xhigh"), false, false), Some("m-high"));
        assert_eq!(pick(Some("max"), true, false), Some("m-thinking-max"));
        assert_eq!(pick_variant(&models, "other", None, false, false), None);
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;
    use crate::host::LocalHost;

    /// A backend that runs as `program` (or names none).
    struct Named(&'static str, Option<&'static str>);

    impl Backend for Named {
        fn id(&self) -> &'static str {
            self.0
        }
        fn name(&self) -> &'static str {
            self.0
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn new_agent(&self) -> Box<dyn Agent> {
            unimplemented!("not started in these tests")
        }
        fn program(&self) -> Option<String> {
            self.1.map(str::to_string)
        }
    }

    const GONE: &str = "wundur-no-such-agent-program";

    #[test]
    fn an_agent_is_installed_when_its_program_is_found() {
        assert_eq!(Named("a", Some("sh")).installed(&LocalHost), Some(true));
        assert_eq!(Named("b", Some(GONE)).installed(&LocalHost), Some(false));
        // Naming no program: not known.
        assert_eq!(Named("c", None).installed(&LocalHost), None);
    }

    #[test]
    fn the_default_is_the_first_installed_agent() {
        let reg = Registry::new(vec![Arc::new(Named("gone", Some(GONE))), Arc::new(Named("here", Some("sh")))]);
        assert_eq!(reg.default_installed(&LocalHost).unwrap().id(), "here");
        // One that can't tell counts as installed.
        let reg = Registry::new(vec![Arc::new(Named("gone", Some(GONE))), Arc::new(Named("unknown", None))]);
        assert_eq!(reg.default_installed(&LocalHost).unwrap().id(), "unknown");
        // None installed: the first, which then offers its installer.
        let reg = Registry::new(vec![Arc::new(Named("gone", Some(GONE))), Arc::new(Named("also", Some(GONE)))]);
        assert_eq!(reg.default_installed(&LocalHost).unwrap().id(), "gone");
    }

    #[test]
    fn both_agents_name_their_program_and_installer() {
        for b in [&crate::backends::claude::ClaudeBackend as &dyn Backend, &crate::backends::cursor::CursorBackend::default()] {
            assert!(b.program().is_some(), "{} names no program", b.name());
            let install = b.install().unwrap_or_else(|| panic!("{} has no installer", b.name()));
            assert!(install.page.starts_with("https://") && install.command.contains("https://"), "{install:?}");
        }
    }
}
