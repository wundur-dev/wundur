//! Play links: a project's web build at a link for friends and testers
//! (`https://gummy-dash-k7x2m9ab.wundur.games`), through the API's shares.
//!
//! - The **setting** ([`PlayLink`]): the build command (optional), the folder the build lands in
//!   (with `index.html` at its top) and whether the build uses threads. It is part of the
//!   project, so it goes in `.wundur/project.json` (kept by git) under `play_link`.
//! - [`detect`] fills it in from what the project has, never by guessing: a Godot Web export
//!   preset, a Unity WebGL build, a `package.json` build script that writes `dist/`, or a folder
//!   with an `index.html`.
//! - The **link** ([`ShareRecord`]): one per project. The first Share makes it; each update sends a
//!   new version to the same link, so the address doesn't change. Its id is this machine's and
//!   this account's, so it goes in `.wundur/share.json`, which git ignores, in the main checkout:
//!   a task's worktree updates the same link.
//! - [`publish`] lists the build's files, asks for upload addresses, uploads and publishes; the
//!   API calls are on [`crate::account::Account::send`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::account::{self, Auth, Error};

/// The project's web build, as kept in `.wundur/project.json` under `play_link`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayLink {
    /// Run in the project's folder before uploading (empty: nothing to run).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub build: String,
    /// The folder with `index.html` at its top, relative to the project's folder (`.` for the
    /// project's folder itself). Empty: not chosen yet.
    pub output: String,
    /// The build uses threads (`SharedArrayBuffer`): the link is served cross-origin isolated.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub threads: bool,
}

impl PlayLink {
    /// The output folder in `root`.
    pub fn output_in(&self, root: &Path) -> Option<PathBuf> {
        let out = self.output.trim();
        if out.is_empty() {
            return None;
        }
        let p = Path::new(out);
        Some(if p.is_absolute() { p.to_path_buf() } else if out == "." { root.to_path_buf() } else { root.join(p) })
    }
}

/// What [`detect`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    /// The setting to start from: none when nothing was found. Its output may be empty when the
    /// kind of project is known but its web build isn't there yet (Unity before a WebGL build).
    pub setting: Option<PlayLink>,
    /// What was found, or what to do: "Found Godot's Web export preset".
    pub note: String,
}

// ----- the setting -----

const PROJECT_JSON: &str = crate::engines::PROJECT_INFO;
const SHARE_JSON: &str = ".wundur/share.json";
/// What `.wundur/.gitignore` says: everything Wundur keeps there stays out of git, except the
/// project's own settings.
const GITIGNORE: &str = "*\n!.gitignore\n!project.json\n";

/// The project's setting, if it has one.
pub fn setting(root: &Path) -> Option<PlayLink> {
    let text = std::fs::read_to_string(root.join(PROJECT_JSON)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(v.get("play_link")?.clone()).ok()
}

/// Keep `link` as the project's setting, leaving the rest of `project.json` as it is.
pub fn save_setting(root: &Path, link: &PlayLink) -> Result<(), String> {
    let path = root.join(PROJECT_JSON);
    let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    if !v.is_object() {
        v = json!({});
    }
    v["play_link"] = serde_json::to_value(link).map_err(|e| e.to_string())?;
    own_folder(root)?;
    let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("Couldn't save {PROJECT_JSON}: {e}"))
}

/// Make `.wundur/` with its `.gitignore`, and make sure that keeps `project.json` in git (a
/// `.gitignore` of just `*`, as other tools write, would leave the setting out).
fn own_folder(root: &Path) -> Result<(), String> {
    let own = root.join(".wundur");
    std::fs::create_dir_all(&own).map_err(|e| format!("Couldn't make .wundur: {e}"))?;
    let ignore = own.join(".gitignore");
    let text = std::fs::read_to_string(&ignore).unwrap_or_default();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let new = if text.trim().is_empty() {
        GITIGNORE.to_string()
    } else {
        let mut t = text.clone();
        if !t.ends_with('\n') {
            t.push('\n');
        }
        for keep in ["!.gitignore", "!project.json"] {
            if !lines.contains(&keep) {
                t.push_str(keep);
                t.push('\n');
            }
        }
        t
    };
    if new != text {
        std::fs::write(&ignore, new).map_err(|e| format!("Couldn't write .wundur/.gitignore: {e}"))?;
    }
    Ok(())
}

// ----- the link -----

/// The project's link, as kept in the main checkout's `.wundur/share.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareRecord {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// Where the project's link is kept: the main checkout of `root`'s repository (a task's
/// worktree shares its project's link), else `root`.
pub fn record_root(root: &Path) -> PathBuf {
    let out = crate::platform::command("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let common = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            match common.file_name() {
                Some(n) if n == ".git" => common.parent().map_or_else(|| root.to_path_buf(), Path::to_path_buf),
                _ => root.to_path_buf(),
            }
        }
        _ => root.to_path_buf(),
    }
}

/// The project's link, if it has one (`root`: from [`record_root`]).
pub fn record(root: &Path) -> Option<ShareRecord> {
    let r: ShareRecord = serde_json::from_str(&std::fs::read_to_string(root.join(SHARE_JSON)).ok()?).ok()?;
    (!r.id.is_empty()).then_some(r)
}

