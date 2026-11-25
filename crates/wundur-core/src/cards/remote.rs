//! Cards and the person's Wundur account: which workflows the service offers and their
//! manifests (kept on disk, so the Create menu is there before the service answers), a card's
//! calls and saves carried out, and each card's record, which brings it back in its chat.
//!
//! Every call here blocks: run them off the UI thread.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::manifest::{Manifest, SCHEMA};
use super::state::{Failure, Request};
use crate::account::{self, Auth};
use crate::assets::Origin;

/// How long a call may take: the service waits out its providers itself, so slow is normal.
const WAIT: Duration = Duration::from_secs(200);

/// A workflow as the Create menu lists it (`GET /v1/workflows`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Listed {
    pub id: String,
    /// Changes whenever the manifest does.
    pub version: String,
    pub schema: u64,
    pub label: String,
    pub icon: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct List {
    workflows: Vec<Listed>,
}

/// Where the list and the manifests are kept between runs.
fn kept() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("workflows"))
}

/// A workflow's id as a file name: ids are plain names, and anything else isn't kept.
fn plain(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The list as last fetched, for the menu before the service answers.
pub fn kept_list() -> Vec<Listed> {
    kept().and_then(|d| std::fs::read_to_string(d.join("list.json")).ok()).and_then(|t| serde_json::from_str::<List>(&t).ok()).map(|l| l.workflows).unwrap_or_default()
}

/// The manifest kept for `id`, if it's the version wanted (any version when `None`).
pub fn kept_manifest(id: &str, version: Option<&str>) -> Option<Manifest> {
    if !plain(id) {
        return None;
    }
    let text = std::fs::read_to_string(kept()?.join(format!("{id}.manifest.json"))).ok()?;
    let manifest = Manifest::parse(&text).ok()?;
    version.is_none_or(|v| manifest.version == v).then_some(manifest)
}

/// Forget what was kept (signed out: the list is the account's).
pub fn forget() {
    if let Some(dir) = kept() {
        let _ = std::fs::remove_file(dir.join("list.json"));
    }
}

/// The workflows the account may use, in the menu's order, each with its manifest: asked of
/// the service, and kept. One whose manifest this app can't read or draw is left out (its
/// format is newer, or it uses a component this app doesn't have).
pub fn fetch() -> Result<Vec<(Listed, Manifest)>, account::Error> {
    let text = account::shared().request(Auth::Key, "GET", "/v1/workflows", None, Duration::from_secs(30), None)?;
    let list: List = serde_json::from_str(&text).map_err(|e| account::Error::Network(format!("an answer that couldn't be read ({e})")))?;
    let mut offered = Vec::new();
    for listed in list.workflows.iter().filter(|w| plain(&w.id) && w.schema <= SCHEMA) {
        let manifest = match kept_manifest(&listed.id, Some(&listed.version)) {
            Some(m) => m,
            None => {
                let Ok(text) = account::shared().request(Auth::Key, "GET", &format!("/v1/workflows/{}", listed.id), None, Duration::from_secs(30), None) else { continue };
                let Ok(manifest) = Manifest::parse(&text) else { continue };
                if let Some(dir) = kept() {
                    let _ = crate::save::whole(&dir.join(format!("{}.manifest.json", listed.id)), &text);
                }
                manifest
            }
        };
        if manifest.id == listed.id && manifest.supported() {
            offered.push((listed.clone(), manifest));
        }
    }
    if let Some(dir) = kept() {
        let kept = json!({ "workflows": offered.iter().map(|(l, _)| l).collect::<Vec<_>>() });
        let _ = crate::save::whole(&dir.join("list.json"), kept.to_string());
    }
    Ok(offered)
}

/// The account as a card reads it (`account`): `GET /v1/me` as the service sends it, with
/// `signed_in`.
pub fn account_scope() -> Value {
    if !account::shared().is_signed_in() {
        return json!({ "signed_in": false });
    }
    let mut me = account::shared().request(Auth::Key, "GET", "/v1/me", None, Duration::from_secs(30), None).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    me["signed_in"] = json!(true);
    me
}

// ----- a card's calls -----

/// A failure worded for the person, with the service's code for the manifest to act on.
fn failure(e: &account::Error) -> Failure {
    Failure { message: crate::asset_flow::explain(e).0, code: e.code().unwrap_or("network").to_string() }
}

/// Whether a card may ask for `path`: one of the service's own, under its base. A manifest
/// can't send the person's key anywhere else.
pub fn allowed(path: &str) -> bool {
    path.starts_with("/v1/") && !path.contains("..") && !path.contains("//") && !path.contains(['\\', '@', '#', ' ', '\n', '\r'])
}

/// Carry out a card's call: the files it names are read into the body as base64, a call that
/// spends carries an idempotency key, and the answer is its JSON (nothing, for an empty one).
pub fn perform(request: &Request) -> Result<Value, Failure> {
    if !allowed(&request.path) {
        return Err(Failure { message: "This card asked for something outside Wundur's service; it wasn't sent.".into(), code: "not_allowed".into() });
    }
    let mut body = request.body.clone();
    for (key, paths) in &request.files {
        let mut read = Vec::new();
        for path in paths {
            read.push(crate::asset_flow::reference(Path::new(path)).map_err(|message| Failure { message, code: "bad_file".into() })?);
        }
        let body = body.get_or_insert_with(|| json!({}));
        if body.is_object() {
            body[key.as_str()] = json!(read);
        }
    }
    let key = request.spends.then(crate::asset_flow::idempotency_key);
    let text = account::shared().request(Auth::Key, &request.method, &request.path, body.as_ref(), WAIT, key.as_deref()).map_err(|e| failure(&e))?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|_| Failure { message: "Wundur's server sent an answer this version can't read. Updating Wundur Studio usually fixes it.".into(), code: "unreadable".into() })
}

