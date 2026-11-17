//! A terminal's commands as blocks, read from the marks in its grid's cells (see
//! `wundur_core::term_marks`): which rows are a command's output, how it ended, which rows
//! are folded away, and the text to copy.

use std::collections::HashMap;
use std::ops::Range;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use wundur_core::term_marks::{self, Block, Info, Kind, Marks, Phase};

/// Output longer than this many lines folds once its command finished.
pub const FOLD_AT: usize = 40;
/// A folded output keeps its first and last lines showing (the end is often what matters: a
/// summary, an error).
pub const FOLD_HEAD: usize = 3;
pub const FOLD_TAIL: usize = 5;

/// A command, with what the marks state knows of it.
#[derive(Clone, Debug)]
pub struct BlockView {
    pub block: Block,
    pub info: Info,
    pub running: bool,
    /// Its output without the blank lines at its end.
    pub output: Range<i32>,
}

impl BlockView {
    pub fn id(&self) -> u64 {
        self.block.id
    }

    /// Every line it takes.
    pub fn lines(&self) -> Range<i32> {
        self.block.start..self.block.output.end
    }

    /// A command ran here (a prompt left empty, or one restored from before, did not).
    pub fn ran(&self) -> bool {
        self.running || self.info.command.is_some() || self.info.exit.is_some()
    }

    /// Drawn as a command: its prompt row a header, with a gap above.
    pub fn headed(&self) -> bool {
        self.ran() && !self.block.command.is_empty()
    }

    pub fn output_len(&self) -> usize {
        (self.output.end - self.output.start).max(0) as usize
    }

    pub fn foldable(&self) -> bool {
        !self.running && self.output_len() > FOLD_AT
    }

    /// The lines a fold hides.
    pub fn hidden(&self) -> Range<i32> {
        self.output.start + FOLD_HEAD as i32..self.output.end - FOLD_TAIL as i32
    }
}

/// The commands in `term`'s grid (none on the full screen, where a program draws as it likes).
pub fn scan<T>(term: &Term<T>, marks: &Marks) -> Vec<BlockView> {
    if term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN) {
        return Vec::new();
    }
    let grid = term.grid();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let screen = grid.screen_lines() as i32;
    let mut rows: Vec<(i32, Vec<(Kind, u64)>)> = Vec::new();
    for l in top..screen {
        let row = &grid[Line(l)];
        let mut found: Vec<(Kind, u64)> = Vec::new();
        // Cells printed under one link share it: each is looked at once.
        let mut last: *const () = std::ptr::null();
        for c in 0..cols {
            let Some(extra) = &row[Column(c)].extra else { continue };
            let ptr = std::sync::Arc::as_ptr(extra) as *const ();
            if ptr == last {
                continue;
            }
            last = ptr;
            if let Some(link) = row[Column(c)].hyperlink()
                && let Some(mark) = term_marks::parse_mark(link.uri())
                && !found.contains(&mark)
            {
                found.push(mark);
            }
        }
        if !found.is_empty() {
            rows.push((l, found));
        }
    }
    if rows.is_empty() {
        return Vec::new();
    }
    let end = (grid.cursor.point.line.0 + 1).min(screen);
    let blank = |l: i32| {
        let row = &grid[Line(l)];
        (0..cols).all(|c| matches!(row[Column(c)].c, ' ' | '\0' | '\t'))
    };
    term_marks::assemble(&rows, end)
        .into_iter()
        .map(|block| {
            let mut out_end = block.output.end;
            while out_end > block.output.start && blank(out_end - 1) {
                out_end -= 1;
            }
            let info = marks.info.get(&block.id).cloned().unwrap_or_default();
            let running = matches!(marks.current, Some((id, Phase::Running)) if id == block.id);
            let output = block.output.start..out_end;
            BlockView { block, info, running, output }
        })
        .collect()
}

/// How much of a command's output shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// All of it.
    Open,
    /// Its first and last lines (long output, once finished).
    Middle,
    /// None: the command's header alone (the person collapsed it).
    All,
}

/// A row as drawn: a line of the grid, or the line standing for a fold (`whole`: the
/// command's whole output).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    Line(i32),
    Fold { id: u64, hidden: usize, first: i32, last: i32, whole: bool },
    /// A command's row of actions (copy, send to chat, more), below its output line `after`.
    Actions { id: u64, after: i32 },
}

impl DisplayRow {
    /// The grid line it stands for (a fold: its first hidden line).
    pub fn line(self) -> i32 {
        match self {
            DisplayRow::Line(l) => l,
            DisplayRow::Fold { first, .. } => first,
            DisplayRow::Actions { after, .. } => after,
        }
    }

