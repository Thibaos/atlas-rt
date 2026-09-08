# Root main scene: boots the full-viewport renderer and drives the camera.
extends Node2D

func _ready() -> void:
	var view: AtlasRtView = AtlasRtView.new()
	view.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	add_child(view)

	view.set_camera($Camera3D)
	view.load_world("res://worlds/example.vox")
