# Proposal: "Help me choose" in the New Project wizard

Status: proposal for the owner to read. Written 2026-11-25.

## The idea in one paragraph

The wizard's engine step keeps its four cards (Three.js, Phaser, Godot, Unity) and gains a
**Help me choose** button. It asks a few short questions, each one shaped by the answers before
it, and ends on a **ranked list of the engines that fit**: "Based on what you picked, I recommend
Godot; Unity or Phaser will also work", with one line on why for each. Engines that can't make
the game (Phaser for 3D) aren't listed. The list advises; it doesn't decide. Picking any row
carries on to the details step as if its card had been clicked.

This replaces the "Not sure" choice in the engine-packs proposal, where the agent asked these
questions in the first session. A few clicks in the wizard is quicker and keeps the choice before
the project is made.

## The flow

```
Kind: Game ─▶ Pick an engine ─┬─ click a card ──────────────▶ (2D or 3D?) ─▶ Details
                              └─ Help me choose
                                   1. Platform
                                   2. Type
                                   3. Camera        (skipped when the type settles it)
                                   4. Code or editor (optional; see below)
                                   ─▶ Recommendation ─▶ Details
```

The questions fill in the 2D/3D choice, so the "2D or 3D?" step is skipped after the
recommendation. Back works through every question, and the step counter shows only the
questions this path actually asks.

## The questions

### 1. Where should it run?

Several can be ticked. **All of these** ticks web, desktop and mobile; consoles stays its own box,
because ticking it rules out the browser engines.

- Web browser
- Desktop (Mac, Windows, Linux)
- Mobile (iPhone, Android)
- Consoles

Consoles is my addition. It's the only platform answer that really narrows the list (in practice,
Unity), so it's worth one checkbox. Multi-select is also my suggestion: "web and mobile" is a common
answer, and a single choice would force people to pick "All".

When more than one is ticked, one more line under the boxes asks **Which comes first?** It lists
the ticked platforms plus **No preference** (the default). The first platform counts double in the
ranking. This matters most for mobile. A browser engine on a phone runs inside a web view: on
iPhone that caps it at 60 fps, and heavier scenes often struggle to reach even that. Unity and
Godot build native apps that don't have that cap. So "mobile first" moves Godot and Unity to the
top, while "web first, mobile too" keeps Phaser or Three.js on top and still says what a phone
costs.

### 2. What type of game?

- **2D**: flat art, seen from the side or from above.
- **3D**: models and a camera that moves through the world.
- **Isometric**: an angled view from above (tactics, builders, classic RPGs).

Isometric gets one follow-up, because it's made in two different ways that suit different engines
and different asset tools:

- **Drawn art** (sprites on an isometric grid): counts as 2D.
- **3D models** (with a fixed angled camera): counts as 3D.

### 3. Where is the camera?

Skipped for isometric, which already says where the camera is. The options depend on the type:

| 2D | 3D |
|---|---|
| Side view (platformers, side-scrollers) | First person |
| Top-down (adventure, shooters, racing) | Behind the player (third person) |
| One fixed screen (puzzle, card, board) | Follows from above (top-down, strategy) |
| | Side-on (a 3D side-scroller) |
| | Chase (racing, flying) |

The camera rarely rules an engine out. It changes the ranking a little (below), and it's worth
keeping for the starter project and the agent's brief (see "Later").

### 4. Write it in code, or work in an editor? (optional; my addition)

- **Code only**: the agent writes everything, and you play it in Wundur.
- **An editor**: a visual scene editor to lay out levels and tweak things by hand.
- **Don't mind** (the default).

I'm proposing this one, and you can strike it. After 2D/3D it separates the engines best: Phaser
and Three.js are all code, while Godot and Unity are built around an editor. It is also the
difference that matters most for how much the agent can do on its own. This is not the language
question: it doesn't mention JavaScript, GDScript or C#.

## The recommendation

A short list, best fit first, with one line each, using the same card art as the engine step:

> **Based on what you picked, I recommend Godot.** Unity or Three.js will also work.
>
> 1. **Godot**: 3D first person, with one project for desktop and mobile. Free; needs Godot 4
>    (not installed yet).
> 2. **Unity**: the same reach plus consoles later, but agents find its scene files hard to edit.
> 3. **Three.js**: runs in a browser and wraps for mobile; you'd build the player controls
>    yourself.
>
> [Pick Godot]   [Pick Unity]   [Pick Three.js]   ·   Back

