//! What's typed in each session's message box and not sent yet, kept across quits and
//! restarts: `drafts.json` in the config folder, by the session's number in the window's list
//! (the number a session keeps when it's opened again). A sent message or a session closed on
//! purpose drops its draft.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

static DRAFTS: Mutex<Option<BTreeMap<String, String>>> = Mutex::new(None);

fn file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("drafts.json"))
}

fn with<T>(f: impl FnOnce(&mut BTreeMap<String, String>) -> T) -> T {
    let mut guard = DRAFTS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(|| file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default());
    f(map)
}

fn write(map: &BTreeMap<String, String>) {
    if let Some(p) = file() {
        let _ = crate::save::whole(&p, serde_json::to_string_pretty(map).unwrap_or_default());
    }
}

/// The draft kept for session `number`, if any.
pub fn get(number: usize) -> Option<String> {
    with(|m| m.get(&number.to_string()).cloned())
}

/// Keep `text` as session `number`'s draft (blank text drops it). Writes only on a change.
pub fn set(number: usize, text: &str) {
    with(|m| {
        let key = number.to_string();
        let changed = if text.trim().is_empty() { m.remove(&key).is_some() } else { m.insert(key, text.to_string()).as_deref() != Some(text) };
        if changed {
            write(m);
        }
    });
}

/// Drop session `number`'s draft (it was closed on purpose).
pub fn forget(number: usize) {
    set(number, "");
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_draft_is_kept_changed_and_dropped() {
        // Only the in-memory map: the test doesn't touch the person's config folder.
        let mut m = std::collections::BTreeMap::new();
        m.insert("7".to_string(), "half a thought".to_string());
        *super::DRAFTS.lock().unwrap() = Some(m);
        assert_eq!(super::with(|m| m.get("7").cloned()).as_deref(), Some("half a thought"));
        super::with(|m| m.remove("7"));
        assert!(super::with(|m| m.get("7").is_none()));
    }
}
