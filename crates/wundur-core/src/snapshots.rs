//! Restore points for undoing an agent's turn: the project's files recorded as git commits in a
//! private repository of Wundur's own (under [`crate::settings::config_dir`]), outside the
//! project. It never touches the project's own repository, branches or history, so it works the
//! same in a git project, one under another version control system, or a plain folder.
//!
//! The repository's index persists between snapshots, so a snapshot only reads the files that
//! changed since the last one (as `git status` does). In a git project it borrows the project's
//! objects (`objects/info/alternates`) instead of copying files that are already committed.
//! Very large files are left out (listed in `info/large`), and so are build output and
//! dependency folders (`info/exclude`): undo can't put those back.
//!
//! Only this machine's projects: a session over SSH has no restore points.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

/// Files larger than this are not recorded (and so not restored).
pub const LARGEST_FILE: u64 = 50 << 20;

/// More new files than this in one snapshot: the folder is too big to keep restore points for
/// (a home folder, a disk), and it is refused rather than copied.
pub const MOST_NEW_FILES: usize = 200_000;

/// Left out of every snapshot: build output, dependencies, engine caches, system clutter.
/// The project's own `.gitignore` files apply as well.
const EXCLUDE: &str = "\
# Wundur's restore points leave these out (rebuilt, downloaded or generated).
.DS_Store
._*
node_modules/
.venv/
venv/
__pycache__/
target/
dist/
build/
.next/
.gradle/
# Unity
/Library/
/Temp/
/Logs/
/obj/
/UserSettings/
# Godot
.godot/
# Unreal
/Binaries/
/Intermediate/
/Saved/
/DerivedDataCache/
";

/// How a file differs between two snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
}

/// One file a turn changed, relative to the project folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: PathBuf,
    pub kind: Kind,
}

/// A project's restore points.
#[derive(Clone, Debug)]
pub struct Store {
    git_dir: PathBuf,
    work_tree: PathBuf,
}

/// One lock per private repository: two sessions in the same folder snapshot one at a time
/// (git's index lock would refuse the second).
fn lock_for(git_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks.get_or_insert_with(HashMap::new).entry(git_dir.to_path_buf()).or_default().clone()
}

/// Where restore points are kept: `snapshots` under Wundur's settings folder.
fn snapshots_dir() -> Option<PathBuf> {
    Some(crate::settings::config_dir()?.join("snapshots"))
}

/// A stable name for a project folder's private repository.
fn repo_name(project: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(project.to_string_lossy().as_bytes());
    let short: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let name = project.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).take(40).collect();
    format!("{name}-{short}.git")
}

/// Folders never given restore points: the home folder and the usual places that hold
/// everything rather than one project.
pub fn too_broad(project: &Path) -> bool {
    if project.parent().is_none() {
        return true;
    }
    let Some(home) = dirs::home_dir() else { return false };
    if project == home {
        return true;
    }
    ["Desktop", "Documents", "Downloads", "Library"].iter().any(|d| project == home.join(d))
}

/// Escape a path for an exclude file, matched literally from the project's top.
fn exclude_line(rel: &Path) -> String {
    let mut out = String::from("/");
    for c in rel.to_string_lossy().chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\' | '!' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn nul_paths(out: &[u8]) -> Vec<PathBuf> {
    out.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| PathBuf::from(String::from_utf8_lossy(p).as_ref())).collect()
}

impl Store {
    /// The restore points of `project` (a folder on this machine), made the first time.
    pub fn open(project: &Path) -> Result<Store> {
        let dir = snapshots_dir().context("no settings folder")?;
        Self::open_in(&dir, project)
    }

    /// As [`Store::open`], keeping the private repositories in `dir`.
    pub fn open_in(dir: &Path, project: &Path) -> Result<Store> {
        let work_tree = project.canonicalize().with_context(|| format!("{} not found", project.display()))?;
        if too_broad(&work_tree) {
            bail!("{} holds too much to keep restore points for", work_tree.display());
        }
        let store = Store { git_dir: dir.join(repo_name(&work_tree)), work_tree };
        if !store.git_dir.join("HEAD").exists() {
            store.create()?;
        }
        Ok(store)
    }

