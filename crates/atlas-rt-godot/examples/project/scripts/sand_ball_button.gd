class_name SandBallButton
extends Button

@export var UI: Control

func _ready() -> void:
	pressed.connect(sand_ball)

func _process(_delta: float) -> void:
	disabled = !Main.is_idle()

func sand_ball() -> void:
	if !Main.is_idle(): return
	return Main.sand_ball()
