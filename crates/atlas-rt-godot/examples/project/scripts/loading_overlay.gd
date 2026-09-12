class_name LoadingOverlay
extends Control

# Reports the load in flight: the world being loaded, how far it has got, and
# the reason if it failed. Main owns one so the world buttons have something to
# tell when a load starts, and the atlas passthroughs stay the only thing that
# calls the view.

var _title: Label
var _bar: ProgressBar
var _error: Label

# Whether a job this overlay was told about is still in flight. Without it the
# overlay would report the startup world's load, which no button started.
var _active := false

func _ready() -> void:
	# Main is a Node2D, so this Control has no Control parent to anchor a preset
	# against: the screen rect is taken directly, and again on every resize.
	get_viewport().size_changed.connect(_match_screen)
	_match_screen()

	mouse_filter = Control.MOUSE_FILTER_IGNORE
	_build()

func _match_screen() -> void:
	size = get_viewport_rect().size

func _process(_delta: float) -> void:
	var atlas := get_parent()

	if !atlas or !atlas.view:
		dismiss()
		return

	var view: AtlasRtView = atlas.view
	var status := view.job_status_name()

	_bar.value = view.job_progress() * 100.0
	_error.text = view.job_error()

	if !_active:
		return

	if status == "loading":
		visible = true
		return

	# Settled. The overlay goes away, but a failure keeps its reason on screen
	# until the next job takes it away.
	_active = status == "failed"
	visible = _active

# Tells the overlay a job is on its way: the world's name, or none for a clear.
func watch(name: String = "") -> void:
	_title.text = "Loading " + name if name else "Returning to the menu"
	_bar.value = 0.0
	_error.text = ""

	_active = true
	visible = true

# A refused request started nothing, so the overlay goes away again. Any job
# that is already in flight keeps reporting itself through `_process`.
func dismiss() -> void:
	_active = false
	visible = false

func _build() -> void:
	var backdrop := ColorRect.new()
	backdrop.size = size
	backdrop.mouse_filter = Control.MOUSE_FILTER_IGNORE
	backdrop.color = Color(0.02, 0.02, 0.03, 0.92)
	add_child(backdrop)

	var stack := VBoxContainer.new()
	stack.set_anchors_and_offsets_preset(Control.PRESET_CENTER)
	stack.grow_horizontal = Control.GROW_DIRECTION_BOTH
	stack.grow_vertical = Control.GROW_DIRECTION_BOTH
	stack.alignment = BoxContainer.ALIGNMENT_CENTER
	stack.add_theme_constant_override("separation", 12)
	add_child(stack)

	_title = Label.new()
	_title.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	stack.add_child(_title)

	_bar = ProgressBar.new()
	_bar.custom_minimum_size = Vector2(320, 16)
	_bar.show_percentage = false
	stack.add_child(_bar)

	_error = Label.new()
	_error.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	_error.custom_minimum_size = Vector2(320, 0)
	_error.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	stack.add_child(_error)

	visible = false
