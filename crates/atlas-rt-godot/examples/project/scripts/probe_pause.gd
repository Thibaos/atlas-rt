extends Node

var root: Node
var ui: Node
var frames := 0

func _ready() -> void:
	process_mode = Node.PROCESS_MODE_ALWAYS
	var scene: PackedScene = load("res://main.tscn")
	root = scene.instantiate()
	add_child(root)
	ui = root.get_node("UI")

func _process(_delta: float) -> void:
	frames += 1

	match frames:
		8:
			print("[probe] ui=", ui, " ui_mode=", ui.process_mode, " ui_visible=", ui.visible, " paused=", get_tree().paused)
			press_escape()
		12:
			print("[probe] after 1st esc: paused=", get_tree().paused, " ui_visible=", ui.visible)
			press_escape()
		16:
			print("[probe] after 2nd esc: paused=", get_tree().paused, " ui_visible=", ui.visible)
			get_tree().quit()

func press_escape() -> void:
	var event := InputEventKey.new()
	event.physical_keycode = KEY_ESCAPE
	event.keycode = KEY_ESCAPE
	event.pressed = true
	Input.parse_input_event(event)
	print("[probe] parsed escape")
