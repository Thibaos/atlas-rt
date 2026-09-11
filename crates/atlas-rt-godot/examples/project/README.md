# AtlasRtView example project wiring

1. Build the extension from the workspace root. Build the whole workspace, or
   just this crate with `-p atlas-rt-godot`:

        cargo build --release

   Then copy the library Godot loads (see "Locating the library" below).

2. Open the project with the custom template binary:

        <godot-cloned>\bin\godot.windows.template_release.x86_64.exe --path .

   The stock editor binary runs CPU delivery per the plan. Zero-copy is
   exercised through the export-run loop.

3. The init log line names the backend. When zero-copy misses the probe the
   log records the reason and delivery degrades per ADR 0005. There is no
   retry in v1.

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
