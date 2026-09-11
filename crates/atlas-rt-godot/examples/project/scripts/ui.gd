extends Control

func _ready() -> void:
	set_active(true)
	pass

func _input(event: InputEvent) -> void:
	if !(event is InputEventKey and event.is_pressed()):
		return
	
	if event.is_action("pause"):
		_toggle_pause()

func _toggle_pause() -> void:
	var t := get_tree()
	t.paused = !t.paused
	visible = t.paused
	set_active(!visible)

func set_active(enable:bool) -> void:
	if enable:
		Input.set_mouse_mode(Input.MOUSE_MODE_CAPTURED)
	else:
		Input.set_mouse_mode(Input.MOUSE_MODE_VISIBLE)