pub fn save_record(root: &Path, r: &ShareRecord) -> Result<(), String> {
    own_folder(root)?;
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    std::fs::write(root.join(SHARE_JSON), text + "\n").map_err(|e| format!("Couldn't save {SHARE_JSON}: {e}"))
}

/// Forget the project's link (it was turned off, or is gone): the next Share makes a new one.
pub fn forget_record(root: &Path) {
    let _ = std::fs::remove_file(root.join(SHARE_JSON));
}

// ----- detection -----

/// Look for the project's web build. Blocking (reads files, looks for Godot): run it off the UI
/// thread.
pub fn detect(root: &Path) -> Detection {
    let found = candidates(root);
    let d = detect_by_engine(root, &found);
    if d.setting.as_ref().is_some_and(|s| !s.output.is_empty()) {
        return d;
    }
    // Nothing the engine names: the newest folder that looks like a build, shown in the
    // sheet's list as the choice.
    match found.first() {
        Some(c) => {
            let threads = d.setting.as_ref().is_some_and(|s| s.threads);
            Detection {
                note: format!("Picked the newest web build: {}/ ({}). Choose another in the list if it isn't the one.", c.output, c.kind.label()),
                setting: Some(PlayLink { build: String::new(), output: c.output.clone(), threads }),
            }
        }
        None => d,
    }
}

fn detect_by_engine(root: &Path, found: &[Candidate]) -> Detection {
    if root.join("project.godot").is_file() {
        return detect_godot(root);
    }
    if root.join("ProjectSettings/ProjectVersion.txt").is_file() {
        return detect_unity(root, found);
    }
    if let Some(d) = detect_package(root) {
        return d;
    }
    for dir in [".", "build", "public", "web"] {
        let folder = if dir == "." { root.to_path_buf() } else { root.join(dir) };
        if folder.join("index.html").is_file() {
            let shown = if dir == "." { "the project's folder".to_string() } else { format!("{dir}/") };
            return Detection {
                setting: Some(PlayLink { output: dir.into(), ..Default::default() }),
                note: format!("Found an index.html in {shown}."),
            };
        }
    }
    Detection { setting: None, note: "No web build yet. Choose the folder your game's web build goes in (the one with index.html).".into() }
}

/// A preset in Godot's `export_presets.cfg`.
#[derive(Default, Debug)]
struct Preset {
    name: String,
    platform: String,
    export_path: String,
    threads: bool,
}

fn godot_presets(text: &str) -> Vec<Preset> {
    let mut presets: Vec<(String, Preset)> = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').to_string();
        // `[preset.0]` has the name, platform and path; `[preset.0.options]` the threads.
        let Some(rest) = section.strip_prefix("preset.") else { continue };
        let (id, options) = match rest.split_once('.') {
            Some((id, "options")) => (id.to_string(), true),
            Some(_) => continue,
            None => (rest.to_string(), false),
        };
        let at = match presets.iter().position(|(i, _)| *i == id) {
            Some(at) => at,
            None => {
                presets.push((id, Preset::default()));
                presets.len() - 1
            }
        };
        let p = &mut presets[at].1;
        match (options, key.trim()) {
            (false, "name") => p.name = value,
            (false, "platform") => p.platform = value,
            (false, "export_path") => p.export_path = value,
            (true, "variant/thread_support") | (true, "threads/thread_support") => p.threads = value == "true",
            _ => {}
        }
    }
    presets.into_iter().map(|(_, p)| p).collect()
}

fn detect_godot(root: &Path) -> Detection {
    let text = std::fs::read_to_string(root.join("export_presets.cfg")).unwrap_or_default();
    let presets = godot_presets(&text);
    let Some(web) = presets.into_iter().find(|p| p.platform == "Web" || p.platform == "HTML5") else {
        return Detection { setting: None, note: "Godot project with no Web export preset. Add one in Godot (Project > Export > Add… > Web), then come back.".into() };
    };
    let path = web.export_path.trim_start_matches("res://").to_string();
    if path.is_empty() {
        return Detection {
            setting: Some(PlayLink { threads: web.threads, ..Default::default() }),
            note: format!("Godot's \"{}\" preset has no export path. Set one in Godot (build/web/index.html), or choose the folder.", web.name),
        };
    }
    let path = Path::new(&path);
    let folder = path.parent().map(|p| p.to_string_lossy().to_string()).filter(|p| !p.is_empty()).unwrap_or_else(|| ".".into());
    let godot = godot_program().map_or_else(|| "godot".to_string(), |p| p.to_string_lossy().to_string());
    let build = format!(
        "mkdir -p {} && {} --headless --export-release {} {}",
        shell_quote(&folder),
        shell_quote(&godot),
        shell_quote(&web.name),
        shell_quote(&path.to_string_lossy())
    );
    Detection {
        setting: Some(PlayLink { build, output: folder, threads: web.threads }),
        note: format!("Found Godot's \"{}\" Web export preset.{}", web.name, if web.threads { " It has Thread Support on." } else { "" }),
    }
}

