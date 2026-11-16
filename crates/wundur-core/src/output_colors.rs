//! Light colouring for command output a program printed plain: labels, numbers and
//! percentages, paths, web addresses, commit hashes, and words like `error`, `warning` and
//! `done`, so a wall of default-coloured text (git's `remote: Resolving deltas: 100% (3/3),
//! done.`) has something for the eye to hold on to. The terminal applies it only to cells the
//! program left in the default colour, and only to command output; the text is unchanged.

use std::ops::Range;

/// What a stretch of text is, which sets its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Noise around the message (`remote:`, a time).
    Dim,
    /// What a line is about (`Resolving deltas:`): bold.
    Label,
    Bad,
    Warn,
    Good,
    Link,
    Path,
    Number,
    Hash,
}

/// The tone of each character of `line` (by character, not byte); `None` stays as printed.
pub fn tones(line: &str) -> Vec<Option<Tone>> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = vec![None; chars.len()];
    let set = |r: Range<usize>, t: Tone, out: &mut Vec<Option<Tone>>| {
        for c in &mut out[r.start.min(chars.len())..r.end.min(chars.len())] {
            *c = Some(t);
        }
    };
    let words = words(&chars);
    let Some(&(first, _)) = words.first() else { return out };

    // A prefix that only says where the line came from.
    let mut body = first;
    let mut word_at = 0;
    let lead: String = chars[words[0].0..words[0].1].iter().collect();
    if matches!(lead.as_str(), "remote:" | "hint:" | "note:") {
        set(words[0].0..words[0].1, Tone::Dim, &mut out);
        body = words.get(1).map_or(chars.len(), |w| w.0);
        word_at = 1;
    }

    // `Label words: the rest`: a short label of words, at the start.
    if let Some(colon) = (body..chars.len()).find(|&i| chars[i] == ':') {
        let label = &chars[body..colon];
        let after_ok = chars.get(colon + 1).is_none_or(|c| c.is_whitespace());
        let wordy = !label.is_empty()
            && label.len() <= 32
            && label.iter().all(|c| c.is_alphabetic() || *c == ' ' || *c == '-' || *c == '_')
            && label.iter().filter(|c| **c == ' ').count() <= 3;
        if wordy && after_ok {
            let low: String = label.iter().collect::<String>().to_lowercase();
            match low.as_str() {
                // The whole line is the failure.
                "error" | "fatal" | "err" => set(body..chars.len(), Tone::Bad, &mut out),
                "warning" | "warn" => set(body..colon + 1, Tone::Warn, &mut out),
                _ => set(body..colon + 1, Tone::Label, &mut out),
            }
            if matches!(low.as_str(), "error" | "fatal" | "err") {
                return out;
            }
        }
    }

    let diffstat = line.contains(" | ");
    let mut prev: Option<String> = None;
    for (i, &(s, e)) in words.iter().enumerate().skip(word_at) {
        let raw: String = chars[s..e].iter().collect();
        // Without the punctuation around it.
        let lead = raw.chars().take_while(|c| "([{<\"'`".contains(*c)).count();
        let trail = raw.chars().rev().take_while(|c| ")]}>\"'`,;.!".contains(*c)).count();
        let (ts, te) = (s + lead, (e - trail).max(s + lead));
        let token: String = chars[ts..te].iter().collect();
        let bare = token.trim_end_matches(':');
        let low = bare.to_lowercase();
        let tone = if is_link(&token) {
            Some((ts..te, Tone::Link))
        } else if let Some(t) = keyword(&low, prev.as_deref()) {
            Some((ts..ts + bare.chars().count(), t))
        } else if is_hash(bare) {
            Some((ts..ts + bare.chars().count(), Tone::Hash))
        } else if is_time(bare) {
            Some((ts..ts + bare.chars().count(), Tone::Dim))
        } else if is_number(bare) {
            // A number and its unit: `1.2 MiB`, `340 ms`.
            let unit = words.get(i + 1).map(|&(us, ue)| {
                let u: String = chars[us..ue].iter().collect();
                (us, ue, u.trim_end_matches([',', '.', ')', ';']).to_string())
            });
            match unit {
                Some((us, _, u)) if is_unit(&u) => Some((ts..us + u.chars().count(), Tone::Number)),
                _ => Some((ts..ts + bare.chars().count(), Tone::Number)),
            }
        } else if is_path(bare) {
            Some((ts..ts + bare.chars().count(), Tone::Path))
        } else if diffstat && prev.as_deref().is_some_and(plain_number) && !bare.is_empty() && bare.chars().all(|c| c == '+' || c == '-') {
            // A diffstat's `| 12 ++--`.
            for (k, c) in bare.chars().enumerate() {
                set(ts + k..ts + k + 1, if c == '+' { Tone::Good } else { Tone::Bad }, &mut out);
            }
            None
        } else {
            None
        };
        if let Some((r, t)) = tone {
            set(r, t, &mut out);
        }
        prev = Some(low);
    }
    out
}

