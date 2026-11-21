# Proposal: engine packs and the sprite packer

Status: proposal for the owner to read. Written 2026-11-21.

## The idea in one paragraph

An **engine pack** is a plugin that teaches Wundur, and every agent in it, one game engine:
what it is good for, how to tell a project uses it, what has to be on the machine for it to run,
what the agent should know when working in it, how to start a new project with it, and how to
write sprite atlases in its format. Four ship with Wundur: **three.js**, **Phaser**, **Godot**
and **Unity**. More can be added the way tool packs are added. Wundur still has no engine of its
own and is tied to none (see the product direction): it helps choose, then gets out of the way.

Alongside it, a **sprite packer** in the spirit of TexturePacker, built for agents: the agent
packs loose images into atlases with a tool, checks the result with another, and Wundur shows
it in a tab on the right. People don't operate it; they tell the agent what to change.

## Why

- Choosing the engine is the first big decision in a game, and the one newcomers are least able
  to make. "Best for 2D" means little until you know what you want to make.
- The commonest early stall is tooling: the agent writes a Godot project and nothing on the
  machine can open it, or Unity needs a Hub, an account and a licence before anything works.
- Agents do noticeably better in an engine when told its conventions (Godot 4, not 3; how Phaser
  scenes fit together; where Unity keeps scenes and why its YAML must not be hand-edited).
- Sprite atlases are needed by every 2D web game and TexturePacker costs money for something
  that is easily repeated. An agent that makes frames with `make_image` should be able to pack
  them and wire them into the game in the same turn.

## The four bundled packs

What each pack's card says when choosing, in plain words:

| Pack | Best for | What you need | Honest note |
|---|---|---|---|
| **Phaser** | 2D games that run in a browser: platformers, puzzle, arcade, top-down. Physics, input, scenes and sound built in. | Node.js | The quickest way to something playable. Plays in Wundur's web panel. |
| **three.js** | 3D in a browser: small 3D games, toys, interactive scenes. | Node.js | A 3D renderer, not a full engine: physics and game structure are added by the agent or a library. Weak for 2D; pick Phaser. |
| **Godot** | 2D or 3D games you want to ship on desktop or mobile, with a real editor. Free and open source. | Godot 4 | Agents work well here: scenes are text files and it runs from the command line. |
| **Unity** | 3D or 2D for people who already know Unity, or need its asset store and platforms. | Unity Hub, the editor version the project names, a Unity account | The hardest for agents: scenes and prefabs are YAML full of GUIDs that break when edited by hand, and you mostly can't run or see the game without the editor open. Works best with an editor bridge (an MCP package). |

PixiJS was considered for 2D web and set aside for Phaser: Pixi is a renderer only, so the agent
would build the game loop, collisions and scenes every time. A Pixi pack can come later for
people building their own engine on it.

## Choosing an engine

### Detect first

When a folder opens (not only when a project is made), each pack's detection rules run:

- Godot: `project.godot` at the root (its `config_version` tells 3 from 4).
- Unity: `ProjectSettings/ProjectVersion.txt` (it names the editor version, `m_EditorVersion`).
- Phaser: `phaser` in `package.json` dependencies.
- three.js: `three` in `package.json` dependencies.

A detected engine is shown on the project (a small badge by its name) and its pack switches on
for the project's sessions. Nothing is asked. A folder can match more than one (three.js inside
a Godot project's web export, say); the one at the root wins and the others are listed.

### Ask only when there is nothing to detect

The New project dialog (`shell.rs`, `ask_project_name`) gets one optional row under the name:

> **Engine**  Phaser · three.js · Godot · Unity · Not a game · Not sure

- Each choice shows its card (the table above) when hovered or selected.
- **Not a game** (or leaving it alone) makes an ordinary project, as today. Wundur stays a
  general agent window; nobody who isn't making a game sees anything new.
