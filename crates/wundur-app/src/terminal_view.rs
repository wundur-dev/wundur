//! The terminal panel: one or more shells, each drawn from its alacritty grid.
//!
//! Drawing: per visible row, cell backgrounds as quads, then the row's text shaped once with a
//! run per style change. Nothing redraws unless the PTY thread reports new output (coalesced
//! to one wakeup per frame) or the user does something.

use std::cell::Cell;
use std::rc::Rc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::palette;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::Sizable;
use crate::pty::{GridSize, Pty, PtyEvent};
use crate::term_blocks::{self, BlockView, DisplayRow, Fold, Part, Placed};

/// The fixed-width font every system has: Menlo on macOS, Consolas on Windows.
pub const FONT_FAMILY: &str = if cfg!(windows) { "Consolas" } else if cfg!(target_os = "linux") { "DejaVu Sans Mono" } else { "Menlo" };

/// The placeholder over a starting program goes once its output has paused this long, or this
/// long after its first output at most, looked for this often, and fades out over this.
const SETTLE_QUIET: std::time::Duration = std::time::Duration::from_millis(250);
const SETTLE_AT_MOST: std::time::Duration = std::time::Duration::from_secs(3);
/// A program silent this long after it started: the placeholder's spinner stops (an animation
/// draws the window on every frame), its words stay.
const SPIN_AT_MOST: std::time::Duration = std::time::Duration::from_secs(20);
const SETTLE_CHECK: std::time::Duration = std::time::Duration::from_millis(80);
const PLACEHOLDER_FADE: std::time::Duration = std::time::Duration::from_millis(180);
/// The font size, from Settings.
pub fn font_size(cx: &App) -> f32 {
    crate::prefs(cx).terminal_font_size()
}

/// One step of a terminal's zoom, in points.
const ZOOM_STEP: f32 = 1.;
/// Zoom steps closer together than this are one change for the program inside: it is told its
/// new size once they stop (each resize redraws an interface's whole screen and reflows the
/// scrollback).
const ZOOM_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

fn hsla(rgb: u32) -> Hsla {
    gpui_kit::rgb(rgb).into()
}

/// What one turn of the wheel does in a terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum Wheel {
    /// Scroll our own scrollback by this many lines (up is positive).
    Scrollback(i32),
    /// Send this to the program.
    Send(String),
}

/// The wheel, as terminals do it: a program that asked for mouse reports gets wheel events
/// (buttons 64 and 65; SGR form when it asked for that); a full-screen program in the
/// alternate screen with alternate scroll on gets arrow keys; otherwise the scrollback
/// scrolls. `col` and `row` are 1-based cells under the pointer.
pub fn wheel_input(mode: TermMode, lines: i32, col: usize, row: usize) -> Wheel {
    let n = lines.unsigned_abs() as usize;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button = if lines > 0 { 64 } else { 65 };
        let one = if mode.contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{button};{col};{row}M")
        } else {
            // The old X10 form: each value plus 32, as one character (cells past 223 clamp).
            let c = |v: usize| char::from((32 + v.min(223)) as u8);
            format!("\x1b[M{}{}{}", c(button), c(col), c(row))
        };
        return Wheel::Send(one.repeat(n));
    }
    if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let key = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => "\x1bOA",
            (false, true) => "\x1bOB",
            (true, false) => "\x1b[A",
            (false, false) => "\x1b[B",
        };
        return Wheel::Send(key.repeat(n));
    }
    Wheel::Scrollback(lines)
}

pub struct TerminalView {
    pub pty: Option<Rc<Pty>>,
    pub focus: FocusHandle,
    pub title: Option<String>,
    pub exited: bool,
    /// Output arrived since the user last looked (the sidebar shows a dot).
    pub unseen: bool,
    /// The program has drawn something (the screen is no longer blank).
    pub drawn: bool,
    /// Shown over the screen while the program starts: the shape of the conversation's last
    /// lines, a spinner and what is starting. It stays until the program has drawn and then
    /// gone quiet for a moment (an interface draws its banner, then the conversation, in
    /// steps), then fades out.
    pub placeholder: Option<Placeholder>,
    /// The program's first screen is complete: the placeholder fades (from `revealed`).
    settled: bool,
    revealed: Option<std::time::Instant>,
    /// When output last came, and when the first did.
    last_output: Option<std::time::Instant>,
    first_output: Option<std::time::Instant>,
    settling: Option<Task<()>>,
    /// For an agent's interface: what Cmd+V with an image does (see [`ImageSupport`]).
    pub images: Option<ImageSupport>,
    /// The image whose `[Image #N]` marker is under the pointer, and where.
    hover_image: Option<(std::path::PathBuf, Point<Pixels>)>,
    error: Option<String>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    cell: Rc<Cell<Size<Pixels>>>,
    selecting: bool,
    /// A click that began on a web address: opened when the button comes up without a drag.
    pending_link: Option<(String, usize)>,
    /// The web address under the pointer: its row and columns (underlined, a hand pointer).
    hover_link: Option<(i32, usize, usize)>,
    /// Wheel movement smaller than a line, kept for the next event (trackpads send many
    /// small deltas).
    wheel_rest: f32,
    /// This terminal's zoom: points over (or under) the size in Settings, for as long as it is
    /// open; and when it last changed (the program's size waits until zooming stops).
    zoom: f32,
    /// An agent's own interface (its size is the agents', not the shells').
    pub agent: bool,
    zoomed_at: Rc<Cell<Option<std::time::Instant>>>,
    zoom_settle: Option<Task<()>>,
    /// 1, 2, ... in opening order: the key of its saved name.
    pub number: usize,
    /// Drawn in a frame on screen now: output redraws the window only then (or when the tab's
    /// title changes).
    on_screen: crate::on_screen::OnScreen,
    started: std::time::Instant,
    /// The foreground program and when it was asked: the tab's title asks on every frame, the
    /// system once a second.
    program: std::cell::RefCell<(Option<std::time::Instant>, Option<String>)>,
    /// For the automatic name: the folder, its git branch and the login shell.
    pub folder: std::path::PathBuf,
    pub branch: Option<String>,
    _events: Option<Task<()>>,
    /// Kept across restarts under this id (shell terminals; not an agent's own interface).
    pub history: Option<String>,
    /// Writing what it shows, a moment after its output stops.
    save: Option<Task<()>>,
    /// Writes it as Wundur quits.
    _on_quit: Option<Subscription>,
    /// The shell's commands (see [`term_blocks`]): read from the grid at most once per batch
    /// of output (`generation` counts the batches).
    blocks: Rc<std::cell::RefCell<BlocksCache>>,
    generation: u64,
    /// Outputs the person folded (`true`) or opened (`false`); the rest fold when long.
    folds: Rc<std::cell::RefCell<std::collections::HashMap<u64, Fold>>>,
    /// Its shell marks its commands: a gutter on the left holds each one's chevron and status.
    integrated: bool,
    /// Redraws once a second while a command runs, for its time.
    _ticker: Option<Task<()>>,
    /// The rows drawn last, top to bottom, when a fold changes what shows (else empty: row
    /// `n` is grid line `n - offset`, as usual).
    shown: Rc<std::cell::RefCell<Vec<Placed>>>,
    /// The command under the pointer (its buttons show), and the part of its command line.
    hover_block: Option<u64>,
    hover_part: Option<Part>,
    hover_fold: bool,
    /// The pointer is over it (its Blocks / Raw switch shows).
    hovered: bool,
    /// Raw: as the program printed it, without headers, folds, buttons or colouring (the
    /// commands are still followed, for switching back).
    raw: bool,
    /// A click that began on a part of a command line: put on the prompt when the button comes
    /// up without a drag.
    pending_part: Option<Part>,
    /// A command whose output was just copied, and when (its button says so for a moment).
    copied: Option<(u64, bool, std::time::Instant)>,
}

/// The gutter left of the grid in a terminal whose shell marks its commands.
const GUTTER: f32 = 30.;

