class_name ClearWorldButton
extends Button

# The status the view reports: 0 empty and idle, 1 a job in flight,
# 2 a world resident, 3 failed.
const IDLE := [0, 2]

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
	return atlas.view.job_status() in IDLE
