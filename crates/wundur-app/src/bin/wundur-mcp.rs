//! `wundur-mcp <socket> <token>`: the tool server (MCP, over stdin and stdout) an agent is
//! started with while Wundur drives its session. Its tools reach the other sessions in the
//! window: `list_sessions`, `send_message` and `ask_for_review`, and the tools the person made
//! from command-line programs in Settings (asked of the window when the agent lists them). It knows only what the tools are called and what
//! they take; each call goes to the window over its local socket as one line,
//! `{"token": ..., "event": "mcp", "payload": {"tool": ..., "arguments": ...}}`, and the
//! window's answer, `{"text": ..., "error": bool}`, is the call's result. With the window gone
//! a call says so.
//!
//! MCP over stdio is JSON-RPC 2.0, one message per line.

use std::io::{BufRead, BufReader, Write};

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};
use serde_json::{Value, json};

/// The protocol version answered when the agent asks for one this does not know.
const PROTOCOL: &str = "2025-06-18";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(socket), Some(token)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: wundur-mcp <socket> <token>");
        return;
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        // Notifications (no id) need no answer.
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => {
                let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "wundur", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                // Wundur's own, then the person's (from the window; none when it is not there).
                let mut all = tools().as_array().cloned().unwrap_or_default();
                let theirs = ask(socket, token, &json!({ "list": true }));
                all.extend(theirs.and_then(|v| v.get("tools").and_then(Value::as_array).cloned()).unwrap_or_default());
                Ok(json!({ "tools": all }))
            }
            "tools/call" => {
                let tool = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(call(socket, token, tool, &arguments))
            }
            _ => Err(json!({ "code": -32601, "message": format!("unknown method {method}") })),
        };
        let answer = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        if writeln!(out, "{answer}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

/// What every agent is told about these tools at the start of its session (Claude Code keeps
/// a server's instructions in context even while its tools wait to be searched for, and cuts
/// them past 4,096 characters): the behaviour wanted most first.
const INSTRUCTIONS: &str = "Wundur Studio is the app this session runs in: a window that shows your replies as Markdown and can do more with them than a terminal can.\n\
- Commands for the person to run themselves: put them in a code block with run after the language on its opening fence (```sh run), one command per line, in order. Wundur shows a Run button on it that types them into a terminal in this folder; nothing runs until they press it, and they can copy the commands instead. Leave run off code you only show, and off commands you run yourself. Chain lines that depend on each other with &&.\n\
- A file path in backticks (`src/main.rs:12`) becomes a link that opens the file, at that line.\n\
Its tools:\n\
- add_tasks: whenever the person has to do something themselves (sign up or sign in somewhere, accept an agreement, add a secret or a DNS record, test on a device, decide something), hand it to them as task cards with add_tasks instead of listing the steps in your reply. One card per step, in order, with the command to run and the page to open when there is one. Never put secret values in a card. The cards are numbered within their list: mention one by list and number (\"step 2 of API service chores\"), not by its id. See what is still open with list_tasks, and tick a step off with complete_task once you have checked it is done.\n\
- notify: tell the person when long work finishes, needs their decision, or went wrong (one notice per thing, not progress updates).\n\
- ask_for_review: ask another agent for a read-only second opinion on work in this folder.\n\
- list_sessions, send_message: reach the person's other open sessions (other agents, other folders); the person sees every message and may have to allow it.\n\
- look_at_model: look at 3D models (GLB/glTF) yourself, from fixed angles, several side by side, with their facts and warnings; check every model you make or change with it. show_model: open one in the person's model viewer.\n\
- The person's tool packs (make_image, edit_image, model_from_*, rig_model, music, sound_effect, voice_line and others) make game assets and more: search for them when a task needs one.";

/// The tools, as MCP describes them.
fn tools() -> Value {
    json!([
        {
            "name": "list_sessions",
            "description": "List the other sessions open in Wundur: each one's name, its agent and its folder. Use a name from here with send_message.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "send_message",
            "description": "Send a message to another session open in Wundur, by its name (see list_sessions). Its agent gets it as a new turn, marked as coming from this session (not from the person); it can answer with send_message to this session. Use it to hand work over (\"the release is out: update the download page\") or to ask for something. The person sees every message and may have to allow it first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "The other session's name, as list_sessions gives it." },
                    "message": { "type": "string", "description": "What to say: complete on its own, since the other agent has not seen this conversation." },
                },
                "required": ["to", "message"],
            },
        },
        {
            "name": "notify",
            // Loaded from the start of the session (not waiting to be searched for): wanted
            // whenever the person has steps of their own.
            "_meta": { "anthropic/alwaysLoad": true },
            "description": "Tell the person something in Wundur's inbox: the one place they see notices from all their sessions at once. Use it when something deserves their attention while they may be looking elsewhere: a long task is done, a decision waits on them, or something went wrong that they should know about. One notice per thing worth their attention, not progress updates; it doesn't wait for an answer. Clicking it opens this session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A short headline, under about 60 characters: \"Release 0.2.4 is built\", \"Pick a database for the save system\"." },
                    "message": { "type": "string", "description": "Optional detail, a sentence or two." },
                    "kind": { "type": "string", "enum": ["info", "done", "needs_you", "problem"], "description": "What it is about: done (finished), needs_you (a decision or input waits on the person), problem (something went wrong), or info." },
                    "choices": { "type": "array", "items": { "type": "string" }, "description": "For needs_you: up to six short answers to offer as buttons (\"Cloudflare Pages\", \"The VPS\"); the person can also write their own. Their answer comes to this session as their next message." },
                },
                "required": ["title"],
            },
        },
        {
            "name": "add_tasks",
            // Loaded from the start of the session (not waiting to be searched for): wanted
            // whenever the person has steps of their own.
            "_meta": { "anthropic/alwaysLoad": true },
            "description": "Use this instead of listing steps the person must take in your reply. Gives the person a checklist of things THEY need to do, shown in Wundur Studio as cards they tick off (outside the chat, kept until done): steps you cannot take yourself, such as signing in somewhere, accepting an agreement, adding a DNS record, setting a secret, testing on their device, or deciding something. Use it instead of listing such steps in your reply. One task per step, in the order to do them; put a command they should run in `command` (shown with a copy button) and a page in `link`. Not for your own plan or steps you will do yourself. Never put secret values in a task. The person sees the cards numbered 1, 2, 3 within the list; when you mention one, call it by that number, not by its id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "What the list is for, short: \"API service chores\", \"Before the 0.3 release\"." },
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "The step, as an instruction: \"Accept Apple's developer agreement\"." },
                                "detail": { "type": "string", "description": "Optional: a sentence or two on how, or why it matters." },
                                "command": { "type": "string", "description": "Optional: a command to run for it." },
                                "link": { "type": "string", "description": "Optional: a page to open for it." },
                            },
                            "required": ["title"],
                        },
                    },
                },
                "required": ["title", "tasks"],
            },
        },
        {
            "name": "complete_task",
            // Loaded from the start of the session (not waiting to be searched for): wanted
            // whenever the person has steps of their own.
            "_meta": { "anthropic/alwaysLoad": true },
            "description": "Tick off one of the person's tasks (from add_tasks) once you have checked it is done (for example, the DNS record now resolves). Pass done: false to put one back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The task's id, as add_tasks or list_tasks gave it." },
                    "done": { "type": "boolean", "description": "Done (the default) or not." },
                },
                "required": ["id"],
            },
        },
        {
            "name": "list_tasks",
            // Loaded from the start of the session (not waiting to be searched for): wanted
            // whenever the person has steps of their own.
            "_meta": { "anthropic/alwaysLoad": true },
            "description": "The person's task lists for this folder: each task's step number (as the person sees it), its id (for complete_task) and whether it is done.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "look_at_model",
            "description": "Look at 3D models yourself: draws GLB or glTF models to one picture and returns it, with each model's facts. A row per model, a column per fixed camera angle (default front, left side, back, three-quarter), the same camera and studio light for every model, so differences come from the models and not the framing. Use it to check a model you made or changed (model_from_*, rig_model, edits), to compare variants side by side, or to look closely at hands, feet or a face with focus. The facts list triangles, vertices, size, every texture's size and format, the skeleton and animations; warnings flag textures that failed to load (they show magenta), WebP textures not marked as such, fully metallic materials that look black in most engines, and extensions it can't read. glTF models face +Z; if a model's front column shows its side, say which way it faces (facing, or 'path @+x'). Free and quick (well under a second a model). The picture is saved in the project's .wundur/looks/ (ignored by git).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "models": { "type": "string", "description": "The models: one path (in the project, absolute, or ~/...), or several separated by commas (a row each, in this order). Add ' @+x' (or @-x, @-z) after a path when that model faces another way than +Z." },
                    "views": { "type": "string", "description": "Camera angles, comma-separated, from: front, back, left, right, three-quarter, back-three-quarter, top, bottom. Default: front, left, back, three-quarter." },
                    "facing": { "type": "string", "enum": ["+z", "-z", "+x", "-x"], "description": "Which way the models face (default +z, glTF's convention); each is turned so 'front' shows its front." },
                    "animation": { "type": "string", "description": "Pose them in an animation (part of its name, e.g. run); default their rest pose." },
                    "time": { "type": "string", "description": "When in the animation: seconds (0.4) or a share of it (50%). Default its start." },
                    "focus": { "type": "string", "description": "A close-up: a joint name or part of one (head, hand, foot_l), a region of the model's box (front top, bottom, left front bottom), or x,y,z fractions of its box (0.5,0.1,0.9). If nothing matches, the facts list its joints." },
                    "zoom": { "type": "number", "description": "How much closer a close-up is (default 3)." },
                    "same_scale": { "type": "boolean", "description": "Keep the models' sizes relative to each other (one camera distance for all) instead of each filling its picture." },
                    "labels": { "type": "string", "description": "Names for the rows, comma-separated in the models' order (default: the file names)." },
                    "size": { "type": "integer", "description": "Pixels per view (default: what fits one readable picture, up to 512)." },
                    "name": { "type": "string", "description": "File name for the picture, without extension." },
                },
                "required": ["models"],
            },
        },
        {
            "name": "show_model",
            "description": "Show a 3D model (GLB or glTF) to the person: opens it in Wundur's model viewer, a tab beside the chat where they can turn and zoom it, play its animations and see it on a floor. Use after making or changing a model so the person sees it; use look_at_model to look at it yourself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "model": { "type": "string", "description": "The model: a path in the project, absolute, or ~/..." },
                    "animation": { "type": "string", "description": "An animation to play as it opens (part of its name, e.g. walk)." },
                },
                "required": ["model"],
            },
        },
        {
            "name": "ask_for_review",
            "description": "Ask another agent for a second opinion on work in this folder: Wundur opens a new, read-only session of that agent here (it can read and run read-only commands, not change files), gives it your request, and its review comes back to this session as a message. The person allows it first. Don't wait for it: finish your turn; the review arrives as a new message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Which agent reviews: \"Claude Code\" or \"Cursor\". Left out: a different agent from this one, when there is one." },
                    "request": { "type": "string", "description": "What to review and what to look for, complete on its own (the reviewer has not seen this conversation): the files or the change, the concerns, what kind of answer you want." },
                },
                "required": ["request"],
            },
        },
    ])
}

