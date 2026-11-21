extends Node3D

# A first scene: a lit, turning box. Replace it with your game.


func _process(delta: float) -> void:
	$Box.rotate_y(delta)
