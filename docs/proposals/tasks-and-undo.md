# Proposal: tasks, undo last turn, and git underneath everything

Status: draft for the owner's review. Written 2026-11-15 from a conversation with the owner.

## The idea in one paragraph

Long sessions are slow, expensive and hit usage caps sooner, and most work splits into smaller
tasks that can run side by side without touching each other. People know this and still keep
one session going, because starting another one takes effort. Wundur should make the short,
parallel way the easy one: **New Task** is one text box that starts an agent in its own
worktree, a task list shows what each one is doing, and **Finish** lands it and cleans up.
Under that sits a safety net every project gets: Wundur bundles git, records a snapshot before
every agent turn, and offers **Undo last turn**, which puts the files back and rewinds the
conversation. People who already use version control keep theirs. People who have never heard
of it are protected without having to learn anything.

## Order of work

1. **Bundled git** and **detecting each project's version control.**
2. **Turn snapshots** in a private repository, for every project.
3. **Undo last turn.**
4. **Tasks:** New Task, the task list, Finish.
5. Later: overlap warnings, the long-session nudge, Ideas → Task, version control other than git.

Undo comes before tasks because it is useful in every project on its own, and tasks reuse its
snapshots.

## 1. Git, always available

**Bundled.** Wundur.app ships a git binary (as GitHub Desktop does), so nobody installs
anything and no Apple Command Line Tools dialog appears. Wundur uses the person's own git when
there is one (`git` on PATH, a working version) and the bundled one otherwise. All git calls go
through one place in `wundur-core` that chooses which binary to run. Today `skill_install.rs`,
`command_changes.rs`, `git_status.rs` and the worktree code in `shell.rs` each run `git`
directly.

Costs and work:
- Tens of MB in the app.
- The Release workflow has to sign and notarize the binary and its helpers (`git-core/*`).
- Git is GPLv2: shipping it as a separate program beside Wundur is fine, but the release notes
  or About window must say where its source comes from.
- Over SSH the remote machine's git is used. A remote host without git loses snapshots and
  tasks (and says so). It does not fall back silently.

**Detected, not asked.** No startup question. Each project folder is checked when it opens:

| Found | Version control | Tasks | Undo |
| --- | --- | --- | --- |
| `.git` (or inside a repository) | git | yes | yes |
| `.p4config`, `P4CONFIG` set | Perforce | not yet | yes |
| `.plastic` | Plastic / Unity VC | not yet | yes |
| `.svn`, `.hg` | other | no | yes |
| nothing | none | after setup (below) | yes |

Undo works everywhere because it does not use the project's version control (see 2). The
project's settings can override what was detected, for the rare folder where detection is wrong.

