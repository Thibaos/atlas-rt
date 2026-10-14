# atlas-rt

A real-time voxel renderer using hardware ray tracing (Vulkan RT pipelines, vulkano). Renders sparse voxel worlds, loaded from `.vox` files or generated from a Seed.

See [GLOSSARY.md](GLOSSARY.md) for the architecture and terminology.

## Layout

Two crates share one workspace, one lockfile, one `target/`, and one clippy
lint table declared in the root `Cargo.toml`:

- `crates/atlas-rt`: the renderer library and the standalone `atlas-rt` binary,
  with `shaders/` and `assets/` beside it.
- `crates/atlas-rt-godot`: the GDExtension, with the Godot example project
  under `examples/project`.

One command builds both:

    cargo build --workspace

Building the whole workspace also builds the GDExtension, which pulls in
`godot` and its bindings-generation build script. For renderer-only work,
`cargo build -p atlas-rt` skips it.

The example project's `lib/` is a junction into `target/`, so a build updates
the library Godot loads with no copy step. See
[crates/atlas-rt-godot/examples/project/README.md](crates/atlas-rt-godot/examples/project/README.md)
for the junction setup.

## Standalone flags

`cargo run -p atlas-rt` runs the standalone binary. It takes these flags. An
unrecognized flag is ignored, and a flag that needs a value and has none is
refused. There is no `--help`: it is unrecognized, so it starts an ordinary run.

| Flag | What it does |
| --- | --- |
| `--world <path>` | Loads a `.vox` file instead of generating a World. The path resolves under `crates/atlas-rt/assets/`, so run from the repository root. |
| `--world-type <type>` | Selects the generated World: `terrain`, the default, or `sand-ball`. |
| `--seed <integer>` | Pins the Seed a Generation is built from. An unsigned or a signed literal, where a negative Seed names the same bits as Godot's. |
| `--extent <integer>` | Bounds a generated World's ground to a square of `extent` by `extent` cells from the Lattice's negative corner. The fill spans the full Lattice depth, so y is ignored. Defaults to the full Lattice. |
| `--fly` | Frees the camera from the simulated player and leaves the simulation running, so a World can be watched settle. |
| `--no-sim` | Runs no simulation thread, so no voxel rule runs and the World changes only where the host edits it. The camera flies free either way, because there is no player pose to follow. |

A run with no `--world`, no `--seed` and no `--world-type sand-ball` generates
terrain from a random Seed and logs that Seed, so a logged Seed repeats a World:

    cargo run -p atlas-rt
    cargo run -p atlas-rt -- --seed 42
    cargo run -p atlas-rt -- --seed 42 --extent 512
    cargo run -p atlas-rt -- --world test/matl-alpha.vox
    cargo run -p atlas-rt -- --world-type sand-ball --fly

Only the fixtures under `crates/atlas-rt/assets/test/` are tracked. The large
worlds, such as `castle.vox` and `sponza.vox`, are per-checkout and absent from a
fresh clone.

How the flags combine:

- `--seed` outranks `--world`, so `--seed 42 --world test/matl-alpha.vox` still
  generates.
- Two World sources cannot be named at once, so `--world` beside `--world-type`
  is refused.
- `--extent` beside `--world` is refused, because there is no ground to bound.
- `--world-type sand-ball` ignores `--seed` and `--extent`, because the ball is a
  fixed shape and neither flag has anything to act on.
- An unrecognized `--world-type` value is refused rather than quietly building
  terrain.

Each refusal names its reason, and prints before any window opens:

    Error: the world type bogus is not terrain or sand-ball
    Error: --world loads a file, so --world-type has nothing to select
    Error: --world loads a file, so --extent has nothing to bound

### The sand ball

`--world-type sand-ball` builds one fixed ball of Sand just above a one-cell
Bedrock floor: about 1.1 million grains, 128 voxels across, its lowest layer 128
voxels above the floor's surface at y = -2047. The floor is the Lattice's bottom
layer at y = -2048, one cell under every one of the 4,096 by 4,096 columns.
Watch the ball with `--fly`. Every grain is queued at activation, and a queued
Falling granular cell does not support the player, so the player free-falls from
the top of the ball.

The scene exists to watch the Falling granular rules at scale. The first grain
reaches the floor after about 4 seconds, and the whole ball drains in about half
an hour at the default 30 Hz, less as the pile grows, so a queue that keeps
running is not a fault. The sand settles on the floor under the ball, so the ball
and the pile stay in one view. The binary logs the frame rate, the queued grain
count and the rate the rules process grains once a second.
