//! Game engines for the New Project wizard: what each is for, what it needs on this machine, and
//! the starter project it makes.
//!
//! An engine comes from a pack: `pack.json` has an `engine` section ([`EngineSpec`]) and the
//! pack's folder has its card image and a `template/` folder, split into `common/` (always),
//! `2d/` and `3d/` (for that kind of game). Text files in it may say `{{name}}`, `{{slug}}`,
//! `{{dimension}}` and `{{unity_version}}`. Wundur carries four ([`builtin`]): Three.js, Phaser,
//! Godot and Unity, kept as packs in `packs/engines/` and built into the app; an installed pack
//! with the same id takes the place of one.
//!
//! A project remembers what it was made as in `.wundur/project.json` ([`ProjectInfo`]), so tools
//! that fit only some projects (sprite packing for 2D Phaser and Godot games) know where they
//! belong.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 2D or 3D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dimension {
    #[serde(rename = "2d")]
    TwoD,
    #[serde(rename = "3d")]
    ThreeD,
}

impl Dimension {
    pub fn id(self) -> &'static str {
        match self {
            Dimension::TwoD => "2d",
            Dimension::ThreeD => "3d",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dimension::TwoD => "2D",
            Dimension::ThreeD => "3D",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "2d" => Some(Dimension::TwoD),
            "3d" => Some(Dimension::ThreeD),
            _ => None,
        }
    }
}

/// Something an engine needs on the machine (an editor, Node.js), how to find it, and where to
/// get it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Requirement {
    pub label: String,
    /// Programs on the PATH (or where installers put them) that count.
    pub programs: Vec<String>,
    /// Paths that count; `~` is the home folder and `*` matches within one part of the path.
    pub paths: Vec<String>,
    /// A search Wundur does itself: `unity-editor` (the editors Unity Hub installed).
    pub probe: String,
    /// Where to get it.
    pub url: String,
    pub help: String,
    /// The project can't be made without it (the engine is shown greyed out).
    pub blocking: bool,
}

/// The `engine` section of a pack's manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSpec {
    pub name: String,
    /// A few words under its name ("2D games in the browser").
    pub tagline: String,
    /// A sentence or two on what it is best at.
    pub description: String,
    /// Its card's picture, in the pack's folder.
    pub image: String,
    /// Where it sits among the others (lowest first).
    pub order: i32,
    /// The kinds of game it makes, in the order offered.
    pub dimensions: Vec<Dimension>,
    pub requires: Vec<Requirement>,
    /// How to run the new project, as the person reads it.
    pub run: String,
    /// A maker built into Wundur that runs before the template: `unity` (the editor makes the
    /// project). Empty: the template is all.
    pub create: String,
    /// Sprite sheet tools fit its 2D projects.
    pub sprite_tools: bool,
}

/// Where an engine's files are.
#[derive(Clone, Debug)]
enum Files {
    /// Built into Wundur: each file's path in the pack and its bytes.
    BuiltIn(&'static [(&'static str, &'static [u8])]),
    /// An installed pack's folder.
    Folder(PathBuf),
}

/// An engine the wizard offers.
#[derive(Clone, Debug)]
pub struct Engine {
    /// Its pack's id.
    pub id: String,
    pub spec: EngineSpec,
    files: Files,
}

impl Engine {
    /// Its card's picture, if it has one.
    pub fn image(&self) -> Option<Cow<'static, [u8]>> {
        if self.spec.image.is_empty() {
            return None;
        }
        self.file(&self.spec.image)
    }

    /// The file `path` in its pack.
    fn file(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        match &self.files {
            Files::BuiltIn(files) => files.iter().find(|(p, _)| *p == path).map(|(_, b)| Cow::Borrowed(*b)),
            Files::Folder(dir) => crate::packs::inside(dir, path).ok().and_then(|p| std::fs::read(p).ok()).map(Cow::Owned),
        }
    }

