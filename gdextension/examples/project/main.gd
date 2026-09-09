# Root main scene: boots the full-viewport renderer and drives the camera.
extends Node2D

var view: AtlasRtView
var probe_frames := 0

const PROBE_FRAME := 90
const PROBE_FRAME_2 := 240

func _ready() -> void:
	var node: AtlasRtView = AtlasRtView.new()
	node.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	add_child(node)
	self.view = node
	
	node.set_camera($player/pivot/camera)
	node.load_world("res://worlds/example.vox")