    /// The last grid line it stands for.
    pub fn last(self) -> i32 {
        match self {
            DisplayRow::Line(l) => l,
            DisplayRow::Fold { last, .. } => last,
            DisplayRow::Actions { after, .. } => after,
        }
    }

    fn contains(self, line: i32) -> bool {
        match self {
            DisplayRow::Line(l) => l == line,
            DisplayRow::Fold { first, last, .. } => (first..=last).contains(&line),
            DisplayRow::Actions { .. } => false,
        }
    }
}

/// How a command's output shows: as the person set it, else folded in the middle when long.
pub fn fold_of(b: &BlockView, folds: &HashMap<u64, Fold>) -> Fold {
    match folds.get(&b.id()) {
        Some(Fold::Middle) if !b.foldable() => Fold::Open,
        Some(Fold::All) if b.output_len() == 0 => Fold::Open,
        Some(f) => *f,
        None if b.foldable() => Fold::Middle,
        None => Fold::Open,
    }
}

/// Whether any of the output is hidden.
pub fn folded(b: &BlockView, folds: &HashMap<u64, Fold>) -> bool {
    fold_of(b, folds) != Fold::Open
}

/// The lines hidden, and whether that's the whole output.
fn hidden_lines(b: &BlockView, folds: &HashMap<u64, Fold>) -> Option<(Range<i32>, bool)> {
    match fold_of(b, folds) {
        Fold::Open => None,
        Fold::Middle => Some((b.hidden(), false)),
        Fold::All => Some((b.output.clone(), true)),
    }
}

/// Every row as drawn, top to bottom, from grid line `top` to `bottom` (exclusive).
pub fn display_rows(blocks: &[BlockView], folds: &HashMap<u64, Fold>, top: i32, bottom: i32, actions: &[u64]) -> Vec<DisplayRow> {
    // The action rows, by the output line they follow.
    let after: HashMap<i32, u64> = blocks
        .iter()
        .filter(|b| actions.contains(&b.id()) && b.output_len() > 0)
        .map(|b| (b.output.end - 1, b.id()))
        .collect();
    let push = |rows: &mut Vec<DisplayRow>, row: DisplayRow| {
        rows.push(row);
        let (from, to) = match row {
            DisplayRow::Fold { first, last, .. } => (first, last),
            other => (other.line(), other.line()),
        };
        if let Some((&line, &id)) = after.iter().find(|(l, _)| (from..=to).contains(*l)) {
            rows.push(DisplayRow::Actions { id, after: line });
        }
    };
    let mut hidden: Vec<(Range<i32>, u64, bool)> = blocks
        .iter()
        .filter_map(|b| hidden_lines(b, folds).map(|(r, whole)| (r, b.id(), whole)))
        .filter(|(r, ..)| r.start < r.end)
        .collect();
    hidden.sort_by_key(|(r, ..)| r.start);
    let mut rows = Vec::with_capacity((bottom - top).max(0) as usize);
    let mut l = top;
    let mut next = hidden.into_iter().peekable();
    while l < bottom {
        // Folds that start above (their start scrolled out of the history) begin here.
        while next.peek().is_some_and(|(r, ..)| r.end <= l) {
            next.next();
        }
        if let Some((r, id, whole)) = next.peek().cloned()
            && r.start <= l
        {
            let last = r.end.min(bottom) - 1;
            push(&mut rows, DisplayRow::Fold { id, hidden: (last - l + 1) as usize, first: l, last, whole });
            l = last + 1;
            next.next();
            continue;
        }
        push(&mut rows, DisplayRow::Line(l));
        l += 1;
    }
    rows
}

/// The index of the row standing for grid line `line`.
pub fn index_of(rows: &[DisplayRow], line: i32) -> Option<usize> {
    let i = rows.partition_point(|r| r.last() < line);
    rows.get(i).filter(|r| r.contains(line)).map(|_| i)
}

/// A row as drawn, and where: its top and height in pixels from the top of the terminal (rows
/// are a line tall, plus a gap above each command).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub row: DisplayRow,
    pub top: f32,
    pub height: f32,
}

