extends Node2D

# A first scene: a square you move with the arrow keys. Replace it with your game.

const SPEED := 220.0
var pos := Vector2(320, 180)


func _process(delta: float) -> void:
	var dir := Input.get_vector("ui_left", "ui_right", "ui_up", "ui_down")
	pos += dir * SPEED * delta
	queue_redraw()


func _draw() -> void:
	draw_rect(Rect2(pos - Vector2(16, 16), Vector2(32, 32)), Color(0.4, 0.8, 1.0))