/// A name for a saved file: the last part of what was asked for, with nothing that could
/// take it out of its folder.
fn file_name(wanted: &str) -> String {
    let last = wanted.split(['/', '\\']).next_back().unwrap_or_default().split(['?', '#']).next().unwrap_or_default();
    let clean: String = last.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '"' | '<' | '>' | '|')).collect();
    let clean = clean.trim_matches(['.', ' ']).to_string();
    if clean.is_empty() { "file".to_string() } else { clean }
}

/// A folder under the project, as a manifest names it: relative, and never above the project.
fn folder_in(project: &Path, folder: &str) -> Option<PathBuf> {
    let folder = Path::new(folder);
    folder.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)).then(|| project.join(folder))
}

/// A path in `dir` for `name` that nothing has yet: `coin.png`, then `coin-2.png`.
fn free(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    };
    let mut path = dir.join(name);
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}{ext}"));
        n += 1;
    }
    path
}

/// Carry out a card's save: each file (a link, or an object with a `url` and a `name`)
/// downloaded into `folder` under the project, under a name not already there. The answer is
/// the saved paths, relative to the project, in order.
pub fn save(files: &Value, folder: &str, project: &Path) -> Result<Value, Failure> {
    let fail = |message: String| Failure { message, code: "save".into() };
    let dir = folder_in(project, folder).ok_or_else(|| fail("This card asked to save outside the project; nothing was saved.".into()))?;
    let mut saved = Vec::new();
    for file in files.as_array().into_iter().flatten() {
        let Some(url) = file.as_str().or_else(|| file.get("url").and_then(Value::as_str)).filter(|u| !u.is_empty()) else { continue };
        let name = file_name(file.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()).unwrap_or(url));
        let path = free(&dir, &name);
        crate::asset_flow::download(url, &path).map_err(|e| fail(format!("It was made but couldn't be saved: {e}")))?;
        saved.push(json!(path.strip_prefix(project).unwrap_or(&path).to_string_lossy()));
    }
    if saved.is_empty() {
        return Err(fail("Wundur's server sent nothing to save.".into()));
    }
    Ok(Value::Array(saved))
}

/// Fetch a link a card wants opened or played to a file of its own (kept with the app, not in
/// the project), and give its path. A link already fetched is used as it is.
pub fn fetched(url: &str) -> Result<PathBuf, String> {
    let dir = crate::settings::config_dir().ok_or("no folder to keep it in")?.join("card-files");
    // The link without its query (a signature that changes) names the file, so one model
    // opened twice is fetched once.
    let plain = url.split(['?', '#']).next().unwrap_or(url);
    let hash = plain.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let path = dir.join(format!("{hash:016x}-{}", file_name(plain)));
    if !path.is_file() {
        crate::asset_flow::download(url, &path)?;
    }
    Ok(path)
}

// ----- a card's record -----

/// The record's file in a card's folder.
pub const RECORD: &str = "card.json";

/// What is kept of a card, so it comes back in its chat as it was.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// The workflow it is a card of, and the manifest version it was last drawn with.
    pub workflow: String,
    pub version: String,
    /// The project it was started in.
    pub project: PathBuf,
    pub form: Value,
    pub ui: Value,
    pub data: Value,
    /// The chat and the turn it was started in.
    pub origin: Option<Origin>,
}

/// Where cards' records are kept: with the app, not in the project (a card is the app's, and
/// what it makes is the project's).
fn records() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("cards"))
}

