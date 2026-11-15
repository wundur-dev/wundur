# Proposal: the hosted asset flow (API contract)

Status: **agreed** (2026-11-15). Drafted by the client session, reviewed by the API session;
its answers and changes are folded in below (the open questions are answered at the end).
Decisions it rests on are Stewart's "Answers on the asset flow" (the nine points); each section
says which one it follows. One change the client asked for after the review is marked
*proposed*: `attempts_left` instead of `rerolls_left`.

## What it's for

Wundur Studio walks a person from an idea to a model in their project:

1. **Describe** the thing and its kind (a humanoid, a creature, a prop, a place) with the fields
   that kind asks for.
2. **Views:** the service returns the views it decided on (three for a humanoid, one for a
   prop). The person approves them, asks for a change, or starts again. Changes always apply to
   the whole set.
3. **Model:** the person picks an engine (its credits and what it's good at shown), sees a
   preview, and may reroll up to three times per approved set at no cost.
4. **Import:** this is where model credits are spent and the files arrive in the project.
5. **Then:** rigging in the background, or Mesh2Motion in the app, or a Mixamo export.

Sound effects and turn summaries are separate flows on the same account.

The app draws all of this natively. **The flow is in the app; what's on offer is data from the
service** (the catalog), so a new kind, engine or rigger appears without an app update. How the
service makes things (one sheet split into views, which provider, self-hosted or not) is the
service's business and never shown.

## Principles

- **The server decides how; the app shows what came back.** The app never assumes a number of
  views, an engine or a provider. (Answers 1, 2.)
- **Only Import spends model credits.** Views spend images; previews and rerolls spend nothing
  the person sees. (Answers 3, 5.)
- **A preview is never the importable file.** (Answer 4.)
- **Agents can start, only people can spend.** The API needs nothing for this: the app enforces
  it (an agent opens the card; approving, choosing an engine, rerolling and Import are clicks).
  (Answer 8.)
- **Every long step is a job the app polls,** the same way for views, models, rigs and sounds.
- **Charging calls are idempotent.** The app sends an `Idempotency-Key` (a new UUID per click)
  on create, change, retry, import, rig and sounds. The server remembers each key per account for
  24 hours and answers a repeat with the first response, so a retry after a dropped connection
  can't charge twice.

## 1. The catalog

`GET /v1/catalog` (any valid key or session; cacheable). The app fetches it at sign-in and when
the asset card opens, and keeps the last copy for offline display. `ETag` / `If-None-Match`
supported.

```json
{
  "version": "2026-11-15.1",
  "kinds": [
    {
      "id": "humanoid",
      "label": "Humanoid",
      "description": "People and anything standing on two legs.",
      "fields": [
        { "id": "style", "label": "Style", "type": "enum", "options": ["realistic", "stylized", "low poly", "pixel"], "default": "stylized" },
        { "id": "height", "label": "Height", "type": "enum", "options": ["child", "adult", "giant"], "default": "adult" },
        { "id": "notes", "label": "Anything else", "type": "text", "required": false }
      ],
      "views": 3,
      "supports_3d": true,
      "engines": ["trellis2", "tripo", "tripo_hd"],
      "default_engine": "trellis2"
    },
    {
      "id": "place",
      "label": "Place",
      "description": "A room, a building, a landscape.",
      "fields": [],
      "views": 1,
      "supports_3d": false,
      "engines": [],
      "offers": ["concept_art"]
    }
  ],
  "engines": [
    {
      "id": "trellis2",
      "label": "Standard",
      "credits": 1,
      "plans": ["free", "comp", "monthly"],
      "seconds": 60,
      "best_for": ["props", "creatures", "stylized characters"],
      "help": "Fast and reliable. Clean shapes; simpler textures.",
      "license": { "commercial": true, "label": "" }
    },
    {
      "id": "tripo_hd",
      "label": "High detail",
      "credits": 3,
      "plans": ["monthly"],
      "seconds": 180,
      "best_for": ["detailed characters"],
      "help": "Finer detail and textures; slower.",
      "license": { "commercial": true, "label": "" }
    }
  ],
  "riggers": [
    {
      "id": "auto",
      "label": "Automatic rig",
      "credits": 1,
      "kinds": ["humanoid"],
      "seconds": 240,
      "help": "Runs on our server; the first rig after a quiet spell can take several minutes.",
      "license": { "commercial": true, "label": "" }
    }
  ],
  "sounds": [
    {
      "id": "sfx",
      "label": "Sound effect",
      "max_seconds": 30,
      "license": { "commercial": true, "label": "" }
    }
  ]
}
```

- **Fields:** `type` is `text`, `enum`, `number` (with `min`, `max`) or `bool`; `default`,
  `required` (default false) and `help` optional. The app renders them as a form, in order.
- **`views`** is what the kind usually returns, for the app's placeholders while it waits. The
  real count is whatever the set holds.
- **`supports_3d: false`** with `offers` tells the app what to show instead (concept art today).
  The app never hides a kind on its own. (Answer 2.)
- **`plans`:** engines the person's plan can't use are shown disabled with "Monthly plan". The
  server refuses them anyway (`engine_not_on_plan`).
- **`license.label`:** empty for cleared commercial use; otherwise shown on the choice ("Personal /
  non-commercial use only"). (Answer 7.)
- **`help`** is plain text, short; the app shows it in a collapsible "Which should I use?".

## 2. Allowances and credits

`GET /v1/me` keeps its shape and its `allowance` becomes buckets (Answer 5):

```json
"allowance": {
  "images":        { "used": 3, "limit": 5,    "left": 2 },
  "model_credits": { "used": 1, "limit": 3,    "left": 2 },
  "sound_seconds": { "used": 0, "limit": 60,   "left": 60 },
  "summaries":     { "used": 12, "limit": 500, "left": 488 },
  "summary_tokens":{ "used": 40000, "limit": 2000000, "left": 1960000 }
}
```

- **Images:** one per generated set, however many views it holds (one image call makes the
  sheet).
- **Model credits:** spent on Import, by the engine's `credits`; rigging by the rigger's.
- **Sound seconds:** spent on generation, by the length asked for.
- The hidden safety cap on unimported generations isn't listed; hitting it is
  `generation_limit` (see Errors).

## 3. Assets and their sets

An **asset** is one thing being made. Its **sets** are the rounds of views (the first, each
change, each fresh start).

### Create

`POST /v1/assets` with `Idempotency-Key`:

```json
{ "kind": "humanoid", "name": "Goblin scout", "description": "A wiry goblin in leather armour with a short bow", "fields": { "style": "stylized", "height": "child" } }
```

→ `202` with the asset; its first set is `generating`. Spends one image (refunded if the set
fails).

### The asset

`GET /v1/assets/{id}` (poll every 2–3 s while anything is `generating` or `running`):

```json
{
  "id": "as_7f2",
  "kind": "humanoid",
  "name": "Goblin scout",
  "description": "A wiry goblin in leather armour with a short bow",
  "fields": { "style": "stylized", "height": "child" },
  "created_at": "2026-11-15T10:00:00Z",
  "sets": [
    {
      "id": "set_1",
      "status": "ready",
      "made_from": { "kind": "new" },
      "views": [
        { "label": "Front", "image_id": "img_a", "url": "https://…", "width": 1024, "height": 1536 },
        { "label": "Side",  "image_id": "img_b", "url": "https://…", "width": 1024, "height": 1536 },
        { "label": "Back",  "image_id": "img_c", "url": "https://…", "width": 1024, "height": 1536 }
      ],
      "approved": false,
      "attempts_left": 4
    }
  ],
  "models": []
}
```

- Set `status`: `generating`, `ready`, `failed` (with `error`, not counted).
- `made_from`: `{ "kind": "new" }`, `{ "kind": "change", "from": "set_1", "instruction": "…" }`
  or `{ "kind": "retry", "from": "set_1" }`.
- Views are cut from one sheet, so each is smaller than a full image (three views on a 1536×1024
  sheet are about 500×1000). The app uses the `width`/`height` given and never assumes 1024.
- If the server can't split a sheet cleanly it tries once more itself; if that fails too the set
  fails (`provider_error`, not counted). The app just shows a failed set.
- View `url`s are short-lived; every `GET` gives fresh ones. The app downloads each view when its
  set is ready (they're paid for, so the person may keep them) into the project's
  `assets/<asset>/views/`.

### Change, retry, approve

- `POST /v1/assets/{id}/sets` with `{ "from": "set_1", "instruction": "Make the armour darker" }`
  → a new set made from the old one (the whole sheet changed and split again). Spends one image.
  (Answer 1.)
- `POST /v1/assets/{id}/sets` with `{ "from": "set_1", "retry": true }` → a new set from the
  description alone. Spends one image.
- `POST /v1/assets/{id}/sets/{set_id}/approve` → the set the models are made from. Only one set
  is approved at a time; approving another moves it. Models need an approved set.

### Edit the description

`PATCH /v1/assets/{id}` with any of `name`, `description`, `fields`. Takes effect from the next
set. (For "start over at the image stage" with a better description.)

## 4. Models: preview, reroll, import

### Start

`POST /v1/assets/{id}/models` with `{ "set": "set_2", "engine": "trellis2" }` → `202` with a
model attempt. Nothing the person sees is spent. One model job at a time per account
(`409 job_in_progress`).

```json
{
  "id": "md_91",
  "asset": "as_7f2",
  "set": "set_2",
  "engine": "trellis2",
  "status": "running",
  "progress": 0.4,
  "preview": null,
  "imported": false,
  "attempts_left": 3,
  "created_at": "…"
}
```

- `status`: `queued`, `running`, `succeeded`, `failed` (with `error`; not counted, and doesn't
  use a reroll).
- `progress` (0–1) when the engine reports one; otherwise absent and the app shows the catalog's
  `seconds` as "usually about a minute".
- When `succeeded`, `preview` is filled (Answer 4):

```json
"preview": {
  "turntable": { "url": "https://…/turntable.webp", "frames": 36 },
  "mesh": { "url": "https://…/preview.glb", "reduced": true },
  "stills": ["https://…/front.png", "https://…/side.png"]
}
```

  At least one of `turntable` (an animated image or a short MP4) and `mesh` (decimated, low
  texture, never the import) is there. **The server does the reduced mesh first** (made on CPU),
  a turntable later. The app prefers `mesh` for its 3D viewer and falls back to the turntable.

### Reroll

`POST /v1/models/{id}/reroll` → a new attempt with the same set and engine (or
`{ "engine": "tripo" }` to change engine). **Four attempts per approved set** in all: the first,
plus three rerolls, counting fresh starts and engine changes alike; failed attempts don't count.
`attempts_left` = 4 − attempts on that set (*proposed name*; the review called it
`rerolls_left`, which reads as if the first try were a reroll). The app shows it as rerolls:
"3 rerolls left" after the first attempt. At zero: `409 rerolls_used`, and the app offers "change
the views" instead. (Answer 3.)

### Import

`POST /v1/models/{id}/import` with `Idempotency-Key` → spends the engine's credits (once per
attempt, however often it's called) and returns the files:

```json
{
  "id": "md_93",
  "imported": true,
  "files": [
    { "kind": "model", "format": "glb", "url": "https://…/goblin-scout.glb" },
    { "kind": "texture", "format": "png", "url": "https://…/albedo.png" }
  ],
  "model_credits_left": 1
}
```

The GLB has its textures embedded; separate texture files are listed only when an engine
provides them, so the app never relies on `texture` entries. It saves them under
`assets/<asset>/`, with the views already there. FBX and OBJ for Mixamo
are made by the app from the GLB (Answer 6); the server needn't.

### Discard

`POST /v1/models/{id}/reject`: optional, frees the preview. Nothing else changes.

## 5. Rigging

`POST /v1/models/{id}/rig` with `{ "rigger": "auto" }` and `Idempotency-Key`, on an imported
model → `202` job; spends the rigger's credits when it succeeds (refunded on failure).

`GET /v1/rigs/{id}`: `status` as for models, `progress` if known, and when done
`files: [{ "kind": "model", "format": "glb", "url": … }]` (the rigged GLB, with whatever
animations the rigger adds listed in `animations: ["idle", "walk"]`).

The app shows "Rigging… usually 2–4 minutes" (the catalog's `seconds`) on the card, keeps polling
in the background, saves the rigged file next to the model when done, and sends an inbox notice
only if the card isn't on screen. (Answer 9.)

## 6. Sound effects

`POST /v1/sounds` with `{ "engine": "sfx", "prompt": "Wooden door creaking open", "seconds": 3,
"variants": 2 }` and `Idempotency-Key` → `202` job; spends `seconds × variants` from
`sound_seconds` (refunded on failure). `GET /v1/sounds/{id}` → `status`, then
`files: [{ "format": "mp3", "url": … }]`. Short sounds often finish in seconds, so the first
`GET` may already be `succeeded`. Every variant generated is charged; the app plays them in a card
and saves the ones the person keeps. At most 30 seconds each (the catalog's `max_seconds`).

## 7. Errors

Same shape as today, `{ "error": { "code", "message" } }`. **402 means used up this month**
(including `image_allowance_used` and `summary_allowance_used`, which move from 429); **429 is
for rate limits** (`hourly_limit`, `generation_limit`). New codes:

| Status | Code | The app does |
|---|---|---|
| 402 | `model_credits_used` | "Out of model credits this month", Upgrade |
| 402 | `sound_seconds_used` | as above for sounds |
| 409 | `rerolls_used` | offer to change the views instead |
| 409 | `set_not_approved` | (a bug in the app; show the message) |
| 403 | `engine_not_on_plan` | show "Monthly plan" and Upgrade |
| 409 | `kind_has_no_3d` | (catalog changed; refresh it) |
| 429 | `generation_limit` | the hidden safety cap: "That's a lot of tries this month; try again tomorrow or contact us" |
| 404 | `not_found` | the asset or model was cleaned up (30 days) |

Existing ones stay (`image_allowance_used`, `hourly_limit`, `content_blocked`, `job_in_progress`,
`provider_error`, `busy`, `invalid_key`…).

## 8. What the app keeps

- The asset's id and its local folder (`assets/<slug>/`), so a card picks up again after a
  restart: it `GET`s the asset and redraws its state.
- Downloaded views, imported models and rigs, as ordinary project files.
- The last catalog.

Nothing is kept on the server for the app beyond the 30 days of the API's storage; the project
files are the record.

## Answered questions

1. **Images per set:** one per set, whatever the view count (one generation call).
2. **Rerolls:** four attempts per approved set, everything counting (rerolls, fresh starts,
   engine changes), failures not.
3. **Preview format:** a reduced mesh first, a turntable later.
4. **Sound variants:** every variant generated is charged.
5. **Concept art for places:** the same set mechanism with `supports_3d: false`; no endpoint of
   its own.
6. **Old endpoints:** `/v1/images/*` stays (a plain image tool may keep using it); `/v1/jobs` and
   `/v1/audio/sound-effects` stay until the app has moved over, then go.
7. **Push vs polling:** polling to start, SSE later.

Server-side note from the client: views cut from a sheet are about 500×1000; the model engines
should get the best-resolution view available (upscaled if an engine needs more), which the app
never sees.

## Build order

API:
1. Credit buckets in `/v1/me`, the new error codes (402/429 split), the hidden generation cap,
   `Idempotency-Key`.
2. `GET /v1/catalog`.
3. Assets and sets: kind-specific prompts, one sheet per set, server-side split, change, retry,
   approve, PATCH.
4. Models: start, poll, reduced-mesh preview, four attempts per set, import.
5. Sounds as jobs with variants.
6. Rigging later, on its own (a self-hosted open rigger, after a licence check).

App, once API steps 1–2 are live: the Account page's buckets and the 402/429 handling, the
catalog client and the asset card's describe step; then views, models, import and sounds as the
API steps land.

## The app side, briefly (for context)

- The asset card is rebuilt natively on this: describe (catalog form) → views (approve / change /
  retry) → model (engine picker with credits and help, preview, reroll, Import) → rig / Mesh2Motion
  / Mixamo export. The same in the asset's tab, larger.
- Agents get a tool that **opens** a card with a description filled in; nothing else.
- Signed out, the card says what an account gives and offers Sign in. Bring-your-own-key
  providers stay in the game-assets pack for people who want them; the account no longer goes
  through packs.
