# {{name}}

A {{dimension}} game made with [Unity](https://unity.com) {{unity_version}}.

## Run

Open this folder in Unity Hub (Add > Add project from disk) and press Play.

## Layout

- `Assets/`: everything in the game: scenes, scripts (C#), prefabs, sprites, models.
- `Packages/manifest.json`: the Unity packages the project uses.
- `ProjectSettings/`: the project's settings. Change these in the editor rather than by hand.
- `Library/`, `Temp/`, `Logs/`: made by the editor; never commit or edit them.

## Notes for agents

- Scripts go in `Assets/Scripts/`, one `MonoBehaviour` per file, named as its class.
- Keep each asset's `.meta` file with it (move and rename them together); Unity links assets by the GUIDs in them.
- Sprites: Unity slices and packs them itself (Sprite Editor, Sprite Atlas).
