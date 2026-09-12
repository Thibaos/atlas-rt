class_name LoadWorldButton
extends Button

# The view reports "empty", "loading", "ready", or "failed". A job starts only
# when nothing is in flight, so a settled failure is idle too.
const IDLE := ["empty", "ready", "failed"]

var UI: Control

func _ready() -> void:
	UI = get_parent().get_parent()
	
	if !UI.atlas: return
	pressed.connect(load_world)
	

func _process(_delta: float) -> void:
	disabled = !_is_idle()

func load_world() -> void:
	if !_is_idle(): return
	if UI.atlas.load_world(text):
		UI.visible = false

func _is_idle() -> bool:
	return UI.atlas.view.job_status_name() in IDLE
