# Engine packs

The game engines the New Project wizard offers. Each is a pack (see
`docs/proposals/tool-packs.md`) with an `engine` section in its `pack.json`; Wundur builds these
four into the app (`crates/wundur-core/src/engines.rs`), and an installed pack with the same id
takes the place of one. Any other installed pack with an `engine` section joins the list.

```
phaser/
  pack.json       the manifest, with "engine": { … }
  card.jpg        the wizard card's picture (640 wide, JPEG)
  template/
    common/       written for every project
    2d/  3d/      written for that kind of game (wins over common/ where both have a file)
```

Text files in the template may say `{{name}}`, `{{slug}}` (a package name), `{{dimension}}`
(`2D`/`3D`) and `{{unity_version}}`.

The `engine` section:

| Field | |
|---|---|
| `name`, `tagline`, `description` | What the card says |
| `image` | The card's picture, in the pack |
| `order` | Where it sits (lowest first) |
| `dimensions` | `["2d"]`, `["3d"]` or both; the wizard asks only when there is a choice |
| `requires` | What it needs on the Mac: `programs` (on the PATH), `paths` (`~` and `*` allowed) or a built-in `probe` (`unity-editor`); `url` and `help` say where to get it; `blocking` greys the engine out when missing |
| `run` | How to run the new project |
| `create` | A maker built into Wundur that runs first (`unity`: the editor makes the project) |
| `sprite_tools` | Sprite sheet tools fit its 2D projects |

A new project records what it was made as in `.wundur/project.json` (kind, engine, dimension,
`sprite_tools`), which git keeps.

Built-in engine files are listed one by one in `engines.rs` (`pack_files!`): add a template file
there too.