/// A command's header: the rows of its prompt and command line, drawn as one.
struct Header {
    /// Its first line (the rule goes above it).
    start: i32,
    rows: std::ops::Range<i32>,
    status: Status,
    /// `Some(open)` when it has output to collapse.
    chevron: Option<bool>,
    /// Shown on the right: its time, and its exit code when it failed.
    meta: Vec<(String, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    None,
    Ok,
    Failed,
    Running,
}

/// "0.4s", "12s", "2m 05s", "1h 02m".
fn duration_text(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..10 => format!("{:.1}s", ms as f64 / 1000.),
        10..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// The headers of `blocks`, from what the marks know of them.
fn headers(blocks: &[BlockView], folds: &std::collections::HashMap<u64, Fold>) -> Vec<Header> {
    let dark = palette::is_dark();
    let red = if dark { 0xe06c6c } else { 0xc0392b };
    let blue = if dark { 0x6aa6ff } else { 0x2f6fde };
    blocks
        .iter()
        .filter(|b| !b.block.command.is_empty())
        .map(|b| {
            let status = match (b.running, b.info.exit) {
                (true, _) => Status::Running,
                (false, Some(0)) => Status::Ok,
                (false, Some(_)) => Status::Failed,
                (false, None) => Status::None,
            };
            let mut meta = Vec::new();
            if b.running {
                let ms = b.info.started.map_or(0, |t| t.elapsed().as_millis() as u64);
                meta.push((format!("Running · {}", duration_text(ms)), blue));
            } else if let Some(ms) = b.info.millis {
                meta.push((duration_text(ms), palette::dim()));
            }
            if let Some(code) = b.info.exit.filter(|c| *c != 0) {
                meta.push((format!("Exit {code}"), red));
            }
            let chevron = (b.output_len() > 0).then(|| term_blocks::fold_of(b, folds) != Fold::All);
            Header { start: b.block.start, rows: b.block.command.clone(), status, chevron, meta }
        })
        .collect()
}

#[derive(Default)]
struct BlocksCache {
    key: (u64, usize, usize, usize),
    list: Rc<Vec<BlockView>>,
}

/// A terminal's text and commands, to write where they're kept.
struct Kept {
    path: std::path::PathBuf,
    text: String,
    marks: Option<(std::path::PathBuf, String)>,
}

impl Kept {
    fn write(self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, self.text);
        if let Some((path, json)) = self.marks {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Cmd on macOS, Ctrl elsewhere.
fn secondary(m: &Modifiers) -> bool {
    if cfg!(target_os = "macos") { m.platform } else { m.control }
}

/// The commands in `pty`'s grid, read again only when its output or size changed.
///
/// Locks the grid, then the marks: the order the PTY thread takes them in (it holds the grid
/// while it reads, and the reading takes the marks).
fn cached_blocks(cache: &std::cell::RefCell<BlocksCache>, pty: &Pty, generation: u64) -> Rc<Vec<BlockView>> {
    let term = pty.term.lock();
    let marks = match pty.marks.lock() {
        Ok(m) if m.active() => m,
        _ => return Rc::default(),
    };
    let grid = term.grid();
    let key = (generation, grid.history_size(), grid.columns(), grid.screen_lines());
    let mut cache = cache.borrow_mut();
    if cache.key != key {
        cache.key = key;
        cache.list = Rc::new(term_blocks::scan(&term, &marks));
    }
    cache.list.clone()
}

/// The rows to draw when a fold changes them (`None`: as usual, the grid's own).
/// The space above each command's header, in pixels: commands read as separate, while the
/// rows of output stay a line apart (aligned columns and box drawing need them so).
const COMMAND_GAP: f32 = 10.;

/// The gap above `row`, given the lines commands start on.
fn gap_for(starts: &[i32]) -> impl Fn(DisplayRow) -> f32 + '_ {
    move |row| match row {
        DisplayRow::Line(l) if starts.binary_search(&l).is_ok() => COMMAND_GAP,
        _ => 0.,
    }
}

/// The lines commands start on (where a gap goes), in order.
fn command_starts(blocks: &[BlockView]) -> Vec<i32> {
    let mut v: Vec<i32> = blocks.iter().filter(|b| !b.block.command.is_empty()).map(|b| b.block.start).collect();
    v.sort_unstable();
    v
}

/// The rows to draw and where, in a shell's terminal with commands (`None`: as usual, the
/// grid's own rows a line apart). At the bottom, the last row is the cursor's or the last
/// with text, so blank rows below it don't push the commands off the top.
fn layout(pty: &Pty, blocks: &[BlockView], folds: &std::collections::HashMap<u64, Fold>, line_h: f32, avail: f32) -> Option<Vec<Placed>> {
    if blocks.is_empty() {
        return None;
    }
    let term = pty.term.lock();
    let grid = term.grid();
    let screen = grid.screen_lines() as i32;
    let offset = grid.display_offset() as i32;
    let all = term_blocks::display_rows(blocks, folds, -(grid.history_size() as i32), screen);
    let bottom_line = if offset == 0 {
        let cols = grid.columns();
        let last_text = (0..screen).rev().find(|&l| (0..cols).any(|c| !matches!(grid[Line(l)][Column(c)].c, ' ' | '\0'))).unwrap_or(0);
        last_text.max(grid.cursor.point.line.0)
    } else {
        screen - 1 - offset
    };
    let bottom = term_blocks::index_of(&all, bottom_line).unwrap_or(all.len().saturating_sub(1));
    let starts = command_starts(blocks);
    Some(term_blocks::place(&all, bottom, avail, line_h, &gap_for(&starts)))
}

/// Keeps a pasted image (its bytes and file extension); the file it is kept in.
pub type PastedFn = dyn Fn(Vec<u8>, &'static str, &mut App) -> Option<std::path::PathBuf>;
/// What to type to attach an image file (bracketed paste or not).
pub type AttachFn = dyn Fn(&std::path::Path, bool) -> Option<Vec<u8>>;

/// How an image on the clipboard reaches the interface.
#[derive(Clone)]
pub enum PasteImage {
    /// A key that makes the interface read the clipboard itself.
    Key(Vec<u8>),
    /// The kept copy's path, typed in (the interface attaches the file at that path).
    Path(Rc<AttachFn>),
}
/// The image behind `[Image #N]`.
pub type ImageFn = dyn Fn(usize, &App) -> Option<std::path::PathBuf>;

/// Images in an agent's interface: Cmd+V with an image on the clipboard has `pasted` keep a
/// copy, then hands it over as `paste` says; image files on the clipboard (copied in Finder)
/// are kept the same way and the copy's path typed with `attach`; `image` gives the image
/// behind an `[Image #N]` marker on screen, for a preview while the pointer is over it.
#[derive(Clone)]
pub struct ImageSupport {
    pub paste: PasteImage,
    pub attach: Option<Rc<AttachFn>>,
    pub pasted: Rc<PastedFn>,
    pub image: Rc<ImageFn>,
}

/// The extension of an image file an agent's interface takes (`None` for any other file).
fn image_ext(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        _ => None,
    }
}

/// Paths as the macOS Terminal pastes them: whole, a backslash before each space and other
/// character a shell would read, separated by spaces.
pub fn shell_paths(paths: &[std::path::PathBuf]) -> String {
    paths
        .iter()
        .map(|p| {
            p.to_string_lossy()
                .chars()
                .flat_map(|c| {
                    let special = c.is_whitespace() || "\\'\"$`!&*()[]{}<>|;?#~".contains(c);
                    special.then_some('\\').into_iter().chain(std::iter::once(c))
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `N` of an `[Image #N]` marker in `line` covering column `col`.
pub fn image_marker_at(line: &str, col: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let text: String = chars.iter().collect();
    let mut from = 0;
    while let Some(i) = text[from..].find("[Image #") {
        let start = from + i;
        let end = start + text[start..].find(']')?;
        let (s, e) = (text[..start].chars().count(), text[..end].chars().count());
        if (s..=e).contains(&col) {
            return text[start + 8..end].parse().ok();
        }
        from = end;
    }
    None
}

/// What a terminal shows while its program starts.
pub struct Placeholder {
    /// The last lines of the conversation so far, oldest first.
    pub lines: Vec<(bool, String)>,
    /// "Starting Claude Code..."
    pub label: String,
}

pub enum TerminalEvent {
    Closed,
    Activated,
    /// The program wrote something (or exited): sent for every batch, drawn or not, for whoever
    /// follows what it shows (an agent's interface read for its reply).
    Output,
    /// A link clicked: the text of its row and the column (in characters), for whoever knows
    /// the project's files to open the file or page named there. A click on a web address,
    /// or Cmd+click on anything (`browser`: a web address then goes to the browser).
    OpenLink { line: String, column: usize, browser: bool },
    /// A command and its output, for the session's message box (`label`: the pill's name).
    SendToChat { text: String, label: String },
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    pub fn new(spec: wundur_core::host::CommandSpec, cx: &mut Context<Self>) -> Self {
        Self::start(spec, None, cx)
    }

    /// A shell terminal kept across restarts as `id`: what it showed last time comes first.
    pub fn kept(spec: wundur_core::host::CommandSpec, id: String, cx: &mut Context<Self>) -> Self {
        Self::start(spec, Some(id), cx)
    }

    fn start(spec: wundur_core::host::CommandSpec, history: Option<String>, cx: &mut Context<Self>) -> Self {
        // Settings: not kept, so neither shown again nor saved.
        let history = history.filter(|_| crate::prefs(cx).keep_terminal_history);
        // A shell on this machine marks its commands (a remote one has no `cwd` here).
        let integrated_spec = match crate::prefs(cx).shell_integration && spec.cwd.is_some() {
            true => wundur_core::shell_integration::dir().and_then(|dir| wundur_core::shell_integration::integrate(&spec, &dir)),
            false => None,
        };
        let integrated = integrated_spec.is_some();
        let spec = integrated_spec.unwrap_or(spec);
        let preface = history
            .as_deref()
            .and_then(wundur_core::term_history::path)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|kept| wundur_core::term_history::preface(&kept));
        // Its commands, with what was kept of them (only alongside the text they belong to).
        let saved_marks = preface
            .as_ref()
            .and(history.as_deref())
            .and_then(wundur_core::term_history::marks_path)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|json| serde_json::from_str::<wundur_core::term_marks::SavedMarks>(&json).ok());
        let focus = cx.focus_handle();
        // Until it is first drawn (an agent's interface may start in a hidden view).
        let initial = GridSize {
            cols: 120,
            rows: 36,
            cell_w: 8,
            cell_h: 16,
        };
        let (pty, error, events) = match Pty::spawn(&spec, initial, preface.as_deref(), saved_marks) {
            Ok((pty, rx)) => {
                let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    while let Ok(first) = rx.recv().await {
                        // Take everything already queued: one redraw for the lot.
                        let mut batch = vec![first];
                        while let Ok(more) = rx.try_recv() {
                            batch.push(more);
                        }
                        let alive = this.update(cx, |view, cx| {
                            // What the tab's title shows, before: a change there redraws even
                            // when the screen is not shown.
                            let before = (view.unseen, view.exited, view.title.clone());
                            for ev in batch {
                                match ev {
                                    PtyEvent::Wakeup => {
                                        view.generation += 1;
                                        crate::perf::pty_output();
                                        view.unseen = true;
                                        view.schedule_save(cx);
                                        if !view.drawn && !view.screen_text().trim().is_empty() {
                                            view.drawn = true;
                                            crate::perf::switch_step("terminal drew its first screen");
                                        }
                                        view.output_came(cx);
                                    }
                                    PtyEvent::Title(t) => view.title = Some(t),
                                    PtyEvent::Copy(text) => {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text))
                                    }
                                    PtyEvent::Exited => {
                                        view.exited = true;
                                        crate::perf::switch_step("terminal process exited");
                                    }
                                }
                            }
                            cx.emit(TerminalEvent::Output);
                            let titled = before != (view.unseen, view.exited, view.title.clone()) || view.program_changed();
                            if view.on_screen.now() || titled {
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                });
                (Some(Rc::new(pty)), None, Some(task))
            }
            Err(e) => (None, Some(format!("Could not start the shell: {e}")), None),
        };
        Self {
            pty,
            focus,
            title: None,
            exited: false,
            unseen: false,
            drawn: false,
            placeholder: None,
            settled: false,
            revealed: None,
            last_output: None,
            first_output: None,
            settling: None,
            images: None,
            hover_image: None,
            pending_link: None,
            hover_link: None,
            error,
            bounds: Rc::new(Cell::new(Bounds::default())),
            cell: Rc::new(Cell::new(size(px(8.), px(16.)))),
            selecting: false,
            wheel_rest: 0.,
            zoom: 0.,
            agent: false,
            zoomed_at: Rc::default(),
            zoom_settle: None,
            number: 0,
            on_screen: Default::default(),
            started: std::time::Instant::now(),
            program: Default::default(),
            folder: std::path::PathBuf::new(),
            branch: None,
            _events: events,
            _on_quit: history.is_some().then(|| {
                cx.on_app_quit(|view: &mut Self, cx| {
                    view.save_now(cx);
                    async {}
                })
            }),
            history,
            save: None,
            blocks: Rc::default(),
            generation: 0,
            folds: Rc::default(),
            shown: Rc::default(),
            integrated,
            // A running command's time moves once a second, when shown.
            _ticker: integrated.then(|| {
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                    cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                    let alive = this.update(cx, |v, cx| {
                        let busy = v.pty.as_ref().is_some_and(|p| p.marks.lock().is_ok_and(|m| m.busy()));
                        if busy && v.on_screen.now() {
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                })
            }),
            hover_block: None,
            hover_part: None,
            hover_fold: false,
            hovered: false,
            raw: false,
            pending_part: None,
            copied: None,
        }
    }

    /// Keep what it shows, two seconds after its output stops (a running program writing
    /// steadily is saved at most every two seconds).
    /// Output came: once the screen has something and the output pauses (or after a while,
    /// for a program that never pauses), the placeholder fades.
    fn output_came(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        self.last_output = Some(now);
        self.first_output.get_or_insert(now);
        if self.settled || self.placeholder.is_none() || !self.drawn || self.settling.is_some() {
            return;
        }
        self.settling = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(SETTLE_CHECK).await;
                let done = this.update(cx, |view, cx| {
                    let quiet = view.last_output.is_none_or(|t| t.elapsed() >= SETTLE_QUIET);
                    let long = view.first_output.is_some_and(|t| t.elapsed() >= SETTLE_AT_MOST);
                    if !(quiet || long || view.exited) {
                        return false;
                    }
                    view.settled = true;
                    view.revealed = Some(std::time::Instant::now());
                    view.settling = None;
                    cx.notify();
                    true
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
            // Once more when the fade is over, to take the placeholder away.
            cx.background_executor().timer(PLACEHOLDER_FADE).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.history.is_none() || self.save.is_some() || !crate::prefs(cx).keep_terminal_history {
            return;
        }
        self.save = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let Ok(Some(kept)) = this.update(cx, |view, _| {
                view.save = None;
                view.to_keep()
            }) else {
                return;
            };
            // Detached: this task's handle was just let go, which would cancel an await here.
            cx.background_spawn(async move { kept.write() }).detach();
        }));
    }

    /// Kept now, as Wundur quits: what came in the last two seconds would otherwise be lost
    /// (a `./scripts/restart` quits the moment its build ends).
    fn save_now(&mut self, cx: &mut Context<Self>) {
        if self.history.is_none() || !crate::prefs(cx).keep_terminal_history {
            return;
        }
        self.save = None;
        if let Some(kept) = self.to_keep() {
            kept.write();
        }
    }

    /// What to keep of it now: its text and its commands.
    fn to_keep(&self) -> Option<Kept> {
        let id = self.history.as_deref()?;
        let path = wundur_core::term_history::path(id)?;
        let marks = self.pty.as_ref().and_then(|p| {
            let m = p.marks.lock().ok()?;
            m.active().then_some(())?;
            Some((wundur_core::term_history::marks_path(id)?, serde_json::to_string(&m.saved()).ok()?))
        });
        Some(Kept { path, text: self.kept_text()?, marks })
    }

    /// The scrollback (its last 3,000 lines) and screen as text with colour codes; `None`
    /// while a full-screen program (an editor, a pager) has the screen, so what was kept
    /// before stays.
    fn kept_text(&self) -> Option<String> {
        use wundur_core::term_history::{Ink, Style, to_ansi};
        use alacritty_terminal::vte::ansi::Color;
        let pty = self.pty.as_ref()?;
        let term = pty.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = term.grid();
        let ink = |c: Color| match c {
            Color::Named(n) if (n as usize) < 16 => Ink::Named(n as u8),
            Color::Named(_) => Ink::Default,
            Color::Indexed(n) => Ink::Indexed(n),
            Color::Spec(rgb) => Ink::Rgb(rgb.r, rgb.g, rgb.b),
        };
        let cols = grid.columns();
        let first = -(grid.history_size().min(3000) as i32);
        let rows = (first..grid.screen_lines() as i32).map(|l| {
            let row = &grid[Line(l)];
            let cells = (0..cols)
                .map(|c| &row[Column(c)])
                .filter(|cell| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
                .map(|cell| {
                    let f = cell.flags;
                    let style = Style {
                        fg: ink(cell.fg),
                        bg: ink(cell.bg),
                        bold: f.contains(Flags::BOLD),
                        dim: f.contains(Flags::DIM),
                        italic: f.contains(Flags::ITALIC),
                        underline: f.intersects(Flags::ALL_UNDERLINES),
                        inverse: f.contains(Flags::INVERSE),
                        mark: cell.hyperlink().and_then(|h| wundur_core::term_marks::parse_mark(h.uri())),
                    };
                    // A tab is kept as the space it takes, so columns line up when replayed.
                    (if cell.c == '\t' { ' ' } else { cell.c }, style)
                })
                .collect::<Vec<_>>();
            (cells, row[Column(cols - 1)].flags.contains(Flags::WRAPLINE))
        });
        Some(to_ansi(rows.collect::<Vec<_>>()))
    }

    fn mode(&self) -> TermMode {
        self.pty
            .as_ref()
            .map(|p| *p.term.lock().mode())
            .unwrap_or(TermMode::empty())
    }

    /// The program asked for bracketed paste.
    pub fn bracketed_paste(&self) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE)
    }

    fn write_str(&self, s: &str) {
        if let Some(pty) = &self.pty {
            pty.write(s.as_bytes().to_vec());
        }
    }

    /// Clear (Cmd+K): the scrollback goes, and at a shell's prompt the screen too: the shell is
    /// sent Ctrl+L to redraw its prompt at the top, and what that pushed into the scrollback
    /// goes once it has. A full-screen program (an editor) keeps its screen.
    fn clear(&mut self, cx: &mut Context<Self>) {
        let Some(pty) = self.pty.clone() else { return };
        let full_screen = {
            let mut term = pty.term.lock();
            term.grid_mut().clear_history();
            term.mode().contains(TermMode::ALT_SCREEN)
        };
        if !full_screen {
            pty.write(b"\x0c".to_vec());
            let pty = Rc::downgrade(&pty);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
                if let Some(pty) = pty.upgrade() {
                    pty.term.lock().grid_mut().clear_history();
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        cx.notify();
    }

    fn scroll_to_bottom(&self) {
        if let Some(pty) = &self.pty {
            pty.term.lock().scroll_display(Scroll::Bottom);
        }
    }

    /// The visible screen as text, one line per row (tests read it over the control channel).
    /// The last `max` lines of scrollback and the screen, top to bottom (trailing spaces
    /// trimmed), with the width in columns.
    pub fn recent_lines(&self, max: usize) -> Option<(Vec<String>, usize)> {
        let pty = self.pty.as_ref()?;
        let term = pty.term.lock();
        let grid = term.grid();
        let screen = grid.screen_lines() as i32;
        let history = (grid.history_size() as i32).min(max.saturating_sub(screen as usize) as i32);
        let lines = (-history..screen)
            .map(|l| {
                let row = &grid[Line(l)];
                let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
                text.trim_end().to_string()
            })
            .collect();
        Some((lines, grid.columns()))
    }

    pub fn screen_text(&self) -> String {
        let Some(pty) = &self.pty else { return String::new() };
        let term = pty.term.lock();
        let grid = term.grid();
        let mut out = String::new();
        for line in 0..grid.screen_lines() {
            let row = &grid[Line(line as i32)];
            let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
            out.push_str(text.trim_end());
            out.push('\n');
        }
        out
    }

    fn copy(&self, cx: &mut Context<Self>) -> bool {
        let Some(pty) = &self.pty else { return false };
        let text = pty.term.lock().selection_to_string();
        match text {
            Some(t) if !t.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(t));
                true
            }
            _ => false,
        }
    }

    /// Files pasted or dropped: images an agent's interface takes are kept and attached (the
    /// copy's path typed, which it turns into `[Image #N]`); the rest are typed as whole paths,
    /// as the macOS Terminal does.
    fn paste_files(&self, files: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let Some(pty) = self.pty.clone() else { return };
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        let mut rest = Vec::new();
        for path in files {
            let attached = match (&self.images, image_ext(&path)) {
                (Some(images), Some(ext)) => images.attach.as_ref().and_then(|attach| {
                    let kept = std::fs::read(&path).ok().and_then(|bytes| (images.pasted)(bytes, ext, cx))?;
                    attach(&kept, bracketed)
                }),
                _ => None,
            };
            match attached {
                Some(keys) => pty.write(keys),
                None => rest.push(path),
            }
        }
        if !rest.is_empty() {
            let text = shell_paths(&rest);
            self.write_str(&if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text });
        }
    }

    /// Tests: Cmd+V.
    pub fn test_paste(&mut self, cx: &mut Context<Self>) {
        self.paste(cx);
    }

    fn paste(&self, cx: &mut Context<Self>) {
        let item = cx.read_from_clipboard();
        if let (Some(images), Some(item)) = (self.images.clone(), item.as_ref()) {
            let image = item.entries().iter().find_map(|e| match e {
                ClipboardEntry::Image(i) => Some(i.clone()),
                _ => None,
            });
            if let Some(image) = image {
                // A copy is kept (to show, and for an interface that takes a path); the
                // interface makes its own `[Image #N]`.
                let ext = match image.format {
                    ImageFormat::Jpeg => "jpg",
                    ImageFormat::Gif => "gif",
                    ImageFormat::Webp => "webp",
                    _ => "png",
                };
                let kept = (images.pasted)(image.bytes.clone(), ext, cx);
                let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
                let keys = match &images.paste {
                    PasteImage::Key(key) => Some(key.clone()),
                    PasteImage::Path(attach) => kept.and_then(|path| attach(&path, bracketed)),
                };
                if let (Some(pty), Some(keys)) = (&self.pty, keys) {
                    pty.write(keys);
                }
                return;
            }
        }
        // Files copied in Finder: the clipboard has their paths (its text is only their names).
        let files: Vec<std::path::PathBuf> = item
            .iter()
            .flat_map(|i| i.entries())
            .filter_map(|e| match e {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !files.is_empty() {
            self.paste_files(files, cx);
            return;
        }
        let Some(text) = item.and_then(|c| c.text()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            self.write_str(&format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")));
        } else {
            self.write_str(&text);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;
        // Cmd+Up/Down (Ctrl elsewhere): the previous or next command, in a shell's terminal.
        if self.integrated
            && secondary(&m)
            && !m.shift
            && !m.alt
            && matches!(ks.key.as_str(), "up" | "down")
            && !self.mode().contains(TermMode::ALT_SCREEN)
        {
            self.jump_command(ks.key == "up", cx);
            cx.stop_propagation();
            return;
        }
        if m.platform {
            match ks.key.as_str() {
                "c" => {
                    if self.copy(cx) {
                        cx.stop_propagation();
                    }
                }
                "v" => {
                    self.paste(cx);
                    cx.stop_propagation();
                }
                "k" => {
                    // Clear, as in Terminal.app: the scrollback goes too (and so does what is
                    // kept of it), then the shell redraws its prompt at the top.
                    if let Some(pty) = &self.pty {
                        // Screen and scrollback cleared here first: cleared by the shell alone,
                        // the screen would be pushed into the scrollback.
                        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
                        parser.advance(&mut *pty.term.lock(), b"\x1b[H\x1b[2J\x1b[3J");
                    }
                    self.write_str("\x0c");
                    self.schedule_save(cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        }
        if m.shift && matches!(ks.key.as_str(), "pageup" | "pagedown") {
            if let Some(pty) = &self.pty {
                let rows = pty.size().rows as i32;
                let delta = if ks.key == "pageup" { rows } else { -rows };
                self.scroll_rows(delta);
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = key_bytes(ks, self.mode()) {
            self.scroll_to_bottom();
            if let Some(pty) = &self.pty {
                pty.term.lock().selection = None;
                crate::perf::key_pressed();
                pty.write(bytes);
            }
            cx.stop_propagation();
        }
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let b = self.bounds.get();
        let c = self.cell.get();
        let x = (position.x - b.origin.x).max(px(0.));
        let y = (position.y - b.origin.y).max(px(0.));
        let col = (x / c.width) as usize;
        let row = (y / c.height) as usize;
        let side = if (x / c.width).fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let Some(pty) = &self.pty else {
            return (GridPoint::new(Line(0), Column(0)), side);
        };
        let term = pty.term.lock();
        let offset = term.grid().display_offset() as i32;
        let row = row.min(term.screen_lines().saturating_sub(1));
        let col = col.min(term.columns().saturating_sub(1));
        let line = match self.placed_at(position.y) {
            Some(p) => p.row.line(),
            None if self.shown.borrow().is_empty() => row as i32 - offset,
            // Below the last row drawn: the last.
            None => self.shown.borrow().last().map_or(row as i32 - offset, |p| p.row.line()),
        };
        (GridPoint::new(Line(line), Column(col)), side)
    }

    /// The row drawn under `position` (a grid line, or a fold).
    fn row_at(&self, position: Point<Pixels>) -> Option<DisplayRow> {
        let b = self.bounds.get();
        let y = position.y - b.origin.y;
        if y < px(0.) {
            return None;
        }
        let row = (y / self.cell.get().height) as usize;
        if !self.shown.borrow().is_empty() {
            return self.placed_at(position.y).map(|p| p.row);
        }
        let pty = self.pty.as_ref()?;
        let term = pty.term.lock();
        (row < term.screen_lines()).then(|| DisplayRow::Line(row as i32 - term.grid().display_offset() as i32))
    }

    /// The row drawn at window height `y` (in a gap: the row below it), when rows are placed.
    fn placed_at(&self, y: Pixels) -> Option<Placed> {
        let y = f32::from(y - self.bounds.get().origin.y);
        let shown = self.shown.borrow();
        shown.iter().find(|p| y < p.top + p.height).copied().filter(|_| y >= 0.)
    }

    /// The shell's commands now on the grid.
    fn blocks(&self) -> Rc<Vec<BlockView>> {
        match &self.pty {
            Some(pty) if !self.raw => cached_blocks(&self.blocks, pty, self.generation),
            _ => Rc::default(),
        }
    }

    /// Blocks or Raw.
    fn set_raw(&mut self, raw: bool, cx: &mut Context<Self>) {
        if self.raw == raw {
            return;
        }
        self.raw = raw;
        self.hover_block = None;
        self.hover_part = None;
        self.hover_fold = false;
        self.shown.borrow_mut().clear();
        cx.notify();
    }

    /// Scroll by `n` rows as drawn (up when positive): a fold is one row.
    fn scroll_rows(&mut self, n: i32) {
        let Some(pty) = &self.pty else { return };
        let blocks = self.blocks();
        let folds = self.folds.borrow();
        if !blocks.iter().any(|b| term_blocks::folded(b, &folds)) {
            pty.term.lock().scroll_display(Scroll::Delta(n));
            return;
        }
        let mut term = pty.term.lock();
        let grid = term.grid();
        let screen = grid.screen_lines() as i32;
        let offset = grid.display_offset() as i32;
        let all = term_blocks::display_rows(&blocks, &folds, -(grid.history_size() as i32), screen);
        let Some(at) = term_blocks::index_of(&all, screen - 1 - offset) else { return };
        let lowest = (screen as usize - 1).min(all.len() - 1);
        let to = (at as i64 - n as i64).clamp(lowest as i64, all.len() as i64 - 1) as usize;
        let bottom = match all[to] {
            DisplayRow::Line(l) => l,
            DisplayRow::Fold { last, .. } => last,
        };
        term.scroll_display(Scroll::Delta(screen - 1 - bottom - offset));
    }

    /// Scroll so grid line `line` is the top row drawn (as near as the history allows).
    fn scroll_to_top(&mut self, line: i32) {
        let Some(pty) = &self.pty else { return };
        let blocks = self.blocks();
        let folds = self.folds.borrow();
        let mut term = pty.term.lock();
        let grid = term.grid();
        let screen = grid.screen_lines() as i32;
        let offset = grid.display_offset() as i32;
        let all = term_blocks::display_rows(&blocks, &folds, -(grid.history_size() as i32), screen);
        let Some(at) = term_blocks::index_of(&all, line) else { return };
        let starts = command_starts(&blocks);
        let to = term_blocks::fit_from(&all, at, f32::from(self.bounds.get().size.height), f32::from(self.cell.get().height), &gap_for(&starts));
        let bottom = match all[to] {
            DisplayRow::Line(l) => l,
            DisplayRow::Fold { last, .. } => last,
        };
        term.scroll_display(Scroll::Delta(screen - 1 - bottom - offset));
    }

    /// Jump to the command before (`up`) or after the one at the top of the view: its header
    /// at the top. Past the last, to the bottom.
    fn jump_command(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        let top = match self.shown.borrow().first() {
            Some(p) => p.row.line(),
            None => -(pty.term.lock().grid().display_offset() as i32),
        };
        let heads: Vec<i32> = self.blocks().iter().filter(|b| !b.block.command.is_empty()).map(|b| b.block.command.start).collect();
        let target = if up { heads.iter().rev().find(|&&l| l < top) } else { heads.iter().find(|&&l| l > top) };
        match target {
            Some(&l) => self.scroll_to_top(l),
            None if !up => self.scroll_to_bottom(),
            None => {}
        }
        cx.notify();
    }

    /// Copy a command's output, or its command line and output.
    fn copy_block(&mut self, id: u64, with_command: bool, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        let Some(b) = self.blocks().iter().find(|b| b.id() == id).cloned() else { return };
        let term = pty.term.lock();
        let output = term_blocks::text(&term, b.output.clone());
        let text = if with_command {
            let command = b.info.command.clone().unwrap_or_else(|| term_blocks::text(&term, b.block.command.clone()));
            format!("$ {command}\n{output}")
        } else {
            output
        };
        drop(term);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some((id, with_command, std::time::Instant::now()));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    /// Tests: the commands, one per line: id, lines, exit, folded, command, parts.
    pub fn describe_blocks(&self) -> String {
        let Some(pty) = &self.pty else { return "no pty".into() };
        let folds = self.folds.borrow();
        let blocks = self.blocks();
        let shell = pty.marks.lock().ok().and_then(|m| m.shell.clone()).unwrap_or_default();
        let term = pty.term.lock();
        let mut out = format!("shell={shell}");
        for b in blocks.iter() {
            let parts: Vec<String> = term_blocks::parts(&term, b).into_iter().map(|p| format!("{}@{:?}", p.text, p.spans)).collect();
            out.push_str(&format!(
                " || #{} lines={:?} out={:?} exit={:?} running={} folded={} cmd={:?} parts={:?}",
                b.id(),
                b.lines(),
                b.output,
                b.info.exit,
                b.running,
                term_blocks::folded(b, &folds),
                b.info.command,
                parts
            ));
        }
        out
    }

    /// Tests: as if the pointer were over the last command with output (its buttons show).
    pub fn hover_last(&mut self, cx: &mut Context<Self>) -> String {
        self.hover_block = self.blocks().iter().rev().find(|b| !b.running && b.output_len() > 0).map(|b| b.id());
        cx.notify();
        format!("{:?}", self.hover_block)
    }

    /// Tests: send the newest finished command to the chat, as its button does.
    pub fn send_last(&mut self, cx: &mut Context<Self>) -> String {
        let Some(id) = self.blocks().iter().rev().find(|b| b.info.exit.is_some()).map(|b| b.id()) else { return "none".into() };
        self.send_block(id, cx);
        id.to_string()
    }

    /// Tests: Blocks or Raw, as its switch does (the switch showing, as under the pointer).
    pub fn test_view(&mut self, raw: bool, cx: &mut Context<Self>) -> String {
        self.hovered = true;
        self.set_raw(raw, cx);
        cx.notify();
        if raw { "raw".into() } else { "blocks".into() }
    }

    /// Tests: Cmd+Up or Cmd+Down; the top row's line after.
    pub fn test_jump(&mut self, up: bool, cx: &mut Context<Self>) -> String {
        self.jump_command(up, cx);
        let offset = self.pty.as_ref().map_or(0, |p| p.term.lock().grid().display_offset());
        format!("offset {offset}")
    }

    /// Tests: Stop on the running command, as its button does.
    pub fn stop_running(&mut self, cx: &mut Context<Self>) -> String {
        if !self.blocks().iter().any(|b| b.running) {
            return "nothing running".into();
        }
        self.interrupt(cx);
        "stopped".into()
    }

    /// Tests: collapse the first command with output, as its chevron does.
    pub fn collapse_first(&mut self, cx: &mut Context<Self>) -> String {
        let Some(id) = self.blocks().iter().find(|b| b.output_len() > 0).map(|b| b.id()) else { return "none".into() };
        self.toggle_collapse(id, cx);
        id.to_string()
    }

    /// Tests: part `n` (from 1) of the last command with parts, put on the prompt as a click
    /// does (`run` after it: as Cmd+click does).
    pub fn use_part(&mut self, arg: &str, cx: &mut Context<Self>) -> String {
        let mut words = arg.split_whitespace();
        let n: usize = words.next().and_then(|w| w.parse().ok()).unwrap_or(1);
        let run = words.next() == Some("run");
        let Some(pty) = self.pty.clone() else { return "no pty".into() };
        let blocks = self.blocks();
        let part = {
            let term = pty.term.lock();
            blocks.iter().rev().map(|b| term_blocks::parts(&term, b)).find(|p| !p.is_empty()).and_then(|p| p.into_iter().nth(n.saturating_sub(1)))
        };
        let Some(part) = part else { return "no such part".into() };
        self.put_on_prompt(&part.text, run, cx);
        part.text
    }

    /// Ctrl+C to what runs.
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(pty) = &self.pty {
            pty.write(b"\x03".to_vec());
        }
        cx.notify();
    }

    /// The command, its output and how it ended, to the session's message box.
    fn send_block(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        let Some(b) = self.blocks().iter().find(|b| b.id() == id).cloned() else { return };
        let term = pty.term.lock();
        let output = term_blocks::text(&term, b.output.clone());
        let command = b.info.command.clone().unwrap_or_else(|| term_blocks::text(&term, b.block.command.clone()));
        drop(term);
        let ended = match b.info.exit {
            Some(0) => "succeeded (exit 0)".to_string(),
            // 128 + SIGINT: Ctrl+C.
            Some(130) => "was stopped with Ctrl+C (exit 130)".to_string(),
            Some(n) => format!("failed (exit {n})"),
            None => "ran".to_string(),
        };
        let text = format!("A command in my terminal {ended}:\n\n```\n$ {command}\n{output}\n```");
        let short: String = command.chars().take(40).collect();
        let label = match b.info.exit {
            Some(n) if n != 0 => format!("{short} (exit {n})"),
            _ => short,
        };
        cx.emit(TerminalEvent::SendToChat { text, label });
    }

    /// Fold a command's long output in the middle, or open it.
    fn toggle_fold(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(b) = self.blocks().iter().find(|b| b.id() == id).cloned() else { return };
        let now = term_blocks::folded(&b, &self.folds.borrow());
        self.folds.borrow_mut().insert(id, if now { Fold::Open } else { Fold::Middle });
        if now {
            // Opened: the scroll position stays on what was there.
            self.scroll_rows(0);
        }
        cx.notify();
    }

    /// Collapse a command to its header, or open it all (its chevron).
    fn toggle_collapse(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(b) = self.blocks().iter().find(|b| b.id() == id).cloned() else { return };
        let all = term_blocks::fold_of(&b, &self.folds.borrow()) == Fold::All;
        self.folds.borrow_mut().insert(id, if all { Fold::Open } else { Fold::All });
        self.scroll_rows(0);
        cx.notify();
    }

    /// The command whose header's gutter is under `position` (its chevron collapses it).
    fn gutter_command(&self, position: Point<Pixels>) -> Option<u64> {
        if !self.integrated || position.x >= self.bounds.get().origin.x {
            return None;
        }
        let Some(DisplayRow::Line(l)) = self.row_at(position) else { return None };
        self.blocks().iter().find(|b| b.block.command.start == l && b.output_len() > 0).map(|b| b.id())
    }

    /// Put `text` on the shell's prompt in place of what was typed there, and run it if `run`.
    /// Nothing happens while a command runs: it would be typed into that program.
    fn put_on_prompt(&mut self, text: &str, run: bool, cx: &mut Context<Self>) {
        let Some(pty) = self.pty.clone() else { return };
        if !pty.marks.lock().is_ok_and(|m| m.at_prompt()) {
            return;
        }
        // Clear the line: PowerShell's Windows keys (Escape), else Ctrl+E then Ctrl+U.
        let clear = if cfg!(windows) { "\x1b" } else { "\x05\x15" };
        let mut keys = format!("{clear}{text}");
        if run {
            keys.push('\r');
        }
        self.scroll_to_bottom();
        pty.term.lock().selection = None;
        pty.write(keys.into_bytes());
        cx.notify();
    }

    /// The text of a row of the grid (scrollback included), one character per cell.
    fn row_text(&self, line: Line) -> String {
        let Some(pty) = &self.pty else { return String::new() };
        let term = pty.term.lock();
        let grid = term.grid();
        if line.0 >= -(grid.history_size() as i32) && line.0 < grid.screen_lines() as i32 {
            let row = &grid[line];
            (0..grid.columns()).map(|c| row[Column(c)].c).collect()
        } else {
            String::new()
        }
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let was_focused = self.focus.is_focused(window);
        window.focus(&self.focus, cx);
        if let Some(id) = self.gutter_command(e.position) {
            self.toggle_collapse(id, cx);
            return;
        }
        if let Some(DisplayRow::Fold { id, .. }) = self.row_at(e.position) {
            self.folds.borrow_mut().insert(id, Fold::Open);
            self.scroll_rows(0);
            cx.notify();
            return;
        }
        let (point, side) = self.grid_point(e.position);
        self.pending_part = None;
        if e.click_count == 1
            && let Some(part) = self.hover_part.clone().filter(|p| p.contains(point.line.0, point.column.0))
        {
            if secondary(&e.modifiers) {
                // Cmd+click (Ctrl+click): run it now.
                self.put_on_prompt(&part.text, true, cx);
                return;
            }
            // A click that only focuses the terminal does nothing more.
            if was_focused {
                self.pending_part = Some(part);
            }
        }
        if e.modifiers.platform && e.click_count == 1 {
            let line = self.row_text(point.line);
            cx.emit(TerminalEvent::OpenLink { line, column: point.column.0, browser: true });
            return;
        }
        // On a web address: opened on the way up, unless the pointer drags (a selection).
        self.pending_link = None;
        if e.click_count == 1 {
            let line = self.row_text(point.line);
            if web_span_at(&line, point.column.0).is_some() {
                self.pending_link = Some((line, point.column.0));
            }
        }
        if let Some(pty) = &self.pty {
            let ty = match e.click_count {
                2 => SelectionType::Semantic,
                3 => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            let mut term = pty.term.lock();
            term.selection = (e.click_count > 1).then(|| Selection::new(ty, point, side));
            if e.click_count == 1 {
                term.selection = Some(Selection::new(SelectionType::Simple, point, side));
            }
        }
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A web address under the pointer: underlined, with a hand.
        if self.pty.is_some() {
            let (point, _) = self.grid_point(e.position);
            let line = self.row_text(point.line);
            let (block, part, fold) = self.command_under(e.position, point);
            let hover = match &part {
                Some(_) => None,
                None => web_span_at(&line, point.column.0).map(|(a, b)| (point.line.0, a, b)),
            };
            if hover != self.hover_link || block != self.hover_block || fold != self.hover_fold || part != self.hover_part {
                self.hover_link = hover;
                self.hover_block = block;
                self.hover_part = part;
                self.hover_fold = fold;
                cx.notify();
            }
            if e.pressed_button == Some(MouseButton::Left)
                && let Some(p) = &self.pending_part
                && !p.contains(point.line.0, point.column.0)
            {
                self.pending_part = None;
            }
            // Dragged off the cell it began on: a selection, not a click on the link.
            if e.pressed_button == Some(MouseButton::Left)
                && let Some((_, col)) = &self.pending_link
                && *col != point.column.0
            {
                self.pending_link = None;
            }
        }
        if let Some(images) = self.images.clone()
            && self.pty.is_some()
        {
            // An `[Image #N]` under the pointer: show that image.
            let (point, _) = self.grid_point(e.position);
            let line = self.row_text(point.line);
            let found = image_marker_at(&line, point.column.0).and_then(|n| (images.image)(n, cx)).map(|p| (p, e.position));
            if found.as_ref().map(|f| &f.0) != self.hover_image.as_ref().map(|h| &h.0) {
                self.hover_image = found;
                cx.notify();
            }
        }
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.grid_point(e.position);
        if let Some(pty) = &self.pty
            && let Some(sel) = pty.term.lock().selection.as_mut()
        {
            sel.update(point, side);
        }
        cx.notify();
    }

    /// The command under the pointer (`position`, grid point `point`): its id, the part of
    /// its command line there (only at the prompt, when a click can use it), and whether
    /// the row is a fold.
    fn command_under(&self, position: Point<Pixels>, point: GridPoint) -> (Option<u64>, Option<Part>, bool) {
        let Some(pty) = &self.pty else { return (None, None, false) };
        if let Some(id) = self.gutter_command(position) {
            return (Some(id), None, true);
        }
        if matches!(self.row_at(position), Some(DisplayRow::Fold { .. })) {
            let id = self.blocks().iter().find(|b| b.lines().contains(&point.line.0)).map(|b| b.id());
            return (id, None, true);
        }
        let blocks = self.blocks();
        let Some(b) = blocks.iter().find(|b| b.lines().contains(&point.line.0)) else {
            return (None, None, false);
        };
        let idle = pty.marks.lock().is_ok_and(|m| m.at_prompt());
        let part = if idle && b.block.command.contains(&point.line.0) {
            let term = pty.term.lock();
            let parts = term_blocks::parts(&term, b);
            parts.into_iter().find(|p| p.contains(point.line.0, point.column.0))
        } else {
            None
        };
        let id = (!b.running && (b.output_len() > 0 || part.is_some())).then(|| b.id());
        (id, part, false)
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Some(part) = self.pending_part.take() {
            self.pending_link = None;
            self.put_on_prompt(&part.text, false, cx);
            return;
        }
        if let Some((line, column)) = self.pending_link.take() {
            if let Some(pty) = &self.pty {
                pty.term.lock().selection = None;
            }
            cx.emit(TerminalEvent::OpenLink { line, column, browser: false });
            cx.notify();
            return;
        }
        if let Some(pty) = &self.pty {
            let mut term = pty.term.lock();
            if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                term.selection = None;
            }
        }
        cx.notify();
    }

    fn scroll(&mut self, e: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        let line_h = self.cell.get().height;
        let exact = self.wheel_rest + e.delta.pixel_delta(line_h).y / line_h;
        let lines = exact.trunc() as i32;
        self.wheel_rest = exact.fract();
        if lines == 0 {
            return;
        }
        let (cell, _) = self.grid_point(e.position);
        let col = cell.column.0 + 1;
        let row = (cell.line.0 + pty.term.lock().grid().display_offset() as i32).max(0) as usize + 1;
        match wheel_input(self.mode(), lines, col, row) {
            Wheel::Scrollback(n) => self.scroll_rows(n),
            Wheel::Send(bytes) => self.write_str(&bytes),
        }
        cx.notify();
    }
}

/// The bytes a key sends to the program.
fn key_bytes(ks: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = ks.modifiers;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let csi = |code: &str, letter: char| -> Vec<u8> {
        let modifier = 1 + (m.shift as u8) + (m.alt as u8) * 2 + (m.control as u8) * 4;
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor && code.is_empty() {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{code}{letter}").into_bytes()
        }
    };
    let tilde = |n: u8| format!("\x1b[{n}~").into_bytes();
    let bytes = match ks.key.as_str() {
        "enter" => {
            if m.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        "backspace" => {
            if m.alt || m.control {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        "tab" => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        "escape" => b"\x1b".to_vec(),
        "up" => csi("", 'A'),
        "down" => csi("", 'B'),
        "right" => csi("", 'C'),
        "left" => csi("", 'D'),
        "home" => csi("", 'H'),
        "end" => csi("", 'F'),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "delete" => tilde(3),
        "insert" => tilde(2),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        key => {
            if m.control {
                let c = key.chars().next()?;
                if key.chars().count() != 1 {
                    return None;
                }
                let code = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    '@' | ' ' | '2' => 0,
                    '[' | '3' => 27,
                    '\\' | '4' => 28,
                    ']' | '5' => 29,
                    '^' | '6' => 30,
                    '_' | '-' | '7' => 31,
                    _ => return None,
                };
                let mut v = Vec::new();
                if m.alt {
                    v.push(0x1b);
                }
                v.push(code);
                v
            } else {
                // Option on macOS composes characters (key_char); Alt elsewhere is Meta.
                let text = ks
                    .key_char
                    .clone()
                    .or_else(|| (key.chars().count() == 1).then(|| key.to_string()))?;
                let mut v = Vec::new();
                if m.alt && !cfg!(target_os = "macos") {
                    v.push(0x1b);
                }
                v.extend(text.into_bytes());
                v
            }
        }
    };
    Some(bytes)
}

impl TerminalView {
    /// The program in the foreground when it is not the shell, else the folder and branch.
    pub fn auto_name(&self) -> String {
        let program = self.program();
        let shell = login_shell();
        let mut name = wundur_core::tabs::auto_name(
            &self.folder,
            self.branch.as_deref(),
            program.as_deref(),
            &shell,
        );
        if self.number > 1 && program.as_deref().is_none_or(|p| p.contains("sh")) {
            name = format!("{name} {}", self.number);
        }
        if self.exited {
            name.push_str(" (exited)");
        }
        name
    }
}

impl TerminalView {
    /// The program in the foreground, asked of the system at most once a second.
    fn program(&self) -> Option<String> {
        let mut cached = self.program.borrow_mut();
        if cached.0.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
            *cached = (Some(std::time::Instant::now()), self.pty.as_ref().and_then(|p| p.foreground_program()));
        }
        cached.1.clone()
    }

    /// Whether the foreground program changed since it was last asked (at most once a second).
    fn program_changed(&self) -> bool {
        let was = self.program.borrow().1.clone();
        self.program() != was
    }
}

/// The login shell, read once.
fn login_shell() -> String {
    static SHELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| std::env::var("SHELL").unwrap_or_default()).clone()
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        // Underlined: the web address or the part of a command line under the pointer.
        let underline: Vec<(i32, usize, usize)> = match &self.hover_part {
            Some(p) => p.spans.iter().map(|(l, c)| (*l, c.start, c.end)).collect(),
            None => self.hover_link.into_iter().collect(),
        };
        let pty = self.pty.clone();
        let pty_paint = self.pty.clone();
        let on_screen = self.on_screen.clone();
        // The placeholder covers the screen until it fades: nothing of the grid shows.
        let covered = self.placeholder.is_some() && !self.settled && !self.exited && self.revealed.is_none();
        let spinning = self.started.elapsed() < SPIN_AT_MOST;
        let bounds_cell = self.bounds.clone();
        let cell_cell = self.cell.clone();
        let cell_paint = self.cell.clone();
        let blocks_cache = self.blocks.clone();
        let generation = self.generation;
        let folds = self.folds.clone();
        let shown = self.shown.clone();
        let buttons = self.block_bars(cx);
        let color_output = crate::prefs(cx).color_output;
        let raw = self.raw;
        let integrated = self.integrated && !raw;
        // Blocks or Raw, while the pointer is over a shell's terminal.
        let switch = (self.integrated && self.hovered).then(|| {
            let segment = |key: &'static str, label: &'static str, on: bool, raw: bool| {
                Button::new(key)
                    .label(label)
                    .xsmall()
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.ghost())
                    .on_click(cx.listener(move |v, _, _, cx| v.set_raw(raw, cx)))
            };
            div()
                .id("blocks-raw")
                .absolute()
                .top(px(4.))
                .right(px(18.))
                .flex()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(palette::dim()).opacity(0.35))
                .bg(hsla(palette::background()))
                .shadow_sm()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(segment("view-blocks", "Blocks", !self.raw, false))
                .child(segment("view-raw", "Raw", self.raw, true))
        });
        // Scrolled up: a way back down.
        let scrolled_up = integrated && self.pty.as_ref().is_some_and(|p| p.term.lock().grid().display_offset() > 0);
        let base = div()
            .id("terminal")
            .size_full()
            .bg(hsla(palette::background()))
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(|v, _: &crate::actions::TerminalZoomIn, _, cx| v.zoom_by(Some(1.), cx)))
            .on_action(cx.listener(|v, _: &crate::actions::TerminalClear, _, cx| v.clear(cx)))
            .on_action(cx.listener(|v, _: &crate::actions::TerminalZoomOut, _, cx| v.zoom_by(Some(-1.), cx)))
            .on_action(cx.listener(|v, _: &crate::actions::TerminalZoomReset, _, cx| v.zoom_by(None, cx)))
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            // Files dropped in: as if pasted.
            .on_drop(cx.listener(|v, paths: &ExternalPaths, _, cx| v.paste_files(paths.paths().to_vec(), cx)))
            .on_mouse_move(cx.listener(Self::mouse_move))
            // The pointer left: no command's buttons, no underline.
            .on_hover(cx.listener(|v, hovered: &bool, _, cx| {
                v.hovered = *hovered;
                if !*hovered {
                    v.hover_block = None;
                    v.hover_part = None;
                    v.hover_fold = false;
                }
                if v.integrated {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll))
            .cursor(if self.hover_link.is_some() || self.hover_part.is_some() || self.hover_fold { CursorStyle::PointingHand } else { CursorStyle::IBeam });
        if let Some(err) = &self.error {
            return base.p_2().text_color(hsla(if palette::is_dark() { 0xcc6666 } else { 0xc0392b })).child(err.clone());
        }
        let text_size = self.text_size(cx);
        let zoomed_at = self.zoomed_at.clone();
        // Fading out once settled, then gone.
        let fading = self.revealed.is_some_and(|t| t.elapsed() < PLACEHOLDER_FADE);
        let placeholder = self
            .placeholder
            .as_ref()
            .filter(|_| (!self.settled && !self.exited) || fading)
            .map(|p| {
                // The conversation's last lines as bars of their length (a skeleton: its shape,
                // not its text, which the program is about to draw its own way).
                let cell = self.cell.get().width;
                let bar = hsla(palette::dim()).opacity(0.22);
                // From the top, where the interface draws: its banner (a logo and two lines),
                // then the conversation.
                let line = |w: f32| div().h(px(text_size * 0.75)).w(cell * w).rounded(px(3.)).bg(bar);
                let mut col = div()
                    .absolute()
                    .inset_0()
                    .px_2()
                    .pt_3()
                    .bg(hsla(palette::background()))
                    .flex()
                    .flex_col()
                    .gap(px(text_size * 0.7))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(cell * 2.)
                            .pb(px(text_size))
                            .child(div().w(cell * 8.).h(px(text_size * 3.)).rounded(px(4.)).bg(bar))
                            .child(div().flex().flex_col().gap(px(text_size * 0.6)).child(line(18.)).child(line(30.))),
                    );
                for (user, line) in &p.lines {
                    let chars = line.chars().count().clamp(4, 120) as f32;
                    col = col.child(
                        div()
                            .h(px(text_size * 0.75))
                            .when(*user, |d| d.ml(cell * 2.))
                            .w(cell * chars)
                            .max_w(relative(0.92))
                            .rounded(px(3.))
                            .bg(if *user { bar.opacity(0.32) } else { bar }),
                    );
                }
                col.child(
                    div()
                        .pt_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(text_size))
                        .text_color(hsla(palette::dim()))
                        .child(if spinning {
                            gpui_kit::component::spinner::Spinner::new().color(hsla(palette::dim())).into_any_element()
                        } else {
                            div().size(px(6.)).rounded_full().bg(hsla(palette::dim())).into_any_element()
                        })
                        .child(p.label.clone()),
                )
            })
            .map(|col| match self.revealed {
                // Animated only while fading: it starts when the fade does.
                Some(_) => col.with_animation("placeholder-fade", Animation::new(PLACEHOLDER_FADE), |d, t| d.opacity(1. - t)).into_any_element(),
                None => col.into_any_element(),
            });
        let gutter = if self.integrated && !self.raw { px(GUTTER) } else { px(4.) };
        base.pl(gutter).pr_1().relative().child(
            canvas(
                move |bounds, window, cx| {
                    bounds_cell.set(bounds);
                    let font_size = px(text_size);
                    let font = font(crate::fonts::code(cx));
                    let ts = window.text_system();
                    let font_id = ts.resolve_font(&font);
                    let cw = ts
                        .advance(font_id, font_size, 'm')
                        .map(|s| s.width)
                        .unwrap_or(px(7.5));
                    let ch = (font_size * 1.35).round();
                    cell_cell.set(size(cw, ch));
                    // While zoom steps follow each other, the program keeps its size (the grid is
                    // drawn at the new font meanwhile); it is told once they stop.
                    let zooming = zoomed_at.get().is_some_and(|t| t.elapsed() < ZOOM_SETTLE);
                    if let Some(pty) = pty.as_ref().filter(|_| !zooming) {
                        let cols = ((bounds.size.width / cw).floor() as u16).max(2);
                        let rows = ((bounds.size.height / ch).floor() as u16).max(1);
                        pty.resize(GridSize {
                            cols,
                            rows,
                            cell_w: f32::from(cw) as u16,
                            cell_h: f32::from(ch) as u16,
                        });
                    }
                },
                move |bounds, (), window, cx| {
                    let Some(pty) = &pty_paint else { return };
                    on_screen.mark(window);
                    if covered {
                        return;
                    }
                    let start = std::time::Instant::now();
                    let blocks = if raw { Rc::default() } else { cached_blocks(&blocks_cache, pty, generation) };
                    let rows = layout(pty, &blocks, &folds.borrow(), f32::from(cell_paint.get().height), f32::from(bounds.size.height));
                    *shown.borrow_mut() = rows.clone().unwrap_or_default();
                    let heads = headers(&blocks, &folds.borrow());
                    let tinted: Vec<_> = if color_output { blocks.iter().map(|b| b.output.clone()).collect() } else { Vec::new() };
                    paint_grid(text_size, pty, bounds, cell_paint.get(), focused, &underline, rows.as_deref(), &heads, &tinted, window, cx);
                    if integrated {
                        paint_marks(pty, &blocks, &folds.borrow(), bounds, window);
                    }
                    crate::perf::record("terminal paint", start.elapsed());
                    crate::perf::terminal_painted();
                },
            )
            .size_full(),
        )
        .children(placeholder)
        .children(buttons)
        .children(switch)
        .when(scrolled_up, |d| {
            d.child(
                div().absolute().bottom(px(10.)).right(px(18.)).child(
                    Button::new("jump-bottom")
                        .icon(IconName::ArrowDown)
                        .label("Jump to bottom")
                        .xsmall()
                        .outline()
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.scroll_to_bottom();
                            cx.notify();
                        })),
                ),
            )
        })
        .children(self.hover_image.clone().map(|(path, at)| {
            let b = self.bounds.get();
            let (x, y) = (at.x - b.origin.x + px(12.), at.y - b.origin.y + px(16.));
            div()
                .absolute()
                .left(x)
                .top(y)
                .p_1()
                .rounded_md()
                .bg(hsla(palette::background()))
                .border_1()
                .border_color(hsla(palette::dim()))
                .shadow_lg()
                .child(img(path).max_w(px(320.)).max_h(px(240.)).object_fit(ObjectFit::Contain))
        }))
    }
}

impl TerminalView {
    /// The bars of buttons on commands' headers: on the one under the pointer; on a running
    /// command (Stop); and on the newest command when it failed (its output to copy or send).
    fn block_bars(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let blocks = self.blocks();
        let mut ids: Vec<u64> = Vec::new();
        ids.extend(blocks.iter().filter(|b| b.running).map(|b| b.id()));
        if let Some(last) = blocks.iter().rev().find(|b| b.info.exit.is_some())
            && last.info.exit != Some(0)
            && !blocks.iter().any(|b| b.block.start > last.block.start && b.info.exit.is_some())
        {
            ids.push(last.id());
        }
        ids.extend(self.hover_block);
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().filter_map(|id| self.block_bar(&blocks, id, cx)).collect()
    }

    fn block_bar(&self, blocks: &[BlockView], id: u64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let b = blocks.iter().find(|b| b.id() == id)?;
        let pty = self.pty.as_ref()?;
        // Its header's row as drawn, else its first row in view.
        let first = if b.block.command.is_empty() { b.block.start } else { b.block.command.start };
        let shown = self.shown.borrow();
        let cell = self.cell.get();
        let (row_top, on_header) = if shown.is_empty() {
            let term = pty.term.lock();
            let offset = term.grid().display_offset() as i32;
            let screen = term.screen_lines() as i32;
            let row = (first.max(-offset)..b.lines().end).map(|l| l + offset).find(|r| (0..screen).contains(r))?;
            (cell.height * row as f32, row - offset == first)
        } else {
            let p = shown.iter().find(|p| b.lines().contains(&p.row.line()))?;
            (px(p.top), p.row == DisplayRow::Line(first))
        };
        // Left of its time and exit code, which stay readable.
        let meta_chars = if on_header {
            headers(std::slice::from_ref(b), &self.folds.borrow())
                .first()
                .map_or(0, |h| h.meta.iter().map(|(t, _)| t.chars().count() + 2).sum::<usize>())
        } else {
            0
        };
        let right = px(8.) + cell.width * 0.9 * meta_chars as f32;
        let copied = self.copied.filter(|(c, _, t)| *c == id && t.elapsed() < std::time::Duration::from_millis(1400)).map(|(_, all, _)| all);
        let has_output = b.output_len() > 0 && !b.running;
        let foldable = b.foldable();
        let folded = term_blocks::folded(b, &self.folds.borrow());
        let key = |name: &'static str| ElementId::NamedInteger(name.into(), id);
        let button = |name: &'static str, icon: IconName, tip: &'static str| Button::new(key(name)).icon(icon).xsmall().ghost().tooltip(tip);
        Some(
            div()
                .id(key("block-bar"))
                .absolute()
                .top(row_top)
                .right(right)
                .flex()
                .items_center()
                .gap(px(2.))
                .px(px(2.))
                .rounded(px(5.))
                .border_1()
                .border_color(hsla(palette::dim()).opacity(0.35))
                .bg(hsla(palette::background()))
                .shadow_sm()
                // A press here is for the buttons, not a selection in the terminal.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(b.running, |d| {
                    d.child(
                        Button::new(key("stop"))
                            .label("Stop")
                            .xsmall()
                            .ghost()
                            .tooltip("Interrupt it (Ctrl+C)")
                            .on_click(cx.listener(|v, _, _, cx| v.interrupt(cx))),
                    )
                })
                .when(has_output, |d| {
                    d.child(
                        button("copy-output", if copied == Some(false) { IconName::Check } else { IconName::Copy }, "Copy output")
                            .on_click(cx.listener(move |v, _, _, cx| v.copy_block(id, false, cx))),
                    )
                    .child(
                        button("copy-all", if copied == Some(true) { IconName::Check } else { IconName::SquareTerminal }, "Copy with command")
                            .on_click(cx.listener(move |v, _, _, cx| v.copy_block(id, true, cx))),
                    )
                    .child(
                        button("send", IconName::MessageSquareShare, "Send to chat (the command, its output and exit code)")
                            .on_click(cx.listener(move |v, _, _, cx| v.send_block(id, cx))),
                    )
                })
                .when(foldable, |d| {
                    d.child(
                        button("fold", if folded { IconName::ChevronsUpDown } else { IconName::ChevronsDownUp }, if folded { "Show all" } else { "Fold" })
                            .on_click(cx.listener(move |v, _, _, cx| v.toggle_fold(id, cx))),
                    )
                })
                .into_any_element(),
        )
    }

    /// The size its text is drawn at: Settings' size, zoomed.
    pub fn text_size(&self, cx: &App) -> f32 {
        let (min, max) = (*wundur_core::settings::TERMINAL_FONT_SIZES.start(), *wundur_core::settings::TERMINAL_FONT_SIZES.end());
        (self.base_size(cx) + self.zoom).clamp(min, max)
    }

    /// Settings' size for it: an agent's interface has its own, larger than a shell's.
    fn base_size(&self, cx: &App) -> f32 {
        if self.agent { crate::prefs(cx).agent_font_size() } else { font_size(cx) }
    }

    /// Zoom by `steps` (in or out), or back to Settings' size with `None`.
    fn zoom_by(&mut self, steps: Option<f32>, cx: &mut Context<Self>) {
        let (min, max) = (*wundur_core::settings::TERMINAL_FONT_SIZES.start(), *wundur_core::settings::TERMINAL_FONT_SIZES.end());
        let base = self.base_size(cx);
        let zoom = match steps {
            // Kept within the sizes allowed, so zooming back starts from what shows.
            Some(n) => ((base + self.zoom + n * ZOOM_STEP).clamp(min, max)) - base,
            None => 0.,
        };
        if zoom == self.zoom {
            return;
        }
        self.zoom = zoom;
        self.zoomed_at.set(Some(std::time::Instant::now()));
        // Drawn again once the steps stop, when the program is told its size.
        self.zoom_settle = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(ZOOM_SETTLE).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        cx.notify();
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    text_size: f32,
    pty: &Pty,
    bounds: Bounds<Pixels>,
    cell: Size<Pixels>,
    focused: bool,
    underline: &[(i32, usize, usize)],
    shown: Option<&[Placed]>,
    headers: &[Header],
    tinted: &[std::ops::Range<i32>],
    window: &mut Window,
    cx: &mut App,
) {
    crate::perf::count(crate::perf::Count::Terminal);
    let term = pty.term.lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cols = term.columns();
    let rows = term.screen_lines();
    let font_size = px(text_size);
    let base_font = font(crate::fonts::code(cx));
    let default_bg = palette::background();
    let grid = term.grid();
    let plain: Vec<Placed>;
    let shown = match shown {
        Some(s) => s,
        None => {
            let h = f32::from(cell.height);
            plain = (0..rows).map(|r| Placed { row: DisplayRow::Line(r as i32 - offset), top: h * r as f32, height: h }).collect();
            &plain
        }
    };

    for (row, placed) in shown.iter().enumerate() {
        let y = bounds.origin.y + px(placed.top);
        let line = match placed.row {
            DisplayRow::Line(l) => Line(l),
            DisplayRow::Fold { hidden, whole, .. } => {
                paint_fold(hidden, whole, point(bounds.origin.x, y), bounds.size.width, cell, font_size, &base_font, window, cx);
                continue;
            }
        };
        // Command output: plain text gets light colouring (see `output_colors`).
        let tones = if tinted.iter().any(|r| r.contains(&line.0)) {
            let row_text: String = (0..cols)
                .map(|c| {
                    let cell = &grid[GridPoint::new(line, Column(c))];
                    if cell.c == '\0' || cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { ' ' } else { cell.c }
                })
                .collect();
            wundur_core::output_colors::tones(&row_text)
        } else {
            Vec::new()
        };
        // A command's header: a rule above it, its rows tinted, its chevron and status in the
        // gutter.
        let header = headers.iter().find(|h| h.rows.contains(&line.0) || h.start == line.0);
        if let Some(h) = header {
            let left = bounds.origin.x - px(GUTTER);
            let width = bounds.size.width + px(GUTTER);
            if h.start == line.0 && row > 0 {
                window.paint_quad(fill(Bounds::new(point(left, y), size(width, px(1.))), hsla(palette::dim()).opacity(0.3)));
            }
            if h.rows.contains(&line.0) {
                window.paint_quad(fill(Bounds::new(point(left, y), size(width, cell.height)), hsla(palette::dim()).opacity(0.07)));
            }
            if h.rows.start == line.0 {
                paint_gutter(h, point(left, y), cell, font_size, &base_font, window, cx);
            }
        }
        let mut text = String::with_capacity(cols);
        let mut runs: Vec<TextRun> = Vec::new();
        let mut bg_start: Option<(usize, u32)> = None;
        let paint_bg = |from: usize, to: usize, color: u32, window: &mut Window| {
            if color != default_bg {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x + cell.width * from as f32, y),
                        size(cell.width * (to - from) as f32, cell.height),
                    ),
                    hsla(color),
                ));
            }
        };
        for col in 0..cols {
            let gp = GridPoint::new(line, Column(col));
            let c = &grid[gp];
            let mut fg_color = if c.flags.contains(Flags::BOLD) {
                palette::brighten(c.fg)
            } else {
                c.fg
            };
            let mut bg_color = c.bg;
            if c.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg_color, &mut bg_color);
            }
            let mut fg = palette::resolve(fg_color, colors, true);
            let mut bg = palette::resolve(bg_color, colors, false);
            let mut tone_bold = false;
            if let Some(Some(tone)) = tones.get(col)
                && c.fg == AnsiColor::Named(NamedColor::Foreground)
                && !c.flags.intersects(Flags::INVERSE | Flags::DIM | Flags::HIDDEN)
            {
                use wundur_core::output_colors::Tone;
                match tone {
                    Tone::Dim => fg = palette::dim(),
                    Tone::Label => tone_bold = true,
                    Tone::Bad => {
                        fg = palette::indexed(1);
                        tone_bold = true;
                    }
                    Tone::Warn => {
                        fg = palette::indexed(3);
                        tone_bold = true;
                    }
                    Tone::Good => fg = palette::indexed(2),
                    Tone::Link => fg = palette::indexed(4),
                    Tone::Path => fg = palette::indexed(6),
                    Tone::Number => fg = palette::indexed(5),
                    Tone::Hash => fg = palette::indexed(3),
                }
            }
            if selection.is_some_and(|s| s.contains(gp)) {
                bg = palette::selection();
                fg = palette::foreground();
            }
            match bg_start {
                Some((start, color)) if color != bg => {
                    paint_bg(start, col, color, window);
                    bg_start = Some((col, bg));
                }
                None => bg_start = Some((col, bg)),
                _ => {}
            }
            if c.flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let ch = if c.c == '\t' || c.c == '\0' || c.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                c.c
            };
            let before = text.len();
            text.push(ch);
            if let Some(zw) = c.zerowidth() {
                text.extend(zw.iter());
            }
            let len = text.len() - before;
            let mut f = base_font.clone();
            if c.flags.contains(Flags::BOLD) || tone_bold {
                f.weight = FontWeight::BOLD;
            }
            if c.flags.contains(Flags::ITALIC) {
                f.style = FontStyle::Italic;
            }
            let color = hsla(palette::readable(fg, bg, c.flags.contains(Flags::DIM)));
            let underline = c
                .flags
                .intersects(Flags::ALL_UNDERLINES)
                .then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: c.flags.contains(Flags::UNDERCURL),
                });
            let strike = c
                .flags
                .contains(Flags::STRIKEOUT)
                .then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                });
            match runs.last_mut() {
                Some(r)
                    if r.font == f
                        && r.color == color
                        && r.underline == underline
                        && r.strikethrough == strike =>
                {
                    r.len += len
                }
                _ => runs.push(TextRun {
                    len,
                    font: f,
                    color,
                    background_color: None,
                    underline,
                    strikethrough: strike,
                }),
            }
        }
        if let Some((start, color)) = bg_start {
            paint_bg(start, cols, color, window);
        }
        // Its time and exit code, on the right of the header's first row, where the row's
        // own text leaves room.
        if let Some(h) = header.filter(|h| h.rows.start == line.0 && !h.meta.is_empty()) {
            let used = text.trim_end().chars().count();
            let label: String = h.meta.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("  ");
            let n = label.chars().count();
            if used + n + 3 <= cols {
                let mut runs = Vec::new();
                for (i, (t, color)) in h.meta.iter().enumerate() {
                    let len = t.len() + if i + 1 < h.meta.len() { 2 } else { 0 };
                    runs.push(TextRun { len, font: base_font.clone(), color: hsla(*color), background_color: None, underline: None, strikethrough: None });
                }
                let shaped = window.text_system().shape_line(label.into(), font_size * 0.9, &runs, None);
                let x = bounds.origin.x + bounds.size.width - shaped.width - px(6.);
                let _ = shaped.paint(point(x, y), cell.height, TextAlign::Left, None, window, cx);
            }
        }
        let trimmed = text.trim_end().len();
        if trimmed == 0 {
            continue;
        }
        // Drop trailing blanks from the runs too.
        let mut left = trimmed;
        let mut kept = Vec::new();
        for mut r in runs {
            if left == 0 {
                break;
            }
            r.len = r.len.min(left);
            left -= r.len;
            kept.push(r);
        }
        text.truncate(trimmed);
        let shaped =
            window
                .text_system()
                .shape_line(text.into(), font_size, &kept, Some(cell.width));
        let _ = shaped.paint(
            point(bounds.origin.x, y),
            cell.height,
            TextAlign::Left,
            None,
            window,
            cx,
        );
        // The web address under the pointer, underlined.
        for &(_, from, to) in underline.iter().filter(|(l, ..)| *l == line.0) {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x + cell.width * from as f32, y + cell.height - px(2.)),
                    size(cell.width * (to - from) as f32, px(1.)),
                ),
                gpui_kit::component::ActiveTheme::theme(cx).blue,
            ));
        }
    }

    // Cursor.
    let cursor = content.cursor;
    if cursor.shape != CursorShape::Hidden
        && let Some(p) = shown.iter().find(|p| p.row == DisplayRow::Line(cursor.point.line.0)) {
            let origin = point(
                bounds.origin.x + cell.width * cursor.point.column.0 as f32,
                bounds.origin.y + px(p.top),
            );
            let color = hsla(palette::cursor());
            let block = Bounds::new(origin, cell);
            if !focused {
                window.paint_quad(outline(block, color, BorderStyle::Solid));
            } else {
                match cursor.shape {
                    CursorShape::Beam => window
                        .paint_quad(fill(Bounds::new(origin, size(px(2.), cell.height)), color)),
                    CursorShape::Underline => window.paint_quad(fill(
                        Bounds::new(
                            point(origin.x, origin.y + cell.height - px(2.)),
                            size(cell.width, px(2.)),
                        ),
                        color,
                    )),
                    _ => {
                        let mut c = color;
                        c.a = 0.6;
                        window.paint_quad(fill(block, c));
                    }
                }
            }
        }
}