    /// Its template's files for a `dimension` game: their paths in the project and bytes
    /// (`common/` first, then the dimension's, which wins where both have a file).
    fn template(&self, dimension: Option<Dimension>) -> Vec<(String, Cow<'static, [u8]>)> {
        let parts: Vec<String> = std::iter::once("common".to_string()).chain(dimension.map(|d| d.id().to_string())).collect();
        let mut out: Vec<(String, Cow<'static, [u8]>)> = Vec::new();
        for part in parts {
            let prefix = format!("template/{part}/");
            let found: Vec<(String, Cow<'static, [u8]>)> = match &self.files {
                Files::BuiltIn(files) => files
                    .iter()
                    .filter_map(|(p, b)| p.strip_prefix(&prefix).map(|rel| (rel.to_string(), Cow::Borrowed(*b))))
                    .collect(),
                Files::Folder(dir) => {
                    let root = dir.join("template").join(&part);
                    walk(&root)
                        .into_iter()
                        .filter_map(|p| {
                            let rel = p.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                            Some((rel, Cow::Owned(std::fs::read(&p).ok()?)))
                        })
                        .collect()
                }
            };
            for (rel, bytes) in found {
                out.retain(|(r, _)| *r != rel);
                out.push((rel, bytes));
            }
        }
        out
    }
}

/// Every file under `dir`, skipping macOS's `._*` and `.DS_Store` litter.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("._") || name == ".DS_Store" {
            continue;
        }
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A built-in engine's files, from `packs/engines/<engine>/`.
macro_rules! pack_files {
    ($engine:literal; $($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packs/engines/", $engine, "/", $path)) as &[u8])),*]
    };
}

const THREEJS: &[(&str, &[u8])] = pack_files!("threejs";
    "pack.json", "card.jpg",
    "template/common/package.json", "template/common/index.html", "template/common/src/main.js",
    "template/common/vite.config.js", "template/common/.gitignore", "template/common/AGENTS.md",
);
const PHASER: &[(&str, &[u8])] = pack_files!("phaser";
    "pack.json", "card.jpg",
    "template/common/package.json", "template/common/index.html", "template/common/src/main.js",
    "template/common/vite.config.js", "template/common/.gitignore", "template/common/AGENTS.md",
);
const GODOT: &[(&str, &[u8])] = pack_files!("godot";
    "pack.json", "card.jpg",
    "template/common/project.godot", "template/common/.gitignore", "template/common/.gitattributes",
    "template/common/AGENTS.md",
    "template/2d/main.tscn", "template/2d/main.gd",
    "template/3d/main.tscn", "template/3d/main.gd",
);
const UNITY: &[(&str, &[u8])] = pack_files!("unity";
    "pack.json", "card.jpg",
    "template/common/.gitignore", "template/common/AGENTS.md",
);

/// The engines Wundur carries.
pub fn builtin() -> Vec<Engine> {
    [THREEJS, PHASER, GODOT, UNITY]
        .into_iter()
        .filter_map(|files| {
            let (_, json) = files.iter().find(|(p, _)| *p == "pack.json")?;
            let manifest: crate::packs::Manifest = serde_json::from_slice(json).ok()?;
            Some(Engine { id: manifest.id, spec: manifest.engine?, files: Files::BuiltIn(files) })
        })
        .collect()
}

/// The engines on offer: Wundur's own and the installed packs' (`packs`), in order. An installed
/// pack takes the place of a built-in one with its id.
pub fn all(packs: &[crate::packs::Pack]) -> Vec<Engine> {
    let mut out = builtin();
    for pack in packs {
        let Some(spec) = pack.manifest.engine.clone() else { continue };
        out.retain(|e| e.id != pack.manifest.id);
        out.push(Engine { id: pack.manifest.id.clone(), spec, files: Files::Folder(pack.dir.clone()) });
    }
    out.sort_by(|a, b| a.spec.order.cmp(&b.spec.order).then_with(|| a.spec.name.cmp(&b.spec.name)));
    out
}

/// What a check of one [`Requirement`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub requirement: Requirement,
    /// Where it was found (none: not on this machine).
    pub found: Option<PathBuf>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.found.is_some()
    }
}

/// Look for what `engine` needs on this machine. Blocking (reads folders): run it off the UI
/// thread.
pub fn check(engine: &Engine) -> Vec<Check> {
    engine.spec.requires.iter().map(|r| Check { requirement: r.clone(), found: find(r) }).collect()
}

/// Whether the checks leave the engine unusable (something it can't do without is missing).
pub fn blocked(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.requirement.blocking && !c.ok())
}

fn find(r: &Requirement) -> Option<PathBuf> {
    if r.probe == "unity-editor" {
        return unity_editors().pop().map(|(_, path)| path);
    }
    r.programs
        .iter()
        .find_map(|p| crate::host::find_program(p))
        .or_else(|| r.paths.iter().find_map(|p| expand(p).into_iter().next()))
}

