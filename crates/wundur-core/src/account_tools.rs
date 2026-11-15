//! The tools a Wundur account gives every agent while the person is signed in: images and sound
//! effects from the hosted API, called by Wundur with this machine's key (the agent never sees
//! it). They spend from the person's monthly allowance, so each run is asked about first, like
//! a costly pack tool. What they make is saved in the session's folder.
//!
//! They take the names the game-assets pack uses for the person's own keys (`make_image`,
//! `edit_image`, `sound_effect`): signed in, these are the ones the agent gets.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::account::{self, Auth};
use crate::asset_flow;
use crate::cli_tools::{Kind, Output, Param, Tool};

/// Where what they make is saved, in the session's folder (the game-assets pack's place too).
pub const FOLDER: &str = "assets/generated";

fn param(name: &str, kind: Kind, required: bool, choices: &[&str], description: &str) -> Param {
    Param { name: name.into(), kind, description: description.into(), required, choices: choices.iter().map(|c| c.to_string()).collect() }
}

fn tool(name: &str, description: &str, params: Vec<Param>) -> Tool {
    Tool { name: name.into(), description: description.into(), params, output: Output::Text, timeout_secs: 180, ask: true, enabled: true, ..Default::default() }
}

/// The tools, as the agent's tool list describes them.
pub fn tools() -> Vec<Tool> {
    let size = || param("size", Kind::Choice, false, &["1024x1024", "1536x1024", "1024x1536", "auto"], "Image size: square, landscape, portrait, or auto.");
    let quality = || param("quality", Kind::Choice, false, &["low", "medium", "high"], "low is quick and fine for ideas; high for final art.");
    let transparent = || param("transparent", Kind::Flag, false, &[], "A transparent background (for sprites and icons).");
    let name = || param("name", Kind::Text, false, &[], "File name for the result, without extension (else from the prompt).");
    vec![
        tool(
            "make_image",
            "Make an image from a description with the person's Wundur account (uses one of their monthly images; the person is asked first). Saves a PNG in assets/generated/.",
            vec![param("prompt", Kind::Text, true, &[], "What the image shows: subject, style, colours, framing."), size(), quality(), transparent(), name()],
        ),
        tool(
            "edit_image",
            "Change one or more PNG images in the project with the person's Wundur account (uses one of their monthly images; the person is asked first). The first image is changed; others are references. Saves a new PNG in assets/generated/.",
            vec![
                param("image", Kind::Text, true, &[], "The PNG(s) to change, paths in the project, comma-separated."),
                param("prompt", Kind::Text, true, &[], "What to change, and what to keep."),
                size(),
                quality(),
                transparent(),
                name(),
            ],
        ),
        tool(
            "sound_effect",
            "Make a sound effect from a description with the person's Wundur account (uses seconds from their monthly allowance; the person is asked first). Saves an MP3 in assets/generated/.",
            vec![
                param("text", Kind::Text, true, &[], "The sound: what makes it, the space it's in, how it starts and ends."),
                param("seconds", Kind::Number, false, &[], "Length, 0.5 to 30 seconds (give it when you know it: that's what's charged)."),
                param("loop", Kind::Flag, false, &[], "A seamless loop (ambience, engines)."),
                name(),
            ],
        ),
    ]
}

/// Whether `name` is one of these tools.
pub fn is_tool(name: &str) -> bool {
    matches!(name, "make_image" | "edit_image" | "sound_effect")
}

/// What a run gives back: text for the agent, whether it failed, and the files it saved.
pub struct Ran {
    pub text: String,
    pub failed: bool,
    pub files: Vec<PathBuf>,
}

fn failed(text: String) -> Ran {
    Ran { text, failed: true, files: Vec::new() }
}

/// A file name from `text`: lowercase words joined by dashes, at most 48 characters.
fn slug(text: &str) -> String {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(48).collect();
    if s.is_empty() { "file".into() } else { s.trim_end_matches('-').to_string() }
}

/// `folder/<base>.<ext>` in `cwd`, another name if taken.
fn free_path(cwd: &Path, base: &str, ext: &str) -> PathBuf {
    let dir = cwd.join(FOLDER);
    let mut path = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.{ext}"));
        n += 1;
    }
    path
}

fn text_arg<'a>(args: &'a Value, name: &str) -> &'a str {
    args.get(name).and_then(Value::as_str).map(str::trim).unwrap_or("")
}

fn flag(args: &Value, name: &str) -> bool {
    match args.get(name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.as_str(), "true" | "yes" | "1"),
        _ => false,
    }
}

/// A charging call, with a fresh idempotency key.
fn post(path: &str, body: &Value, wait: Duration) -> Result<Value, (String, bool)> {
    let key = asset_flow::idempotency_key();
    let text = account::shared().request(Auth::Key, "POST", path, Some(body), wait, Some(&key)).map_err(|e| asset_flow::explain(&e))?;
    serde_json::from_str(&text).map_err(|e| (format!("Wundur's server sent an answer that couldn't be read ({e})."), false))
}

fn why((text, upgrade): (String, bool)) -> Ran {
    failed(if upgrade { format!("{text} The person can upgrade in Wundur's Settings > Account.") } else { text })
}

/// Run tool `name` with the agent's `args`, saving into `cwd` (blocking: off the UI thread).
pub fn run(name: &str, args: &Value, cwd: &Path) -> Ran {
    match name {
        "make_image" | "edit_image" => image(name == "edit_image", args, cwd),
        "sound_effect" => sound(args, cwd),
        other => failed(format!("No account tool {other}.")),
    }
}