/// A folder for a new card of `workflow`.
pub fn new_folder(workflow: &str) -> Option<PathBuf> {
    let dir = records()?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let mut n = 0;
    loop {
        let path = dir.join(format!("{workflow}-{stamp}-{n}"));
        if !path.exists() {
            std::fs::create_dir_all(&path).ok()?;
            return Some(path);
        }
        n += 1;
    }
}

pub fn load(dir: &Path) -> Option<Record> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(RECORD)).ok()?).ok()
}

pub fn store(dir: &Path, record: &Record) -> std::io::Result<()> {
    crate::save::whole(&dir.join(RECORD), serde_json::to_string(record).map_err(std::io::Error::other)?)
}

/// Take a closed card's record away. Only a folder that holds one is removed.
pub fn discard(dir: &Path) {
    if records().is_some_and(|r| dir.starts_with(r)) && dir.join(RECORD).is_file() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The cards started in the chat with session id `session` in `project`, each with the turn
/// it was started in, to put them back when the chat is read in again.
pub fn started_in(project: &Path, session: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut found = Vec::new();
    let Some(Ok(dirs)) = records().map(std::fs::read_dir) else { return found };
    for dir in dirs.flatten().map(|d| d.path()) {
        if let Some(record) = load(&dir)
            && record.project == project
            && let Some(origin) = &record.origin
            && origin.session == session
        {
            found.push((dir, origin.turn.clone()));
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_may_only_ask_the_service_s_own_paths() {
        assert!(allowed("/v1/images/generations"));
        assert!(allowed("/v1/assets/ast_1/sets?view=card"));
        for path in ["https://evil.example/v1/x", "//evil.example/v1/x", "/v2/x", "/v1/../admin", "/v1/x@evil.example", "v1/x", "/v1/a b", "/v1/x\r\nHost: evil"] {
            assert!(!allowed(path), "{path}");
        }
    }

    #[test]
    fn a_refused_path_is_never_sent() {
        let request = Request { name: "x".into(), method: "POST".into(), path: "https://evil.example/collect".into(), body: None, spends: false, files: Vec::new() };
        assert_eq!(perform(&request).unwrap_err().code, "not_allowed");
    }

    #[test]
    fn saved_files_get_safe_names_that_are_free() {
        assert_eq!(file_name("a-gold-coin.png"), "a-gold-coin.png");
        assert_eq!(file_name("../../etc/passwd"), "passwd");
        assert_eq!(file_name("https://files.example/x/model.glb?sig=abc"), "model.glb");
        assert_eq!(file_name("..\\..\\evil.bat"), "evil.bat");
        assert_eq!(file_name("  "), "file");
        assert_eq!(file_name(".hidden."), "hidden");

        let dir = std::env::temp_dir().join(format!("wundur-card-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free(&dir, "coin.png"), dir.join("coin.png"));
        std::fs::write(dir.join("coin.png"), b"x").unwrap();
        assert_eq!(free(&dir, "coin.png"), dir.join("coin-2.png"));
        std::fs::write(dir.join("coin-2.png"), b"x").unwrap();
        assert_eq!(free(&dir, "coin.png"), dir.join("coin-3.png"));
        assert_eq!(free(&dir, "notes"), dir.join("notes"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_stays_inside_the_project() {
        let project = Path::new("/projects/game");
        assert_eq!(folder_in(project, "assets/generated"), Some(project.join("assets/generated")));
        assert_eq!(folder_in(project, "./assets"), Some(project.join("./assets")));
        assert_eq!(folder_in(project, "../elsewhere"), None);
        assert_eq!(folder_in(project, "/etc"), None);
        assert_eq!(folder_in(project, "assets/../../out"), None);
        let refused = save(&json!(["https://files.example/a.png"]), "../out", project).unwrap_err();
        assert_eq!(refused.code, "save");
        assert_eq!(save(&json!([]), "assets", project).unwrap_err().code, "save", "nothing to save is said, not passed over");
    }

    #[test]
    fn a_record_comes_back_as_it_was_stored() {
        let dir = std::env::temp_dir().join(format!("wundur-card-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = Record {
            workflow: "image".into(),
            version: "9f2c".into(),
            project: "/projects/game".into(),
            form: json!({ "prompt": "a coin" }),
            ui: json!({ "step": "done" }),
            data: json!({ "url": "https://x/y.png" }),
            origin: Some(Origin { session: "s1".into(), turn: Some("t1".into()) }),
        };
        store(&dir, &record).unwrap();
        assert_eq!(load(&dir), Some(record));
        assert_eq!(load(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_plain_ids_are_kept_as_files() {
        assert!(plain("iso-building") && plain("asset_3d"));
        assert!(!plain("") && !plain("../x") && !plain("a/b") && !plain("a b"));
    }
}
