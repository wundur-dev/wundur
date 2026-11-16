# Proposal: the terminal knows its commands (fold, copy, click a part)

Status: built 2026-11-16 (the owner said go; a click puts a part on the prompt, Cmd+click or Ctrl+click runs it). Folded output keeps its first 3 and last 5 lines. Saved terminal history does not keep the marks yet. Written 2026-11-16.

## The idea in one paragraph

Today the terminal is a grid of characters: it can't tell a prompt from a command from that
command's output. If the shell marks those boundaries, the terminal can treat each command as a
**block** (prompt, command text, output, exit code) and offer three things: long output folds
and can be unfolded, a command's output copies in one action, and each part of `a && b && c` can
be clicked to use on its own. All of it has to work the same on macOS, Linux and Windows.

## What we control already

The terminal is ours. `alacritty_terminal` keeps the grid and runs the process (a Unix pty, or
ConPTY on Windows), and Wundur draws every cell itself (`paint_grid` in
`crates/wundur-app/src/terminal_view.rs`). Hiding rows, adding a gutter, underlining parts of a
line and drawing buttons over a block are all drawing changes. The one thing we lack is
knowing where the boundaries are.

## Step 1: shell integration (the base for everything else)

Shells can be made to print invisible markers, the `OSC 133` convention shared by VS Code,
Ghostty, iTerm2, WezTerm and Windows Terminal:

| Marker | Meaning |
|---|---|
| `133;A` | a prompt starts |
| `133;B` | the prompt ends; what follows is the typed command |
| `133;C` | the command runs; output follows |
| `133;D;<code>` | the command finished, with this exit code |

**Loading it with no setup.** When Wundur starts a shell it knows, it loads a small script
shipped with the app. The person's own rc files still load as usual, and nothing is written to
their home folder:

- **zsh:** point `ZDOTDIR` at Wundur's folder; its `.zshenv`/`.zshrc` restore the real
  `ZDOTDIR`, source the person's files, then add `precmd`/`preexec` hooks (VS Code's method).
- **bash:** start it with `--rcfile` pointing at a Wundur file that sources the person's
  `.bashrc` (or the login files) and adds `PROMPT_COMMAND` plus a `DEBUG` trap or `PS0`.
- **fish:** add Wundur's folder to `XDG_DATA_DIRS` so fish loads a `vendor_conf.d` script with
  `fish_prompt`/`fish_preexec`/`fish_postexec` events.
- **PowerShell (Windows, and pwsh anywhere):** `-NoExit -Command . <script>`; the script wraps
  `prompt` and uses PSReadLine's `AddToHistoryHandler`/`CommandValidationHandler` to mark
  where a command starts. This is what Windows Terminal documents.
- **cmd.exe:** only the prompt can be marked (`PROMPT=$e]133;A$e\...`). Blocks still separate,
  but with no exit code and no exact command-text bounds.
