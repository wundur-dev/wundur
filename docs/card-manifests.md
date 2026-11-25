# Card manifests

A workflow's card in Wundur (new image, new sound effect, new 3D asset, and whatever comes
next) can be described by the Wundur API and drawn by the app. The API says what the card holds,
how it is laid out and what each button does; the app draws it from components it already has.
A new workflow built from those components reaches everyone without a new download.

This is the reference for whoever writes manifests, which is mostly the API. It is written to
be copied into the API repository's `AGENTS.md`.

## Where this stands

Built: the manifest format, the renderer (`crates/wundur-cards`), the card's data and actions
(`crates/wundur-core/src/cards`), the preview program, and three reference manifests in
`cards/` that reproduce today's cards.

Built in the app too: it asks the API which workflows the account may use
(`GET /v1/workflows`), lists them in the Create menu, draws their cards in the chat, performs
their calls and saves, and keeps each card with its chat. The cards built into the app (3D
asset, image, sound effect) stay until the API offers a workflow with the same id (`asset-3d`,
`image`, `sound-effect`), whose manifest then takes that card's place. Signed out, the menu
offers only signing in.

Not built yet: telling the person when a long wait ends while they are looking elsewhere,
lists a person edits (add and remove items), and a progress bar.

## The split

| The API decides | The app decides |
| --- | --- |
| Which workflows exist | How each component looks and behaves |
| A card's layout: which components, where, how big | The theme: colours, fonts, corner and shadow of the card |
| Its text, costs and options | How a call is sent (sign-in, retries, idempotency keys) |
| What each button does, from the actions below | Where files are saved and how they are named apart |
| The data the card shows | When spending is allowed: only from a person's click |

A manifest has no arithmetic, no loops beyond repeating a node over a list, and no code. It
lays data out and tests it. **Anything worked out is worked out by the API and sent as data**:
"3 credits", "Views v2 approved", which set is approved, whether something is still being made.

## A manifest

```json
{
  "schema": 1,
  "id": "sound-effect",
  "version": "1",
  "label": "Sound effect…",
  "icon": "audio-lines",
  "form": { "prompt": "", "seconds": 2, "loop": false },
  "ui": { "step": "edit" },
  "refresh": { "call": { "path": "/v1/things/{data.id}" }, "while": "data.busy", "every": 2.5 },
  "actions": { "make": [ … ] },
  "parts": { "header": { … } },
  "card": { "type": "column", "children": [ … ] },
  "row": { … }
}
```

| Key | What it is |
| --- | --- |
| `schema` | The format's number, `1`. The app doesn't offer a manifest with a later one. |
| `id` | The workflow's id. The file is named for it. |
| `version` | Changes whenever the manifest does. Any text. |
| `label`, `icon` | The Create menu's entry. The icon is a [Lucide](https://lucide.dev/icons) name. |
| `form`, `ui` | What those two scopes start as (see Data). |
| `refresh` | A call repeated while a condition holds (see Refresh). |
| `actions` | Named actions (see Actions). |
| `parts` | Named pieces of layout, drawn with a `use` node. |
| `card` | The layout. |
| `row` | Optional: the layout when the card is folded to one line (`ui.folded` is true). |

The app draws the card's own frame (rounded border, ground, shadow) around `card`.

### Versions

A card in a chat lasts for days. It keeps the data it has, and is drawn with the newest layout
of its workflow. So a layout change reaches cards already open, and must keep reading the data
older cards hold: add to the data a card reads, don't rename or remove.

### What the app can't draw

A manifest that names a component the app doesn't have isn't offered at all (an older app
never shows a broken card). A property the app doesn't know is ignored. So new properties are
safe to send to old apps and new components are not: a workflow that needs a new component
waits for the app release that has it.

## Data

A card reads six scopes:

| Scope | What it holds | Who writes it |
| --- | --- | --- |
| `form` | What the person has filled in | Inputs, and `set` |
| `ui` | How the card is shown: the step, what's open | `set`, `toggle`, a call's `into` |
| `data` | The API's answer for this card | Each call's answer replaces it |
| `catalog` | `GET /v1/catalog` | The app |
| `account` | `GET /v1/me`, plus `signed_in` | The app |
| `card` | `busy`, `working`, `action`, `error`, `error_code`, `width` | The app |

