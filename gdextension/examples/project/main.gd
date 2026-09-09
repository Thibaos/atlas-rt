# Root main scene: boots the full-viewport renderer and drives the camera.
extends Node2D

var view: AtlasRtView
var probe_frames := 0

const PROBE_FRAME := 90

func _ready() -> void:
	var node: AtlasRtView = AtlasRtView.new()
	node.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	add_child(node)
	self.view = node

	node.set_camera($Camera3D)
	node.load_world("res://worlds/example.vox")

func _process(_delta: float) -> void:
	# Set ATLAS_RT_PROBE to an absolute PNG path to capture frame 90.
	probe_frames += 1
	var target := OS.get_environment("ATLAS_RT_PROBE")
	if target != "" and probe_frames == PROBE_FRAME:
		get_viewport().get_texture().get_image().save_png(target)
		print("probe: saved ", target)