- **Anything else** (SSH into a server, a shell we don't recognise): no markers, and the
  terminal works exactly as it does today. Every feature below is skipped quietly when there
  are no blocks.

A setting turns the integration off for people who don't want Wundur touching their shell
startup.

**Catching the markers.** I checked: the parser (vte 0.15, inside `alacritty_terminal`) drops
OSC sequences it doesn't know, so they never reach our code. Alacritty's `EventLoop` reads the
pty and feeds the parser itself, so we have two options:

1. **Our own read loop (recommended).** Replace `EventLoop` with a loop that reads the pty,
   scans the bytes for `ESC ] 133 ;` and feeds the parser. This is about 200 lines, but it
   needs separate unix and Windows code for polling and writing. It also gives us one place to
   look at output, which other features can reuse.
2. **A patched vte/alacritty** that forwards unknown OSCs to a callback. Less code today, but
   a fork to keep in step with upstream.

**Windows risk, to test first.** Older ConPTY versions re-drew the screen and dropped unknown
OSC sequences. Newer ones (Windows 11 builds, and the `conpty.dll` that ships with Windows
Terminal) pass them through. The first thing to do on the `C:\dev\kt` copy is run PowerShell
with the script and see whether `133` arrives. If it doesn't, we can ship `conpty.dll` and
`OpenConsole.exe` next to the exe; alacritty already loads that DLL when it's present.

## The hard part: keeping blocks in place

A block's position is a grid line, and grid lines move. Resizing re-wraps text, history drops
its oldest lines past 10,000, `clear` wipes the screen, and saved history is replayed when a
kept terminal opens again. Storing line numbers and patching them up after each of those events
is fragile.

**Proposed: put the mark in the cell itself.** When a marker arrives, the read loop turns it
into an OSC 8 link with a private address (`wundur-mark:<id>;A`) on the next character
printed. Alacritty keeps links in the cell, so marks move with the text through re-wrap, drop
off with trimmed history, and vanish on `clear` with no extra code. To find blocks we scan the
rows we draw for those cells. The scan is cheap, and its result is cached until the grid
changes.

Two things follow from this:

- The link-hover code ignores `wundur-mark:` addresses.
- `kept_text`/`to_ansi` writes them back out as OSC 8, so saved history keeps its blocks.

Fallback, if cell marks turn out to misbehave: keep a list of absolute line numbers, shift it
on scroll and trim, and drop the blocks above the screen on resize.

## The three features

### 1. Fold long output

When a finished command printed more than N lines (setting, default 40), its block shows the
first few lines and a row reading `▸ 2,340 more lines`. Clicking that row, or the block's
gutter, unfolds it. Folds are drawing only: the grid is never changed, so selection, search and
copy still see every line.
- Only finished commands in the normal screen fold. A running command streams as it does today,
  and full-screen programs (vim, htop) are never touched.
- Once `claude` runs inside the terminal, everything it prints counts as that one command's
  output, so nothing inside it folds.

### 2. Copy a command's output

Hovering a block shows a small copy button with two choices: **Output**, and
**Command and output**. Lines that wrapped on screen are joined back together. A shortcut
copies the last command's output. Failed commands (exit code other than 0) get a thin red mark
in the gutter. The same exit code later lets the agent be told "the last command failed",
which ties into the failure fixes.

### 3. Click one part of a chained command

Each block knows its command text, so hovering it underlines the parts separated by `&&`, `||`,
`;` and `|`. The split is quote-aware: `echo "a && b"` stays one part, and `$(…)` and
backslashes are respected. It works on any earlier command in the scrollback, and on the line
you're typing once the up arrow has brought one back.

- **Click:** the part goes onto the prompt, replacing what was being typed, ready for Enter.
  This uses the shell's line-editor key for "clear the line" (`Ctrl-U` for zsh/bash/fish,
  `Esc` for PowerShell), then types the text.
- **Cmd-click** (Ctrl-click on Windows and Linux): runs it right away.
- Neither does anything while a program is running, so text is never typed into it.

**Open question for the owner:** should a plain click run the part straight away? I recommend
it doesn't. A click in a terminal often just means focusing it or starting a selection, and a
stray click on `git checkout main` or `rm -rf build` can't be taken back. Running on click is
the same code with one line different.

## Order of work

1. The read loop and marker catching, plus the zsh script. Test PowerShell on Windows the same
   week, since the ConPTY risk decides the Windows plan.
2. Marks in cells, the list of blocks, and the bash, fish and PowerShell scripts.
3. Copy output, and the exit-code gutter.
4. Folding.
5. Clickable command parts.

Each step is its own PR. Step 1 has no visible change except an internal list of blocks that
tests can check.

## Not in this proposal

- Separate per-command panes like Warp's. Blocks here stay inside one real terminal, so every
  program keeps working exactly as in any other terminal.
- Re-running a whole block with one button. Easy to add once blocks exist, if wanted.