- **Left out entirely:** engines that can't make the game. Nothing says "Phaser: no".
- **Not installed:** still listed and ranked on fit, with "needs …" in the reason; installing
  works as it does from the cards today. Being installed only breaks ties.
- **One engine left** (consoles + 2D, say): "Unity is the one that fits", with its reason, and
  no "also work" line.
- **None left:** can't happen with the four built-in engines (every platform and type has at
  least Unity). If an installed pack replaces an engine and leaves a gap, the list says so and
  offers the cards.

## How it ranks

Every rule comes from the engine's pack, not from code that names engines, so an installed pack
that adds or replaces an engine takes part automatically.

**Rule out** an engine when:
- its `dimensions` don't include the answer's 2D/3D (Phaser for 3D, Three.js for 2D), or
- it says `no` for a platform that was ticked.

**Score** the rest. Each fact a pack gives is `best`, `works` or `no`:

| | Phaser | Three.js | Godot | Unity |
|---|---|---|---|---|
| Web | best | best | works (larger download) | works (large builds, weak on phones) |
| Desktop | works (wrapped) | works (wrapped) | best | best |
| Mobile | works (in a web view: 60 fps cap on iPhone, struggles with heavy scenes) | works (same, and 3D feels it more) | best (native app) | best (native app) |
| Consoles | no | no | works (through a porting company) | best |
| Code only | best | best | works | works |
| Editor | no | no | best | best |
| Camera: first/third person | – | works (controls built by hand) | best | best |

`best` scores 2, `works` scores 1, and an empty cell scores 1. With several platforms ticked, the
platform scores add up, and the platform that comes first counts double. Consoles is the only
answer that rules engines out; every other platform answer only weights the ranking. **Code or editor** counts only when answered, and "Editor" rules out
Phaser and Three.js (an answer of `no`). Ties are broken by: installed first, then how well
agents work in the engine (a pack field, `agent_fit`, which puts Unity last for the reasons in
the engine-packs proposal), then the cards' order.

**The reason line** is built from the two strongest facts for that engine plus its biggest
caveat, using a short phrase each pack gives for each fact ("one project for desktop and
mobile", "large web builds"). That keeps the reasons honest and specific without hand-writing
every combination.

### Worked examples

| Answers | List |
|---|---|
| Web · 2D · side view | Phaser, Godot, Unity |
| Mobile · 3D · first person | Godot, Unity, Three.js |
| Web + mobile, **web first** · 2D | Phaser, Godot, Unity (Phaser's line warns about the 60 fps cap on iPhone) |
| Web + mobile, **mobile first** · 2D | Godot, Unity, Phaser |
| All · isometric drawn art | Godot, Unity, Phaser |
| All · isometric 3D models | Godot, Unity, Three.js |
| Web · 3D · chase · code only | Three.js, Godot, Unity |
| Desktop · 2D · editor | Godot, Unity |
| Consoles · anything | Unity, Godot |

## What changes

- **`EngineSpec`** (`crates/wundur-core/src/engines.rs`) gains a `fit` section: `platforms`
  (web/desktop/mobile/consoles → best/works/no plus a phrase), `workflow` (code/editor →
  best/works/no plus a phrase), `cameras` (optional, same shape), and `agent_fit` (a number).
  A pack without `fit` scores `works` everywhere, so older installed packs still appear.
- **The four `packs/engines/*/pack.json`** get those values (the table above).
- **A pure ranking function** in `wundur-core`: answers + engines → ranked list with reasons.
  Most of the tests go here, one per worked example plus the edge cases.
- **`project_wizard.rs`**: a Help me choose button on the engine step, four question steps, the
  result step, and Back/step-counter handling. The 2D/3D answer is passed on so the Dimension step
  is skipped.

## Later (not in this proposal)

- Save the answers (platforms, type, camera) in `.wundur/project.json`, so the starter template
  can match the camera (a side-view scene vs a top-down one), the first session's brief can
  mention it, and the asset tools know whether to make sprites or models.
- Offer the type and camera questions to people who pick a card directly, as an optional "about
  your game" step, since they're useful even when the engine is already chosen.
