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

var view: AtlasRtView

func _ready() -> void:
	var node: AtlasRtView = AtlasRtView.new()
	node.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)

	var composite := ShaderMaterial.new()
	composite.shader = COMPOSITE_SHADER
	composite.set_shader_parameter("target_linear", TARGET_LINEAR)
	node.material = composite

	add_child(node)
	self.view = node

	node.set_camera($player/pivot/camera)
	node.load_world("res://worlds/example.vox")
