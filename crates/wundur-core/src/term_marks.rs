//! A terminal's commands, from the marks its shell prints (see [`crate::shell_integration`]).
//!
//! The terminal's parser drops the `OSC 133`/`OSC 633` marks, so [`MarkFilter`] takes them out
//! of the output first and puts in their place a link (`OSC 8`) with a private address,
//! `wundur-mark:<kind>:<id>`. Every character printed after it carries that address in its
//! cell, so a command's marks move with its text when the terminal re-wraps, and go with it
//! when the oldest history is dropped or the screen cleared. [`assemble`] turns the rows' marks
//! back into [`Block`]s; what the cells can't hold (the command's exact text, its exit code)
//! is kept in [`Marks`], by id.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

/// The part of a command a mark begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Prompt,
    Command,
    Output,
}

const SCHEME: &str = "wundur-mark:";

impl Kind {
    fn letter(self) -> char {
        match self {
            Kind::Prompt => 'p',
            Kind::Command => 'c',
            Kind::Output => 'o',
        }
    }
}

/// The link address a mark is written as.
pub fn mark_uri(kind: Kind, id: u64) -> String {
    format!("{SCHEME}{}:{id}", kind.letter())
}

/// A mark from a cell's link address (`None` for an ordinary link).
pub fn parse_mark(uri: &str) -> Option<(Kind, u64)> {
    let rest = uri.strip_prefix(SCHEME)?;
    let (k, id) = rest.split_once(':')?;
    let kind = match k {
        "p" => Kind::Prompt,
        "c" => Kind::Command,
        "o" => Kind::Output,
        _ => return None,
    };
    Some((kind, id.parse().ok()?))
}

/// Where the newest command is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The prompt is being drawn.
    Prompt,
    /// The person is typing.
    Command,
    /// It runs.
    Running,
    /// It finished.
    Done,
}

/// What a command's cells can't hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Info {
    /// The command as the shell read it.
    pub command: Option<String>,
    pub exit: Option<i32>,
    /// How long it ran, once it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
    /// When it started, while it runs (not kept across restarts).
    #[serde(skip)]
    pub started: Option<std::time::Instant>,
}

/// What [`Marks`] keeps across a restart, with the terminal's text (see `term_history`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedMarks {
    pub next: u64,
    pub info: BTreeMap<u64, Info>,
}

/// The marks seen so far, shared by the reader thread and the window.
#[derive(Debug, Default)]
pub struct Marks {
    /// The newest command and where it is.
    pub current: Option<(u64, Phase)>,
    pub info: BTreeMap<u64, Info>,
    /// The shell, as its script names itself (`zsh`, `bash`, `fish`, `pwsh`).
    pub shell: Option<String>,
    next: u64,
}

/// Commands remembered at most (their cells are long gone by then).
const KEPT: usize = 4096;

impl Marks {
    /// What to keep across a restart.
    pub fn saved(&self) -> SavedMarks {
        SavedMarks { next: self.next, info: self.info.clone() }
    }

    /// Commands kept from before a restart: all finished; new ones get new ids.
    pub fn restored(saved: SavedMarks) -> Self {
        Self { current: None, info: saved.info, shell: None, next: saved.next }
    }

    /// The shell has marked anything at all: the terminal has commands.
    pub fn active(&self) -> bool {
        self.shell.is_some() || self.current.is_some()
    }

    /// A command runs: what is typed goes to it, not to the shell.
    pub fn busy(&self) -> bool {
        matches!(self.current, Some((_, Phase::Running)))
    }

    /// At the prompt, nothing running.
    pub fn at_prompt(&self) -> bool {
        matches!(self.current, Some((_, Phase::Prompt | Phase::Command)))
    }

    fn new_id(&mut self) -> u64 {
        self.next += 1;
        self.info.insert(self.next, Info::default());
        while self.info.len() > KEPT {
            self.info.pop_first();
        }
        self.next
    }

    /// The command being prompted for, or a new one.
    fn prompting(&mut self) -> u64 {
        match self.current {
            Some((id, Phase::Prompt | Phase::Command)) => id,
            _ => self.new_id(),
        }
    }
}

/// Takes the shell's marks out of a terminal's output (see the module's notes).
pub struct MarkFilter {
    marks: Arc<Mutex<Marks>>,
    state: State,
    buf: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    Esc,
    Osc,
}

/// Longer than any mark: a sequence this long is not one.
const MAX_MARK: usize = 64 * 1024;

impl MarkFilter {
    pub fn new(marks: Arc<Mutex<Marks>>) -> Self {
        Self { marks, state: State::Normal, buf: Vec::new() }
    }