- **Not sure** makes the project and starts the first session with the engine question handed
  to the agent: it asks what the person wants to make and recommends one, using the same cards.
  The choice is then recorded the same way.
- The row only appears for an empty folder (or one with nothing detected).

The choice is kept in the project's settings, so it can be changed later from the project's
menu (Engine › …), which re-runs the checks and offers the new pack's scaffold.

### Check the tooling, never install it

After choosing (or detecting), Wundur checks what the pack says it needs:

- Godot: `Godot.app` in `/Applications` or `~/Applications`, or `godot` on the PATH; version
  `4.x` from `--version`.
- Unity: Unity Hub installed; the editor version from `ProjectVersion.txt` installed under the
  Hub's editors folder.
- Phaser and three.js: `node` and `npm` on the PATH, at the version the pack names.

Anything missing becomes task cards ("Install Godot 4", with the download page), and Wundur
checks again while the cards are open so they tick off once installed. Wundur never downloads
or installs an engine itself.

### Scaffold a starter

For a new project, the pack's skill has the agent make the smallest project that runs: a Vite
+ Phaser scene with one sprite moving; a Vite + three.js scene with a lit cube and orbit
controls; a Godot 4 `project.godot` with a main scene and a script; for Unity, the steps to make
the project in the Hub (it can't be scaffolded from outside reliably), then the agent takes over.
The agent does it, from the pack's instructions, so the first Run works and the person watches
it happen; Wundur ships no template folders.

## What a pack contains

An engine pack is a tool pack with an `engine` section, not a second plugin system. It can still
carry tools of its own (a script that turns the packer's atlas into the engine's format, below).

```
wundur-godot/
  pack.json
  skill/SKILL.md          what the agent should know in this engine
  skill/scaffold.md       how to start a new project
  export-atlas.js         sprite atlas -> SpriteFrames .tres
  README.md
```

```json
{
  "id": "wundur/godot",
  "name": "Godot",
  "version": "1.0.0",
  "description": "2D or 3D games for desktop and mobile, with a real editor. Free and open source.",
  "engine": {
    "card": {
      "best_for": "2D or 3D games you want to ship on desktop or mobile.",
      "needs": "Godot 4",
      "note": "Agents work well here: scenes are text files and it runs from the command line."
    },
    "detect": [
      { "file": "project.godot" }
    ],
    "requires": [
      {
        "name": "Godot 4",
        "app": ["/Applications/Godot.app", "~/Applications/Godot.app"],
        "command": "godot",
        "version": { "args": ["--version"], "match": "^4\\." },
        "install": "https://godotengine.org/download/macos/"
      }
    ],
    "skill": "skill/SKILL.md",
    "scaffold": "skill/scaffold.md",
    "atlas_export": "export-atlas.js"
  },
  "tools": [
    { "name": "godot_atlas", "description": "…", "script": "export-atlas.js", "params": [], "hidden": true }
  ]
}
```

Detection and tooling checks are declared, not scripted: pack scripts run in the QuickJS
sandbox, which has no files outside the project and no processes, and that stays true. Wundur
evaluates `detect` and `requires` itself (file exists, JSON key present, app bundle exists,
command on PATH, its output matches). Running the engine (Godot headless, `npm run dev`) is the
agent's own shell, as now; the skill tells it the commands.

### Native to each agent

The skill content is given to each agent in the form it reads, per the native-over-normalized
rule: a project skill in `.claude/skills/` for Claude Code (which Cursor also reads), the
equivalent for other backends. It is installed with the existing skill installer's bookkeeping
(`skill_install`), so Wundur knows it put it there, can update it when the pack updates, and
removes it if the engine changes.

### Bundled

The four ship inside Wundur.app and are installed into the packs folder on first run (updated
when Wundur updates, unless the person changed one). Settings › Packs lists them like any other
pack and each can be turned off.

## The sprite packer

### For agents, shown to people

There is no packer window to operate. The agent works it with tools; Wundur shows the result.
When the person wants a change ("the run animation is too fast", "the coin's pivot is off"),
they say so and the agent edits it, as with 3D models today.

### A settings file is the source of truth

`<project>/<anywhere>/*.atlas.json`, written and edited by the agent:

```json
{
  "inputs": ["art/player", "art/items"],
  "output": "public/atlas/game",
  "max_size": 2048,
  "power_of_two": true,
  "trim": true,
  "padding": 2,
  "extrude": 1,
  "animations": {
    "run":  { "fps": 12, "loop": true },
    "jump": { "fps": 10, "loop": false }
  },
  "pivots": { "coin": [0.5, 0.5] },
  "engine": "phaser"
}
```

The same settings and images always give the same atlas (stable order, no timestamps), so
changes are readable diffs and survive snapshots and undo. Frames are grouped into animations
by file name: `run_01.png`, `run_02.png` … become `run`.

### Tools

- **`pack_sprites(settings)`**: packs (MaxRects, with rotation off by default since not every
  engine reads it), trims, pads and extrudes, splits into more than one page when it won't fit,
  writes the pages and a neutral atlas JSON, then calls the engine pack's `atlas_export` to write
  the engine's own file. Returns facts and warnings, as `look_at_model` does: page sizes, how
  full each is, sprites that didn't fit, frames in one animation that differ in size, a gap in a
  numbered sequence, a sprite larger than `max_size`, edges that may bleed.
- **`look_at_atlas(settings)`**: returns the page images with each sprite's outline and name,
  and each animation's frames as a strip, so the agent sees what it made before saying it's done.

The packing itself is Rust in `wundur-core` (a new `atlas` module), served through `wundur-mcp`
like the other built-in tools, so it is fast, needs nothing installed, and works for every agent.

### Export, per engine

- **Phaser**: its multi-atlas JSON plus animation definitions the scene can load.
- **three.js**: the neutral JSON (frame rects in UV terms) and a small loader the skill describes.
- **Godot**: the atlas page plus a `SpriteFrames` `.tres` with each animation, its speed and loop.
- **Unity**: no atlas of its own. Unity packs sprites itself (Sprite Atlas, at build time), so the
  Unity pack writes trimmed loose sprites with `.meta` import settings (pivot, sprite mode) and
  the agent adds a Sprite Atlas asset in the editor.

### The tab

A tab on the right, beside the terminal (the default layout keeps the terminal there):

- the pages, with each sprite outlined and named on hover;
- each animation playing at its speed, frame by frame on click;
- the facts and warnings from the last pack.

It updates when the settings file or any input image changes. It has no editing controls: the
only action is "Ask the agent" with the sprite or animation already named in the message.

### Not in the first version

Polygon (mesh) packing, normal-map packing, compressed texture formats (ASTC, ETC), and export
formats beyond the four engines.

## Order of work

1. Engine packs: the `engine` manifest section, detection on open, the project dialog row, the
   "Not sure" handoff, tooling checks with task cards, skill install per agent.
2. The Godot and three.js packs, complete (card, detection, checks, skill, scaffold).
3. The Phaser pack, complete.
4. The Unity pack: card, detection, checks, skill; an editor bridge after.
5. The sprite packer: `atlas` module and `pack_sprites`, then `look_at_atlas`, then the tab,
   then each pack's `atlas_export`.

## Open questions for the owner

- **Where the engine skill lives.** In the project (`.claude/skills/`, committed, visible, works
  if the project is opened without Wundur) or given by Wundur at session start (nothing added to
  the repo, but only in Wundur). This proposal picks the project, for portability.
- **"Not sure" handoff wording.** Whether the first message is written by Wundur and shown to
  the person, or the agent gets it as context and opens the conversation itself.
- **Unity editor bridge.** Recommend one of the existing Unity MCP packages in the Unity skill, or
  wait and build our own later.
- **Engine badge.** Whether the detected engine shows on the project in the sidebar, or only in
  the project's menu.
