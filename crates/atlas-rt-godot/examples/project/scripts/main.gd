class_name Atlas
extends Node2D

# The host owns display encoding: AtlasRtView publishes linear radiance and this
# material turns it into displayable color. The extension drives the `mode`
# uniform from the view's current render mode.
#
# `target_linear` must match the project's 2D color space, and getting it wrong
# double-encodes or skips the sRGB encode outright. Leave it false for the
# default SDR canvas; set it true only alongside:
#   rendering/viewport/hdr_2d = true
const TARGET_LINEAR := false

const COMPOSITE_SHADER := preload("res://atlas_composite.gdshader")
const OVERLAY := preload("res://scripts/loading_overlay.gd")

var view: AtlasRtView
var overlay: OVERLAY

func _ready() -> void:
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

	node.set_camera($player/pivot/camera)
	node.load_world("res://worlds/castle.vox")

# A world change is one job: a clear before a load would be refused while the
# load is in flight. The overlay only appears once the view has taken the job,
# so a refusal leaves the screen as it was.
func load_world(name: String) -> bool:
	if !view: return false

	if !view.load_world("res://worlds/" + name + ".vox"): return false

	overlay.watch(name)
	return true

# A clear is one job too, and it leaves the view with no world.
func clear_world() -> bool:
	if !view: return false

	if !view.clear_world(): return false

	overlay.watch()
	return true