/// The row standing for a folded output: how many lines it hides, and that a click shows them.
#[allow(clippy::too_many_arguments)]
fn paint_fold(
    hidden: usize,
    whole: bool,
    at: Point<Pixels>,
    width: Pixels,
    cell: Size<Pixels>,
    font_size: Pixels,
    base_font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    let dim = palette::dim();
    window.paint_quad(fill(
        Bounds::new(point(at.x, at.y + px(2.)), size(width, cell.height - px(4.))),
        hsla(dim).opacity(0.12),
    ));
    let label = if whole {
        format!("  {} output {} hidden", group_digits(hidden), if hidden == 1 { "line" } else { "lines" })
    } else {
        format!("  ⋯ {} lines folded, click to show", group_digits(hidden))
    };
    let run = TextRun {
        len: label.len(),
        font: base_font.clone(),
        color: hsla(dim),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window.text_system().shape_line(label.into(), font_size, &[run], Some(cell.width));
    let _ = shaped.paint(at, cell.height, TextAlign::Left, None, window, cx);
}

/// Marks down the right edge where each command starts, over the whole history: grey when
/// it ran, red when it failed, blue while it runs; and the part in view, faintly.
fn paint_marks(pty: &Pty, blocks: &[BlockView], folds: &std::collections::HashMap<u64, Fold>, bounds: Bounds<Pixels>, window: &mut Window) {
    let (top, screen, offset) = {
        let term = pty.term.lock();
        let grid = term.grid();
        (-(grid.history_size() as i32), grid.screen_lines() as i32, grid.display_offset() as i32)
    };
    if top == 0 || blocks.is_empty() {
        return;
    }
    let all = term_blocks::display_rows(blocks, folds, top, screen);
    let total = all.len().max(1) as f32;
    let x = bounds.origin.x + bounds.size.width + px(1.);
    let h = bounds.size.height;
    let at = |i: usize| bounds.origin.y + h * (i as f32 / total);
    // The view.
    if let Some(end) = term_blocks::index_of(&all, screen - 1 - offset) {
        let start = (end + 1).saturating_sub(screen as usize);
        let y = at(start);
        window.paint_quad(fill(Bounds::new(point(x, y), size(px(3.), (at(end + 1) - y).max(px(6.)))), hsla(palette::dim()).opacity(0.18)));
    }
    let dark = palette::is_dark();
    for b in blocks.iter().filter(|b| !b.block.command.is_empty()) {
        let Some(i) = term_blocks::index_of(&all, b.block.command.start) else { continue };
        let color = if b.running {
            hsla(if dark { 0x6aa6ff } else { 0x2f6fde })
        } else if b.info.exit.is_some_and(|c| c != 0) {
            hsla(if dark { 0xe06c6c } else { 0xc0392b })
        } else {
            hsla(palette::dim()).opacity(0.6)
        };
        window.paint_quad(fill(Bounds::new(point(x, at(i)), size(px(3.), px(2.))), color));
    }
}

/// A header's chevron (open or collapsed) and status (done, failed, running) in the gutter.
fn paint_gutter(h: &Header, at: Point<Pixels>, cell: Size<Pixels>, font_size: Pixels, base_font: &Font, window: &mut Window, cx: &mut App) {
    let dark = palette::is_dark();
    let glyph = |text: &str, color: u32, x: Pixels, window: &mut Window, cx: &mut App| {
        let run = TextRun { len: text.len(), font: base_font.clone(), color: hsla(color), background_color: None, underline: None, strikethrough: None };
        let shaped = window.text_system().shape_line(text.to_string().into(), font_size * 0.85, &[run], None);
        let _ = shaped.paint(point(at.x + x, at.y), cell.height, TextAlign::Left, None, window, cx);
    };
    if let Some(open) = h.chevron {
        glyph(if open { "▾" } else { "▸" }, palette::dim(), px(3.), window, cx);
    }
    match h.status {
        Status::Ok => glyph("✓", if dark { 0x5cb85c } else { 0x2e8b3a }, px(15.), window, cx),
        Status::Failed => glyph("✕", if dark { 0xe06c6c } else { 0xc0392b }, px(15.), window, cx),
        Status::Running => glyph("●", if dark { 0x6aa6ff } else { 0x2f6fde }, px(15.), window, cx),
        Status::None => {}
    }
}

/// 2340 → "2,340".
fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(7), "7");
        assert_eq!(group_digits(2340), "2,340");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[::core::prelude::v1::test]
    fn finds_the_image_marker_under_the_pointer() {
        let line = "❯ [Image #1] [Image #12] what is this";
        assert_eq!(image_marker_at(line, 4), Some(1));
        assert_eq!(image_marker_at(line, 15), Some(12));
        assert_eq!(image_marker_at(line, 30), None);
    }

    #[::core::prelude::v1::test]
    fn the_wheel_scrolls_the_normal_screen() {
        // A shell at its prompt: the default modes include alternate scroll, but there is no
        // alternate screen, so nothing reaches the shell (no history cycling).
        assert_eq!(wheel_input(TermMode::default(), 3, 5, 5), Wheel::Scrollback(3));
        assert_eq!(wheel_input(TermMode::default(), -2, 5, 5), Wheel::Scrollback(-2));
    }

    #[::core::prelude::v1::test]
    fn files_paste_as_the_macos_terminal_pastes_them() {
        let paths = [std::path::PathBuf::from("/Volumes/Extreme SSD/shot (1).png"), std::path::PathBuf::from("/tmp/a.txt")];
        assert_eq!(super::shell_paths(&paths), "/Volumes/Extreme\\ SSD/shot\\ \\(1\\).png /tmp/a.txt");
        assert_eq!(super::image_ext(std::path::Path::new("x.JPEG")), Some("jpg"));
        assert_eq!(super::image_ext(std::path::Path::new("x.pdf")), None);
    }

    #[::core::prelude::v1::test]
    fn full_screen_programs_get_arrows() {
        let mode = TermMode::default() | TermMode::ALT_SCREEN;
        assert_eq!(wheel_input(mode, 2, 1, 1), Wheel::Send("\x1b[A\x1b[A".into()));
        assert_eq!(wheel_input(mode | TermMode::APP_CURSOR, -1, 1, 1), Wheel::Send("\x1bOB".into()));
        // Alternate scroll turned off: the scrollback.
        assert_eq!(wheel_input(TermMode::ALT_SCREEN, 1, 1, 1), Wheel::Scrollback(1));
    }

    #[::core::prelude::v1::test]
    fn programs_that_want_the_mouse_get_wheel_events() {
        // Claude Code's interface: alternate screen, mouse reports, SGR form.
        let mode = TermMode::default() | TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(wheel_input(mode, 1, 10, 4), Wheel::Send("\x1b[<64;10;4M".into()));
        assert_eq!(wheel_input(mode, -2, 10, 4), Wheel::Send("\x1b[<65;10;4M".repeat(2)));
        let x10 = TermMode::MOUSE_DRAG;
        assert_eq!(wheel_input(x10, 1, 1, 1), Wheel::Send("\x1b[M`!!".into()));
    }
}

/// The web address at column `column` of a row's text (a character per column): its columns.
fn web_span_at(line: &str, column: usize) -> Option<(usize, usize)> {
    let at = line.char_indices().nth(column).map_or(line.len(), |(i, _)| i);
    let (from, to) = wundur_core::web::web_link_span(line, at)?;
    Some((line[..from].chars().count(), line[..to].chars().count()))
}

#[cfg(test)]
mod link_tests {
    use super::web_span_at;

    #[::core::prelude::v1::test]
    fn a_web_address_is_found_by_its_columns() {
        let line = "  ➜  Local:   http://localhost:5173/ (ready)";
        let start = line.chars().position(|c| c == 'h').unwrap();
        assert_eq!(web_span_at(line, start + 3), Some((start, start + "http://localhost:5173/".len())));
        assert_eq!(web_span_at(line, 2), None, "not on the address");
        assert_eq!(web_span_at("see https://example.com.", 10), Some((4, 23)), "the full stop is not part of it");
    }
}
