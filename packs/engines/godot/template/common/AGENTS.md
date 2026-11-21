# {{name}}

A {{dimension}} game made with [Godot](https://godotengine.org) 4.

## Run

Open `project.godot` in Godot and press Play (F5). From a terminal, with `godot` on the PATH:
`godot --path .` runs the game and `godot --path . --editor` opens the editor.

## Layout

- `project.godot`: the project's settings; `run/main_scene` is the scene that starts.
- `main.tscn` and `main.gd`: the first scene and its script.
- `.godot/`: Godot's import cache; never commit or edit it.

## Notes for agents

- Godot 4 and GDScript 2: typed variables (`var speed: float`), `@export`, `@onready`, signals with `signal_name.connect(callable)`. Godot 3 examples often don't work.
- Scenes (`.tscn`) and resources (`.tres`) are text; edit them carefully and keep their `[ext_resource]` ids consistent.
- Check scripts with `godot --headless --path . --check-only --script <file>` when Godot is on the PATH.
