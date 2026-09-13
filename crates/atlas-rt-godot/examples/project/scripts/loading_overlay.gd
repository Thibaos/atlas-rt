class_name LoadingOverlay
extends Control

# Reports the load in flight: the world being loaded, how far it has got, and
# the reason if it failed. Main owns one so the world buttons have something to
# tell when a load starts, and the atlas passthroughs stay the only thing that
# calls the view.

var _body: Control
var _title: Label
var _bar: ProgressBar
var _failure: Label

var _active := false

func _ready() -> void:
	get_viewport().size_changed.connect(_match_screen)
	_match_screen()

	mouse_filter = Control.MOUSE_FILTER_IGNORE
	_build()

func _match_screen() -> void:
	size = get_viewport_rect().size

func _process(_delta: float) -> void:
	if !Main.view:
		dismiss()
		return

	var view: AtlasRtView = Main.view
	var status := view.job_status_name()
	var reason := view.job_error()

	_bar.value = view.job_progress() * 100.0
	_failure.text = reason
	_failure.visible = !reason.is_empty()

	if !_active:
		return

	if status == "loading":
		visible = true
		return

	_active = false
	visible = false

func watch(text: String = "") -> void:
	_title.text = "Loading " + text if text else "Returning to the menu"
	_bar.value = 0.0
	_failure.text = ""
	_failure.visible = false

	_active = true
	visible = true

func dismiss() -> void:
	_active = false
	visible = false

func _build() -> void:
	var backdrop := ColorRect.new()
	backdrop.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	backdrop.mouse_filter = Control.MOUSE_FILTER_IGNORE
	backdrop.color = Color(0.02, 0.02, 0.03, 0.92)
	add_child(backdrop)

	_body = VBoxContainer.new()
	_body.set_anchors_and_offsets_preset(Control.PRESET_CENTER)
	_body.grow_horizontal = Control.GROW_DIRECTION_BOTH
	_body.grow_vertical = Control.GROW_DIRECTION_BOTH
	_body.alignment = BoxContainer.ALIGNMENT_CENTER
	_body.add_theme_constant_override("separation", 12)
	add_child(_body)

	_title = Label.new()
	_title.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	_body.add_child(_title)

	_bar = ProgressBar.new()
	_bar.custom_minimum_size = Vector2(320, 16)
	_bar.show_percentage = false
	_body.add_child(_bar)

	_failure = Label.new()
	_failure.set_anchors_and_offsets_preset(Control.PRESET_BOTTOM_WIDE)
	_failure.offset_top = -40.0
	_failure.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	_failure.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	add_child(_failure)

	visible = false
	_failure.visible = false
