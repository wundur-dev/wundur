# Proposal: asset tools first, cards second

Status: proposal for the owner to read. Written 2026-11-26. Nothing here is built yet.

## The idea in one paragraph

Today the hosted 3D flow (Describe → Views → Model → Rig) is a card: the person drives it with
clicks, and the agent at most opens it. This proposal flips the default. The agent gets each step
as a **tool** and drives the flow itself, inside a **budget** the person approves once. It brings
the person in through a **review** tool where the person asked to be consulted, or where the
agent judges it needs a decision. The card stays. It becomes the surface the agent opens when it
wants an answer, and the way a person starts something themselves from the Create menu. Both work
on the same asset, so either can pick up where the other stopped.

## Why

- **The agent knows the game. The card doesn't.** It knows the engine, the scale, the art style,
  the folder the files go in, and what the asset is for. With tools it can chain views → model →
  import → rig → place it in the scene → look at it with `look_at_model` → fix what's wrong. On
  "Humanoid Raptor" the views had a tail and the model didn't. An agent comparing the two would
  have caught that before Import.
- **Many requests don't need a person at every step.** "Make me ten crates and barrels in the
  style of the ones in `assets/props`" is a batch job. Clicking through ten cards is the slow part.
- **The person still decides what matters to them.** Whatever the person marks as theirs to decide
  still comes to them. Everything else goes ahead.

Speed is a side effect, not the reason. Most of the wait is the providers: a model job takes
minutes whether a click or a tool starts it. What we save is the person waiting by the card to
press the next button.

## What Wundur offers once the agent drives

If any agent can call the API through MCP, what is the window for? The owner's answer is that
Wundur knows how to read what the tools return, which a plain harness doesn't, so the person can
work with it. That's the centre of it. Four more things belong with it:

1. **It draws what comes back.** In a terminal, a tool's result is JSON and a path. In Wundur it
   is the views side by side, the model turning, the sound with a play button, each with the
   actions that make sense (approve, change, reroll, open in the 3D view). That's what makes the
   tools usable by someone who isn't reading JSON.
2. **It holds the keys and the purse.** The agent never sees the account's key. Spending is
   checked by the window against a budget the person set. The agent can't overspend by
   misreading the rules, and a harness without Wundur has no such guard.
3. **It turns the person's answer into data.** The review tool returns the decision ("approve set
   2", "change: give it a tail") as structured input the agent acts on. A chat message would have
   to be parsed.
4. **It keeps the asset together.** Views, previews, the imported model and the rig are one
   asset, saved in the project under one folder, with the card able to reopen it later. In a
   plain harness they're files the agent happened to write.

The API's value (the pipeline: views cut from one sheet, engines picked per kind, rigging) is
there with or without the window. Wundur's value is the human side of it. That matches why
Wundur exists: built for the owner's own way of working, with the hosted pipeline as the product
experiment.

## What exists today

- **Hosted tools for images and sound.** `make_image`, `edit_image` and `sound_effect` call the
  hosted API with the account's key (`crates/wundur-core/src/account_tools.rs`). Each run asks
  first (Allow / Always for this session / Deny, from `tool_runs.rs`). Results are saved in
  `assets/generated/`.
- **Own-key tools for 3D.** The game-assets pack has `model_from_text`, `model_from_views` and
  `rig_model` with the person's own provider keys.
- **The hosted 3D flow is card-only.** `asset_flow.rs` and the asset card call the routes in
  `docs/proposals/hosted-asset-flow.md`: `/v1/assets`, sets, approve, models, import, rig.
- **Agents can open the card but not spend.** The agreed contract says *"Agents can start, only
  people can spend."* This proposal changes that rule (see "Spending" below), so it needs the
  owner's explicit yes.
- **`look_at_model`** already lets the agent check a GLB from fixed angles.

## The tools

All of these are signed-in account tools, served by `wundur-mcp` like the current ones. They use
the existing routes, so **step 1 needs no API change**.