**Git is set up when a task first needs it, not when a folder opens** (decided with the owner
2026-11-15). Every folder already gets undo from the private snapshots, so nothing in a person's
folder changes unasked. The first time someone uses New Task in a folder with no version
control, one confirmation explains it in plain words ("Tasks need a project history; Wundur
will set one up") and lists what the first commit will include, flagging likely secrets and
very large files, with the default ignore rules (below) already applied. It's never offered
for folders that aren't projects: the home folder, Desktop, Downloads, Documents, `/`.

**Default ignore rules** (written to `.gitignore` only when Wundur creates the repository; an
existing `.gitignore` is never changed):
- Secrets: `.env`, `.env.*` (keeping `.env.example`), `*.pem`, `*.key`, `*.p12`, credentials
  files.
- Models and weights: `*.gguf`, `*.safetensors`, `*.ckpt`, `*.pt`, `*.onnx`, `*.bin` over a
  size.
- Build output and dependencies: `target/`, `build/`, `dist/`, `node_modules/`, `.venv/`,
  `__pycache__/`.
- Engine caches, by detected engine: Unity `Library/ Temp/ Logs/ obj/ UserSettings/`, Godot
  `.godot/`, Unreal `Binaries/ Intermediate/ Saved/ DerivedDataCache/`.
- OS junk: `.DS_Store`, `._*`.
- Large binary assets (raw art, audio, video over a size) are not ignored. They go to Git LFS
  when it's bundled too, or get a warning. Open question below.

## 2. Turn snapshots

Before each agent turn, Wundur records the project's files as a git commit in a **private
repository outside the project**: `snapshots/<project id>.git` under `settings::config_dir()`
(`wundur-studio`; never a folder named `Wundur`, which belongs to another app), used with
`--git-dir=<that> --work-tree=<project>`.

- It never touches the project's own repository, branches, stash or history, so it works the
  same for git, Perforce, Plastic and folders with no version control.
- The project's `.gitignore` files apply (they are in the work tree). In a project without git,
  the default ignore rules above apply from the private repository's `info/exclude`.
- In a git project the private repository borrows the project's objects (`objects/info/
  alternates`), so it doesn't copy files that are already committed.
- A snapshot uses a temporary index (`GIT_INDEX_FILE`), `add -A`, `write-tree`, `commit-tree`,
  and a ref per session and turn (`refs/wundur/<session>/<turn>`). Untracked files are
  captured too, which fixes the known gap in `command_changes.rs`: today's `git stash create`
  baseline has no copy of untracked files.
- The same snapshot can then be the baseline for the Changes panel, replacing the
  `stash create` baseline, so a turn costs one snapshot and not two.
- Old snapshots are pruned: keep every turn of open sessions and the last N days, then
  `gc`. Size shown in Settings, with a "Clear history" button.

Speed matters: a snapshot runs before the turn is sent. On a large project `add -A` with the
fsmonitor and untracked cache should take well under a second. If a project is slower than a
threshold, Wundur takes the snapshot in parallel with the turn's first moments and refuses undo
for that turn if a file changed before the snapshot finished. To be measured on a real Unity
project before committing to it.

## 3. Undo last turn

A button on the last turn (and a shortcut): "Undo this turn." It does two things.

**Puts the files back, only the ones that turn changed.** Wundur compares the snapshot before
the turn with the one after it and reverses exactly those files: restores changed and deleted
ones, removes new ones. It does not restore the whole project, because another session in the
same folder may have changed other files in the meantime. If a file the turn changed has been
changed again since (by the person or another session), Wundur lists it and asks: keep the
newer version, or undo it anyway.

**Rewinds the conversation, natively per agent:**
- **Claude Code:** resume the session from the message before the turn, so the agent no longer
  remembers it. (Check which mechanism the current CLI supports: resuming at a message, or
  forking at it. Claude Code's own `/rewind` checkpoints only cover its edit tools, not shell
  commands, so Wundur's file restore is used either way.)
- **An agent that can't rewind its conversation:** the files are restored and the turn stays in
  the chat, marked "Undone". The agent's next turn begins with a short note that the person
  undid its last turn. Hiding the turn while the agent still remembers it would confuse both.

The person's message comes back into the input box, so "undo, change a word, send again" is
quick.

**What undo can't reverse,** said plainly in the confirmation when the turn did any of it
(Wundur sees the turn's commands): packages installed outside the project, commits pushed or
messages sent, anything over the network, files outside the project folder.

Only the last turn in the first version. Undoing several turns, back to any point, is the same
mechanism and can follow.

## 4. Tasks

**New Task** sits beside + on each project. It is one input box, the same as the chat's
(attachments, long pastes), with two chips under it: agent and model, set to what was last used
for a task in this project. Submitting:

1. Names the task from its text (a few words, by the local summary model when one is set up,
   else the first words), and checks the name is free.
2. Creates the worktree off an up-to-date default branch: `git fetch` when there is a remote,
   then `git worktree add -b wundur/<name>`, using the existing `make_worktree` code. Wundur
   does this itself. The agent is not asked to make worktrees.
3. Runs the project's **worktree setup**, if it has one (below).
4. Starts the session in the worktree with the task as its first message.

The + dialog keeps its "new git worktree" checkbox for people who want the full set of choices.

**The task list.** Under each project, its open tasks with a status: working, needs you
(waiting on a question or approval), done (ready to review), PR open, merged. A notification
when a task needs you or finishes. This list is what makes running several at once manageable.
Without it, cheap tasks would only produce a pile of forgotten worktrees.

**Finish.** On a done task: shows the changes (the Changes panel for the whole task), then
lands it the way the project is set up:
- **Pull request** (projects with a GitHub remote, like this one): rebase onto the default
  branch, push, `gh pr create` with a description drafted from the task. The task stays in the
  list as "PR open" until it's merged.
- **Local merge** (no remote, or chosen): merge into the default branch on this machine.

After merging, Wundur removes the worktree and the branch (only branches it created) and closes
the session. If a rebase conflicts, the task goes back to its agent: "Rebase onto main hit
conflicts in these files; resolve them."

**Worktree setup.** Worktrees are cheap for small projects and expensive for some:
- This repository: a separate target dir per worktree means a full gpui build each time, and
  several of them filled the disk twice on 2026-11-15. A shared `CARGO_TARGET_DIR` reuses the
  dependencies (each worktree's own crates still build separately); the cost is that builds
  wait on each other's lock. Shared should be the default for Rust projects.
- Node projects need `node_modules`. Unity projects need `Library/` rebuilt (minutes).

So a project can declare a setup step in `.wundur/worktree-setup` (a script run in the new
worktree), plus a choice of shared (the default) or per-worktree build cache. Without one, Wundur says what
the first build in a new worktree may cost. `./scripts/restart` needs to build from the
worktree whose change is being tried, not always the main folder.

**Starting a task from where you are.** The moment someone realizes "this should be separate"
is usually in the middle of a long session. So:
- `/task <text>` in any session starts a New Task with that text.
- "Make this a task" on any message does the same, carrying a short brief from the session
  (the existing handoff code drafts it).

## 5. Later

- **Overlap warning.** When a new task starts, Wundur compares it with the other open tasks:
  which files each worktree has changed against the default branch, and the task descriptions
  (local summary model). It shows a warning ("may touch `chat.rs`, which *fix-scroll* is
  editing") and never blocks. It does not ask the other agents: that would interrupt them and
  spend their context, and they don't know which files they'll touch either.
- **Long-session nudge.** When a session's context or turn count grows past a size: "This
  session is long; start the next piece as a task?" A suggestion, dismissable per session.
- **Ideas → Task.** One click from the Ideas panel (in the open queue).
- **Other version control.** Tasks go through a small trait in `wundur-core`: make an isolated
  copy of the project, show its changes, land it, remove it. Git implements it now. Perforce
  (a second workspace or a stream) and Plastic (branches) only when a real user asks, built
  against their real setup. Undo needs none of this.

## Open questions for the owner

1. **Git LFS:** bundle it and route large binary assets through it by default in new projects,
   or warn only? (LFS in a purely local repository is harmless. It matters once there's a
   remote.)
2. **Where task worktrees live:** next to the project, as the + dialog does now
   (`../<name>`), or under one folder (`../<project>.tasks/<name>`) so they don't clutter the
   parent folder? The second is my suggestion.
3. **Undo with the Claude rewind:** should the undone turn disappear from the chat, or stay
   folded as "Undone" for every agent, for one consistent look? Disappearing matches what you
   described; folded keeps a record.
4. **Snapshot retention:** how many days, and a size cap?
