class_name Atlas
extends Node2D

const TARGET_LINEAR := false

const COMPOSITE_SHADER := preload("res://atlas_composite.gdshader")
const OVERLAY := preload("res://scripts/loading_overlay.gd")

const IDLE := ["empty", "ready", "failed"]

var view: AtlasRtView
var overlay: OVERLAY
var ui: Control

func _ready() -> void:
	await get_tree().process_frame

	process_mode = Node.PROCESS_MODE_ALWAYS

	var node: AtlasRtView = AtlasRtView.new()
	node.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)

	var composite := ShaderMaterial.new()
	composite.shader = COMPOSITE_SHADER
	composite.set_shader_parameter("target_linear", TARGET_LINEAR)
	node.material = composite

	add_child(node)
	self.view = node

	var screen: OVERLAY = OVERLAY.new()
	add_child(screen)
	self.overlay = screen

	ui = get_node("/root/main/UI")

	var cam: Camera3D = get_node("/root/main/player/pivot/camera")
	node.set_camera(cam)
	node.load_world("res://worlds/castle.vox")

	set_mouse_captured(true)

func _input(event: InputEvent) -> void:
	if !(event is InputEventKey and event.is_pressed()):
		return

	if !Main.is_idle(): return

	if event.is_action("pause"):
		set_pause(!get_tree().paused)

func load_world(world_name: String) -> bool:
	if !view: return false
	if !view.load_world("res://worlds/" + world_name + ".vox"): return false

	set_pause(false)

	overlay.watch(world_name)

	return true

func clear_world() -> bool:
	if !view: return false
	if !view.clear_world(): return false

	overlay.watch()

	return true

func is_idle() -> bool:
	return view.job_status_name() in IDLE

func set_pause(active: bool) -> void:
	var t := get_tree()
	t.paused = active
	ui.visible = active
	set_mouse_captured(!active)

func set_mouse_captured(enable: bool) -> void:
	if enable:
		Input.set_mouse_mode(Input.MOUSE_MODE_CAPTURED)
	else:
		Input.set_mouse_mode(Input.MOUSE_MODE_VISIBLE)
