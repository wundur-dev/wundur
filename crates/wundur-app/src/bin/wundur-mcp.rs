//! `wundur-mcp <socket> <token>`: the tool server (MCP, over stdin and stdout) an agent is
//! started with while Wundur drives its session. Its tools reach the other sessions in the
//! window: `list_sessions`, `send_message` and `ask_for_review`, the cards it shows the person
//! (`ask_to_choose`, `present_card`), and the tools the person made
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
- pack_sprites: cut the sprites out of a sheet (one you made, with a plain or transparent background) and pack them into a game-ready sheet with its JSON; it opens for the person. show_sprite_sheet: open an existing sheet.\n\
- The person's tool packs (make_image, edit_image, model_from_*, rig_model, music, sound_effect, voice_line and others) make game assets and more: search for them when a task needs one.\n\
- ask_to_choose: when the person should pick between pictures or models you made (variants, an edit and its original), or confirm one, show them with this rather than describing them or asking in text. Ask when it is a matter of their taste or you are unsure; skip it when they said to just go ahead.\n\
- present_card: show the person one of Wundur's cards (3D asset, image, sound effect, and others their account offers), filled in from what you know, when they should make or approve the thing themselves. They press its button, which is where it is spent; what they make comes back to you as a message.";

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
            "name": "pack_sprites",
            "description": "Cut the separate sprites out of one or more pictures (a sheet an image tool made, with a plain or transparent background) and pack them into one sprite sheet: a PNG and a JSON beside it in TexturePacker's JSON hash layout (Phaser, PixiJS and the Godot and Unity importers read it), with one animation through the frames in order. Two ways to pack: frames (default: every sprite in a cell of one size, bottoms on one baseline, so an animation doesn't jump; for a walk cycle, an attack) and atlas (each trimmed and packed tight; for props, icons, tiles). Sprites are found as shapes on the background, close shapes joined, specks dropped, in reading order; give grid when the sheet is a regular grid (each sprite then keeps its place in its cell). The result includes a picture of the sources with each sprite's box and number: look at it to check the cut. A background that isn't flat (a gradient, noise, a fake checkerboard) won't cut well: remove it first (edit_image). Free and local (well under a second). Opens the sheet for the person unless show is false.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "images": { "type": "string", "description": "The pictures: one path (in the project, absolute, or ~/...), or several separated by commas; their sprites are packed together in this order." },
                    "mode": { "type": "string", "enum": ["frames", "atlas"], "description": "frames (default): one cell size, aligned, for animation. atlas: trimmed and tight, for unrelated sprites." },
                    "grid": { "type": "string", "description": "Cut by a regular grid instead of finding shapes: columns x rows, like 4x2." },
                    "split": { "type": "boolean", "description": "false: use each picture whole as one sprite (separate frame files to pack together). Default true." },
                    "anchor": { "type": "string", "enum": ["bottom", "center"], "description": "frames: where each sprite sits in its cell (default bottom: feet on one baseline)." },
                    "columns": { "type": "integer", "description": "frames: how many columns (default: the squarest sheet)." },
                    "padding": { "type": "integer", "description": "Clear pixels around each sprite (default 2), so neighbours don't bleed in when drawn scaled." },
                    "names": { "type": "string", "description": "Names for the sprites, comma-separated in order (default: name_00, name_01...)." },
                    "name": { "type": "string", "description": "The base name for sprites and the animation (default: from the first picture's file name)." },
                    "out": { "type": "string", "description": "Where to write the sheet's PNG (the JSON goes beside it). Default: beside the first picture, as <name>-packed.png." },
                    "merge": { "type": "integer", "description": "Shapes this many pixels apart or closer are one sprite (default: from the picture's size, about 8 for 1024). Raise it when one sprite comes out as several boxes; lower it when two sprites share a box." },
                    "tolerance": { "type": "integer", "description": "How far from the background colour a pixel must be to be part of a sprite, 0-255 (default 32)." },
                    "min_pixels": { "type": "integer", "description": "Shapes smaller than this many pixels are dropped as specks (default: from the picture's size)." },
                    "power_of_two": { "type": "boolean", "description": "atlas: round the sheet's sides up to powers of two (some older engines need it)." },
                    "max_size": { "type": "integer", "description": "The largest side allowed (default 4096)." },
                    "show": { "type": "boolean", "description": "Open the sheet in the person's sprite sheet view (default true)." },
                },
                "required": ["images"],
            },
        },
        {
            "name": "show_sprite_sheet",
            "description": "Show a sprite sheet to the person: opens it in Wundur's sprite sheet view, a tab beside the chat with each frame boxed and the frames playing in order as an animation. Takes the sheet's JSON (TexturePacker's layout, which pack_sprites writes) or its PNG with the JSON beside it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sheet": { "type": "string", "description": "The sheet: its JSON or PNG, a path in the project, absolute, or ~/..." },
                },
                "required": ["sheet"],
            },
        },
        {
            "name": "ask_to_choose",
            "description": "Show the person pictures or 3D models you made, in the chat, and ask them to choose: which variant, whether one will do, or the edit or the original. You pick what to show and how (show); Wundur lays them out and gives the person that way's buttons and a note box, so the answer always has the same shape: {\"picked\": [paths], \"note\": \"...\"} (picked is empty when they reject). Use it whenever the choice is a matter of the person's taste (a style, a character's look, an icon) or you are unsure which is right, instead of describing the options in text. Don't use it when they told you to go ahead without asking. Free: nothing is made or spent. By default the call waits for their answer (a few minutes at most); if they haven't answered by then, or with wait: false, it returns at once and their answer comes to this session as a new message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "What they are choosing, short: \"Which coin fits the HUD?\", \"Is this the knight you had in mind?\"." },
                    "files": { "type": "array", "items": { "type": "string" }, "description": "The pictures (PNG, JPEG, WebP, GIF) or models (GLB, glTF) to show, paths in the project, in the order to show them. For before_after: the original first, then the edit." },
                    "show": { "type": "string", "enum": ["one", "compare", "grid", "before_after"], "description": "How to show them. one: a single file, large, to Confirm or Reject. compare: 2 or 3 side by side at the same size, to pick one. grid: 2 to 12 small, to pick one or more. before_after: an original and its edit, to keep one. Default: one for a single file, compare for 2 or 3, grid for more." },
                    "wait": { "type": "boolean", "description": "Wait for the answer (default true). false: show it and carry on; the answer comes as a new message." },
                },
                "required": ["question", "files"],
            },
        },
        {
            "name": "present_card",
            "description": "Show the person one of Wundur's cards in the chat, filled in from what you know: the cards they could open themselves from the Create menu. Built in: asset-3d (a 3D game asset, made from views the person approves; fill kind, name, description), image (fill prompt, quality: low|medium|high, size: 1024x1024|1536x1024|1024x1536, transparent), sound-effect (fill prompt, seconds, loop); their Wundur account may offer more (a wrong name lists them, with what each can be filled with). Use it when the person should make or approve the thing themselves, or asked to. They change what they like and press the card's button: anything spent is spent by their click, from their account. What they make (the files, in the project) comes back to this session as a new message, so finish your turn or carry on; don't present the same card again. Needs them signed in to Wundur. You can't design cards: only these, filled in.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "card": { "type": "string", "description": "Which card: asset-3d, image, sound-effect, or another the account offers." },
                    "fill": { "type": "object", "description": "Values to fill in, by name (only the ones the card has), e.g. {\"prompt\": \"a gold coin icon, pixel art\", \"transparent\": true}." },
                    "question": { "type": "string", "description": "Optional: shown above the card, e.g. \"Here's the knight from your description: change anything, then make it.\"" },
                },
                "required": ["card"],
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
