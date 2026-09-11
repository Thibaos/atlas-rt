class_name LoadWorldButton
extends Button

@export var atlas: Atlas

func _ready() -> void:
	if !atlas: return
	pressed.connect(load_world)

func load_world() -> void:
	atlas.load_world("res://worlds/" + text + ".vox")