fn image(editing: bool, args: &Value, cwd: &Path) -> Ran {
    let prompt = text_arg(args, "prompt");
    if prompt.is_empty() {
        return failed("Say what the image shows (prompt).".into());
    }
    let size = match text_arg(args, "size") {
        "" => if editing { "auto" } else { "1024x1024" },
        s => s,
    };
    let quality = match text_arg(args, "quality") {
        "" | "auto" => "medium",
        "xhigh" => "high",
        q => q,
    };
    let mut body = json!({ "prompt": prompt, "size": size, "quality": quality, "background": if flag(args, "transparent") { "transparent" } else { "auto" } });
    let mut base = text_arg(args, "name").to_string();
    if editing {
        let paths: Vec<&str> = text_arg(args, "image").split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
        if paths.is_empty() {
            return failed("edit_image needs the image to change.".into());
        }
        let mut images = Vec::new();
        for p in &paths {
            let file = cwd.join(p);
            let inside = Path::new(p).components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir));
            if !inside || !p.to_lowercase().ends_with(".png") {
                return failed(format!("{p}: edit_image takes PNG files in the project."));
            }
            match std::fs::read(&file) {
                Ok(bytes) => images.push(base64::engine::general_purpose::STANDARD.encode(bytes)),
                Err(e) => return failed(format!("Couldn't read {p}: {e}")),
            }
        }
        body["images_base64"] = json!(images);
        if base.is_empty() {
            base = format!("{}-edit", Path::new(paths[0]).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
        }
    }
    let path = if editing { "/v1/images/edits" } else { "/v1/images/generations" };
    let answer = match post(path, &body, Duration::from_secs(150)) {
        Ok(a) => a,
        Err(e) => return why(e),
    };
    let Some(url) = answer["url"].as_str() else { return failed("Wundur's server sent no image.".into()) };
    let file = free_path(cwd, &slug(if base.is_empty() { prompt } else { &base }), "png");
    if let Err(e) = asset_flow::download(url, &file) {
        return failed(format!("The image was made but couldn't be saved: {e}"));
    }
    let shown = file.strip_prefix(cwd).unwrap_or(&file).display().to_string();
    let left = answer["images_left"].as_i64().map(|n| format!(" {n} image{} left this month.", if n == 1 { "" } else { "s" })).unwrap_or_default();
    Ran { text: format!("Saved {shown} ({size}, {quality}).{left}"), failed: false, files: vec![file] }
}

fn sound(args: &Value, cwd: &Path) -> Ran {
    let text = text_arg(args, "text");
    if text.is_empty() {
        return failed("Say what the sound is (text).".into());
    }
    let mut body = json!({ "text": text });
    let seconds = args.get("seconds").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    if let Some(s) = seconds {
        body["duration_seconds"] = json!(s.clamp(0.5, 30.0));
    }
    if flag(args, "loop") {
        body["loop"] = json!(true);
    }
    let answer = match post("/v1/audio/sound-effects", &body, Duration::from_secs(90)) {
        Ok(a) => a,
        Err(e) => return why(e),
    };
    let Some(url) = answer["url"].as_str() else { return failed("Wundur's server sent no sound.".into()) };
    let base = match text_arg(args, "name") {
        "" => slug(text),
        n => slug(n),
    };
    let file = free_path(cwd, &base, "mp3");
    if let Err(e) = asset_flow::download(url, &file) {
        return failed(format!("The sound was made but couldn't be saved: {e}"));
    }
    let shown = file.strip_prefix(cwd).unwrap_or(&file).display().to_string();
    let length = answer["duration_seconds"].as_f64().map(|d| format!(", {d:.1} s")).unwrap_or_default();
    let left = answer["sound_seconds_left"].as_f64().map(|n| format!(" {n:.0} seconds of sound effects left this month.")).unwrap_or_default();
    Ran { text: format!("Saved {shown}{length}.{left}"), failed: false, files: vec![file] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tools_ask_first_and_describe_their_spend() {
        let t = tools();
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["make_image", "edit_image", "sound_effect"]);
        assert!(t.iter().all(|t| t.ask && t.description.contains("asked first")));
        assert!(t.iter().all(|t| is_tool(&t.name)));
        assert!(!is_tool("music"));
    }

    #[test]
    fn names_are_slugs_and_never_overwrite() {
        assert_eq!(slug("A Shiny Gold Coin, pixel art!"), "a-shiny-gold-coin-pixel-art");
        assert_eq!(slug("??"), "file");
        let dir = std::env::temp_dir().join(format!("wundur-account-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(FOLDER)).unwrap();
        std::fs::write(dir.join(FOLDER).join("coin.png"), b"x").unwrap();
        assert_eq!(free_path(&dir, "coin", "png"), dir.join(FOLDER).join("coin-2.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_calls_fail_before_anything_is_spent() {
        let cwd = std::env::temp_dir();
        assert!(run("make_image", &json!({}), &cwd).failed);
        assert!(run("edit_image", &json!({ "prompt": "x", "image": "photo.jpg" }), &cwd).failed);
        assert!(run("edit_image", &json!({ "prompt": "x", "image": "../outside.png" }), &cwd).failed, "only files in the project");
        assert!(run("sound_effect", &json!({}), &cwd).failed);
    }

    /// A real sound effect from the account (spends about a second of the month's allowance):
    /// `cargo test -p wundur-core account_sound_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn account_sound_live() {
        let cwd = std::env::temp_dir().join(format!("wundur-account-sound-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        let ran = run("sound_effect", &json!({ "text": "a short coin pickup chime", "seconds": 1 }), &cwd);
        eprintln!("{}", ran.text);
        assert!(!ran.failed, "{}", ran.text);
        assert!(std::fs::metadata(&ran.files[0]).unwrap().len() > 1000);
    }
}