`card.busy` is true while a call or save is waited for; `card.working` is that call's
`working` text and `card.action` the action's name. `card.error` and `card.error_code` hold the
last failure (the API's error `message` and `code`, or a `check`'s message with the code
`check`) until the next action starts. `card.width` is the card's width in points.

`form`, `ui` and `data` are kept with the card and come back when the chat is reopened.

### Paths

`data.sets.0.label` reaches into objects and lists. In a list, a name finds the item whose `id`
it is, and `first` and `last` are its ends. Square brackets look a step up:

```
catalog.kinds[form.kind].label        the kind whose id is form.kind
data.sets[ui.set] || data.sets.last   the set picked, else the newest
form.fields[field.id]                 a place named by a loop's item
```

Anything missing reads as nothing. It is never an error, so a card drawn before its data
arrives is merely empty.

### Conditions

`when`, `selected`, `disabled`, `loading`, a `check` and `refresh.while` are conditions:

```
ui.step == 'views' && !card.busy
card.width < 520
(form.fields.style || look.default) == option
```

`==` `!=` `<` `<=` `>` `>=`, `&&` `||` `!`, brackets, text in quotes, numbers, `true`, `false`,
`null`. Nothing, `false`, `0`, empty text and an empty list or object don't hold. `a || b` is
`a` when it holds, else `b`, so it also picks a fallback.

### Text

Any other text property is a template: `"Views v{set.number}"`. A hole holds a path or a
condition, then filters:

| Filter | Does |
| --- | --- |
| `{name\|or:Untitled}` | The text after `or:` when the value is empty |
| `{option\|capital}` | First letter capitalised |
| `{x\|upper}`, `{x\|lower}` | Case |
| `{data.sets\|count}` | How many |
| `{form.prompt\|slug}` | A file name: `a-gold-coin-top-down` |