    fn create(&self) -> Result<()> {
        std::fs::create_dir_all(&self.git_dir)?;
        let out = crate::platform::command("git").arg("init").arg("--quiet").arg("--bare").arg(&self.git_dir).output()?;
        if !out.status.success() {
            bail!("git init: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        for (key, value) in [
            ("core.bare", "false"),
            ("core.autocrlf", "false"),
            ("core.quotepath", "false"),
            ("core.untrackedCache", "true"),
            ("advice.addEmbeddedRepo", "false"),
            ("core.excludesFile", &self.git_dir.join("info/large").to_string_lossy()),
        ] {
            self.git(&["config", key, value])?;
        }
        let info = self.git_dir.join("info");
        std::fs::create_dir_all(&info)?;
        std::fs::write(info.join("exclude"), EXCLUDE)?;
        std::fs::write(info.join("large"), "# Files too large for restore points.\n")?;
        // A git project: borrow its objects rather than copy what is committed.
        if let Ok(out) = crate::platform::command("git")
            .arg("-C")
            .arg(&self.work_tree)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()
            && out.status.success()
        {
            let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            let objects = common.join("objects");
            if objects.is_dir() {
                std::fs::create_dir_all(self.git_dir.join("objects/info"))?;
                std::fs::write(self.git_dir.join("objects/info/alternates"), format!("{}\n", objects.display()))?;
            }
        }
        Ok(())
    }

    /// The project folder.
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    fn command(&self) -> Command {
        let mut cmd = crate::platform::command("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .arg("--literal-pathspecs")
            .current_dir(&self.work_tree)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Wundur")
            .env("GIT_AUTHOR_EMAIL", "restore-points@wundur.invalid")
            .env("GIT_COMMITTER_NAME", "Wundur")
            .env("GIT_COMMITTER_EMAIL", "restore-points@wundur.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        cmd
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.git_with(args, None)
    }

    fn git_with(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut cmd = self.command();
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        let mut child = cmd.spawn().context("could not run git")?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("git {}: {}", args.first().unwrap_or(&""), err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim());
        }
        Ok(out.stdout)
    }

    fn text(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.git(args)?).trim().to_string())
    }

    /// Record the project's files as they are now; the commit's id. Blocking (run it off the
    /// UI thread): fast once the first one is made, as only changed files are read.
    pub fn snapshot(&self) -> Result<String> {
        let lock = lock_for(&self.git_dir);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        // New files: refuse a folder that is far too big, and leave out the very large ones.
        let new = nul_paths(&self.git(&["ls-files", "--others", "--exclude-standard", "-z"])?);
        if new.len() > MOST_NEW_FILES {
            bail!("{} has too many files to keep restore points for", self.work_tree.display());
        }
        let large: Vec<String> = new
            .iter()
            .filter(|rel| std::fs::symlink_metadata(self.work_tree.join(rel)).is_ok_and(|m| m.is_file() && m.len() > LARGEST_FILE))
            .map(|rel| exclude_line(rel))
            .collect();
        if !large.is_empty() {
            let mut f = std::fs::OpenOptions::new().append(true).create(true).open(self.git_dir.join("info/large"))?;
            writeln!(f, "{}", large.join("\n"))?;
        }
        self.git(&["add", "--all", "--ignore-errors", "--", "."]).or_else(|e| {
            // `--ignore-errors` still exits non-zero when a file could not be read; what could
            // be read is in the index.
            if e.to_string().contains("adding files failed") { Ok(Vec::new()) } else { Err(e) }
        })?;
        let tree = self.text(&["write-tree"])?;
        self.text(&["commit-tree", &tree, "-m", "restore point"])
    }

    /// Keep `commit` from being cleaned away, under `name` (`session/turn`).
    pub fn keep(&self, name: &str, commit: &str) -> Result<()> {
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/') { c } else { '-' }).collect();
        self.git(&["update-ref", &format!("refs/wundur/{name}"), commit])?;
        Ok(())
    }

