//! The window's end of its tool server: a local socket that `wundur-mcp` (the tool server
//! each agent is started with) connects to, one line per tool call. Calls are routed to the
//! session whose token they carry, and each keeps its connection open until it is answered.
//!
//! Each session gets a token and a tool servers file (in a folder of this window's own under
//! the system's temporary folder) that names `wundur-mcp` with this socket and that token;
//! the agent is started with it. Nothing in the person's own agent settings is touched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::*;
use interprocess::local_socket::traits::ListenerExt as _;
use serde_json::Value;

use crate::agent_session::AgentSession;
use crate::local_socket;

/// One call.
pub struct Call {
    pub token: String,
    pub event: String,
    pub payload: Value,
    /// The open connection of a call that waits for an answer.
    pub reply: Option<Reply>,
}

/// Where a waiting call's answer goes.
pub struct Reply(interprocess::local_socket::Stream);

impl Reply {
    pub fn send(mut self, answer: &str) {
        let _ = self.0.write_all(format!("{answer}\n").as_bytes());
    }
}

/// The window's socket and the sessions it serves.
#[derive(Clone, Default)]
pub struct Hooks(pub Rc<RefCell<State>>);

#[derive(Default)]
pub struct State {
    /// The socket, when it could be opened.
    pub socket: Option<PathBuf>,
    /// The tool server (MCP) next to this one: the messaging tools (see `messaging`).
    pub mcp: Option<PathBuf>,
    /// The folder for the sessions' tool servers files.
    pub dir: PathBuf,
    sessions: HashMap<String, WeakEntity<AgentSession>>,
    next: u64,
}

impl Global for Hooks {}

/// Open the socket and start serving it. Without a socket (or the tool server), sessions run
/// without Wundur's own tools.
pub fn init(cx: &mut App) {
    // Named for this instance's config folder too, so the tidy-up below only ever looks at its
    // own instances' folders: a test copy (another config folder) never removes the person's
    // window's sockets, whatever it can or can't tell about other processes from a sandbox.
    let dir = std::env::temp_dir().join(format!("wundur-{}-{}", crate::instance::config_tag(), std::process::id()));
    // Folders of windows that ended without tidying up (killed), off the UI thread.
    std::thread::spawn(remove_stale);
    let mcp = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(if cfg!(windows) { "wundur-mcp.exe" } else { "wundur-mcp" }))
        .filter(|p| p.is_file());
    let mut state = State { dir: dir.clone(), mcp, ..Default::default() };
    if state.mcp.is_some() && std::fs::create_dir_all(&dir).is_ok() {
        let (tx, rx) = async_channel::unbounded::<Call>();
        let socket = local_socket::address(&dir.join("hooks.sock"), &format!("wundur-{}-hooks", std::process::id()));
        if listen(&socket, tx) {
            state.socket = Some(socket);
            cx.spawn(async move |cx: &mut AsyncApp| {
                while let Ok(call) = rx.recv().await {
                    cx.update(|cx| route(call, cx));
                }
            })
            .detach();
        }
    }
    if state.socket.is_none() {
        eprintln!("wundur: no tool server socket; sessions run without Wundur's own tools");
    }
    cx.set_global(Hooks(Rc::new(RefCell::new(state))));
}

/// Remove this instance's earlier folders (`wundur-<tag>-<pid>`) whose window process is gone.
/// Folders named the old way (`wundur-<pid>`, or the app's earlier names) only by an instance
/// on the default config folder: they were the person's own window's. On Windows the socket is
/// a named pipe, not a file in the folder: any such folder.
fn remove_stale() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    let own = format!("wundur-{}-", crate::instance::config_tag());
    let old_names: &[&str] = if wundur_core::env_var("CONFIG_DIR").is_err() { &["wundur-", "keleuo-", "keleion-", "workbench-"] } else { &[] };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let pid = name
            .strip_prefix(&own)
            .or_else(|| old_names.iter().find_map(|p| name.strip_prefix(p)))
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid
            && pid != std::process::id()
            && (e.path().join("hooks.sock").exists() || cfg!(windows) && e.path().is_dir())
            && !wundur_core::host::process_alive(pid)
            // Whatever the process check says, a socket something still answers is in use (its
            // sessions' tools reach it): never removed.
            && !(e.path().join("hooks.sock").exists() && local_socket::connect(&e.path().join("hooks.sock")).is_ok())
        {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The folder and socket are this process's own: remove them on the way out.
pub fn cleanup(cx: &App) {
    if let Some(h) = cx.try_global::<Hooks>() {
        let _ = std::fs::remove_dir_all(&h.0.borrow().dir);
    }
}

fn listen(socket: &Path, tx: async_channel::Sender<Call>) -> bool {
    // The folder is this process's own: a socket file already there is from an earlier
    // process with the same id, and goes.
    let Ok(listener) = local_socket::listen(socket, true) else { return false };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    return;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else { return };
                let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let event = s("event");
                // A tool call waits for its answer.
                let waits = event == "mcp";
                let call = Call {
                    token: s("token"),
                    payload: v.get("payload").cloned().unwrap_or(Value::Null),
                    reply: waits.then_some(Reply(stream)),
                    event,
                };
                let _ = tx.send_blocking(call);
            });
        }
    });
    true
}

