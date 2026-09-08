# Full-viewport atlas-rt view: the view node draws under a CanvasLayer UI.
# UI keeps layer >= 1 above the view per the gameplay surface contract.
extends Node

func _ready() -> void:
	var view: AtlasRtView = AtlasRtView.new()
	view.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	add_child(view)

	view.set_camera($Camera3D)
	view.load_world("res://worlds/example.vox")

	view.submit_microchunk(
		Vector3i(0, 0, 0),
		PackedByteArray([0x01] + [0] * 63),
		PackedByteArray([1]),
	)