/// One message to the window, its answer (`None` when it is not there).
fn ask(socket: &str, token: &str, payload: &Value) -> Option<Value> {
    let mut stream = Stream::connect(socket.to_fs_name::<GenericFilePath>().ok()?).ok()?;
    let line = json!({ "token": token, "event": "mcp", "payload": payload });
    stream.write_all(format!("{line}\n").as_bytes()).ok()?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;
    serde_json::from_str(answer.trim()).ok()
}

/// One tool call, answered by the window (`{"text", "error", "images": [{"data", "mimeType"}]}`),
/// as MCP's result.
fn call(socket: &str, token: &str, tool: &str, arguments: &Value) -> Value {
    match ask(socket, token, &json!({ "tool": tool, "arguments": arguments })) {
        Some(v) => {
            let text = v.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            let mut content = vec![json!({ "type": "text", "text": text })];
            for image in v.get("images").and_then(Value::as_array).cloned().unwrap_or_default() {
                content.push(json!({ "type": "image", "data": image["data"], "mimeType": image["mimeType"] }));
            }
            json!({ "content": content, "isError": v.get("error").and_then(Value::as_bool).unwrap_or(false) })
        }
        None => json!({ "content": [{ "type": "text", "text": "Wundur is not answering (its window may have closed)." }], "isError": true }),
    }
}