/// The whitespace-separated words of `chars`, as character ranges.
fn words(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let s = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        out.push((s, i));
    }
    out
}

fn keyword(low: &str, prev: Option<&str>) -> Option<Tone> {
    // "0 errors", "no warnings": good news.
    let none = matches!(prev, Some("0" | "no" | "zero"));
    match low {
        "error" | "errors" | "fatal" | "failed" | "failure" | "failures" | "fail" | "panicked" | "panic" | "denied"
        | "rejected" | "exception" | "abort" | "aborted" | "conflict" => Some(if none { Tone::Good } else { Tone::Bad }),
        "warning" | "warnings" | "warn" | "deprecated" => Some(if none { Tone::Good } else { Tone::Warn }),
        "done" | "ok" | "passed" | "pass" | "success" | "successful" | "successfully" | "succeeded" | "finished"
        | "up-to-date" | "up to date" | "created" | "merged" => Some(Tone::Good),
        _ => None,
    }
}

fn is_link(t: &str) -> bool {
    ["http://", "https://", "ssh://", "git://", "file://"].iter().any(|p| t.starts_with(p)) || (t.starts_with("git@") && t.contains(':'))
}

/// A commit hash: 7 to 40 lowercase hex characters with a digit and a letter (or a range of
/// two, `a1b2c3d..e4f5a6b`).
fn is_hash(t: &str) -> bool {
    let hex = |h: &str| (7..=40).contains(&h.len()) && h.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    let mixed = |h: &str| h.chars().any(|c| c.is_ascii_digit()) && h.chars().any(|c| c.is_ascii_alphabetic());
    match t.split_once("..") {
        // A range: either end may happen to be all digits.
        Some((a, b)) => {
            let b = b.trim_start_matches('.');
            hex(a) && hex(b) && (mixed(a) || mixed(b))
        }
        None => hex(t) && mixed(t),
    }
}

/// `12:04`, `12:04:59`, `2026-11-16`, or both joined with `T`.
fn is_time(t: &str) -> bool {
    let digits = |s: &str, n: usize| s.len() == n && s.chars().all(|c| c.is_ascii_digit());
    let clock = |s: &str| {
        let s = s.split(['.', 'Z', '+']).next().unwrap_or(s);
        let p: Vec<&str> = s.split(':').collect();
        (p.len() == 2 || p.len() == 3) && p.iter().all(|x| digits(x, 2))
    };
    let date = |s: &str| {
        let p: Vec<&str> = s.split('-').collect();
        p.len() == 3 && digits(p[0], 4) && digits(p[1], 2) && digits(p[2], 2)
    };
    match t.split_once('T') {
        Some((d, c)) => date(d) && clock(c),
        None => date(t) || clock(t),
    }
}

/// `42`, `3.5`, `1,024`, `100%`, `(3/3)`'s `3/3`, `12ms`, `1.2MiB`, `+5`, `-3`.
fn is_number(t: &str) -> bool {
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    if let Some((a, b)) = t.split_once('/') {
        return plain_number(a) && plain_number(b);
    }
    let digits_end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(t.len());
    let (n, rest) = t.split_at(digits_end);
    plain_number(n) && (rest.is_empty() || rest == "%" || is_unit(rest))
}

fn plain_number(n: &str) -> bool {
    !n.is_empty()
        && n.chars().next().is_some_and(|c| c.is_ascii_digit())
        && n.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',')
        && !n.ends_with(['.', ','])
}

fn is_unit(u: &str) -> bool {
    matches!(
        u,
        "B" | "KB" | "MB" | "GB" | "TB" | "KiB" | "MiB" | "GiB" | "TiB" | "kB" | "bytes" | "ms" | "s" | "sec" | "secs"
            | "m" | "min" | "h" | "µs" | "us" | "ns" | "KiB/s" | "MiB/s" | "GiB/s" | "KB/s" | "MB/s" | "kB/s" | "%"
            | "files" | "file" | "lines" | "objects" | "insertions(+)" | "deletions(-)" | "insertion(+)" | "deletion(-)"
    )
}

