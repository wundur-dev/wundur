//! Everything made for a project, found in its `assets/` folder: pictures, sounds and 3D
//! models, whoever put them there (Wundur's make cards, an agent's tools, the person). A 3D
//! asset made step by step (a folder with a record, see [`crate::assets`] and
//! [`crate::asset_flow`]) is one entry, not its files.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Model,
    Audio,
}

/// One thing in the library.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    /// The file, or a step-by-step asset's folder.
    pub path: PathBuf,
    /// A picture of it: the image itself, or one of an asset's views.
    pub thumb: Option<PathBuf>,
    pub modified: Option<SystemTime>,
    pub bytes: u64,
    /// A step-by-step asset's folder (it opens as its own tab).
    pub asset: bool,
    /// A step-by-step asset not finished yet: something there waits on the person.
    pub in_progress: bool,
}

/// At most this many entries, and this deep: a project's `assets/` can hold anything.
const MOST: usize = 5000;
const DEPTH: usize = 6;

/// What a file is, by its extension.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => Some(Kind::Image),
        "glb" | "gltf" | "fbx" | "obj" => Some(Kind::Model),
        "mp3" | "wav" | "ogg" | "m4a" | "flac" | "aiff" => Some(Kind::Audio),
        _ => None,
    }
}

/// The project's library: unfinished assets first, then the newest.
pub fn scan(project: &Path) -> Vec<Entry> {
    let mut found = Vec::new();
    walk(&project.join("assets"), 0, &mut found);
    found.sort_by(|a, b| b.in_progress.cmp(&a.in_progress).then(b.modified.cmp(&a.modified)).then(a.path.cmp(&b.path)));
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        if found.len() >= MOST {
            return;
        }
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            match asset(&path) {
                Some(entry) => found.push(entry),
                None if depth < DEPTH => walk(&path, depth + 1, found),
                None => {}
            }
        } else if let Some(kind) = kind_of(&path) {
            let thumb = (kind == Kind::Image).then(|| path.clone());
            found.push(Entry { kind, name, thumb, modified: meta.modified().ok(), bytes: meta.len(), path, asset: false, in_progress: false });
        }
    }
}

/// A step-by-step asset's folder as one entry.
fn asset(dir: &Path) -> Option<Entry> {
    let in_dir = |file: &String| dir.join(file);
    let (name, thumb, in_progress, record) = if let Some(r) = crate::asset_flow::load(dir) {
        let thumb = r.views.values().filter_map(|files| files.first()).next_back().map(in_dir);
        (r.name.clone(), thumb, !r.cancelled && r.imported.is_none(), crate::asset_flow::RECORD)
    } else {
        let a = crate::assets::load(dir)?;
        let thumb = a.concept.clone().or_else(|| a.concepts.last().map(|c| c.file.clone())).as_ref().map(in_dir);
        (a.name.clone(), thumb, false, crate::assets::RECORD)
    };
    let name = if name.trim().is_empty() { dir.file_name()?.to_string_lossy().to_string() } else { name };
    let thumb = thumb.filter(|t| t.is_file()).or_else(|| first_picture(dir));
    let modified = std::fs::metadata(dir.join(record)).and_then(|m| m.modified()).ok();
    Some(Entry { kind: Kind::Model, name, path: dir.to_path_buf(), thumb, modified, bytes: 0, asset: true, in_progress })
}

fn first_picture(dir: &Path) -> Option<PathBuf> {
    let mut pictures: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| kind_of(p) == Some(Kind::Image)).collect();
    pictures.sort();
    pictures.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_what_is_in_the_assets_folder() {
        let root = std::env::temp_dir().join(format!("wundur-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("assets/generated/coin.png", "x");
        write("assets/generated/jump.mp3", "xx");
        write("assets/generated/.hidden.png", "x");
        write("assets/generated/notes.txt", "x");
        write("assets/props/crate.glb", "x");
        // A step-by-step asset: one entry, its files not listed.
        write("assets/creatures/raptor/hosted.json", r#"{"id":"as_1","name":"Raptor","views":{"s1":["front.png"]}}"#);
        write("assets/creatures/raptor/front.png", "x");
        write("assets/creatures/raptor/raptor.glb", "x");
        write("src/logo.png", "x");

        let found = scan(&root);
        let names: Vec<(&str, Kind)> = found.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        // The unfinished asset first.
        assert_eq!(names[0], ("Raptor", Kind::Model));
        assert!(found[0].asset && found[0].in_progress);
        assert_eq!(found[0].thumb.as_deref(), Some(root.join("assets/creatures/raptor/front.png").as_path()));
        assert!(names.contains(&("coin.png", Kind::Image)));
        assert!(names.contains(&("jump.mp3", Kind::Audio)));
        assert!(names.contains(&("crate.glb", Kind::Model)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