| Tool | Does | Calls | Spends |
| --- | --- | --- | --- |
| `asset_kinds` | The kinds, their fields, engines and riggers with their credits | `GET /v1/catalog` | nothing |
| `new_asset` | Starts an asset from a kind, name, description and fields; returns its first set when ready | `POST /v1/assets` | 1 image |
| `change_views` | A new set from an old one plus an instruction, or a fresh retry | `POST /v1/assets/{id}/sets` | 1 image |
| `approve_views` | Marks the set models are made from | `POST …/sets/{set}/approve` | nothing |
| `make_model` | A model attempt from the approved set with an engine; returns the preview | `POST /v1/assets/{id}/models` | nothing (preview) |
| `reroll_model` | Another attempt, same or another engine | `POST /v1/models/{id}/reroll` | nothing |
| `import_model` | Saves the real files into the project | `POST /v1/models/{id}/import` | the engine's credits |
| `rig_model` | Rigs an imported model | `POST /v1/models/{id}/rig` | the rigger's credits |
| `asset_status` | Where an asset or job has got, and waits for it | `GET /v1/assets/{id}`, `/v1/rigs/{id}` | nothing |
| `review_asset` | Opens the card on this asset with a question; returns the person's answer | (the window) | nothing |

Each result gives the agent what it needs to carry on: ids, statuses, the paths saved in the
project, the stills as images it can see, and what was spent and what's left of the budget. View
images and preview stills go back as image content, so the agent can compare them with its own
eyes before going on.

`rig_model` would take the pack tool's name. Signed in, the account version wins, as
`make_image` does today.

### Long jobs

Tool calls time out (ours at 180 s; Claude Code's MCP timeout is its own). A rig takes 2–4
minutes. So every tool that starts a job **waits up to about 150 s** and then returns either the
result or `{ "status": "running", "id": … }` with a line telling the agent to call
`asset_status` to keep waiting. Most image and model steps finish inside one call, so the agent
usually makes one call per step and only polls for rigs.

### What the chat shows

The window recognises its own tools by name. Their tool cards in the chat are drawn as small
asset cards rather than as JSON: the views strip for a set, a still and "Open 3D view" for a
model, the files for an import. They carry the same buttons as the full card, and a click there
is the person acting directly; the agent is told about it (see "Cards a person drives").

## Spending

The current rule, "agents can start, only people can spend", has to become **"people set the
budget, agents spend inside it."** Asking on every call makes the agent slower than the card: a
humanoid is a set, maybe a change, a model, an import and a rig, so five or six prompts.

- **Approve once per job.** The first spending call in a turn shows one card: *"Make Goblin scout:
  up to 3 images and 4 model credits. Allowance left: 2 images, 3 credits."* with Allow, Change
  the limit, Deny. The numbers come from the agent's plan (an optional `budget` argument on
  `new_asset`) or, without one, from the catalog's cost for one pass through the kind's steps.
- **The window counts.** Each spending response reports what it spent. The window adds it up and
  refuses a call that would go over, with a message the agent can act on ("over budget: 1 credit
  left, `import_model` needs 3; ask the person"). The agent never sees, and can't change, the
  count.
- **Free steps are free.** Previews, rerolls, approve and status need no approval, since the
  contract already makes them cost nothing the person sees.
- **A standing budget is opt-in.** Settings > Account could hold "let agents spend up to N a day
  without asking", off by default, for people who batch.

## When the agent asks

The owner's version lets the agent decide when to ask, guided by what it was told. My worry
there: the step where the person's judgement matters most is picking between views, and that's a
taste call models are weak at. "What you told it" also drifts between turns and sessions. So the
ask points are a **setting the window enforces**, and the agent's own judgement adds to it but
never removes from it.

- **Ask me at:** *Views* (default) · *Views and model* · *Only to spend more* · *Never*. It's set
  per project and shown on the budget card, so it can be changed right there ("just make it" is
  one click).
- **Enforced where it can be.** With *Views*, `approve_views` from the agent is refused with "the
  person approves views in this project: call `review_asset`". With *Views and model*,
  `import_model` is refused the same way. The agent can't skip a check the person asked for.
- **The agent may always ask more.** If it's unsure (two sets both look right, the model lost
  something the views had), it calls `review_asset` on its own. The tool descriptions say when
  that's worth doing.

