//! Pictures drawn from files, at the size they are shown and only while they are shown.
//!
//! GPUI's own `img(path)` decodes a file at its full size and keeps it for the life of the app,
//! in memory and again as a GPU texture: a Retina screenshot is 14 to 24 MB twice over for a
//! thumbnail 160 points wide, and scrolling through one chat with pictures took Wundur from
//! 570 MB to 1.67 GB (measured 2026-11-24). Here a picture is decoded off the UI thread, made no
//! larger than asked for, and kept in a store with a budget: when the store is over it, the
//! pictures drawn longest ago are let go, from memory and from the GPU. One that comes back
//! into view is decoded again (its place is empty for a moment).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::*;

/// How much decoded picture the store keeps (the GPU holds as much again).
const BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// How often a kept picture's file is looked at again, for one changed on disk.
const LOOK_AGAIN: Duration = Duration::from_secs(2);

/// Sizes asked for, in pixels (twice the points, for a Retina screen), by what shows it.
pub const SMALL: u32 = 160;
pub const TILE: u32 = 400;
pub const LARGE: u32 = 1024;
/// The picture viewer: as it is, up to a 4K screen's worth.
pub const FULL: u32 = 4096;

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    /// Not a picture, or not there.
    Failed,
}

struct Entry {
    state: State,
    /// The file's time when it was decoded, and when that was last compared.
    modified: Option<SystemTime>,
    looked: Instant,
}

/// What is kept and what goes: pure, so it can be tested without a window.
#[derive(Default)]
struct Ledger<K> {
    /// Each kept picture's size and when it was last used.
    kept: HashMap<K, (usize, u64)>,
    uses: u64,
}

impl<K: std::hash::Hash + Eq + Clone> Ledger<K> {
    fn used(&mut self, key: &K) {
        self.uses += 1;
        if let Some(k) = self.kept.get_mut(key) {
            k.1 = self.uses;
        }
    }

    fn keep(&mut self, key: K, bytes: usize) {
        self.uses += 1;
        self.kept.insert(key, (bytes, self.uses));
    }

    fn forget(&mut self, key: &K) {
        self.kept.remove(key);
    }

    /// The pictures to let go for the rest to fit in `budget`: those used longest ago first,
    /// never `keep` (the one just decoded, however large).
    fn over(&mut self, budget: usize, keep: &K) -> Vec<K> {
        let mut total: usize = self.kept.values().map(|(b, _)| b).sum();
        let mut oldest: Vec<(u64, K, usize)> = self.kept.iter().filter(|(k, _)| *k != keep).map(|(k, (b, u))| (*u, k.clone(), *b)).collect();
        oldest.sort_by_key(|(used, _, _)| *used);
        let mut gone = Vec::new();
        for (_, key, bytes) in oldest {
            if total <= budget {
                break;
            }
            total -= bytes;
            self.kept.remove(&key);
            gone.push(key);
        }
        gone
    }
}

type Key = (PathBuf, u32);

#[derive(Default)]
struct Store {
    entries: HashMap<Key, Entry>,
    ledger: Ledger<Key>,
}

impl Global for Store {}

/// The picture in `path`, no larger than `max_px` on its longer side, as an element. Empty
/// while it is being decoded (the window is drawn again when it is there) and for a file that
/// isn't a picture.
pub fn picture(path: &Path, max_px: u32) -> Img {
    let path = path.to_path_buf();
    // Left to GPUI: a GIF (it plays there; here it would be its first frame) and an SVG (drawn
    // from its shapes, not decoded).
    if matches!(path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("gif" | "svg")) {
        return img(path);
    }
    img(ImageSource::Custom(Arc::new(move |_, cx| get(&path, max_px, cx).map(Ok))))
}