    /// `input` with its marks replaced, appended to `out`. A mark split across two reads is
    /// held until the rest comes.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Normal => {
                if b == 0x1b {
                    self.state = State::Esc;
                    self.buf.push(b);
                } else {
                    out.push(b);
                }
            }
            State::Esc => {
                if b == b']' {
                    self.buf.push(b);
                    self.state = State::Osc;
                } else {
                    self.flush(out);
                    self.byte(b, out);
                }
            }
            State::Osc => {
                self.buf.push(b);
                let body = &self.buf[2..];
                let ended = b == 0x07 || (b == b'\\' && body.len() >= 2 && body[body.len() - 2] == 0x1b);
                if ended {
                    let end = if b == 0x07 { body.len() - 1 } else { body.len() - 2 };
                    let content = body[..end].to_vec();
                    match self.handle(&content) {
                        Some(replacement) => {
                            out.extend_from_slice(&replacement);
                            self.buf.clear();
                            self.state = State::Normal;
                        }
                        None => self.flush(out),
                    }
                    return;
                }
                let ours = |prefix: &[u8]| {
                    let n = body.len().min(prefix.len());
                    body[..n] == prefix[..n]
                };
                if !(ours(b"133;") || ours(b"633;")) || self.buf.len() > MAX_MARK {
                    // Not a mark: through as it came; the rest of it follows untouched.
                    self.flush(out);
                }
            }
        }
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.buf);
        self.state = State::Normal;
    }

    /// What replaces a mark (`None`: not one, pass it through).
    fn handle(&mut self, content: &[u8]) -> Option<Vec<u8>> {
        let text = String::from_utf8_lossy(content);
        let mut marks = self.marks.lock().ok()?;
        let link = |kind: Kind, id: u64| format!("\x1b]8;id=wundur{id};{}\x1b\\", mark_uri(kind, id)).into_bytes();
        if let Some(rest) = text.strip_prefix("133;") {
            let mut params = rest.split(';');
            return Some(match params.next().unwrap_or("") {
                "A" => {
                    let id = marks.prompting();
                    marks.current = Some((id, Phase::Prompt));
                    link(Kind::Prompt, id)
                }
                "B" => {
                    let id = marks.prompting();
                    marks.current = Some((id, Phase::Command));
                    link(Kind::Command, id)
                }
                "C" => {
                    let id = marks.prompting();
                    marks.current = Some((id, Phase::Running));
                    if let Some(info) = marks.info.get_mut(&id) {
                        info.started = Some(std::time::Instant::now());
                    }
                    link(Kind::Output, id)
                }
                "D" => match marks.current {
                    Some((id, Phase::Running)) => {
                        let exit = params.next().and_then(|c| c.trim().parse().ok());
                        if let Some(info) = marks.info.get_mut(&id) {
                            info.exit = exit;
                            info.millis = info.started.take().map(|t| t.elapsed().as_millis() as u64);
                        }
                        marks.current = Some((id, Phase::Done));
                        b"\x1b]8;;\x1b\\".to_vec()
                    }
                    // A prompt drawn with no command run before it (bash says so each time).
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            });
        }
        if let Some(rest) = text.strip_prefix("633;") {
            if let Some(command) = rest.strip_prefix("E;") {
                let command = unescape(command.split(';').next().unwrap_or(""));
                let id = match marks.current {
                    Some((id, _)) => id,
                    None => {
                        let id = marks.new_id();
                        marks.current = Some((id, Phase::Command));
                        id
                    }
                };
                if let Some(info) = marks.info.get_mut(&id) {
                    info.command = Some(command);
                }
            } else if let Some(shell) = rest.strip_prefix("P;Shell=") {
                marks.shell = Some(shell.to_string());
            }
            return Some(Vec::new());
        }
        None
    }
}

/// A command's text as the scripts escape it: `\\`, `\x3b` (`;`) and `\x0a` (a new line).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("\\\\") {
            out.push('\\');
            rest = r;
        } else if rest.len() >= 4
            && rest.as_bytes()[1] == b'x'
            && let Ok(n) = u8::from_str_radix(&rest[2..4], 16)
        {
            out.push(n as char);
            rest = &rest[4..];
        } else {
            out.push('\\');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// One command on the screen or in the scrollback, by grid line (negative: scrollback).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: u64,
    /// Its first line (the prompt's, when the prompt is still there).
    pub start: i32,
    /// The lines of its prompt and command.
    pub command: Range<i32>,
    /// The lines its output takes (blank lines at its end included).
    pub output: Range<i32>,
}

