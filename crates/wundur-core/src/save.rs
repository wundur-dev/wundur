//! Writing a file whole, safely. `std::fs::write` empties the file and then fills it: a crash,
//! a full disk or a second writer in between leaves it empty or half written, and for the
//! settings that meant starting again from the defaults. Here the new contents go to a file
//! beside it first and are moved over it in one step, so the file is always either the old
//! one or the new one.

use std::io::Write;
use std::path::Path;

/// Write `contents` as the whole of `path` (its folder is made if it isn't there).
pub fn whole(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    // Beside it (the same disk, so the move is one step), named for this process: two Wundurs
    // saving at once each write their own.
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.part", std::process::id()));
    let part = path.with_file_name(name);
    let written = (|| {
        let mut file = std::fs::File::create(&part)?;
        file.write_all(contents.as_ref())?;
        // On the disk before it takes the old one's place.
        file.sync_all()?;
        std::fs::rename(&part, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wundur-save-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_file_is_replaced_and_nothing_is_left_beside_it() {
        let d = dir("replace");
        let file = d.join("deep/settings.json");
        whole(&file, "one").unwrap();
        whole(&file, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        let beside: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(beside.len(), 1, "only the file itself: {beside:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_write_that_fails_leaves_the_old_file_as_it_was() {
        let d = dir("fail");
        let file = d.join("settings.json");
        whole(&file, "kept").unwrap();
        // A folder where the file should go: the move can't happen.
        let blocked = d.join("blocked");
        std::fs::create_dir_all(blocked.join("inside")).unwrap();
        assert!(whole(&blocked, "new").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "kept");
        assert!(!std::fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".part")), "no part file left");
        let _ = std::fs::remove_dir_all(d);
    }
}
