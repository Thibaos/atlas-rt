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
| `--world-type <type>` | Selects the generated World: `terrain`, the default, `sand-ball`, or `raining`. |
| `--seed <integer>` | Pins the Seed a Generation is built from. An unsigned or a signed literal, where a negative Seed names the same bits as Godot's. |
| `--extent <integer>` | Bounds a generated World's ground to a square of `extent` by `extent` cells from the Lattice's negative corner. The fill spans the full Lattice depth, so y is ignored. Defaults to the full Lattice. |
| `--fly` | Frees the camera from the simulated player and leaves the simulation running, so a World can be watched settle. |
| `--no-sim` | Runs no simulation thread, so no voxel rule runs and the World changes only where the host edits it. The camera flies free either way, because there is no player pose to follow. |

A run with no `--world`, no `--seed` and no `--world-type` generates terrain from
a random Seed and logs that Seed, so a logged Seed repeats a World:

    cargo run -p atlas-rt
    cargo run -p atlas-rt -- --seed 42
    cargo run -p atlas-rt -- --seed 42 --extent 512
    cargo run -p atlas-rt -- --world test/matl-alpha.vox
    cargo run -p atlas-rt -- --world-type sand-ball --fly
    cargo run -p atlas-rt -- --world-type raining

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
- `--world-type raining` ignores `--seed` and `--extent` for the same reason.
- An unrecognized `--world-type` value is refused rather than quietly building
  terrain.

Each refusal names its reason, and prints before any window opens:

    Error: the world type bogus is not terrain, sand-ball or raining
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

The scene exists to watch the Falling granular rules at scale. The ball falls as
a body: every grain moves on every tick, its vertical span stays at 128, and the
bottom reaches the floor at tick 128. A tick serves the queue a Micro-chunk at a
time and the commit writes a dense Micro-chunk whole, so the per-cell
bookkeeping no longer dominates the tick.

The ball's bench prints one row per window over 190 ticks. The fall, ticks 1 to
128, holds its tick p95 to 7.4 ms against a 10 ms gate. The pile, ticks 129 to
190, holds its tick p95 to 88.4 ms against a 360 ms gate. The same bench feeds
the largest free-fall tick's batch, 2,722 Micro-chunk snapshots, to the input
worker and prints the pack it takes for them: 1.9 ms, recorded rather than
asserted. At the fixed 30 Hz tick the fall's 128 ticks are 128 / 30 = 4.3
seconds of wall time.

The sand settles on the floor under the ball, so the ball and the pile stay in
one view. The binary logs the frame rate, the queued grain count and the rate
the rules process grains once a second.

### The rain

`--world-type raining` builds one fixed Bedrock plate full of holes 1,024 voxels
above a one-cell Bedrock floor, and the simulation rains Sand through it. The
plate is a disc of radius 256 at y = -1024 with 3,209 holes, a hole every eight
cells in x and z, and 202,652 cells of Bedrock around them. The floor is the
Lattice's bottom layer at y = -2048, as the sand ball's is.

The holes are an emitter rather than a shape: every tick the simulation plants
128 grains in the holes that are empty, and a hole fires at most every second
tick, so no column ever holds two grains one cell apart. A grain falls from
-1024 to the pile 1,023 cells below it, so the queue fills to about 131,000
grains over the first 1,023 ticks and then holds there, and a tick's p95 over
the plateau is about 148 ms. A grain changing its Micro-chunk every tick is most
of that cost, since a falling grain is usually alone in its chunk.

The player spawns on the floor at the centre column, under a hole, so the rain
falls on it and the pile grows at its feet. The pile is a grain deep over the
whole disc within a few minutes and needs about 55 hours of arrivals to reach the
plate, so the scene keeps raining. The frame log reports the grains emitted and
skipped a tick beside the queue and the tick rate.

Run it without `--fly`: the floor holds the player, unlike the ball's crown.
`--no-sim` builds the plate and the floor and rains nothing, because there is no
tick to plant from.
