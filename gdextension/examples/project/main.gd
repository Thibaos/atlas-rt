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

	node.set_camera($Camera3D)
	node.load_world("res://worlds/example.vox")

func _process(delta: float) -> void:
	# Set ATLAS_RT_PROBE to a PNG prefix path to capture two camera angles.
	var target := OS.get_environment("ATLAS_RT_PROBE")
	if target == "":
		return

	$Camera3D.rotate_y(delta * 0.15)
	probe_frames += 1

	if probe_frames == PROBE_FRAME:
		get_viewport().get_texture().get_image().save_png(target + "-1.png")
		print("probe: saved ", target + "-1.png")

	if probe_frames == PROBE_FRAME_2:
		get_viewport().get_texture().get_image().save_png(target + "-2.png")
		print("probe: saved ", target + "-2.png")
