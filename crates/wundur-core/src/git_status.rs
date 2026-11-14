//! What git says changed in a working tree, per file, for the file tree's marks: `git status`
//! read once (porcelain v1, NUL-separated), the paths made absolute.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::host::Host;

/// A file's state against the last commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Modified,
    Added,
    Deleted,
    Renamed,
    /// Not tracked by git (a new file).
    Untracked,
    /// A merge left it in conflict.
    Conflict,
}

impl Mark {
    /// The letter the file tree shows.
    pub fn letter(self) -> &'static str {
        match self {
            Mark::Modified => "M",
            Mark::Added => "A",
            Mark::Deleted => "D",
            Mark::Renamed => "R",
            Mark::Untracked => "U",
            Mark::Conflict => "!",
        }
    }

    fn of(x: u8, y: u8) -> Self {
        match (x, y) {
            (b'?', b'?') => Mark::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Mark::Conflict,
            (b'A', _) | (_, b'A') => Mark::Added,
            (b'D', _) | (_, b'D') => Mark::Deleted,
            (b'R', _) | (_, b'R') | (b'C', _) => Mark::Renamed,
            _ => Mark::Modified,
        }
    }
}

/// Each changed file, from `git status --porcelain=v1 -z` output (`XY path\0`, a rename
/// followed by its old path; paths from the repository's top folder), as a path under `root`:
/// the folder asked about, which is `prefix` (git's `--show-prefix`) inside the repository.
/// Files outside it are left out. Mapping through the prefix, not comparing absolute paths,
/// keeps a folder reached through a link (macOS's /tmp is /private/tmp) right.
pub fn parse(out: &[u8], root: &Path, prefix: &str) -> HashMap<PathBuf, Mark> {
    let mut marks = HashMap::new();
    let mut parts = out.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let mark = Mark::of(entry[0], entry[1]);
        let path = String::from_utf8_lossy(&entry[3..]).to_string();
        if matches!(entry[0], b'R' | b'C') {
            // The old path follows; the new one is what is shown.
            parts.next();
        }
        if let Some(rel) = path.trim_end_matches('/').strip_prefix(prefix) {
            marks.insert(root.join(rel), mark);
        }
    }
    marks
}

/// The folders holding a changed file, up to (not including) `root`.
pub fn dirty_folders(marks: &HashMap<PathBuf, Mark>, root: &Path) -> HashSet<PathBuf> {
    let mut dirs = HashSet::new();
    for path in marks.keys() {
        let mut at = path.parent();
        while let Some(dir) = at {
            if dir == root || !dir.starts_with(root) || !dirs.insert(dir.to_path_buf()) {
                break;
            }
            at = dir.parent();
        }
    }
    dirs
}

/// What git says changed in the working tree holding `root`; empty when it is not one.
pub fn status(host: &dyn Host, root: &Path) -> HashMap<PathBuf, Mark> {
    let Some(prefix) = host.git(root, &["rev-parse", "--show-prefix"]) else { return HashMap::new() };
    let prefix = String::from_utf8_lossy(&prefix).trim().to_string();
    let Some(out) = host.git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"]) else { return HashMap::new() };
    parse(&out, root, &prefix)
}

/// How a run of lines differs from the last commit, for an editor's change bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChange {
    Added,
    Modified,
    /// Lines were removed here (the mark sits between lines).
    Deleted,
}

/// A run of changed lines in the text now: zero-based lines `lines` (empty for a deletion,
/// at the line it was before).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMark {
    pub change: LineChange,
    pub lines: std::ops::Range<usize>,
}

/// What `current` changed from `base` (the file at the last commit), line by line. Gives up
/// on the smallest diff after a fifth of a second, so typing in a big file never stalls.
pub fn line_marks(base: &str, current: &str) -> Vec<LineMark> {
    use similar::DiffOp;
    let diff = similar::TextDiff::configure().timeout(std::time::Duration::from_millis(200)).diff_lines(base, current);
    diff.ops()
        .iter()
        .filter_map(|op| match *op {
            DiffOp::Equal { .. } => None,
            DiffOp::Insert { new_index, new_len, .. } => Some(LineMark { change: LineChange::Added, lines: new_index..new_index + new_len }),
            DiffOp::Delete { new_index, .. } => Some(LineMark { change: LineChange::Deleted, lines: new_index..new_index }),
            DiffOp::Replace { new_index, new_len, .. } => Some(LineMark { change: LineChange::Modified, lines: new_index..new_index + new_len }),
        })
        .collect()
}

/// The file at `path` as it is in the last commit of the repository holding `root`; `None`
/// when it is not in one, or was never committed.
pub fn head_text(host: &dyn Host, root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let out = host.git(root, &["show", &format!("HEAD:./{}", rel.display())])?;
    Some(String::from_utf8_lossy(&out).replace("\r\n", "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_marked_added_modified_and_deleted() {
        let base = "a\nb\nc\nd\ne\n";
        let now = "a\nB\nc\nnew\nd\n";
        assert_eq!(
            line_marks(base, now),
            [
                LineMark { change: LineChange::Modified, lines: 1..2 },
                LineMark { change: LineChange::Added, lines: 3..4 },
                LineMark { change: LineChange::Deleted, lines: 5..5 },
            ]
        );
        assert!(line_marks(base, base).is_empty());
    }

    #[test]
    fn reads_porcelain_with_renames_and_new_files() {
        let out = b" M src/a.rs\0?? notes/new.md\0R  b.rs\0old_b.rs\0A  c.rs\0 D gone.rs\0UU merge.rs\0";
        let m = parse(out, Path::new("/r"), "");
        assert_eq!(m.get(Path::new("/r/src/a.rs")), Some(&Mark::Modified));
        assert_eq!(m.get(Path::new("/r/notes/new.md")), Some(&Mark::Untracked));
        assert_eq!(m.get(Path::new("/r/b.rs")), Some(&Mark::Renamed));
        assert!(!m.contains_key(Path::new("/r/old_b.rs")));
        assert_eq!(m.get(Path::new("/r/c.rs")), Some(&Mark::Added));
        assert_eq!(m.get(Path::new("/r/gone.rs")), Some(&Mark::Deleted));
        assert_eq!(m.get(Path::new("/r/merge.rs")), Some(&Mark::Conflict));
    }

    #[test]
    fn folders_above_a_change_are_dirty_up_to_the_root() {
        let m = parse(b" M app/src/ui/x.rs\0 M other/y.rs\0", Path::new("/w/app"), "app/");
        assert_eq!(m.len(), 1, "a file outside the folder asked about is left out");
        assert!(m.contains_key(Path::new("/w/app/src/ui/x.rs")));
        let d = dirty_folders(&m, Path::new("/w/app"));
        assert!(d.contains(Path::new("/w/app/src")) && d.contains(Path::new("/w/app/src/ui")));
        assert!(!d.contains(Path::new("/w/app")));
    }
}