/// The picture as the store has it now, asked for each time it is drawn (which is what keeps
/// it: see the module's note).
fn get(path: &Path, max_px: u32, cx: &mut App) -> Option<Arc<RenderImage>> {
    let key: Key = (path.to_path_buf(), max_px);
    let now = Instant::now();
    let store = cx.default_global::<Store>();
    let mut stale = false;
    if let Some(e) = store.entries.get_mut(&key) {
        store.ledger.used(&key);
        // Changed on disk since it was decoded (a picture made again under the same name)?
        if !matches!(e.state, State::Loading) && now.duration_since(e.looked) >= LOOK_AGAIN {
            e.looked = now;
            stale = modified(path) != e.modified;
        }
        if !stale {
            return match &e.state {
                State::Ready(image) => Some(image.clone()),
                State::Loading | State::Failed => None,
            };
        }
    }
    // The one drawn meanwhile stays until the new one is decoded.
    let shown = match store.entries.get(&key).map(|e| &e.state) {
        Some(State::Ready(image)) => Some(image.clone()),
        _ => None,
    };
    if !stale {
        store.entries.insert(key.clone(), Entry { state: State::Loading, modified: None, looked: now });
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        let file = key.0.clone();
        let decoded = cx.background_spawn(async move { (decode(&file, max_px), modified(&file)) }).await;
        cx.update(|cx| {
            let store = cx.default_global::<Store>();
            let (state, bytes) = match decoded.0 {
                Some((image, bytes)) => (State::Ready(image), bytes),
                None => (State::Failed, 0),
            };
            let old = store.entries.insert(key.clone(), Entry { state, modified: decoded.1, looked: Instant::now() });
            store.ledger.keep(key.clone(), bytes);
            let mut gone: Vec<Arc<RenderImage>> = Vec::new();
            if let Some(Entry { state: State::Ready(image), .. }) = old {
                gone.push(image);
            }
            for k in store.ledger.over(BUDGET_BYTES, &key) {
                if let Some(Entry { state: State::Ready(image), .. }) = store.entries.remove(&k) {
                    gone.push(image);
                }
            }
            // Off the GPU too (GPUI keeps a texture for each picture it has drawn).
            for image in gone {
                cx.drop_image(image, None);
            }
            cx.refresh_windows();
        });
    })
    .detach();
    shown
}

/// Let go of the picture kept of `path` at `max_px` (its viewer closed).
pub fn release(path: &Path, max_px: u32, cx: &mut App) {
    let key: Key = (path.to_path_buf(), max_px);
    let store = cx.default_global::<Store>();
    // One still being decoded is left: its task puts it in the store when it is done.
    if !matches!(store.entries.get(&key).map(|e| &e.state), Some(State::Ready(_))) {
        return;
    }
    store.ledger.forget(&key);
    if let Some(Entry { state: State::Ready(image), .. }) = store.entries.remove(&key) {
        cx.drop_image(image, None);
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The file decoded and made to fit `max_px`, as GPUI keeps pictures (BGRA), with its size in
/// bytes. Its full-size pixels live only for the length of this call.
fn decode(path: &Path, max_px: u32) -> Option<(Arc<RenderImage>, usize)> {
    let bytes = std::fs::read(path).ok()?;
    let full = image::load_from_memory(&bytes).ok()?;
    drop(bytes);
    let (w, h) = fit(full.width(), full.height(), max_px);
    let small = if (w, h) == (full.width(), full.height()) { full.into_rgba8() } else { image::imageops::thumbnail(&full, w, h) };
    let mut small = small;
    for p in small.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    let size = small.as_raw().len();
    Some((Arc::new(RenderImage::new(vec![image::Frame::new(small)])), size))
}

/// `w` x `h` scaled down so its longer side is at most `max` (never up).
fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let longer = w.max(h);
    if longer <= max || longer == 0 {
        return (w, h);
    }
    let scale = max as f64 / longer as f64;
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would come with it.
    use super::{Ledger, fit};

    #[test]
    fn a_picture_is_made_smaller_never_larger_and_keeps_its_shape() {
        assert_eq!(fit(3000, 2000, 400), (400, 267));
        assert_eq!(fit(1000, 4000, 400), (100, 400));
        assert_eq!(fit(200, 100, 400), (200, 100), "already small: as it is");
        assert_eq!(fit(4000, 1, 400), (400, 1), "never to nothing");
    }

    #[test]
    fn over_the_budget_the_ones_used_longest_ago_go() {
        let mut l: Ledger<&str> = Ledger::default();
        l.keep("a", 40);
        l.keep("b", 40);
        l.keep("c", 40);
        l.used(&"a");
        // 120 kept, room for 100: the one used longest ago goes, and that is "b" now.
        assert_eq!(l.over(100, &"c"), vec!["b"]);
        assert!(l.kept.contains_key("a") && l.kept.contains_key("c"));
        assert_eq!(l.over(100, &"c"), Vec::<&str>::new(), "within the budget: nothing goes");
    }

    #[test]
    fn the_one_just_decoded_stays_however_large() {
        let mut l: Ledger<&str> = Ledger::default();
        l.keep("old", 10);
        l.keep("huge", 500);
        assert_eq!(l.over(100, &"huge"), vec!["old"]);
        assert!(l.kept.contains_key("huge"));
    }
}