/// The Godot program: on the PATH, or in an app in Applications.
fn godot_program() -> Option<PathBuf> {
    ["godot", "godot4"].iter().find_map(|p| crate::host::find_program(p)).or_else(|| {
        ["/Applications/Godot*.app", "~/Applications/Godot*.app"]
            .iter()
            .flat_map(|p| crate::engines::expand(p))
            .map(|app| app.join("Contents/MacOS/Godot"))
            .find(|p| p.is_file())
    })
}

fn detect_unity(root: &Path, found: &[Candidate]) -> Detection {
    let threads = std::fs::read_to_string(root.join("ProjectSettings/ProjectSettings.asset"))
        .unwrap_or_default()
        .lines()
        .any(|l| matches!(l.trim().split_once(':'), Some(("webGLThreadsSupport", v)) if v.trim() == "1"));
    // The newest Unity build anywhere in the project (an agent may build into a hidden copy).
    match found.iter().find(|c| c.kind == BuildKind::Unity) {
        Some(c) => Detection {
            note: format!("Found a Unity WebGL build in {}/. Build it again in Unity before you update the link.{}", c.output, if threads { " Multithreading is on." } else { "" }),
            setting: Some(PlayLink { build: String::new(), output: c.output.clone(), threads }),
        },
        None => Detection {
            setting: Some(PlayLink { threads, ..Default::default() }),
            note: "Unity project with no WebGL build yet. Build one in Unity (File > Build Profiles > Web), then choose its folder.".into(),
        },
    }
}

// ----- folders that look like builds -----

/// What kind of web build a folder looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildKind {
    /// `index.html` beside `Build/*.loader.js`.
    Unity,
    /// `index.html` with a `.wasm` and a `.pck`.
    Godot,
    /// A bundler's output (`dist/`, `build/`, `out/`…) whose `index.html` loads a script that's
    /// there.
    JavaScript,
}

impl BuildKind {
    pub fn label(self) -> &'static str {
        match self {
            BuildKind::Unity => "Unity WebGL",
            BuildKind::Godot => "Godot web",
            BuildKind::JavaScript => "JavaScript",
        }
    }
}

/// A folder in the project that looks like a web game build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Relative to the project's folder, with forward slashes.
    pub output: String,
    pub kind: BuildKind,
    /// When its `index.html` was written: when it was built.
    pub built: std::time::SystemTime,
}

/// Folders never looked in: version control, Wundur's own, other tasks' worktrees, packages,
/// and Unity's caches.
const NOT_SEARCHED: [&str; 10] = [".git", ".wundur", ".worktrees", "node_modules", "Library", "Temp", "Logs", "obj", "UserSettings", "ProjectSettings"];

/// Names a bundler's output folder goes by.
const OUTPUT_NAMES: [&str; 7] = ["dist", "build", "out", "release", "www", "web-build", "public_html"];

/// The folders in `root` that look like web game builds, newest first. Looks in hidden folders
/// too (`.build/`), up to six levels down. Blocking.
pub fn candidates(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut look = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = look.pop() {
        seen += 1;
        if seen > 20_000 {
            break;
        }
        if let Some(kind) = build_kind(&dir) {
            let built = std::fs::metadata(dir.join("index.html")).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let rel = dir.strip_prefix(root).unwrap_or(&dir).to_string_lossy().replace('\\', "/");
            out.push(Candidate { output: if rel.is_empty() { ".".into() } else { rel }, kind, built });
            // A build's own folders aren't builds of their own.
            continue;
        }
        if depth >= 6 {
            continue;
        }
        // A Unity project's Assets are its source (its WebGL templates aren't builds).
        let unity = dir.join("ProjectSettings/ProjectVersion.txt").is_file();
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if NOT_SEARCHED.contains(&name.as_str()) || (unity && (name == "Assets" || name == "Packages")) {
                continue;
            }
            // Not through links: they can loop.
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                look.push((e.path(), depth + 1));
            }
        }
    }
    out.sort_by(|a, b| b.built.cmp(&a.built).then_with(|| a.output.cmp(&b.output)));
    out
}

/// What kind of build `dir` looks like, if any.
fn build_kind(dir: &Path) -> Option<BuildKind> {
    let html = std::fs::read_to_string(dir.join("index.html")).ok()?;
    let names: Vec<String> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    if std::fs::read_dir(dir.join("Build")).is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".loader.js"))) {
        return Some(BuildKind::Unity);
    }
    if names.iter().any(|n| n.ends_with(".wasm")) && names.iter().any(|n| n.ends_with(".pck")) {
        return Some(BuildKind::Godot);
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    // Source folders have the project's package.json; output folders don't.
    if OUTPUT_NAMES.contains(&name.as_str()) && !names.iter().any(|n| n == "package.json") && loads_local_script(dir, &html) {
        return Some(BuildKind::JavaScript);
    }
    None
}

/// Whether `html` loads a script file that is in `dir` (`./assets/index-abc.js`, `/assets/…`).
fn loads_local_script(dir: &Path, html: &str) -> bool {
    let mut rest = html;
    while let Some(at) = rest.find("src=") {
        rest = &rest[at + 4..];
        let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
        let value: String = rest[1..].chars().take_while(|c| *c != q).collect();
        let path = value.split(['?', '#']).next().unwrap_or("");
        if path.ends_with(".js") && !path.contains("://") && !path.starts_with("//") {
            let rel = path.trim_start_matches("./").trim_start_matches('/');
            if !rel.contains("..") && dir.join(rel).is_file() {
                return true;
            }
        }
    }
    false
}