/// The blocks from the marks found in rows (`(line, marks)`, top to bottom, only rows with
/// marks); `end` is one past the last line the newest command can reach.
pub fn assemble(rows: &[(i32, Vec<(Kind, u64)>)], end: i32) -> Vec<Block> {
    #[derive(Default)]
    struct Seen {
        first: Option<i32>,
        first_pc: Option<i32>,
        last_pc: Option<i32>,
        first_o: Option<i32>,
    }
    let mut seen: BTreeMap<u64, Seen> = BTreeMap::new();
    for (line, marks) in rows {
        for &(kind, id) in marks {
            let s = seen.entry(id).or_default();
            s.first = Some(s.first.map_or(*line, |f| f.min(*line)));
            match kind {
                Kind::Prompt | Kind::Command => {
                    s.first_pc = Some(s.first_pc.map_or(*line, |f| f.min(*line)));
                    s.last_pc = Some(s.last_pc.map_or(*line, |l| l.max(*line)));
                }
                Kind::Output => s.first_o = Some(s.first_o.map_or(*line, |f| f.min(*line))),
            }
        }
    }
    let mut order: Vec<(u64, Seen)> = seen.into_iter().collect();
    order.sort_by_key(|(id, s)| (s.first, *id));
    let mut blocks = Vec::with_capacity(order.len());
    for (i, (id, s)) in order.iter().enumerate() {
        let start = s.first.unwrap_or(0);
        let next = order.get(i + 1).and_then(|(_, n)| n.first).unwrap_or(end).max(start);
        let command = match (s.first_pc, s.last_pc) {
            (Some(a), Some(b)) => a..b + 1,
            _ => start..start,
        };
        // Output starts below the command (a redrawn command line may carry the output mark).
        let out_start = s.first_o.unwrap_or(command.end).max(command.end).min(next);
        blocks.push(Block { id: *id, start, command, output: out_start..next });
    }
    blocks
}

