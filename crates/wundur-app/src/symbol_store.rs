//! One index of definitions per project folder, shared by every session open on it.
//!
//! Each session used to read the folder's source files and keep an index of its own, at startup
//! and after every turn, shown or not: with sixteen sessions open that was the largest thing in
//! Wundur's memory (measured 2026-11-24: 48 MB of 125), and several sessions on one folder read
//! the same files at the same moment. Here a folder has one index; a session asks for it when it
//! is first shown, gets the one already there at once, and one reading at a time brings it up to
//! date (a request made meanwhile is answered by one more reading, after). The index goes when
//! the last session using it does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpui_kit::*;
use wundur_core::host::Host;
use wundur_core::links::FileIndex;
use wundur_core::symbols::SymbolIndex;

/// Where a session keeps the index it looks definitions up in (see `doc_panel::DocContext`).
pub type Slot = Rc<RefCell<Arc<SymbolIndex>>>;

/// A folder on a machine: the host's label (`local`, or the ssh destination) and the path.
type Key = (String, PathBuf);

/// Whether a reading is under way, and whether another was asked for meanwhile: what decides
/// when the files are read.
#[derive(Default, Debug, PartialEq)]
struct Reading<T> {
    under_way: bool,
    /// The file list to read once the one under way is done (the latest asked for).
    next: Option<T>,
}

impl<T> Reading<T> {
    /// A reading of `files` is asked for: `Some(files)` to start it now, `None` when one is
    /// under way (these are read after it).
    fn ask(&mut self, files: T) -> Option<T> {
        if self.under_way {
            self.next = Some(files);
            None
        } else {
            self.under_way = true;
            Some(files)
        }
    }

    /// The reading under way is done: the files to read next, if any were asked for meanwhile.
    fn done(&mut self) -> Option<T> {
        let next = self.next.take();
        self.under_way = next.is_some();
        next
    }
}

struct Shared {
    index: Arc<SymbolIndex>,
    /// The sessions using it (those still open).
    slots: Vec<Weak<RefCell<Arc<SymbolIndex>>>>,
    reading: Reading<Arc<FileIndex>>,
}

#[derive(Default)]
struct Store(HashMap<Key, Shared>);

impl Global for Store {}

/// Bring the folder's index up to date with `files`, for the session keeping it in `slot`: the
/// slot has the folder's index as it is at once, and the new one when the files are read.
pub fn update(host: Arc<dyn Host>, root: PathBuf, files: Arc<FileIndex>, slot: &Slot, cx: &mut App) {
    let key = (host.label(), root);
    let store = cx.default_global::<Store>();
    // Folders whose sessions have all closed: their indexes go.
    store.0.retain(|k, s| *k == key || s.reading.under_way || s.slots.iter().any(|w| w.strong_count() > 0));
    let shared = store.0.entry(key.clone()).or_insert_with(|| Shared { index: Default::default(), slots: Vec::new(), reading: Default::default() });
    shared.slots.retain(|w| w.strong_count() > 0);
    if !shared.slots.iter().any(|w| w.as_ptr() == Rc::as_ptr(slot)) {
        shared.slots.push(Rc::downgrade(slot));
    }
    *slot.borrow_mut() = shared.index.clone();
    if let Some(files) = shared.reading.ask(files) {
        read(host, key, files, cx);
    }
}

/// Read the folder's source files off the UI thread, index their definitions, and hand the
/// index to every session on the folder; then once more if that was asked for meanwhile.
fn read(host: Arc<dyn Host>, key: Key, files: Arc<FileIndex>, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let started = std::time::Instant::now();
        let reader = host.clone();
        let symbols = cx
            .background_spawn(async move {
                let mut out = SymbolIndex::default();
                for path in files.files().take(8_000) {
                    if wundur_core::files::language_for(&path) == "text" {
                        continue;
                    }
                    if let Ok(bytes) = reader.read(&path)
                        && bytes.len() < 512 * 1024
                        && let Ok(text) = String::from_utf8(bytes)
                    {
                        out.add(wundur_core::symbols::definitions(&path, &text));
                    }
                }
                out
            })
            .await;
        if crate::perf::enabled() {
            eprintln!("perf symbol index: {} definitions in {:?}", symbols.len(), started.elapsed());
        }
        cx.update(|cx| {
            let store = cx.default_global::<Store>();
            let Some(shared) = store.0.get_mut(&key) else { return };
            shared.index = Arc::new(symbols);
            shared.slots.retain(|w| w.strong_count() > 0);
            for slot in shared.slots.iter().filter_map(Weak::upgrade) {
                *slot.borrow_mut() = shared.index.clone();
            }
            let next = shared.reading.done();
            // Every session on the folder closed while it was read: nothing keeps it.
            if shared.slots.is_empty() {
                store.0.remove(&key);
            } else if let Some(files) = next {
                read(host, key, files, cx);
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would come with it.
    use super::Reading;

    #[test]
    fn one_reading_at_a_time_and_one_more_for_what_was_asked_meanwhile() {
        let mut r: Reading<u32> = Reading::default();
        assert_eq!(r.ask(1), Some(1), "nothing under way: read now");
        assert_eq!(r.ask(2), None, "one under way: after it");
        assert_eq!(r.ask(3), None);
        assert_eq!(r.done(), Some(3), "the latest asked for, once");
        assert_eq!(r.ask(4), None, "that one is under way now");
        assert_eq!(r.done(), Some(4));
        assert_eq!(r.done(), None, "nothing more was asked for");
        assert_eq!(r.ask(5), Some(5), "idle again: read now");
    }
}
