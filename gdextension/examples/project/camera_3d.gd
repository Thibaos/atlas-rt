extends Camera3D

@export var movement_speed := 1000.0

var movement: Vector3 = Vector3.ZERO

func _input(event: InputEvent) -> void:
	var direction := Vector3.ZERO
	
	if event.is_action_pressed("forward"):
		direction += Vector3.FORWARD
	elif event.is_action_pressed("backward"):
		direction += Vector3.BACK
	elif event.is_action_pressed("left"):
		direction += Vector3.LEFT
	elif event.is_action_pressed("right"):
		direction += Vector3.RIGHT
		
	movement = direction.normalized()

func _process(delta: float) -> void:
	translate(delta * movement * (-global_basis.z) * movement_speed)
