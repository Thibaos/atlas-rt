class_name ClearWorldButton
extends Button

# The view reports "empty", "loading", "ready", or "failed". A job starts only
# when nothing is in flight.
const IDLE := ["empty", "ready"]

@export var atlas: Atlas

func _ready() -> void:
	if !atlas: return
	pressed.connect(clear_world)

func _process(_delta: float) -> void:
	disabled = !_idle()

func clear_world() -> void:
	if !_idle(): return
	atlas.clear_world()

func _idle() -> bool:
	return atlas.view.job_status_name() in IDLE
