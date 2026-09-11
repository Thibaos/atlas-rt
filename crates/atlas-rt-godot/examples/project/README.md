# AtlasRtView example project wiring

1. Build the extension from the workspace root. Build the whole workspace, or
   just this crate with `-p atlas-rt-godot`:

        cargo build --release

   No copy step: `lib/` is a junction to the workspace target directory
   (see "Locating the library" below), so the build's output is what Godot
   loads.

2. Open the project with the custom template binary:

        <godot-cloned>\bin\godot.windows.template_release.x86_64.exe --path .

   The stock editor binary runs cpu delivery per the plan; zero-copy is
   exercised through the export-run loop.

3. The init log line names the backend. When zero-copy misses the probe the
   log records the reason and delivery degrades per ADR 0005; no retry in v1.

Locating the library. `atlas_rt.gdextension` names `res://lib/atlas_rt_godot.dll`,
and `lib/` is a directory junction into the workspace's `target/` directory, so a
build updates the library the project loads and nothing is ever copied by hand.
Recreate the junction on a fresh checkout:

        cmd /c mklink /J crates\atlas-rt-godot\examples\project\lib target\debug

The junction points at one profile. It is set to `debug`, so `cargo build` (not
`--release`) is what updates it; point it at `target\release` instead if you work
in release. `lib/` is gitignored, so the junction is per-checkout setup and
Windows-only. Without it, copy
`target\<profile>\atlas_rt_godot.dll` into `lib/` after each build.

Scene layout. main.tscn hosts a Camera3D; main.gd adds an AtlasRtView
full-rect that reads the camera each tick and loads a .vox world.

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