/// The paths matching `pattern` (`~` the home folder, `*` within one part), sorted.
pub fn expand(pattern: &str) -> Vec<PathBuf> {
    let pattern = match pattern.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest).to_string_lossy().to_string(),
            None => return Vec::new(),
        },
        None => pattern.to_string(),
    };
    let mut found = vec![PathBuf::from(if pattern.starts_with('/') { "/" } else { "." })];
    for part in pattern.split('/').filter(|p| !p.is_empty()) {
        let mut next = Vec::new();
        for base in &found {
            if part.contains('*') {
                let Ok(entries) = std::fs::read_dir(base) else { continue };
                let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
                names.sort();
                next.extend(names.into_iter().filter(|n| !n.starts_with('.') && wildcard(part, n)).map(|n| base.join(n)));
            } else {
                let p = base.join(part);
                if p.exists() {
                    next.push(p);
                }
            }
        }
        found = next;
    }
    found
}

/// Whether `name` matches `pattern`, where `*` stands for any run of characters.
fn wildcard(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else { return false };
    let parts: Vec<&str> = parts.collect();
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            return rest.len() >= part.len() && rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.is_empty()
}

/// The Unity editors installed through Unity Hub: their versions and the editor program, oldest
/// first (by version). Only editors whose program is there count (a cancelled install leaves an
/// empty folder).
pub fn unity_editors() -> Vec<(String, PathBuf)> {
    let mut roots = vec![PathBuf::from("/Applications/Unity/Hub/Editor")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications/Unity/Hub/Editor"));
        // Unity Hub's own setting for where it installs editors.
        let custom = home.join("Library/Application Support/UnityHub/secondaryInstallPath.json");
        if let Some(path) = std::fs::read_to_string(custom).ok().and_then(|s| serde_json::from_str::<String>(&s).ok()).filter(|s| !s.trim().is_empty()) {
            roots.push(PathBuf::from(path));
        }
    }
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            let version = e.file_name().to_string_lossy().to_string();
            let program = e.path().join("Unity.app/Contents/MacOS/Unity");
            if program.is_file() && !out.iter().any(|(v, _)| *v == version) {
                out.push((version, program));
            }
        }
    }
    out.sort_by_key(|(v, _)| version_key(v));
    out
}

/// A Unity version (`6000.5.2f1`) as numbers to sort by.
fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect()
}

/// What a project was made as, kept in its `.wundur/project.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectInfo {
    /// `game` or `other`.
    pub kind: String,
    /// The engine's pack id, for a game.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub engine: String,
    /// `2d` or `3d`, for a game.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dimension: String,
    /// Sprite sheet tools are offered in it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sprite_tools: bool,
}

/// Where a project's [`ProjectInfo`] is.
pub const PROJECT_INFO: &str = ".wundur/project.json";

/// What the project in `root` was made as, if Wundur made it.
pub fn project_info(root: &Path) -> Option<ProjectInfo> {
    serde_json::from_str(&std::fs::read_to_string(root.join(PROJECT_INFO)).ok()?).ok()
}

/// What to make.
#[derive(Clone, Debug)]
pub struct NewProject {
    pub name: String,
    /// The folder to make it in (it gets a folder of its own there, named after it).
    pub parent: PathBuf,
    /// The engine and kind of game, for a game; none for other work.
    pub game: Option<(Engine, Dimension)>,
}

/// What [`create`] made.
#[derive(Clone, Debug)]
pub struct Created {
    pub folder: PathBuf,
    /// Things that didn't go to plan but didn't stop it (no git, no first commit).
    pub notes: Vec<String>,
}

/// Where new projects go unless the person picks another folder: their setting, else
/// `~/Wundur Projects` (made when the first project goes there).
pub fn projects_folder(setting: Option<&Path>) -> PathBuf {
    match setting {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(DEFAULT_PROJECTS_FOLDER),
    }
}

/// The default projects folder's name, in the home folder.
pub const DEFAULT_PROJECTS_FOLDER: &str = "Wundur Projects";

/// The folder name for a project called `name`: the name, less what a folder name can't have
/// and what trips build tools up in a path (Vite fails on a quote in one).
pub fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '?' | '*' | '$'))
        .map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c })
        .collect();
    cleaned.trim_start_matches('.').trim().to_string()
}

/// A package name for a project called `name` (`my-game`): lowercase letters, digits and
/// hyphens.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "game".into() } else { out }
}

