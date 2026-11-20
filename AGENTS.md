# Wundur

Instructions for coding agents (Claude Code, Codex and others) working on this repo.

A native macOS window for Claude Code, in Rust with GPUI. Crates: `wundur-core` (sessions,
backends, files; no UI) and `wundur-app` (the window).

## Build and check

```sh
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo build -p wundur-app --release
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo test -p wundur-core -p wundur-app
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo clippy -p wundur-core -p wundur-app
```

On an exFAT drive, macOS leaves `._*` files that break builds and git; delete them first:
`find . -name '._*' -type f -not -path './target/*' -delete`.

## At completion: run commands

When the work is done (built, tests and clippy pass, changes committed), end the reply with two
blocks, each marked `run` on its fence, in this order:

1. The PR. It pushes the task's branch and opens the PR from the task's worktree. Write the PR
   body (what changed and why, how it was checked, and the line
   `🤖 Generated with [Claude Code](https://claude.com/claude-code)`) to a file outside the repo,
   such as `/tmp/pr-<branch>.md`, and pass it with `--body-file`.

   ```sh run
   cd "<worktree path>" && git push -u origin <branch> && gh pr create --base main --head <branch> --title "<title>" --body-file /tmp/pr-<branch>.md
   ```

2. The restart, from the root checkout (the folder that holds `main`), not from the worktree:

   ```sh run
   cd "$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")" && ./scripts/restart
   ```

   The restart builds the root checkout as it stands, so it shows the change only after the PR
   is merged and root is on an up-to-date `main`. If root is behind, check
   `git branch --show-current` and put `git pull --ff-only origin main` before the restart.

Wundur shows a Run button on each block. The restart script builds the release binary, quits the
running Wundur and starts the new build in the same folder with the same arguments; open
sessions come back. Never run either block yourself: the PR block pushes, and the restart quits
the app this session runs in.

## Changes go through pull requests

`main` changes only by merging a pull request; the owner reviews and merges.

- Every task works in its own worktree off `origin/main` by default, not in the root checkout,
  which other sessions share:
  `git fetch origin main && git worktree add .worktrees/<branch> -b <branch> origin/main`
  Edit, build and commit there. Don't switch, stash or reset the root checkout.
- Commit on the task's branch. Push and open the PR with the PR run command (see "At
  completion" above); don't push or run `gh` yourself.
- Never push to `main`, merge a PR, or delete a branch someone else made.
- After the owner merges, bring the root checkout up to date on `main` before restarting.

## Releasing

The version lives in `Cargo.toml` (`[workspace.package] version`); the About window shows it.

1. A PR bumps the version (semver: `0.1.0` → `0.1.1` for fixes, `0.2.0` for features).
2. After it is merged, the owner tags `main` with the same version and pushes the tag:
   `git tag v0.2.0 && git push origin v0.2.0`.
3. The Release workflow builds, signs and notarizes Wundur.app and publishes it as a GitHub
   release. It fails if the tag does not match `Cargo.toml`.

Agents prepare the version bump; they do not push release tags.