fn route(call: Call, cx: &mut App) {
    let target = cx
        .try_global::<Hooks>()
        .and_then(|h| h.0.borrow().sessions.get(&call.token).cloned());
    let Some(session) = target.and_then(|w| w.upgrade()) else { return };
    if call.event == "mcp" {
        crate::messaging::tool_call(session, call, cx);
    }
}

/// A session's place on the socket: its tool servers file. `None` when the window has no
/// socket or no tool server.
pub struct Registration {
    /// The tool servers file to start its agent with (see [`Registration::write_mcp`]).
    pub mcp: PathBuf,
    /// The window's own server's entry in it.
    wundur_mcp: serde_json::Value,
}

impl Registration {
    /// Write the tool servers file for a start of its agent: the window's own server and
    /// `servers` (those set up in Settings for its folder). Its path, when written.
    pub fn write_mcp(&self, servers: &[wundur_core::mcp::Server]) -> Option<PathBuf> {
        let mut own = serde_json::Map::new();
        own.insert("wundur".into(), self.wundur_mcp.clone());
        let config = wundur_core::mcp::claude_config(servers, &own);
        std::fs::write(&self.mcp, config.to_string()).ok().map(|_| self.mcp.clone())
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.mcp);
    }
}

pub fn register(session: WeakEntity<AgentSession>, cx: &mut App) -> Option<Registration> {
    let hooks = cx.try_global::<Hooks>()?.clone();
    let mut state = hooks.0.borrow_mut();
    let socket = state.socket.clone()?;
    let program = state.mcp.clone()?;
    state.next += 1;
    let token = format!("s{}-{}", std::process::id(), state.next);
    // The window's tool server, named with the socket and the session's token; the file with
    // it is written as the agent starts (see `write_mcp`).
    let wundur_mcp = serde_json::json!({ "type": "stdio", "command": program.to_string_lossy(), "args": [socket.to_string_lossy(), token] });
    let mcp = state.dir.join(format!("{token}-mcp.json"));
    state.sessions.insert(token, session);
    Some(Registration { mcp, wundur_mcp })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// What `wundur-mcp` does, by hand: a tool call keeps its connection, and the answer
    /// comes back on it; anything else does not wait.
    #[::core::prelude::v1::test]
    fn calls_arrive_and_answers_go_back() {
        let dir = std::env::temp_dir().join(format!("wb-hooks-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = local_socket::address(&dir.join("t.sock"), &format!("wb-hooks-test-{}", std::process::id()));
        let (tx, rx) = async_channel::unbounded::<Call>();
        assert!(listen(&socket, tx));

        let mut c = local_socket::connect(&socket).unwrap();
        c.write_all(b"{\"token\":\"s1-1\",\"event\":\"Stop\",\"payload\":{}}\n").unwrap();
        let call = rx.recv_blocking().unwrap();
        assert_eq!((call.token.as_str(), call.event.as_str()), ("s1-1", "Stop"));
        assert!(call.reply.is_none());

        let mut c = local_socket::connect(&socket).unwrap();
        c.write_all(b"{\"token\":\"s1-1\",\"event\":\"mcp\",\"payload\":{\"tool_name\":\"Bash\"}}\n").unwrap();
        let call = rx.recv_blocking().unwrap();
        assert_eq!(call.payload["tool_name"], "Bash");
        call.reply.expect("waits").send("{\"ok\":true}");
        let mut answer = String::new();
        c.read_to_string(&mut answer).unwrap();
        assert_eq!(answer.trim(), "{\"ok\":true}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