/// The rows that fit in `avail` pixels, ending with `all[bottom]` (fewer at the top of all),
/// placed from the top; `gap(row)` is the space above a row.
pub fn place(all: &[DisplayRow], bottom: usize, avail: f32, line: f32, gap: &dyn Fn(DisplayRow) -> f32) -> Vec<Placed> {
    let Some(bottom) = (!all.is_empty()).then(|| bottom.min(all.len() - 1)) else { return Vec::new() };
    let mut used = 0.;
    let mut first = bottom;
    loop {
        let h = line + gap(all[first]);
        // At least the bottom row; a row's gap may be cut at the top.
        if used + line > avail && first != bottom {
            first += 1;
            break;
        }
        used += h;
        if first == 0 {
            break;
        }
        first -= 1;
    }
    let mut top = 0.;
    let mut out = Vec::with_capacity(bottom + 1 - first);
    for (i, &row) in all[first..=bottom].iter().enumerate() {
        // The first row's gap is left out: nothing above it to keep apart from.
        let g = if i == 0 { 0. } else { gap(row) };
        top += g;
        out.push(Placed { row, top, height: line });
        top += line;
    }
    // Ends below the bottom: drawn from the bottom up instead (it was a fit with gaps cut).
    if top > avail {
        let shift = top - avail;
        out.retain_mut(|p| {
            p.top -= shift;
            p.top >= 0.
        });
    }
    out
}

/// The last row that fits in `avail` pixels when `all[start]` is the top row drawn.
pub fn fit_from(all: &[DisplayRow], start: usize, avail: f32, line: f32, gap: &dyn Fn(DisplayRow) -> f32) -> usize {
    let mut used = line;
    let mut i = start;
    while i + 1 < all.len() {
        let h = line + gap(all[i + 1]);
        if used + h > avail {
            break;
        }
        used += h;
        i += 1;
    }
    i
}