/// The folder [`create`] would make, and why it can't, if it can't.
pub fn target(new: &NewProject) -> Result<PathBuf, String> {
    let folder = folder_name(&new.name);
    if folder.is_empty() {
        return Err("Give it a name.".into());
    }
    if !new.parent.is_dir() {
        return Err(format!("{} isn't a folder.", new.parent.display()));
    }
    let path = new.parent.join(&folder);
    if path.exists() {
        let empty = std::fs::read_dir(&path).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !empty {
            return Err(format!("There is already a {folder} folder there."));
        }
    }
    Ok(path)
}

/// Make the project: its folder, the engine's starter (for a game), `.wundur/project.json`, and
/// a git repository with a first commit. Blocking (Unity's editor takes a minute or two): run it
/// off the UI thread. `progress` hears what it is doing.
pub fn create(new: &NewProject, progress: &dyn Fn(&str)) -> Result<Created, String> {
    let folder = target(new)?;
    let mut notes = Vec::new();
    let mut unity_version = String::new();
    if let Some((engine, dimension)) = &new.game
        && engine.spec.create == "unity"
    {
        let (version, editor) = unity_editors().pop().ok_or("Unity isn't installed on this Mac.")?;
        progress(&format!("Unity {version} is making the project (a minute or two)…"));
        make_unity_project(&editor, &folder, *dimension)?;
        unity_version = version;
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("Couldn't make {}: {e}", folder.display()))?;
    let mut info = ProjectInfo { kind: "other".into(), ..Default::default() };
    if let Some((engine, dimension)) = &new.game {
        progress(&format!("Writing the {} starter…", engine.spec.name));
        let name = safe_name(&new.name);
        let words = [
            ("{{name}}", name.as_str()),
            ("{{slug}}", &slug(&new.name)),
            ("{{dimension}}", dimension.label()),
            ("{{unity_version}}", &unity_version),
        ];
        for (rel, bytes) in engine.template(Some(*dimension)) {
            let out = crate::packs::inside(&folder, &rel)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let bytes = match std::str::from_utf8(&bytes) {
                Ok(text) => words.iter().fold(text.to_string(), |t, (k, v)| t.replace(k, v)).into_bytes(),
                Err(_) => bytes.into_owned(),
            };
            std::fs::write(&out, bytes).map_err(|e| format!("Couldn't write {rel}: {e}"))?;
        }
        info = ProjectInfo {
            kind: "game".into(),
            engine: engine.id.clone(),
            dimension: dimension.id().into(),
            sprite_tools: engine.spec.sprite_tools && *dimension == Dimension::TwoD,
        };
    }
    write_info(&folder, &info)?;
    progress("Starting git…");
    notes.extend(git_start(&folder, &new.name));
    Ok(Created { folder, notes })
}

/// The name as it goes into a template's files: without what would break a quoted string.
fn safe_name(name: &str) -> String {
    name.trim().chars().filter(|c| !matches!(c, '"' | '\\' | '`' | '<' | '>') && !c.is_control()).collect()
}

/// Write `.wundur/project.json`, and keep the rest of `.wundur/` (Wundur's scratch files) out of
/// git.
fn write_info(folder: &Path, info: &ProjectInfo) -> Result<(), String> {
    let own = folder.join(".wundur");
    std::fs::create_dir_all(&own).map_err(|e| e.to_string())?;
    std::fs::write(own.join(".gitignore"), "*\n!.gitignore\n!project.json\n").map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(info).map_err(|e| e.to_string())?;
    std::fs::write(folder.join(PROJECT_INFO), json + "\n").map_err(|e| e.to_string())
}

/// `git init` and a first commit of everything; what went wrong, if anything (the project is
/// made either way).
fn git_start(folder: &Path, name: &str) -> Vec<String> {
    let git = |args: &[&str]| crate::platform::command("git").args(args).current_dir(folder).env("GIT_TERMINAL_PROMPT", "0").output();
    match git(&["init", "-q", "-b", "main"]) {
        Ok(o) if o.status.success() => {}
        Ok(o) => return vec![format!("git init failed: {}", String::from_utf8_lossy(&o.stderr).trim())],
        Err(_) => return vec!["git isn't installed, so the project has no repository yet (xcode-select --install adds it).".into()],
    }
    let _ = git(&["add", "-A"]);
    match git(&["commit", "-q", "-m", &format!("Start {}", safe_name(name))]) {
        Ok(o) if o.status.success() => Vec::new(),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let why = if err.contains("user.email") || err.contains("identity") {
                "git doesn't know your name and email yet (git config --global user.name / user.email)".to_string()
            } else {
                err.trim().lines().last().unwrap_or("it failed").to_string()
            };
            vec![format!("The repository is made, but not its first commit: {why}.")]
        }
        Err(e) => vec![format!("The first commit failed: {e}")],
    }
}

