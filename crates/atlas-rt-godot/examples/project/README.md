# AtlasRtView example project wiring

1. Build the extension from the workspace root. Build the whole workspace, or
   just this crate with `-p atlas-rt-godot`:

        cargo build --release

   Then copy the library Godot loads (see "Locating the library" below).

2. Open the project with the custom template binary:

        <godot-cloned>\bin\godot.windows.template_release.x86_64.exe --path .

3. The init log line names the backend. When zero-copy misses the probe the
   log records the reason and delivery degrades per ADR 0005. There is no
   retry in v1.

The engine build. The custom fork used for the runs behind the loading
work is:

    C:\Users\Thiba\Desktop\dev\godot-fork\godot\bin\godot.windows.editor.x86_64.exe

It reports `4.7.2.stable.custom_build` and carries the `VulkanHooksBridge`
module, which is what the zero-copy probe needs. The stock binary at
`C:\Users\Thiba\Desktop\Godot\stable\godot4.exe` does not have that module:
the probe logs `init probe failed: VulkanHooksBridge singleton missing` and
delivery is CPU. A failed probe is not fatal, since the probe picks the
backend and not whether the view coordinates at all, but a run on the stock
binary is not a run of the delivery path the fork exists for.

Driving the example without a hand on the keyboard. Add a small `Node` to
`main.tscn` and drive it from `_process`: find `atlas`, emit `pressed` on a
button in `UI/HBoxContainer`, and read `atlas.view.job_status_name()`,
`job_progress()`, and `job_error()`. A node in the scene sees the real
startup path, the real init probe, and the real `frame_post_draw`
connection, which a `SceneTree` script run with `--script` does not: it
opens its own tree, so the startup world is the one it loads itself. Emit
`pressed` rather than synthesizing input, and have the node quit the tree
when its sequence is done, since nothing else will end the run.

Locating the library. `atlas_rt.gdextension` names `res://lib/atlas_rt_godot.dll`,
so the build's output has to land in `lib/` before Godot sees it:

        copy target\debug\atlas_rt_godot.dll crates\atlas-rt-godot\examples\project\lib\

Name the profile you actually built, `target\debug` for `cargo build` and
`target\release` for `cargo build --release`. `lib/` is gitignored, so the copy
is per-checkout setup and the project does not carry the binary.

Scene layout. main.tscn hosts a Camera3D; main.gd adds an AtlasRtView
full-rect that reads the camera each tick and loads a .vox world.
fly_camera.gd is the Godot demo camera, kept close to upstream; its doc
block follows upstream's style, not this repo's.

World loading. The view reads a world file on a background thread so the
game keeps drawing while it is read, parsed, built, and queued. That
thread can make no Godot call, so the view resolves the path with
`ProjectSettings.globalize_path` on the main thread and the loader reads
it with `std::fs`. `res://` therefore resolves to a real file, which an
exported project does not have: worlds shipped inside a `.pck` are out of
reach until the load moves to a res:// source that reads on the main
thread.

Loading screen. `scripts/loading_overlay.gd` is a full-rect Control that
reads the view's status, progress, and error string each frame, and
`main.gd` adds one above the pause UI. The world buttons tell it a load is
starting, and the atlas passthroughs call the view and nothing else: the
old wrapper's clear-then-load pair is refused by the view's one-job rule.
A load suppresses the outgoing world at the call, so the overlay covers
the blank viewport until the frame that carries the new world is admitted,
which is also when the status settles to `ready` and the overlay goes
away. A failed load leaves the overlay up with the reason, and the world
that was resident stays gone.

Display path. The extension publishes raw linear radiance and does no
display encoding, so the host owns it: main.gd attaches
`atlas_composite.gdshader` as a ShaderMaterial on the view, which applies
ACES, gamma 2.2, and a dithered 8-bit store. That shader is the reference
host wiring to copy into a game, and the extension drives its `mode`
uniform from the render mode it is rendering, so debug modes stay
ungraded and exact.

One coupling to respect: the shader's `target_linear` uniform must match
the project's 2D color space. Leave it false under the default SDR canvas
(`rendering/viewport/hdr_2d` off); set it true only alongside
`rendering/viewport/hdr_2d = true`. Nothing on the engine side can detect
a mismatch, and getting it wrong double-encodes or skips the sRGB encode.
Enabling that project setting also converts every other 2D draw in the
viewport to linear, so scope it deliberately.