fn detect_package(root: &Path) -> Option<Detection> {
    let pkg: Value = serde_json::from_str(&std::fs::read_to_string(root.join("package.json")).ok()?).ok()?;
    let script = pkg.get("scripts")?.get("build")?.as_str()?.to_string();
    let built = root.join("dist/index.html").is_file();
    let vite_default = script.contains("vite build") && !vite_sets_out_dir(root);
    if !built && !vite_default {
        // Its index.html is the source, not the build: say so rather than share it.
        return Some(Detection { setting: None, note: format!("package.json builds with \"{script}\", but where it writes isn't clear. Choose the folder the build goes in.") });
    }
    let runner = if root.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if root.join("yarn.lock").is_file() {
        "yarn"
    } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        "bun"
    } else {
        "npm"
    };
    let build = if runner == "yarn" { "yarn build".to_string() } else { format!("{runner} run build") };
    Some(Detection { setting: Some(PlayLink { build: build.clone(), output: "dist".into(), threads: false }), note: format!("Found a build script in package.json: {build} makes dist/.") })
}

fn vite_sets_out_dir(root: &Path) -> bool {
    ["js", "ts", "mjs", "mts", "cjs", "cts"].iter().any(|ext| std::fs::read_to_string(root.join(format!("vite.config.{ext}"))).is_ok_and(|t| t.contains("outDir")))
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ----- the build -----

/// Run the build command in `root` through the person's login shell (a window started from the
/// Finder lacks the shell's PATH, where npm and the like are). Blocking. On failure, the end of
/// what it printed.
pub fn run_build(root: &Path, command: &str) -> Result<(), String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let out = crate::platform::command(&shell)
        .args(["-lc", command])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't start the build: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    let code = out.status.code().map_or_else(|| "was stopped".to_string(), |c| format!("ended with code {c}"));
    Err(if tail.is_empty() { format!("The build {code}.") } else { format!("The build {code}:\n{tail}") })
}

// ----- the files -----

/// A file of the build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildFile {
    /// Relative to the build folder, with forward slashes.
    pub path: String,
    pub bytes: u64,
}

/// What's in a build folder, ready to upload.
#[derive(Clone, Debug, Default)]
pub struct Build {
    pub folder: PathBuf,
    pub files: Vec<BuildFile>,
    pub bytes: u64,
    /// Things worth a look before it goes up (paths from the site's root).
    pub warnings: Vec<String>,
}

/// Names that never go up: hidden files and folders (`.git`, `.wundur`, `.DS_Store`),
/// `node_modules`, and Unity's debug folders marked not to ship.
fn skipped(name: &str) -> bool {
    // Unity leaves debug folders it says not to ship in the build folder: Burst's
    // `<Game>_BurstDebugInformation_DoNotShip` and IL2CPP's `…_BackUpThisFolder_ButDontShipItWithYourGame`.
    name.starts_with('.') || name == "node_modules" || name.ends_with("_DoNotShip") || name.ends_with("ButDontShipItWithYourGame")
}