### `review_asset`

```json
{ "asset": "as_7f2", "question": "Which views should the model be made from?", "focus": "views" }
```

The window opens the asset card in the chat on that asset, scrolled to `focus` (`views`,
`model`, `rig`), with the question on top. The agent's call waits. The person approves a set,
asks for a change, picks a model attempt, or writes a note. The tool returns that as data:

```json
{ "decision": "change", "set": "set_1", "instruction": "Give it a long tail like a raptor's" }
```

If the person doesn't answer before the timeout, it returns `{ "decision": "none_yet" }` and the
card stays open. When they do answer, the answer reaches the session as a new turn (the way
`send_message` does) so the agent picks it up. If the session's agent is busy with something
else, it gets it then.

## Cards a person drives

The Create menu and its cards stay. When a person drives a card themselves (or clicks a button
on a small card in a tool result), the window posts one line into the chat for the agent at the
next turn: *"The person imported Goblin scout (as_7f2): assets/goblin-scout/goblin-scout.glb,
rigged: no."* That's how "I made the goblin in the card, now put it in the level" works without
the person retyping paths.

## Later: the API serves the tool definitions

Card manifests exist so the API can ship a new workflow without an app release. Tools written in
Rust would lose that. The fix is for the API to serve tool definitions the way it serves
workflows (`GET /v1/tools`, or a `tools` list in each workflow): name, description, input
schema, which call to make, what it spends, and which kind of result it returns (`asset_set`,
`model`, `files`, `sound`) so the window knows how to draw it.

I'd **not** start there. The shapes above will change once an agent actually uses them, and
changing Rust is cheaper than changing a served format. Build them in the app first, settle the
shapes, then move the definitions to the API, the way the cards went from built-in to manifests.

## Order of work

1. **The 3D tools in the app** against today's routes, with the waiting described above and
   results as plain text plus images. Each still asks per call, as the image tools do. *Checks
   that an agent can drive the flow end to end.*
2. **The budget**, replacing the per-call asks for all account tools (images and sounds too).
3. **The ask-points setting and `review_asset`**, with the refusals that enforce it.
4. **Small asset cards in tool results**, and the line posted when a person drives a card.
5. **Served tool definitions** (API work), once 1–4 have settled.

Steps 1–3 are mostly `wundur-core` (`account_tools.rs`, `asset_flow.rs`) and `tool_runs.rs`.
Step 4 reuses the asset card's pieces in `chat_panel/asset_card.rs`.

## Out of scope

- Own-key 3D tools (the pack's): unchanged.
- How the API makes things: unchanged.
- Placing the model in a game engine's scene: that's the engine packs' job; these
  tools hand over paths.

## Open questions for the owner

1. **The spending rule.** Are you happy to replace "agents can start, only people can spend" with
   "people set the budget, agents spend inside it"?
2. **The default ask point.** Is *Views* the right default, or would you rather the default be
   *Only to spend more* and let people tighten it?
3. **Standing budget.** Do you want the "up to N a day without asking" setting at all, or should
   every job be approved?
4. **Outside Wundur.** Should the API offer these tools as its own MCP server for people using a
   plain harness (no drawing, no budget guard beyond the server's), or keep them a reason to use
   Wundur? It changes what the hosted service is selling.
5. **Images and sound.** Should `make_image` and `sound_effect` move to the budget at the same
   time (step 2), or keep asking per call because they're cheap and quick?
