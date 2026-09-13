class_name LoadWorldButton
extends Button

@export var UI: Control

func _ready() -> void:
	pressed.connect(load_world)

func _process(_delta: float) -> void:
	disabled = !Main.is_idle()

func load_world() -> void:
	if !Main.is_idle(): return
	return Main.load_world(text)
