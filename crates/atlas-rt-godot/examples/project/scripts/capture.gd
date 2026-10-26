extends Node

## Writes one render image of the running example to a PNG and quits, so an
## agent can look at a frame without opening a window. Reads its request after
## `--`:
##
##     godot --headless --path <project> -- --capture <png> [--world <name>]
##         [--frames <n>] [--size <width>x<height>] [--eye x,y,z --target x,y,z]
##
## `--world` names `castle`, `bistro`, `sponza` or `nuke` for the `.vox` files
## under `worlds/`, `sand-ball` for the fixed ball, or `generated` for a
## procedural World. Naming none leaves the startup World in place.
##
## `--eye` and `--target` aim the scene camera in the scene's own coordinates,
## which the fixed World types need: they build at the lattice's bottom, far
## below where a run's camera starts.
##
## A node in the scene rather than a second scene run by `--script`, because the
## autoload `Main` starts the example either way: this node waits on that view
## instead of standing up a second one. A run without `--capture` disables it,
## so a windowed run is untouched.

const HEADLESS := "headless"

## The frames waited after the World settles, so the frame the image is taken
## from is not the one that first carried the new World.
const DEFAULT_FRAMES := 30

const DEFAULT_SIZE := Vector2i(1280, 720)

## How long a capture waits for its World before giving up, so a broken load
## fails a run instead of hanging one. The largest worlds under `worlds/` read
## in seconds even in a debug build.
const DEADLINE_MS := 600_000

var out_path := ""
var world := ""
var frames := DEFAULT_FRAMES
var size := DEFAULT_SIZE
var eye := Vector3.ZERO
var look_at := Vector3.ZERO
var aimed := false

var enabled := false
var requested := false
var settled := 0
var warned := false
var started := 0


func _ready() -> void:
	var args := OS.get_cmdline_user_args()
	var eye_text := ""
	var target_text := ""

	var index := 0
	while index < args.size():
		var flag: String = args[index]
		match flag:
			"--capture":
				out_path = _value_after(args, index)
				index += 2
			"--world":
				world = _value_after(args, index)
				index += 2
			"--frames":
				frames = int(_value_after(args, index))
				index += 2
			"--size":
				size = _parse_size(_value_after(args, index))
				index += 2
			"--eye":
				eye_text = _value_after(args, index)
				index += 2
			"--target":
				target_text = _value_after(args, index)
				index += 2
			_:
				index += 1

	if out_path.is_empty():
		set_process(false)
		return

	if !eye_text.is_empty() or !target_text.is_empty():
		if eye_text.is_empty() or target_text.is_empty():
			push_error("atlas capture: --eye and --target place a camera together")
			get_tree().quit(1)

			return

		var parts := eye_text.split(",") + target_text.split(",")

		if parts.size() != 6:
			push_error("atlas capture: --eye and --target take X,Y,Z each")
			get_tree().quit(1)

			return

		eye = Vector3(float(parts[0]), float(parts[1]), float(parts[2]))
		look_at = Vector3(float(parts[3]), float(parts[4]), float(parts[5]))
		aimed = true

	enabled = true

	# A headless run draws no frame at all unless an output asks for one, since
	# `DisplayServerHeadless::can_any_window_draw` is false and RenderingServer
	# only draws when a window or an additional output can present. Registering
	# this node is what keeps `frame_post_draw`, and so the view's frame, coming.
	if DisplayServer.get_name() == HEADLESS:
		DisplayServer.register_additional_output(self)

	get_window().size = size

	started = Time.get_ticks_msec()

	print("atlas capture: %dx%d to %s" % [size.x, size.y, out_path])


func _exit_tree() -> void:
	if enabled and DisplayServer.get_name() == HEADLESS:
		DisplayServer.unregister_additional_output(self)


func _process(_delta: float) -> void:
	if !enabled:
		return

	var view = _view()

	if view == null:
		return

	if aimed:
		_aim()

	var status: String = view.job_status_name()

	if status == "failed":
		push_error("atlas capture: the World failed: %s" % view.job_error())
		_quit(1)
		return

	var waited: int = Time.get_ticks_msec() - started

	if waited > DEADLINE_MS:
		push_error("atlas capture: gave up after %d ms at status %s" % [waited, status])
		_quit(1)
		return

	# Ready is the status a load reaches only once a frame carrying it has been
	# admitted, so the frame this capture saves is the World that was asked for.
	if status != "ready":
		settled = 0
		return

	if !requested:
		requested = true

		if !_request_world():
			push_error("atlas capture: the view refused the World %s" % world)
			_quit(1)

		return

	settled += 1

	if settled < frames:
		return

	if !view.save_capture(out_path):
		push_error("atlas capture: the view refused to save %s" % out_path)
		_quit(1)

		return

	print("atlas capture: saved %s" % out_path)
	_quit(0)


## Puts the scene camera on the requested eye and target. Applied every frame,
## because the view reads the camera before this node runs.
func _aim() -> void:
	var camera = get_node_or_null("/root/main/player/pivot/camera")

	if camera == null:
		if !warned:
			warned = true
			push_error("atlas capture: no camera at /root/main/player/pivot/camera to aim")

		return

	camera.look_at_from_position(eye, look_at, Vector3.UP)


## The view the autoload `Main` builds, or null before it exists. Untyped so a
## project without the extension still parses this script.
func _view():
	var main = get_node_or_null("/root/Main")

	if main == null:
		return null

	return main.get("view")


## Asks for the World the flags name, through the same entry points the pause
## menu uses. True when nothing was asked or the request was accepted.
func _request_world() -> bool:
	if world.is_empty() or world == Atlas.STARTUP_WORLD:
		return true

	var main = get_node_or_null("/root/Main")

	if main == null:
		return false

	match world:
		"sand-ball":
			return main.sand_ball()
		"generated":
			return main.generate_world()
		_:
			return main.load_world(world)


func _quit(code: int) -> void:
	set_process(false)
	get_tree().quit(code)


func _value_after(args: PackedStringArray, index: int) -> String:
	if index + 1 >= args.size():
		push_error("atlas capture: %s needs a value" % args[index])
		return ""

	return args[index + 1]


func _parse_size(value: String) -> Vector2i:
	var parts := value.split("x")

	if parts.size() != 2:
		push_error("atlas capture: %s is not WIDTHxHEIGHT" % value)
		return DEFAULT_SIZE

	return Vector2i(maxi(1, int(parts[0])), maxi(1, int(parts[1])))