/// List the build in `folder`. Blocking.
pub fn scan(folder: &Path) -> Result<Build, String> {
    if !folder.is_dir() {
        return Err(format!("{} isn't there. Run the build first, or choose another folder.", folder.display()));
    }
    if !folder.join("index.html").is_file() {
        return Err(format!("{} has no index.html at its top level.", folder.display()));
    }
    let mut files = Vec::new();
    let mut look = vec![folder.to_path_buf()];
    while let Some(dir) = look.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("Couldn't read {}: {e}", dir.display()))?;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if skipped(&name) {
                continue;
            }
            // Follows links, as a web server would.
            let Ok(meta) = std::fs::metadata(e.path()) else { continue };
            if meta.is_dir() {
                look.push(e.path());
            } else if meta.is_file() {
                let rel = e.path().strip_prefix(folder).unwrap_or(&e.path()).to_string_lossy().replace('\\', "/");
                files.push(BuildFile { path: rel, bytes: meta.len() });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let bytes = files.iter().map(|f| f.bytes).sum();
    let html = std::fs::read_to_string(folder.join("index.html")).unwrap_or_default();
    let mut warnings = Vec::new();
    let roots = root_paths(&html);
    if !roots.is_empty() {
        warnings.push(format!(
            "index.html loads {} from the site's root, which won't load on the link (the game is served from a folder of its own). Use relative paths; for Vite, set base: './' in vite.config.js.",
            roots.iter().take(3).map(|r| format!("\"{r}\"")).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(Build { folder: folder.to_path_buf(), files, bytes, warnings })
}

/// The `src` and `href` values in `html` that start at the site's root (`/assets/x.js`, but not
/// `//cdn…`).
pub fn root_paths(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attr in ["src=", "href="] {
        let mut rest = html;
        while let Some(at) = rest.find(attr) {
            rest = &rest[at + attr.len()..];
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let value: String = rest[1..].chars().take_while(|c| *c != q).collect();
            if value.starts_with('/') && !value.starts_with("//") && !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out
}

// ----- the API -----

/// A share as the API shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ShareView {
    pub id: String,
    pub title: String,
    pub url: String,
    /// `uploading`, `live`, `hidden` (under review), `expired`, `revoked`, `removed`, `deleted`.
    pub status: String,
    pub version: u64,
    pub files: u64,
    pub bytes: u64,
    pub published_at: Option<String>,
    pub expires_at: Option<String>,
    /// Until when an expired link can still be extended.
    pub revivable_until: Option<String>,
    pub plays_total: u64,
    pub players_today: u64,
    /// `expires_soon` within two days of lapsing.
    pub warning: Option<String>,
    pub isolated: bool,
}

impl ShareView {
    /// The link is gone for good: a new Share makes a new one.
    pub fn gone(&self) -> bool {
        matches!(self.status.as_str(), "revoked" | "removed" | "deleted")
    }
}

#[derive(Debug, Deserialize)]
struct Upload {
    path: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct Uploads {
    id: String,
    url: String,
    #[serde(default)]
    uploads: Vec<Upload>,
}

/// The key when this machine has one (it lasts), else the signed-in session.
fn auth() -> Auth {
    if account::shared().api_key().is_some() { Auth::Key } else { Auth::Session }
}

fn call<T: serde::de::DeserializeOwned>(method: &str, path: &str, body: Option<&Value>) -> Result<T, Error> {
    let text = account::shared().send(auth(), method, path, body)?;
    serde_json::from_str(&text).map_err(|e| Error::Network(format!("an answer that couldn't be read ({e})")))
}

fn encode(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// `GET /v1/shares`: the account's shares.
pub fn list() -> Result<Vec<ShareView>, Error> {
    #[derive(Deserialize)]
    struct Shares {
        #[serde(default)]
        shares: Vec<ShareView>,
    }
    Ok(call::<Shares>("GET", "/v1/shares", None)?.shares)
}

/// `POST /v1/shares/{id}/extend`: 7 more days.
pub fn extend(id: &str) -> Result<ShareView, Error> {
    call("POST", &format!("/v1/shares/{}/extend", encode(id)), None)
}

/// `DELETE /v1/shares/{id}`: the link stops at once.
pub fn revoke(id: &str) -> Result<(), Error> {
    account::shared().send(auth(), "DELETE", &format!("/v1/shares/{}", encode(id)), None).map(|_| ())
}

fn publish_version(id: &str) -> Result<ShareView, Error> {
    call("POST", &format!("/v1/shares/{}/publish", encode(id)), None)
}

/// What [`publish`] is doing, for its progress.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Asking,
    Uploading { done: u64, total: u64 },
    Publishing,
}

/// Why [`publish`] stopped.
#[derive(Debug)]
pub enum PublishError {
    /// The API, or the network.
    Api(Error),
    /// A file couldn't be read or sent.
    Upload(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Api(e) => f.write_str(&message(e)),
            PublishError::Upload(e) => f.write_str(e),
        }
    }
}

impl PublishError {
    pub fn code(&self) -> Option<&str> {
        match self {
            PublishError::Api(e) => e.code(),
            PublishError::Upload(_) => None,
        }
    }
}

/// What to show for an API error: the server's own message, with what to do where the code
/// says.
pub fn message(e: &Error) -> String {
    match e {
        Error::Api(a) => {
            let said = if a.message.is_empty() { format!("The server said {} ({}).", a.code, a.status) } else { a.message.clone() };
            match a.code.as_str() {
                "too_many_shares" => format!("{said} Turn off an old link first."),
                "shares_unavailable" => format!("Sharing isn't available right now. {said}"),
                "gone" | "not_updatable" | "not_found" => format!("{said} Make a new link to share again."),
                _ => said,
            }
        }
        Error::SignedOut => "Sign in to share a play link.".into(),
        other => {
            let t = other.to_string();
            let mut c = t.chars();
            c.next().map_or(t.clone(), |f| f.to_uppercase().collect::<String>() + c.as_str())
        }
    }
}

/// The link is gone for good (turned off, taken down, deleted, or not this account's): offer a
/// new one.
pub fn is_gone(code: Option<&str>) -> bool {
    matches!(code, Some("gone" | "not_updatable" | "not_found"))
}

/// Send `build` to the link `existing` (a new version), or to a new link called `title`;
/// upload, publish, and return the link as it is now. Blocking. A version that went stale
/// meanwhile, or files the server didn't get whole, are tried once more. `made` hears a new
/// link's record as soon as it exists, so a failed upload after it is updated, not made again.
pub fn publish(
    existing: Option<&str>,
    title: &str,
    build: &Build,
    threads: bool,
    progress: &(dyn Fn(Step) + Sync),
    made: &dyn Fn(ShareRecord),
) -> Result<ShareView, PublishError> {
    let files: Vec<Value> = build.files.iter().map(|f| json!({ "path": f.path, "bytes": f.bytes })).collect();
    progress(Step::Asking);
    let mut id = existing.map(str::to_string);
    let ask = |id: &Option<String>| -> Result<Uploads, Error> {
        match id {
            Some(id) => call("POST", &format!("/v1/shares/{}/versions", encode(id)), Some(&json!({ "files": files, "isolated": threads }))),
            None => call("POST", "/v1/shares", Some(&json!({ "title": title, "files": files, "isolated": threads }))),
        }
    };
    let mut retried = false;
    loop {
        let uploads = ask(&id).map_err(PublishError::Api)?;
        if id.is_none() {
            made(ShareRecord { id: uploads.id.clone(), url: uploads.url.clone(), title: title.to_string() });
        }
        id = Some(uploads.id.clone());
        upload(&build.folder, &build.files, &uploads.uploads, progress)?;
        progress(Step::Publishing);
        match publish_version(&uploads.id) {
            Ok(view) => return Ok(view),
            Err(e) if !retried && matches!(e.code(), Some("stale_version" | "files_missing")) => {
                retried = true;
                progress(Step::Asking);
            }
            Err(e) => return Err(PublishError::Api(e)),
        }
    }
}

/// PUT each file to its address: exactly its bytes and no headers of ours (the size is signed),
/// four at a time.
fn upload(folder: &Path, files: &[BuildFile], uploads: &[Upload], progress: &(dyn Fn(Step) + Sync)) -> Result<(), PublishError> {
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    let done = AtomicU64::new(0);
    let next = AtomicUsize::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    progress(Step::Uploading { done: 0, total });
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(600))).http_status_as_error(false).build().into();
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    if failed.lock().is_ok_and(|f| f.is_some()) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(up) = uploads.get(i) else { return };
                    let result = send_file(&http, folder, files, up);
                    match result {
                        Ok(n) => {
                            let now = done.fetch_add(n, Ordering::SeqCst) + n;
                            progress(Step::Uploading { done: now, total });
                        }
                        Err(e) => {
                            if let Ok(mut f) = failed.lock() {
                                f.get_or_insert(e);
                            }
                            return;
                        }
                    }
                }
            });
        }
    });
    match failed.into_inner().ok().flatten() {
        Some(e) => Err(PublishError::Upload(e)),
        None => Ok(()),
    }
}