/// Have Unity's `editor` make a project in `folder`, from its 2D or 3D template when it has one.
fn make_unity_project(editor: &Path, folder: &Path, dimension: Dimension) -> Result<(), String> {
    let mut cmd = crate::platform::command(editor);
    cmd.args(["-batchmode", "-quit", "-createProject"]).arg(folder);
    if let Some(template) = unity_template(editor, dimension) {
        cmd.arg("-cloneFromTemplate").arg(template);
    }
    let log = std::env::temp_dir().join(format!("wundur-unity-create-{}.log", std::process::id()));
    cmd.arg("-logFile").arg(&log);
    let status = cmd.status().map_err(|e| format!("Couldn't start Unity: {e}"))?;
    let made = folder.join("Assets").is_dir() && folder.join("ProjectSettings").is_dir();
    if !status.success() || !made {
        let tail: String = std::fs::read_to_string(&log)
            .map(|l| l.lines().rev().filter(|l| !l.trim().is_empty()).take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        return Err(format!("Unity couldn't make the project (its log: {}).\n{tail}", log.display()));
    }
    let _ = std::fs::remove_file(&log);
    Ok(())
}

/// The editor's own project template for a `dimension` game (`com.unity.template.2d-…`), if it
/// has one; the newest when it has several.
fn unity_template(editor: &Path, dimension: Dimension) -> Option<PathBuf> {
    match dimension {
        Dimension::TwoD => unity_template_named(editor, "com.unity.template.2d"),
        Dimension::ThreeD => unity_template_named(editor, "com.unity.template.3d-cross-platform").or_else(|| unity_template_named(editor, "com.unity.template.3d")),
    }
}

/// The newest of the editor's templates whose file name starts with `prefix`.
fn unity_template_named(editor: &Path, prefix: &str) -> Option<PathBuf> {
    // `<Unity.app>/Contents/MacOS/Unity` → `<Unity.app>/Contents/Resources/PackageManager/ProjectTemplates`.
    let dir = editor.parent()?.parent()?.join("Resources/PackageManager/ProjectTemplates");
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix) && n.to_string_lossy().ends_with(".tgz")))
        .collect();
    found.sort();
    found.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty scratch folder of its own for a test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wundur-engines-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn engine(id: &str) -> Engine {
        builtin().into_iter().find(|e| e.id == id).unwrap_or_else(|| panic!("no {id}"))
    }

    #[test]
    fn four_engines_are_built_in_in_order() {
        let names: Vec<String> = all(&[]).into_iter().map(|e| e.spec.name).collect();
        assert_eq!(names, ["Three.js", "Phaser", "Godot", "Unity"]);
        for e in builtin() {
            assert!(e.image().is_some_and(|b| b.len() > 10_000), "{} has no card", e.id);
            assert!(!e.spec.dimensions.is_empty(), "{}", e.id);
        }
    }

    #[test]
    fn dimensions_fit_each_engine() {
        assert_eq!(engine("wundur/engine-threejs").spec.dimensions, [Dimension::ThreeD]);
        assert_eq!(engine("wundur/engine-phaser").spec.dimensions, [Dimension::TwoD]);
        assert_eq!(engine("wundur/engine-godot").spec.dimensions, [Dimension::TwoD, Dimension::ThreeD]);
        assert_eq!(engine("wundur/engine-unity").spec.dimensions, [Dimension::TwoD, Dimension::ThreeD]);
        // Sprite tools: Phaser and Godot only (Unity packs its own; Three.js is 3D).
        let tools: Vec<String> = builtin().into_iter().filter(|e| e.spec.sprite_tools).map(|e| e.id).collect();
        assert_eq!(tools, ["wundur/engine-phaser", "wundur/engine-godot"]);
    }

    #[test]
    fn only_unity_blocks_without_its_tool() {
        for e in builtin() {
            let blocking = e.spec.requires.iter().any(|r| r.blocking);
            assert_eq!(blocking, e.id == "wundur/engine-unity", "{}", e.id);
            assert!(e.spec.requires.iter().all(|r| r.url.starts_with("https://")), "{}", e.id);
        }
    }

    #[test]
    fn template_takes_the_dimension_files() {
        let godot = engine("wundur/engine-godot");
        let files = |d| godot.template(Some(d)).into_iter().map(|(p, _)| p).collect::<Vec<_>>();
        let two = files(Dimension::TwoD);
        assert!(two.contains(&"project.godot".to_string()) && two.contains(&"main.tscn".to_string()));
        let main = godot.template(Some(Dimension::ThreeD)).into_iter().find(|(p, _)| p == "main.tscn").unwrap().1;
        assert!(String::from_utf8_lossy(&main).contains("Camera3D"));
    }

    #[test]
    fn names_make_folders_and_slugs() {
        assert_eq!(folder_name("  My Game "), "My Game");
        assert_eq!(folder_name("a/b:c"), "a-b-c");
        assert_eq!(folder_name(".hidden"), "hidden");
        assert_eq!(folder_name("Sky \"Hop\" $1?"), "Sky Hop 1");
        assert_eq!(slug("My Cool Game!"), "my-cool-game");
        assert_eq!(slug("???"), "game");
        assert_eq!(safe_name("A \"quoted\" <name>"), "A quoted name");
    }

    #[test]
    fn projects_go_in_wundur_projects_unless_set() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(projects_folder(None), home.join("Wundur Projects"));
        assert_eq!(projects_folder(Some(Path::new(""))), home.join("Wundur Projects"));
        assert_eq!(projects_folder(Some(Path::new("/tmp/games"))), PathBuf::from("/tmp/games"));
    }

    #[test]
    fn wildcards_match_within_a_part() {
        assert!(wildcard("Godot*.app", "Godot.app"));
        assert!(wildcard("Godot*.app", "Godot_v4.5-stable.app"));
        assert!(!wildcard("Godot*.app", "Godot.zip"));
        assert!(wildcard("*", "anything"));
        assert!(wildcard("a*b*c", "aXbYc"));
        assert!(!wildcard("a*b*c", "aXbY"));
    }

    #[test]
    fn expand_finds_matching_paths() {
        let dir = scratch("expand");
        for v in ["Godot.app", "Godot_mono.app", "Other.app"] {
            std::fs::create_dir(dir.join(v)).unwrap();
        }
        let found = expand(&format!("{}/Godot*.app", dir.display()));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn unity_versions_sort_by_number() {
        let mut v = vec!["6000.10.0f1", "6000.5.2f1", "2022.3.9f1"];
        v.sort_by_key(|s| version_key(s));
        assert_eq!(v, ["2022.3.9f1", "6000.5.2f1", "6000.10.0f1"]);
    }

    #[test]
    fn creates_a_phaser_game_with_git() {
        let dir = scratch("phaser");
        let new = NewProject { name: "Sky \"Hop\"".into(), parent: dir.clone(), game: Some((engine("wundur/engine-phaser"), Dimension::TwoD)) };
        let made = create(&new, &|_| {}).unwrap();
        assert_eq!(made.folder, dir.join("Sky Hop"));
        let pkg = std::fs::read_to_string(made.folder.join("package.json")).unwrap();
        assert!(pkg.contains("\"name\": \"sky-hop\""), "{pkg}");
        assert!(std::fs::read_to_string(made.folder.join("index.html")).unwrap().contains("<title>Sky Hop</title>"));
        let info = project_info(&made.folder).unwrap();
        assert_eq!((info.kind.as_str(), info.dimension.as_str(), info.sprite_tools), ("game", "2d", true));
        assert!(made.folder.join(".git").is_dir());
        // A second one with the same name doesn't overwrite it.
        assert!(create(&new, &|_| {}).is_err());
    }

    #[test]
    fn godot_3d_has_no_sprite_tools_and_other_work_is_bare() {
        let dir = scratch("godot");
        let new = NewProject { name: "Cube".into(), parent: dir.clone(), game: Some((engine("wundur/engine-godot"), Dimension::ThreeD)) };
        let made = create(&new, &|_| {}).unwrap();
        assert!(std::fs::read_to_string(made.folder.join("project.godot")).unwrap().contains("config/name=\"Cube\""));
        assert!(!project_info(&made.folder).unwrap().sprite_tools);

        let new = NewProject { name: "Notes".into(), parent: dir.clone(), game: None };
        let made = create(&new, &|_| {}).unwrap();
        assert_eq!(project_info(&made.folder).unwrap().kind, "other");
        assert!(made.folder.join(".git").is_dir());
    }
}
