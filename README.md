# atlas-rt

A real-time voxel renderer using hardware ray tracing (Vulkan RT pipelines, vulkano). Renders sparse voxel worlds loaded from `.vox` files.

See [CONTEXT.md](CONTEXT.md) for the architecture and terminology.

## Layout

Two crates share one workspace, one lockfile, one `target/`, and one clippy
lint table declared in the root `Cargo.toml`:

- `crates/atlas-rt` — the renderer library and the standalone `atlas-rt` binary,
  with `shaders/` and `assets/` beside it.
- `crates/atlas-rt-godot` — the GDExtension, with the Godot example project
  under `examples/project`.

Both build together:

    cargo build --workspace

Building the whole workspace also builds the GDExtension, which pulls in
`godot` and its bindings-generation build script. For renderer-only work,
`cargo build -p atlas-rt` skips it.

The example project's `lib/` is a junction into `target/`, so a build updates
the library Godot loads with no copy step. See
[crates/atlas-rt-godot/examples/project/README.md](crates/atlas-rt-godot/examples/project/README.md)
for the junction setup.