`{{` and `}}` are the braces themselves. Where a value is wanted (a call's body, a `set`), a
text that is one hole and nothing else gives the value itself with its type: `"{form.loop}"`
is `true`, not `"true"`.

There is no plural and no sum. Send `"cost_label": "3 credits"`, or word around it: "Images
left this month: 4".

## Layout

A node is an object with a `type`. Sizes are in points.

### Every node

| Property | What it does |
| --- | --- |
| `id` | Its name: tests and automation find it by this. Inside a loop it gets `:0`, `:1`… |
| `when` | Drawn only while this holds |
| `each`, `as` | Drawn once for each item of a list; the item is named by `as` (else `item`), with `<as>_index` (from 0) and `<as>_number` (from 1) beside it |
| `let` | Names for this node and what's in it: `{ "set": "data.sets[ui.set] \|\| data.sets.last" }` |
| `responsive` | Other properties at narrow widths (below) |
| `width`, `height`, `min_width`, `max_width`, `min_height`, `max_height` | Points, `"fill"`, or a share: `"50%"` |
| `grow`, `shrink`, `basis` | Its share of spare room in its row or column |
| `align_self` | `start`, `center`, `end`, `stretch` |
| `opacity` | 0 to 1 |
| `tooltip` | Text shown on hover |

`each` comes first, then `let`, then `when`: a `when` can test the loop's item and so filter.

### Boxes: `row`, `column`, `grid`

They hold `children`. `grid` adds `columns` (a number).

| Property | Values |
| --- | --- |
| `gap` | Points, or `[across, down]` |
| `padding` | Points, `[vertical, horizontal]`, or `[top, right, bottom, left]` |
| `align` | Across the box's direction: `start`, `center`, `end`, `baseline` |
| `justify` | Along it: `start`, `center`, `end`, `between`, `around` |
| `wrap` | Children wrap onto more lines |
| `background` | A colour |
| `border`, `border_top`, `border_bottom`, `border_left`, `border_right` | `true`, or a colour |
| `radius`, `clip`, `shadow`, `aspect` | Corner radius; cut children at the edge; a soft shadow; width over height |
| `divide` | A line between children |
| `click` | An action. The box shows a hover and a pointer. |
| `selected` | A condition: accent border colour and tint while it holds |
| `disabled` | A condition: faded, and its click does nothing |
| `overlay` | A node laid over the box while the overlay's own `when` holds. The box is faded and takes no clicks. |

### Colours

`text`, `muted`, `accent`, `danger`, `success`, `warning`, `border`, `surface`, `background`,
`skeleton`, `white`, `transparent`. Each can be thinned: `accent/12` is the accent at 12%.
They follow the theme, light and dark. There are no other colours.

### Responsive

Two ways, and the first covers most cases:

1. **Wrapping.** A `row` with `wrap`, whose children have `grow`, `basis` and `min_width`,
   stacks by itself when the card is narrow.
2. **Rules.** `responsive` is a list of `{ "under": 520, …properties }`. While the card is
   narrower than `under`, those properties replace the node's own; later rules win. And any
   condition can test `card.width`.

```json
{ "type": "row", "gap": 12, "responsive": [{ "under": 520, "wrap": true, "justify": "end" }], "children": [ … ] }
{ "type": "text", "text": "{engine.uses}", "when": "card.width == 0 || card.width >= 520" }
```

`card.width` is `0` until the card has been drawn once: treat `0` as wide.

### Components

| `type` | Properties | Notes |
| --- | --- | --- |
| `text` | `text`, `size` (`xs` `sm` `base` `lg` `xl`, or points), `weight` (`normal` `medium` `semibold` `bold`), `color`, `truncate`, `align` (`center` `end`), `underline` | `sm` and the text colour unless said. Wraps unless `truncate`. |
| `icon` | `name` (Lucide), `size` (`xs` `sm` `md` `lg` `xl`, or points), `color` | |
| `image` | `src`, `fit` (`contain` `cover` `fill`), `radius`, `border`, `background`, `aspect`, `click` | `src` is an `https` link, or a file in the project. Give it a size. |
| `spinner` | `size` (`sm`, or points), `color` | |
| `placeholder` | `radius`, `aspect` | A still block where a picture will be |
| `divider` | `vertical`, `color` | |
| `spacer` | | Takes the spare room |
| `button` | `label`, `icon`, `icon_right`, `style` (`primary` `outline` `ghost` `danger`), `size` (`xs` `sm` `md` `lg`), `disabled`, `loading`, `click` | `outline` and `sm` unless said. No `label`: the icon alone. |
| `link` | `text`, `icon`, `size`, `disabled`, `click` | |
| `input` | `bind`, `placeholder`, `number`, `size` (`sm`), `disabled` | With `number` it writes a number, or nothing while it isn't one |
| `textarea` | `bind`, `placeholder`, `rows` (`[least, most]`), `disabled` | Grows with its text |
| `segmented` | `bind`, `options`, `default`, `capitalize`, `style` (`chips`), `disabled` | One of several. `options` is a list, or a path to one; each is a value or `{ "id", "label" }`. |
| `toggle` | `bind`, `label`, `icon`, `size`, `on`, `off`, `disabled` | Writes `true`/`false`, or the `on` and `off` values given |
| `files` | `bind`, `max`, `label`, `accept` (extensions) | Files from the person's disk; writes their paths. Send them with a call's `files`. |
| `steps` | `bind`, `items` (`[{ "id", "label" }]`), `reached`, `default` | The card's steps as tabs. `reached` is how many are done (a number) or the step got to (an id); those are ticked and a click away. |
| `use` | `part` | Draws a part from `parts` |

`bind` names a place in `form` or `ui`. `default` (on `segmented` and `steps`) is what shows as
picked while the place is empty: the API's own default, so the form doesn't have to hold it.

## Actions

A `click`, and each entry of `actions`, is an action: the name of one in `actions`, one step, or
a list of steps done in order. A step that fails stops the rest.

| Step | Does |
| --- | --- |
| `{ "set": { "ui.step": "views", "form.kind": "{kind.id}" } }` | Writes values in `form` or `ui` |
| `{ "toggle": "ui.brief_open" }` | Flips one |
| `{ "check": "form.prompt", "message": "Say what the image shows first." }` | Goes on only if the condition holds; else the message is the card's error |
| `{ "call": { … } }` | Asks the API (below) |
| `{ "save": { "files": …, "folder": "assets/generated", "into": "ui.saved" } }` | Saves files into the project (below) |
| `{ "open": "{ui.saved.0}" }` | Opens a file or link: a picture in the viewer, a model in the model viewer |
| `{ "play": "{ui.saved.0}" }` | Plays a sound |
| `{ "reset": "form" }` | Puts `form` or `ui` back as the manifest starts it |
| `{ "close": true }` | Takes the card out of the chat |
| `{ "app": "sign_in" }` | Something the app does: `sign_in`, `upgrade`, `request_access` |
| `{ "run": "make" }` | Another action |

An action clicked inside a loop or a `let` can use those names: an `approve` action that posts
to `…/sets/{set.id}/approve` works from any button drawn where `set` is known.

### Calls

```json
{ "call": {
    "method": "POST",
    "path": "/v1/assets/{data.id}/sets?view=card",
    "body": { "from": "{set.id}", "instruction": "{form.note}" },
    "files": { "references": "{form.references}" },
    "spends": true,
    "working": "Making the revision",
    "into": "data"
} }
```

| Key | What it is |
| --- | --- |
| `path` | Under the API's base. Only the Wundur API can be called. |
| `method` | `GET`, `POST`, `PATCH`, `DELETE`. `POST` with a `body`, else `GET`, unless said. |
| `body` | JSON with holes. Entries that come out as nothing are left out. |
| `files` | Files from disk: a name in the body, and paths (a `files` component's value). The app reads each and puts them in the body under that name as a list of base64 texts. |
| `spends` | It uses images, seconds or credits. The app sends an `Idempotency-Key`, and only ever runs it from a click. |
| `working` | What the card can say while it waits (`card.working`) |
| `into` | Where the answer goes: `data` unless said |
| `name` | What it's known by; the action's name unless said |

The app sends it signed in as the person. While it's waited for, `card.busy` is true and other
calls are ignored. A refusal (`{ "error": { "code", "message" } }`) becomes `card.error` and
`card.error_code`, and the action's remaining steps don't run.

### Saves

```json
{ "save": { "files": [{ "url": "{data.url}", "name": "{form.prompt|slug}.png" }], "folder": "assets/generated", "into": "ui.saved" } }
```

`files` is a list of links, or of `{ "url", "name" }`. The app downloads each into `folder`
under the project, under a name not already there, and writes the saved paths (a list, in
order) to `into`. Only `https` links are downloaded.

### Refresh

```json
"refresh": { "call": { "path": "/v1/assets/{data.id}?view=card" }, "while": "data.busy", "every": 2.5 }
```

While `while` holds, the app repeats the call every `every` seconds and puts the answer in
`data`. It doesn't make the card busy, and a failure is tried again silently. It never spends.

The app also makes this call on its own, whatever `while` says, to catch a card up when its
data may be behind: three times over about eight times `every` after each of an action's
calls comes back (done or refused), and when a card is read back from its record. So a card
whose answer said "nothing is running" a moment before the work started still finds out. This
is a safety net, not the contract: an answer to a call that starts work should already say
`busy` and show the work as started.

## What the API must provide

1. **One shape per card.** Every call a card makes answers with the same resource: the card's
   whole `data`. A card can't merge a partial answer. If `POST …/models` answers with just the
   model, the card has nothing to draw from.
2. **Worked-out values in the data.** Labels with their counts and plurals, which item is
   current, what is allowed now (`can_build`), what is being made (`busy`), how far it has got.
3. **One link per picture.** A card is drawn many times a minute, and the app fetches each
   link once and keeps the file. So a picture keeps the same link for as long as it is that
   picture (or it is downloaded again with every answer), and gets a different link when it
   changes (or the card goes on showing the old one). A stable address that redirects to a
   signed one is fine; put the picture's own id in it (`…/assembled.png?v=img_d4df`), so a
   redraw changes the link.
4. **Empty means absent.** A blank name is sent as `""`. Treat empty text as not given.
5. **Defaults of its own.** A form that leaves a field alone sends nothing for it.
6. **Errors a person can read.** `message` is shown as it is. `code` lets the manifest offer the
   way on (`quota_exceeded` → Upgrade, `plan_required` → Request access).
7. **The manifests themselves.** A list of workflows (`id`, `version`, `label`, `icon`) for the
   Create menu, and each manifest by id. Suggested: `GET /v1/workflows` and
   `GET /v1/workflows/{id}`. Both must say when they change (the `version`), so the app can
   keep a copy and ask rarely.

## The three reference manifests

They are in `cards/`, each with its scenario.

### Image (`cards/image.json`) and sound effect (`cards/sound-effect.json`)

These need nothing new from the API. They call the endpoints the app calls today:

| | Call | Reads from the answer |
| --- | --- | --- |
| Image | `POST /v1/images/generations` with `prompt`, `size`, `quality`, `background` | `url`, `images_left` |
| Sound | `POST /v1/audio/sound-effects` with `text`, `duration_seconds`, `loop` | `url`, `duration_seconds`, `sound_seconds_left` |

One action each: check the prompt, call, save the file, show the result. `account.signed_in`
picks between the form and "Sign in".

Two differences from today's cards: the result line says "Images left this month: 4" where it
said "4 images left this month" (no plurals), and the file is named from the prompt by the
`slug` filter.

### 3D asset (`cards/asset-3d.json`)

This one needs the API to add a card view of an asset. The manifest asks for it with
`?view=card` on the existing asset endpoints, and reads the fields below. Everything in the
scenario file (`cards/asset-3d.scenario.json`, under `states`) is an example of this shape.

```json
{
  "id": "ast_7c1",
  "slug": "goblin-scout",
  "name": "Goblin scout",
  "subtitle": "Humanoid · Stylized",
  "description": "A wiry goblin scout…",
  "settings_line": "Height: child · Build: slim",
  "references": [{ "url": "…" }],
  "thumb": "…",

  "stage": "views",
  "steps_done": 1,
  "busy": false,
  "status_line": "Views ready to review",
  "review": "Review",
  "views_status": "Next: choose model settings",

  "sets": [{
    "id": "set_1", "number": 1, "status": "ready", "approved": false,
    "state_label": "Ready to review", "caption": "", "working": "",
    "thumb": "…",
    "views": [{ "label": "Front", "url": "…" }],
    "error": { "message": "…" }
  }],
  "approved_set": "set_2",

  "model": {
    "engine": "wundur", "detail": "standard",
    "engines": [{ "id": "wundur", "label": "Wundur (preview)", "uses": "Several views", "cost_label": "1 credit", "locked": false, "finishes": ["smooth", "faceted"], "help": "…", "license": "…" }],
    "details": [{ "id": "standard", "label": "30k", "help": "Up to 30,000 triangles · A good default for most games." }],
    "attempt": { "id": "mdl_1", "number": 1, "status": "succeeded", "imported": false, "line": "Wundur (preview) · 30k · Smooth", "preview_url": "…", "import_label": "Import · 1 credit", "error": { "message": "…" } },
    "building": false, "stage_label": "Painting the back · 40%", "note": "Running, usually about a minute…",
    "history": "", "can_build": true, "status": "Free to build · Import: 1 credit", "status_tip": "…", "locked_note": "…"
  },

  "delivered": [{ "url": "…", "name": "goblin-scout.glb" }]
}
```

| Field | Meaning |
| --- | --- |
| `stage` | Where the asset has got: `views`, `model`, `imported`. The step shown unless the person picks another. |
| `steps_done` | How many of Brief, Views, 3D model are done: 1 once created, 2 once views are approved, 3 once imported |
| `busy` | Something is being made. The card refreshes while it is true. |
| `status_line`, `review` | The folded card's line, and its button's label when something waits on the person |
| `sets[].status` | `generating`, `ready`, `failed`. A set being made still lists its `views` with their labels, without links. |
| `sets[].state_label`, `caption` | "Ready to review"; " · Approved" for the versions strip |
| `approved_set` | The id of the approved set, once there is one |
| `model.engines` | The engines for this asset's kind and this account, with their labels worked out |
| `model.engine`, `model.detail` | The defaults, shown as picked until the person picks |
| `model.attempt` | The latest attempt from the approved set, or nothing |
| `model.building` | A model is queued or running: the card is covered and says `stage_label` and `note` |
| `delivered` | After import: the files to save |

The calls, each answering with this same shape:

| Action | Call |
| --- | --- |
| `create` | `POST /v1/assets?view=card` with `kind`, `name`, `description`, `fields`, and `references` as base64 |
| refresh | `GET /v1/assets/{id}?view=card` |
| `change` | `POST /v1/assets/{id}/sets?view=card` with `from`, `instruction` |
| `retry` | The same with `from`, `retry: true` |
| `approve` | `POST /v1/assets/{id}/sets/{set}/approve?view=card` |
| `build` | `POST /v1/assets/{id}/models?view=card` with `engine`, `detail`, `finish` |
| `discard` | `POST /v1/assets/{id}/models/{model}/reject?view=card` |
| `import` | `POST /v1/assets/{id}/models/{model}/import?view=card`; then the card saves `delivered` |

The form reads the catalog as it is today (`kinds[].fields` with `type`, `options`, `default`,
`help`, `option_help`, `option_images`), with one change: `option_images` values must be full
links.

What the manifest card leaves out that today's card has: the asset's own tab (Open), the name
suggested from the description as you type, pasting and dropping reference pictures (the Add
button works), the one-line summary beside "More options", and Cancel job. Each needs
something from the app; none is a format problem.

## Building a new workflow

1. Write the manifest and a scenario beside it (below). Start from the reference manifest
   closest in shape: `image.json` for ask-and-get, `asset-3d.json` for steps with review.
2. Decide the card's `data` shape first: what the API answers with, including every label.
   Write it as scenario states before writing layout.
3. Run the preview on it and look at every state at every width.
4. Serve the manifest and the endpoints it calls.

If the workflow needs something the components can't do (a grid picker, a sprite on an
isometric grid, a set of directions approved together), that is a new component: an app change
and a release. Say so early, so several can go in one release.

## The preview

In the Wundur repository:

```sh
cargo run -p wundur-cards --bin wundur-card-preview -- cards/sound-effect.json
```

It draws the manifest with the app's own renderer and theme, and again each time the file is
saved. The bar at the top picks a state, a width (380, 520, 680, 860 or the window's) and light
or dark. Clicking works: each call, save, open and play is listed at the bottom with what would
be sent, newest first, and answered by the scenario. Nothing is sent anywhere and nothing is
spent. Problems in the manifest are listed under the bar, each with the node it's in.

Options: `--scenario <file>`, `--state <name>`, `--width <points>`, `--dark`, and
`--do <action>` to run an action once the card is up.

### Scenarios

`<id>.scenario.json` beside the manifest:

```json
{
  "catalog": { … },
  "account": { "signed_in": true },
  "states": [
    { "name": "Empty" },
    { "name": "Making", "form": { "prompt": "a door" }, "card": { "busy": true, "working": "Making it" } },
    { "name": "Done", "ui": { "step": "done" }, "data": { "url": "…" } }
  ],
  "replies": {
    "make": { "after": 2, "data": { "url": "…" } },
    "make:save": { "data": ["samples/coin.png"] },
    "refresh": [
      { "if": "data.model.building", "after": 3, "state": "Model ready" },
      { "after": 2.5, "state": "Views ready" }
    ]
  }
}
```

A state lays its `form`, `ui`, `card`, `account` and `catalog` over a new card's, and replaces
its `data`. A reply answers a call by its name after `after` seconds with `data` (the answer),
`merge` (the card's `data` with this over it), `state` (a named state's `data`) or `error` (a
message, or `{ "message", "code" }`). A list of replies answers successive calls in order; or,
when its replies have an `if`, the first whose condition holds answers. `<action>:save` answers
a save with the paths it "saved"; name pictures beside the scenario so the card has something
to show. Paths in a scenario are looked for from its own folder.