    /// The files that differ between snapshots `from` and `to`.
    pub fn changes(&self, from: &str, to: &str) -> Result<Vec<Changed>> {
        let out = self.git(&["diff-tree", "-r", "--no-renames", "--name-status", "-z", from, to])?;
        let mut parts = out.split(|b| *b == 0).filter(|p| !p.is_empty());
        let mut changed = Vec::new();
        while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
            let kind = match status.first() {
                Some(b'A') => Kind::Added,
                Some(b'D') => Kind::Deleted,
                _ => Kind::Modified,
            };
            changed.push(Changed { path: PathBuf::from(String::from_utf8_lossy(path).as_ref()), kind });
        }
        Ok(changed)
    }

    /// Of `files`, those changed again since snapshot `since` (by the person or another
    /// session): undoing them would lose that.
    pub fn changed_since(&self, since: &str, files: &[Changed]) -> Result<Vec<PathBuf>> {
        let now = self.snapshot()?;
        let later: HashSet<PathBuf> = self.changes(since, &now)?.into_iter().map(|c| c.path).collect();
        Ok(files.iter().filter(|c| later.contains(&c.path)).map(|c| c.path.clone()).collect())
    }

    /// Put `files` back as they were in snapshot `to`: changed and deleted ones restored, new
    /// ones removed (and folders left empty by that).
    pub fn restore(&self, to: &str, files: &[Changed]) -> Result<()> {
        let lock = lock_for(&self.git_dir);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut back = Vec::new();
        for c in files.iter().filter(|c| c.kind != Kind::Added) {
            back.extend_from_slice(c.path.to_string_lossy().as_bytes());
            back.push(0);
        }
        if !back.is_empty() {
            self.git_with(&["checkout", to, "--pathspec-from-file=-", "--pathspec-file-nul"], Some(&back))?;
        }
        for c in files.iter().filter(|c| c.kind == Kind::Added) {
            let path = self.work_tree.join(&c.path);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("could not remove {}", c.path.display())),
            }
            let mut dir = path.parent();
            while let Some(d) = dir.filter(|d| *d != self.work_tree && d.starts_with(&self.work_tree)) {
                if std::fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wundur-snap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sh(dir: &Path, script: &str) {
        let out = Command::new("sh").arg("-c").arg(script).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn read(dir: &Path, rel: &str) -> Option<String> {
        std::fs::read_to_string(dir.join(rel)).ok()
    }

    #[cfg(unix)]
    #[test]
    fn a_turn_is_undone_in_a_plain_folder() {
        let (store_dir, project) = (temp("store-plain"), temp("plain project's"));
        sh(&project, "printf 'one\\n' > kept.txt && printf 'gone\\n' > deleted.txt && mkdir -p node_modules && printf x > node_modules/dep.js");
        let store = Store::open_in(&store_dir, &project).unwrap();
        let before = store.snapshot().unwrap();
        // The turn: edits one file, deletes one, adds one in a new folder.
        sh(&project, "printf 'two\\n' > kept.txt && rm deleted.txt && mkdir -p new/deep && printf 'n\\n' > new/deep/made.txt");
        let after = store.snapshot().unwrap();
        let mut changes = store.changes(&before, &after).unwrap();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            changes,
            [
                Changed { path: "deleted.txt".into(), kind: Kind::Deleted },
                Changed { path: "kept.txt".into(), kind: Kind::Modified },
                Changed { path: "new/deep/made.txt".into(), kind: Kind::Added },
            ]
        );
        // Nothing has changed since the turn.
        assert!(store.changed_since(&after, &changes).unwrap().is_empty());
        store.restore(&before, &changes).unwrap();
        assert_eq!(read(&project, "kept.txt").as_deref(), Some("one\n"));
        assert_eq!(read(&project, "deleted.txt").as_deref(), Some("gone\n"));
        assert!(!project.join("new").exists(), "the folders the turn made go too");
        assert!(project.join("node_modules/dep.js").exists());
        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn a_git_project_is_left_alone_and_later_changes_are_found() {
        let (store_dir, project) = (temp("store-git"), temp("git-project"));
        sh(
            &project,
            "git init -q && git config user.email t@t && git config user.name t && \
             printf 'a\\n' > a.rs && printf 'b\\n' > b.rs && git add . && git commit -qm one && printf 'dirty\\n' >> a.rs",
        );
        let status = |p: &Path| String::from_utf8(Command::new("git").arg("-C").arg(p).args(["status", "--porcelain"]).output().unwrap().stdout).unwrap();
        let was = status(&project);
        let store = Store::open_in(&store_dir, &project).unwrap();
        let before = store.snapshot().unwrap();
        assert_eq!(status(&project), was, "the project's own repository is untouched");
        assert!(std::fs::read_to_string(store.git_dir.join("objects/info/alternates")).unwrap().contains("objects"));
        sh(&project, "printf 'A\\n' > a.rs && printf 'B\\n' > b.rs");
        let after = store.snapshot().unwrap();
        let changes = store.changes(&before, &after).unwrap();
        assert_eq!(changes.len(), 2);
        // The person changes b.rs again after the turn.
        sh(&project, "printf 'mine\\n' > b.rs");
        assert_eq!(store.changed_since(&after, &changes).unwrap(), [PathBuf::from("b.rs")]);
        store.restore(&before, &changes).unwrap();
        assert_eq!(read(&project, "a.rs").as_deref(), Some("a\ndirty\n"), "back to before the turn, uncommitted edit included");
        assert_eq!(status(&project), was);
        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn large_files_are_left_out_by_their_literal_path() {
        assert_eq!(exclude_line(Path::new("art/big [v2]*.psd")), "/art/big \\[v2\\]\\*.psd");
        assert!(too_broad(Path::new("/")));
        if let Some(home) = dirs::home_dir() {
            assert!(too_broad(&home) && too_broad(&home.join("Downloads")));
            assert!(!too_broad(&home.join("work/game")));
        }
    }
}
