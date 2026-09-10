extends Node2D

var view: AtlasRtView

func _ready() -> void:
	var node: AtlasRtView = AtlasRtView.new()
	node.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	add_child(node)
	self.view = node

	node.set_camera($player/pivot/camera)
	node.load_world("res://worlds/example.vox")
