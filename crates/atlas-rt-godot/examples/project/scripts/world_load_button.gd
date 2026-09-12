class_name LoadWorldButton
extends Button

# The view reports "empty", "loading", "ready", or "failed". A job starts only
# when nothing is in flight.
const IDLE := ["empty", "ready"]

@export var atlas: Atlas

func _ready() -> void:
	if !atlas: return
	pressed.connect(load_world)

func _process(_delta: float) -> void:
	disabled = !_is_idle()

func load_world() -> void:
	if !_is_idle(): return
	atlas.load_world("res://worlds/" + text + ".vox")

func _is_idle() -> bool:
	return atlas.view.job_status_name() in IDLE