/// The text of grid lines `lines`, as typed: a line the terminal wrapped is joined to the
/// next, trailing spaces go.
pub fn text<T>(term: &Term<T>, lines: Range<i32>) -> String {
    let grid = term.grid();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let mut out = String::new();
    for l in lines.start.max(top)..lines.end.min(grid.screen_lines() as i32) {
        let row = &grid[Line(l)];
        let mut line = String::with_capacity(cols);
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            line.push(if cell.c == '\0' { ' ' } else { cell.c });
        }
        if row[Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            out.push_str(&line);
        } else {
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// A part of a command line, where it's drawn: one span (line, columns) per row it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub id: u64,
    pub spans: Vec<(i32, Range<usize>)>,
    pub text: String,
}

impl Part {
    pub fn contains(&self, line: i32, col: usize) -> bool {
        self.spans.iter().any(|(l, cols)| *l == line && cols.contains(&col))
    }
}

/// The parts of `b`'s command line, where they're drawn (a line the terminal wrapped
/// included; a command typed over several lines has none).
pub fn parts<T>(term: &Term<T>, b: &BlockView) -> Vec<Part> {
    let Some(command) = b.info.command.as_deref().filter(|c| !c.is_empty() && !c.contains('\n')) else {
        return Vec::new();
    };
    if b.block.command.is_empty() {
        return Vec::new();
    }
    let grid = term.grid();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32;
    let chars: Vec<char> = command.chars().collect();
    let cell_char = |l: i32, c: usize| grid[Line(l)][Column(c)].c;
    // Where it starts: the cell with its command mark, else (fish marks no command) where its
    // text is found after the prompt; the first row of it that matches.
    let mut start = None;
    for l in b.block.command.clone() {
        if l < top || l >= bottom {
            continue;
        }
        let fits = |at: usize| {
            let n = chars.len().min(cols - at);
            n > 0 && (0..n).all(|i| cell_char(l, at + i) == chars[i])
        };
        let marked = (0..cols).find(|&c| {
            grid[Line(l)][Column(c)]
                .hyperlink()
                .and_then(|h| term_marks::parse_mark(h.uri()))
                .is_some_and(|m| m == (Kind::Command, b.id()))
        });
        start = match marked {
            Some(at) if fits(at) => Some((l, at)),
            _ => (0..cols).find(|&at| fits(at)).map(|at| (l, at)),
        };
        if start.is_some() {
            break;
        }
    }
    let Some((mut line, mut col)) = start else { return Vec::new() };
    // Each character's cell, following the rows the terminal wrapped it onto.
    let mut cells = Vec::with_capacity(chars.len());
    for &ch in &chars {
        if col == cols {
            if !grid[Line(line)][Column(cols - 1)].flags.contains(Flags::WRAPLINE) || line + 1 >= bottom {
                break;
            }
            line += 1;
            col = 0;
        }
        if cell_char(line, col) != ch {
            break;
        }
        cells.push((line, col));
        col += 1;
    }
    term_marks::split_command(command)
        .into_iter()
        .filter_map(|r| {
            let from = command[..r.start].chars().count();
            let to = command[..r.end].chars().count().min(cells.len());
            let mut spans: Vec<(i32, Range<usize>)> = Vec::new();
            for &(l, c) in cells.get(from..to)? {
                match spans.last_mut() {
                    Some((sl, cs)) if *sl == l => cs.end = c + 1,
                    _ => spans.push((l, c..c + 1)),
                }
            }
            (!spans.is_empty()).then(|| Part { id: b.id(), spans, text: command[r].to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u64, start: i32, out: Range<i32>) -> BlockView {
        BlockView {
            block: Block { id, start, command: start..start + 1, output: out.clone() },
            info: Info::default(),
            running: false,
            output: out,
        }
    }

    #[test]
    fn long_output_folds_keeping_its_ends() {
        let blocks = vec![block(1, 0, 1..101), block(2, 101, 102..103)];
        let rows = display_rows(&blocks, &HashMap::new(), 0, 104, &[]);
        // Prompt, 3 head lines, the fold, 5 tail lines, then the next command.
        assert_eq!(rows[0], DisplayRow::Line(0));
        assert_eq!(rows[3], DisplayRow::Line(3));
        assert_eq!(rows[4], DisplayRow::Fold { id: 1, hidden: 92, first: 4, last: 95, whole: false });
        assert_eq!(rows[5], DisplayRow::Line(96));
        assert_eq!(rows.len(), 104 - 92 + 1);
        assert_eq!(index_of(&rows, 50), Some(4));
        assert_eq!(index_of(&rows, 96), Some(5));
    }

    #[test]
    fn unfolded_or_running_output_shows_whole() {
        let mut running = block(1, 0, 1..101);
        running.running = true;
        assert!(display_rows(&[running], &HashMap::new(), 0, 101, &[]).iter().all(|r| matches!(r, DisplayRow::Line(_))));
        let opened = HashMap::from([(1, Fold::Open)]);
        assert_eq!(display_rows(&[block(1, 0, 1..101)], &opened, 0, 101, &[]).len(), 101);
    }

    #[test]
    fn a_collapsed_command_shows_its_header_alone() {
        let blocks = vec![block(1, 0, 1..4), block(2, 4, 5..6)];
        let rows = display_rows(&blocks, &HashMap::from([(1, Fold::All)]), 0, 6, &[]);
        assert_eq!(rows, [DisplayRow::Line(0), DisplayRow::Fold { id: 1, hidden: 3, first: 1, last: 3, whole: true }, DisplayRow::Line(4), DisplayRow::Line(5)]);
        // Short output is not folded in the middle, even if asked.
        assert_eq!(fold_of(&blocks[1], &HashMap::from([(2, Fold::Middle)])), Fold::Open);
    }

    #[test]
    fn an_action_row_follows_a_commands_output() {
        let blocks = vec![block(1, 0, 1..3), block(2, 3, 4..5)];
        let rows = display_rows(&blocks, &HashMap::new(), 0, 5, &[1]);
        assert_eq!(rows[3], DisplayRow::Actions { id: 1, after: 2 });
        assert_eq!(rows[4], DisplayRow::Line(3));
        assert_eq!(index_of(&rows, 2), Some(2));
        assert_eq!(index_of(&rows, 3), Some(4));
        // Collapsed: after the fold.
        let rows = display_rows(&blocks, &HashMap::from([(1, Fold::All)]), 0, 5, &[1]);
        assert_eq!(rows[2], DisplayRow::Actions { id: 1, after: 2 });
    }

    #[test]
    fn gaps_go_above_commands_and_fewer_rows_fit() {
        let all: Vec<DisplayRow> = (0..10).map(DisplayRow::Line).collect();
        // A gap of half a line above lines 4 and 8.
        let gap = |r: DisplayRow| if matches!(r, DisplayRow::Line(4 | 8)) { 5. } else { 0. };
        let placed = place(&all, 9, 60., 10., &gap);
        // 60px: lines 5..=9 with the gap above 8 (55px), line 4's gap would not fit with it.
        assert_eq!(placed.first().unwrap().row, DisplayRow::Line(5));
        assert_eq!(placed.last().unwrap().row, DisplayRow::Line(9));
        assert_eq!(placed[3].top, 35.);
        // All of it fits: from the top, the first row without its gap.
        let placed = place(&all, 9, 500., 10., &gap);
        assert_eq!(placed[0].top, 0.);
        assert_eq!(placed[4].top, 45.);
        assert_eq!(fit_from(&all, 0, 60., 10., &gap), 4);
    }

}
