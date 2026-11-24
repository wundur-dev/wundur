//! Answers kept for a moment. A card in the chat asks the disk the same things on every frame it
//! is drawn (is this folder a hosted asset's, is its record there, is a picture fetched), and a
//! chat that is streaming or animating draws many frames a second. Each answer is kept for
//! [`A_MOMENT`] and asked again after, so a change on disk still shows, a moment late at most.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// How long an answer is kept.
pub const A_MOMENT: Duration = Duration::from_secs(2);

pub struct Kept<K, V> {
    answers: HashMap<K, (V, Instant)>,
}

impl<K, V> Default for Kept<K, V> {
    fn default() -> Self {
        Self { answers: HashMap::new() }
    }
}

impl<K: Hash + Eq, V> Kept<K, V> {
    /// The answer for `key`, if one was kept within the last moment before `now`.
    pub fn get<Q>(&self, key: &Q, now: Instant) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.answers.get(key).filter(|(_, at)| now.saturating_duration_since(*at) < A_MOMENT).map(|(v, _)| v)
    }

    pub fn put(&mut self, key: K, answer: V, now: Instant) {
        // Answers past their moment go when the map has grown, so it stays the size of what
        // is on screen.
        if self.answers.len() >= 256 {
            self.answers.retain(|_, (_, at)| now.saturating_duration_since(*at) < A_MOMENT);
        }
        self.answers.insert(key, (answer, now));
    }

    /// Ask again the next time (what it answers changed just now).
    pub fn forget<Q>(&mut self, key: &Q)
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.answers.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_kept_for_a_moment_then_asked_again() {
        let mut kept: Kept<String, bool> = Kept::default();
        let start = Instant::now();
        assert_eq!(kept.get("a", start), None, "nothing kept yet");
        kept.put("a".into(), true, start);
        assert_eq!(kept.get("a", start + A_MOMENT / 2), Some(&true));
        assert_eq!(kept.get("a", start + A_MOMENT), None, "a moment on: asked again");
        assert_eq!(kept.get("b", start), None);
    }

    #[test]
    fn a_forgotten_answer_is_asked_again_at_once() {
        let mut kept: Kept<String, bool> = Kept::default();
        let start = Instant::now();
        kept.put("a".into(), false, start);
        kept.forget("a");
        assert_eq!(kept.get("a", start), None);
    }

    #[test]
    fn old_answers_go_when_it_has_grown() {
        let mut kept: Kept<u32, u32> = Kept::default();
        let start = Instant::now();
        for n in 0..256 {
            kept.put(n, n, start);
        }
        kept.put(1000, 0, start + A_MOMENT * 2);
        assert_eq!(kept.answers.len(), 1);
    }
}