/// A path: `src/main.rs`, `./x`, `/usr/bin`, `~/code`, `Cargo.toml`, maybe with `:line:col`.
fn is_path(t: &str) -> bool {
    if t.len() < 3 || t.contains("://") {
        return false;
    }
    let ok = |c: char| c.is_alphanumeric() || "._-/~@+:\\".contains(c);
    if !t.chars().all(ok) {
        return false;
    }
    // Without a `:12:3` at its end.
    let base = t.trim_end_matches(|c: char| c.is_ascii_digit() || c == ':');
    if base.is_empty() || base.ends_with('/') && base.len() == 1 {
        return false;
    }
    if base.starts_with("./") || base.starts_with("../") || base.starts_with("~/") || (base.starts_with('/') && base.len() > 1) {
        return true;
    }
    let file = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let has_ext = file.rsplit_once('.').is_some_and(|(name, ext)| {
        !name.is_empty() && (2..=6).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    });
    // Folders: `a/b` with letters on both sides (not `3/3`, not `and/or` alone).
    let foldered = base.contains('/') && base.chars().any(|c| c.is_alphabetic()) && base.split('/').filter(|s| !s.is_empty()).count() >= 2;
    has_ext || (foldered && (has_ext || base.split('/').count() >= 3 || base.ends_with('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of each toned stretch, with its tone.
    fn spans(line: &str) -> Vec<(String, Tone)> {
        let chars: Vec<char> = line.chars().collect();
        let t = tones(line);
        let mut out: Vec<(String, Tone)> = Vec::new();
        let mut i = 0;
        while i < t.len() {
            let Some(tone) = t[i] else {
                i += 1;
                continue;
            };
            let s = i;
            while i < t.len() && t[i] == Some(tone) {
                i += 1;
            }
            out.push((chars[s..i].iter().collect(), tone));
        }
        out
    }

    #[test]
    fn git_progress_lines() {
        assert_eq!(
            spans("remote: Resolving deltas: 100% (3/3), done."),
            [
                ("remote:".into(), Tone::Dim),
                ("Resolving deltas:".into(), Tone::Label),
                ("100%".into(), Tone::Number),
                ("3/3".into(), Tone::Number),
                ("done".into(), Tone::Good)
            ]
        );
        assert_eq!(
            spans("Receiving objects: 100% (12/12), 4.21 KiB | 4.21 MiB/s, done."),
            [
                ("Receiving objects:".into(), Tone::Label),
                ("100%".into(), Tone::Number),
                ("12/12".into(), Tone::Number),
                ("4.21 KiB".into(), Tone::Number),
                ("4.21 MiB/s".into(), Tone::Number),
                ("done".into(), Tone::Good)
            ]
        );
    }

    #[test]
    fn hashes_paths_and_links() {
        assert_eq!(
            spans("   3844551..66a800b  main       -> origin/main"),
            [("3844551..66a800b".into(), Tone::Hash)]
        );
        assert_eq!(
            spans(" crates/wundur-app/src/shell.rs | 12 ++++--"),
            [
                ("crates/wundur-app/src/shell.rs".into(), Tone::Path),
                ("12".into(), Tone::Number),
                ("++++".into(), Tone::Good),
                ("--".into(), Tone::Bad)
            ]
        );
        assert_eq!(spans("To https://github.com/a/b.git"), [("https://github.com/a/b.git".into(), Tone::Link)]);
    }

    #[test]
    fn failures_stand_out() {
        assert_eq!(spans("fatal: not a git repository"), [("fatal: not a git repository".into(), Tone::Bad)]);
        assert_eq!(spans("warning: unused import"), [("warning:".into(), Tone::Warn)]);
        assert_eq!(
            spans("test result: FAILED. 3 passed; 1 failed"),
            [
                ("test result:".into(), Tone::Label),
                ("FAILED".into(), Tone::Bad),
                ("3".into(), Tone::Number),
                ("passed".into(), Tone::Good),
                ("1".into(), Tone::Number),
                ("failed".into(), Tone::Bad)
            ]
        );
        assert_eq!(spans("0 errors"), [("0".into(), Tone::Number), ("errors".into(), Tone::Good)]);
    }

    #[test]
    fn plain_prose_stays_plain() {
        assert!(spans("Already on the branch you asked for").is_empty());
        assert!(spans("and/or e.g. it's v").is_empty());
        // A version is not a path.
        assert_eq!(spans("git version 2.50.1"), [("2.50.1".into(), Tone::Number)]);
    }

    #[test]
    fn times_are_dim() {
        assert_eq!(spans("at 12:04:59"), [("12:04:59".into(), Tone::Dim)]);
        assert_eq!(spans("2026-11-16"), [("2026-11-16".into(), Tone::Dim)]);
    }
}