fn send_file(http: &ureq::Agent, folder: &Path, files: &[BuildFile], up: &Upload) -> Result<u64, String> {
    let declared = files.iter().find(|f| f.path == up.path).ok_or_else(|| format!("The server asked for {}, which isn't in the build.", up.path))?;
    let bytes = std::fs::read(folder.join(&up.path)).map_err(|e| format!("Couldn't read {}: {e}", up.path))?;
    if bytes.len() as u64 != declared.bytes {
        return Err(format!("{} changed while uploading. Share again.", up.path));
    }
    let mut last = String::new();
    // A dropped connection is tried again twice.
    for _ in 0..3 {
        match http.put(&up.url).send(&bytes[..]) {
            Ok(r) if r.status().is_success() => return Ok(declared.bytes),
            Ok(r) => {
                last = format!("Uploading {} failed (the storage answered {}).", up.path, r.status().as_u16());
                if r.status().is_client_error() {
                    break;
                }
            }
            Err(e) => last = format!("Uploading {} failed: {e}", up.path),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wundur-play-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn godot_web_preset_fills_command_output_and_threads() {
        let root = scratch("godot");
        write(&root, "project.godot", "config_version=5\n");
        write(
            &root,
            "export_presets.cfg",
            "[preset.0]\nname=\"macOS\"\nplatform=\"macOS\"\nexport_path=\"build/mac.zip\"\n\n[preset.0.options]\n\n[preset.1]\nname=\"Web Build\"\nplatform=\"Web\"\nexport_path=\"build/web/index.html\"\n\n[preset.1.options]\nvariant/thread_support=true\n",
        );
        let d = detect(&root);
        let s = d.setting.unwrap();
        assert_eq!(s.output, "build/web");
        assert!(s.threads);
        assert!(s.build.contains("--headless --export-release 'Web Build' build/web/index.html"), "{}", s.build);
        assert!(s.build.starts_with("mkdir -p build/web && "), "{}", s.build);
        assert!(d.note.contains("Web Build"), "{}", d.note);
    }

    #[test]
    fn godot_without_web_preset_says_what_to_do() {
        let root = scratch("godot-none");
        write(&root, "project.godot", "");
        let d = detect(&root);
        assert!(d.setting.is_none());
        assert!(d.note.contains("Web export preset"), "{}", d.note);
    }

    #[test]
    fn unity_finds_its_webgl_build_and_thread_setting() {
        let root = scratch("unity");
        write(&root, "ProjectSettings/ProjectVersion.txt", "m_EditorVersion: 6000.0.1f1\n");
        write(&root, "ProjectSettings/ProjectSettings.asset", "PlayerSettings:\n  webGLThreadsSupport: 1\n");
        write(&root, "Library/Junk/index.html", "");
        write(&root, "Builds/WebGL/index.html", "<html></html>");
        write(&root, "Builds/WebGL/Build/Game.loader.js", "");
        let s = detect(&root).setting.unwrap();
        assert_eq!(s.output, "Builds/WebGL");
        assert!(s.threads);
        assert!(s.build.is_empty());
    }

    /// Mark `rel`'s index.html as built `hours_ago`.
    fn built(root: &Path, rel: &str, hours_ago: u64) {
        let f = std::fs::File::options().write(true).open(root.join(rel).join("index.html")).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(hours_ago * 3600)).unwrap();
    }

    #[test]
    fn candidates_are_builds_only_newest_first_hidden_folders_included() {
        let root = scratch("candidates");
        write(&root, "ProjectSettings/ProjectVersion.txt", "");
        // An old Unity build, and a newer one an agent made in a hidden copy of the project.
        write(&root, "Builds/WebGL/index.html", "<html></html>");
        write(&root, "Builds/WebGL/Build/a.loader.js", "");
        write(&root, ".build/copy/ProjectSettings/ProjectVersion.txt", "");
        write(&root, ".build/copy/Builds/WebGLRelease/index.html", "<html></html>");
        write(&root, ".build/copy/Builds/WebGLRelease/Build/b.loader.js", "");
        // Not builds: Unity templates (source), a tool page, Unity's cache, a source index.html.
        write(&root, "Assets/WebGLTemplates/Mine/index.html", "{{{ PRODUCT_NAME }}}");
        write(&root, ".build/copy/Assets/WebGLTemplates/Mine/index.html", "x");
        write(&root, "tools/viewer/index.html", "<script src=\"./viewer.js\"></script>");
        write(&root, "tools/viewer/viewer.js", "");
        write(&root, "Library/x/dist/index.html", "<script src=\"a.js\"></script>");
        write(&root, "Library/x/dist/a.js", "");
        write(&root, "web/package.json", "{}");
        write(&root, "web/index.html", "<script type=\"module\" src=\"/src/main.js\"></script>");
        write(&root, "web/src/main.js", "");
        // Builds: a Vite build and a Godot export.
        write(&root, "web/dist/index.html", "<script type=\"module\" crossorigin src=\"./assets/index-abc.js?v=1\"></script>");
        write(&root, "web/dist/assets/index-abc.js", "");
        write(&root, "exports/godot/index.html", "<html></html>");
        write(&root, "exports/godot/index.wasm", "");
        write(&root, "exports/godot/index.pck", "");
        built(&root, "Builds/WebGL", 300);
        built(&root, ".build/copy/Builds/WebGLRelease", 2);
        built(&root, "web/dist", 30);
        built(&root, "exports/godot", 50);
        let found: Vec<(String, BuildKind)> = candidates(&root).into_iter().map(|c| (c.output, c.kind)).collect();
        assert_eq!(
            found,
            [
                (".build/copy/Builds/WebGLRelease".to_string(), BuildKind::Unity),
                ("web/dist".to_string(), BuildKind::JavaScript),
                ("exports/godot".to_string(), BuildKind::Godot),
                ("Builds/WebGL".to_string(), BuildKind::Unity),
            ]
        );
        // Unity's detection takes the newest Unity build.
        assert_eq!(detect(&root).setting.unwrap().output, ".build/copy/Builds/WebGLRelease");
    }

    #[test]
    fn with_nothing_named_the_newest_build_is_picked() {
        let root = scratch("fallback");
        write(&root, "package.json", r#"{"scripts":{"build":"webpack"}}"#);
        write(&root, "out/index.html", "<script src=\"bundle.js\"></script>");
        write(&root, "out/bundle.js", "");
        let d = detect(&root);
        assert_eq!(d.setting.unwrap().output, "out");
        assert!(d.note.starts_with("Picked the newest web build: out/"), "{}", d.note);
    }

    #[test]
    fn unity_before_a_build_has_no_output() {
        let root = scratch("unity-none");
        write(&root, "ProjectSettings/ProjectVersion.txt", "");
        let d = detect(&root);
        assert_eq!(d.setting.unwrap().output, "");
        assert!(d.note.contains("no WebGL build"), "{}", d.note);
    }

    #[test]
    fn vite_build_script_means_dist_unless_out_dir_is_set() {
        let root = scratch("vite");
        write(&root, "package.json", r#"{"scripts":{"dev":"vite","build":"vite build"}}"#);
        write(&root, "index.html", "<script src=\"/src/main.js\"></script>");
        write(&root, "pnpm-lock.yaml", "");
        let s = detect(&root).setting.unwrap();
        assert_eq!((s.build.as_str(), s.output.as_str()), ("pnpm run build", "dist"));
        // An outDir of its own: not guessed (the source index.html isn't the build either).
        write(&root, "vite.config.js", "export default { build: { outDir: 'out' } }");
        let d = detect(&root);
        assert!(d.setting.is_none());
        assert!(d.note.contains("Choose the folder"), "{}", d.note);
        // Unless dist/ has a build in it already.
        write(&root, "dist/index.html", "");
        assert_eq!(detect(&root).setting.unwrap().output, "dist");
    }

    #[test]
    fn plain_folder_with_index_html() {
        let root = scratch("plain");
        write(&root, "web/index.html", "");
        assert_eq!(detect(&root).setting.unwrap().output, "web");
        write(&root, "index.html", "");
        assert_eq!(detect(&root).setting.unwrap().output, ".");
        let empty = scratch("plain-empty");
        let d = detect(&empty);
        assert!(d.setting.is_none());
        assert!(d.note.starts_with("No web build yet"), "{}", d.note);
    }

    #[test]
    fn setting_keeps_the_rest_of_project_json_and_stays_in_git() {
        let root = scratch("setting");
        write(&root, ".wundur/project.json", r#"{"kind":"game","engine":"phaser","dimension":"2d"}"#);
        // Another tool's .gitignore that would hide project.json.
        write(&root, ".wundur/.gitignore", "*\n");
        let link = PlayLink { build: "npm run build".into(), output: "dist".into(), threads: false };
        save_setting(&root, &link).unwrap();
        assert_eq!(setting(&root), Some(link));
        let info = crate::engines::project_info(&root).unwrap();
        assert_eq!(info.engine, "phaser");
        let ignore = std::fs::read_to_string(root.join(".wundur/.gitignore")).unwrap();
        assert_eq!(ignore, "*\n!.gitignore\n!project.json\n");
        let text = std::fs::read_to_string(root.join(".wundur/project.json")).unwrap();
        assert!(!text.contains("threads"), "{text}");
    }

    #[test]
    fn record_round_trips_and_is_kept_out_of_git() {
        let root = scratch("record");
        assert!(record(&root).is_none());
        let r = ShareRecord { id: "k7x2m9ab".into(), url: "https://gummy-dash-k7x2m9ab.wundur.games".into(), title: "Gummy Dash".into() };
        save_record(&root, &r).unwrap();
        assert_eq!(record(&root), Some(r));
        assert_eq!(std::fs::read_to_string(root.join(".wundur/.gitignore")).unwrap(), GITIGNORE);
        forget_record(&root);
        assert!(record(&root).is_none());
    }

    #[test]
    fn worktree_records_live_in_the_main_checkout() {
        let root = scratch("git");
        let git = |dir: &Path, args: &[&str]| {
            let o = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
        git(&root, &["worktree", "add", "-q", ".worktrees/task", "-b", "task"]);
        let main = std::fs::canonicalize(&root).unwrap();
        assert_eq!(std::fs::canonicalize(record_root(&root.join(".worktrees/task"))).unwrap(), main);
        assert_eq!(std::fs::canonicalize(record_root(&root)).unwrap(), main);
        let plain = scratch("no-git");
        assert_eq!(record_root(&plain), plain);
    }

    #[test]
    fn scan_skips_hidden_and_node_modules_and_needs_index_html() {
        let root = scratch("scan");
        assert!(scan(&root).unwrap_err().contains("no index.html"));
        write(&root, "index.html", "<script src=\"./game.js\"></script>");
        write(&root, "game.js", "12345");
        write(&root, "Build/a.wasm.br", "xy");
        write(&root, ".git/HEAD", "x");
        write(&root, ".DS_Store", "x");
        write(&root, "node_modules/x/index.js", "x");
        write(&root, "Game_BurstDebugInformation_DoNotShip/Data/x.txt", "x");
        write(&root, "Game_BackUpThisFolder_ButDontShipItWithYourGame/x.cpp", "x");
        let b = scan(&root).unwrap();
        let paths: Vec<&str> = b.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Build/a.wasm.br", "game.js", "index.html"]);
        assert_eq!(b.bytes, 2 + 5 + "<script src=\"./game.js\"></script>".len() as u64);
        assert!(b.warnings.is_empty());
    }

    #[test]
    fn root_paths_are_warned_about() {
        let html = r#"<link href="/assets/a.css"><script type="module" src='/assets/b.js'></script><script src="//cdn.x/y.js"></script><img src="./ok.png"><a href="https://x">"#;
        assert_eq!(root_paths(html), ["/assets/b.js", "/assets/a.css"]);
        let root = scratch("scan-root");
        write(&root, "index.html", html);
        let b = scan(&root).unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert!(b.warnings[0].contains("base: './'"), "{}", b.warnings[0]);
    }

    #[test]
    fn messages_are_the_servers_with_what_to_do() {
        let api = |code: &str, msg: &str| Error::Api(account::ApiError { status: 409, code: code.into(), message: msg.into(), retry_after: None });
        assert_eq!(message(&api("invalid_files", "No index.html at the top level.")), "No index.html at the top level.");
        assert!(message(&api("too_many_shares", "You can have 10 active shares; revoke one first.")).ends_with("Turn off an old link first."));
        assert!(message(&api("shares_unavailable", "Share links are not available right now.")).starts_with("Sharing isn't available right now."));
        assert!(is_gone(api("gone", "").code()));
        assert!(!is_gone(api("files_missing", "").code()));
    }

    #[test]
    fn view_reads_the_api_shape() {
        let v: ShareView = serde_json::from_str(
            r#"{"id":"k7x2m9ab","title":"Gummy Dash","slug":"gummy-dash","url":"https://gummy-dash-k7x2m9ab.wundur.games","status":"live","version":2,"files":41,"bytes":18234567,"created_at":"2026-11-20T10:00:00+00:00","published_at":"2026-11-20T10:00:00+00:00","expires_at":"2026-11-27T10:00:00+00:00","revivable_until":"2026-12-11T10:00:00+00:00","plays_total":12,"players_today":3,"warning":"expires_soon","isolated":true}"#,
        )
        .unwrap();
        assert_eq!((v.plays_total, v.players_today, v.warning.as_deref(), v.isolated), (12, 3, Some("expires_soon"), true));
        assert!(!v.gone());
    }
}