/// The parts of a command line joined by `&&`, `||`, `;`, `|` or `&`, as byte ranges with
/// their outer spaces trimmed; quotes, `\`, `$(…)` and `(…)` respected. One part for a plain
/// command.
pub fn split_command(cmd: &str) -> Vec<Range<usize>> {
    let bytes = cmd.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut i = 0;
    let push = |parts: &mut Vec<Range<usize>>, from: usize, to: usize| {
        let s = &cmd[from..to];
        let lead = s.len() - s.trim_start().len();
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            parts.push(from + lead..from + lead + trimmed.len());
        }
    };
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\\' => i += 1,
            b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'`' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b';' | b'\n' | b'|' | b'&' if depth == 0 => {
                let next = bytes.get(i + 1).copied();
                let prev = i.checked_sub(1).map(|p| bytes[p]);
                // `2>&1`, `&>`, `>|`: redirections, not joins.
                let redirect = (b == b'&' && (prev == Some(b'>') || prev == Some(b'<') || next == Some(b'>')))
                    || (b == b'|' && prev == Some(b'>'));
                if !redirect {
                    push(&mut parts, start, i);
                    // `&&`, `||`, `|&`, `;;`
                    if matches!((b, next), (b'&', Some(b'&')) | (b'|', Some(b'|')) | (b'|', Some(b'&')) | (b';', Some(b';'))) {
                        i += 1;
                    }
                    start = i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    push(&mut parts, start, bytes.len().min(cmd.len()));
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> (MarkFilter, Arc<Mutex<Marks>>) {
        let marks = Arc::new(Mutex::new(Marks::default()));
        (MarkFilter::new(marks.clone()), marks)
    }

    fn run(f: &mut MarkFilter, input: &[u8]) -> String {
        let mut out = Vec::new();
        f.feed(input, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn marks_become_links_and_the_rest_passes() {
        let (mut f, marks) = filter();
        let out = run(&mut f, b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]633;E;ls\x07\x1b]133;C\x07a b\r\n\x1b]133;D;0\x07");
        assert_eq!(
            out,
            "\x1b]8;id=wundur1;wundur-mark:p:1\x1b\\$ \x1b]8;id=wundur1;wundur-mark:c:1\x1b\\ls\r\n\x1b]8;id=wundur1;wundur-mark:o:1\x1b\\a b\r\n\x1b]8;;\x1b\\"
        );
        let m = marks.lock().unwrap();
        assert_eq!(m.info[&1].command.as_deref(), Some("ls"));
        assert_eq!(m.info[&1].exit, Some(0));
        assert!(m.info[&1].millis.is_some() && m.info[&1].started.is_none());
        assert_eq!(m.current, Some((1, Phase::Done)));
    }

    #[test]
    fn other_sequences_are_untouched() {
        let (mut f, _) = filter();
        let input = b"\x1b[31mred\x1b[0m \x1b]0;title\x07 \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ \x1b]1337;x\x07 \x1bc";
        assert_eq!(run(&mut f, input).as_bytes(), input);
    }

    #[test]
    fn a_mark_split_across_reads_is_held() {
        let (mut f, marks) = filter();
        let a = run(&mut f, b"x\x1b]13");
        let b = run(&mut f, b"3;A\x1b");
        let c = run(&mut f, b"\\y");
        assert_eq!(format!("{a}{b}{c}"), "x\x1b]8;id=wundur1;wundur-mark:p:1\x1b\\y");
        assert!(marks.lock().unwrap().at_prompt());
    }

    #[test]
    fn a_redrawn_prompt_keeps_its_command() {
        let (mut f, marks) = filter();
        run(&mut f, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        run(&mut f, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(marks.lock().unwrap().current, Some((1, Phase::Command)));
        run(&mut f, b"\x1b]133;C\x07\x1b]133;D;2\x07\x1b]133;A\x07");
        let m = marks.lock().unwrap();
        assert_eq!(m.info[&1].exit, Some(2));
        assert_eq!(m.current, Some((2, Phase::Prompt)));
    }

    #[test]
    fn a_finish_with_nothing_running_is_dropped() {
        let (mut f, marks) = filter();
        assert_eq!(run(&mut f, b"\x1b]133;D;0\x07\x1b]133;A\x07"), "\x1b]8;id=wundur1;wundur-mark:p:1\x1b\\");
        assert_eq!(marks.lock().unwrap().info[&1].exit, None);
    }

    #[test]
    fn restored_commands_keep_their_ids_clear() {
        let (mut f, marks) = filter();
        run(&mut f, b"\x1b]133;A\x07\x1b]633;E;ls\x07\x1b]133;C\x07\x1b]133;D;3\x07");
        let saved = marks.lock().unwrap().saved();
        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedMarks = serde_json::from_str(&json).unwrap();
        let marks = Arc::new(Mutex::new(Marks::restored(back)));
        assert_eq!(marks.lock().unwrap().info[&1].exit, Some(3));
        let mut f = MarkFilter::new(marks.clone());
        // The next command gets an id the restored one doesn't have.
        assert!(run(&mut f, b"\x1b]133;A\x07").contains("wundur-mark:p:2"));
    }

    #[test]
    fn command_text_is_unescaped() {
        assert_eq!(unescape(r"echo a\x3b b \\ c\x0ad"), "echo a; b \\ c\nd");
    }

    #[test]
    fn marks_parse_back() {
        assert_eq!(parse_mark(&mark_uri(Kind::Output, 42)), Some((Kind::Output, 42)));
        assert_eq!(parse_mark("https://example.com"), None);
    }

    #[test]
    fn blocks_from_rows() {
        // Line 0: prompt and command; 1-3: output; 4: next prompt; 5: its output (running).
        let rows = vec![
            (0, vec![(Kind::Prompt, 1), (Kind::Command, 1)]),
            (1, vec![(Kind::Output, 1)]),
            (3, vec![(Kind::Output, 1)]),
            (4, vec![(Kind::Prompt, 2), (Kind::Command, 2)]),
            (5, vec![(Kind::Output, 2)]),
        ];
        let blocks = assemble(&rows, 8);
        assert_eq!(blocks[0], Block { id: 1, start: 0, command: 0..1, output: 1..4 });
        assert_eq!(blocks[1], Block { id: 2, start: 4, command: 4..5, output: 5..8 });
    }

    #[test]
    fn output_starts_below_a_redrawn_command_line() {
        // The command line was redrawn after the output mark began (PowerShell does).
        let rows = vec![(0, vec![(Kind::Prompt, 1), (Kind::Output, 1)]), (2, vec![(Kind::Prompt, 2)])];
        assert_eq!(assemble(&rows, 3)[0].output, 1..2);
    }

    #[test]
    fn a_block_whose_prompt_scrolled_away_keeps_its_output() {
        let rows = vec![(-5, vec![(Kind::Output, 1)]), (0, vec![(Kind::Prompt, 2)])];
        assert_eq!(assemble(&rows, 1)[0], Block { id: 1, start: -5, command: -5..-5, output: -5..0 });
    }

    fn parts(cmd: &str) -> Vec<&str> {
        split_command(cmd).into_iter().map(|r| &cmd[r]).collect()
    }

    #[test]
    fn splits_joined_commands() {
        assert_eq!(parts("git checkout main &&  git pull origin main && claude"), ["git checkout main", "git pull origin main", "claude"]);
        assert_eq!(parts("a || b; c | d & e"), ["a", "b", "c", "d", "e"]);
        assert_eq!(parts("ls"), ["ls"]);
    }

    #[test]
    fn quotes_and_redirections_stay_whole() {
        assert_eq!(parts(r#"echo "a && b" 'c; d' && e"#), [r#"echo "a && b" 'c; d'"#, "e"]);
        assert_eq!(parts("make 2>&1 | tee log"), ["make 2>&1", "tee log"]);
        assert_eq!(parts("cmd &> out && x"), ["cmd &> out", "x"]);
        assert_eq!(parts("echo $(a && b) && c"), ["echo $(a && b)", "c"]);
        assert_eq!(parts(r"echo a\;b; c"), [r"echo a\;b", "c"]);
    }
}
