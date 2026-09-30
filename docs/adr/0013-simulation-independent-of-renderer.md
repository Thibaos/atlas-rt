# The simulation never depends on the renderer

The simulation lives in `atlas_rt::sim`, exported from `lib.rs` beside `world`
and `render`. It pulls in no winit, no vulkano, and no new crate, and it does
not depend on the `render` module. A crate split can be revisited when a
boundary needs one. For now the API surface is the boundary, not the crate
wall.

Each due tick applies its edits and publishes on its own, so a host update
running several ticks submits several batches in tick order.

1. The rules read one immutable view of the `World` for the whole tick and emit a pending `VoxelEdit` list. The rules never mutate the `World` directly.
2. One `edit_world` call applies the pending list at the tick end, then the returned snapshots go to `submit_batch`. A tick with no snapshots submits nothing.

The publish path never calls `wait_until_idle`. The guarantee is one-way: every
submitted batch is eventually applied, last-wins per Micro-chunk by the change
queue, and the renderer may show any intermediate state or trail the `World`
freely. If a tick's batch fails validation, the simulation logs and drops it in
all builds and asserts in debug builds. The `World` is left consistent by the
all-or-nothing contract and the frame keeps running, so a failure means a rule
emitted an out-of-lattice edit, which is a bug.

## Considered options

- **A separate simulation crate**. Rejected for now. It adds a crate wall without a boundary that needs one, and the first slice's requirement is that the simulation be drivable headless, which the module boundary already gives. Revisit when the adapter's needs are known rather than predicted.
- **Rules mutating the `World` during evaluation**. Rejected. The immutable-read-then-single-commit shape is what makes the commit window the only place a frame can stall, and what lets a discarded tick leave no partial mutation behind.
- **Re-emitting the world at activation**. Rejected: the loader already emitted it with progress reporting behind it, and a test pins the two equal.
- **A `TrackedCoords` mirror per owner**. Dropped for the simulation's own publish path, where the simulation plans every batch and `TrackedCoords` therefore has one owner. Palette upload, display suppression, and `settle_job` stay. See the gap below for what this does not yet cover.

## Verification

`tests/sim_boundary.rs` pins the boundary headlessly, with no renderer and no
window. It covers the activation ordering, where the batch answers before
readiness and renders; the new `World` staying readable through the shared lock
after the swap; spawn placement on the center column, on the roofline for an
empty column, on the lattice floor for an empty world, and upward depenetration
for a straddle, including staying buried with no clear position; a `clear` being
an activation that still ticks; a load in flight keeping its ticks, its commands,
and its timing until the activation lands; the push carrying the remainder and
the snap flag; the five-tick cap reporting its discard; ticks running only while
frames arrive; evaluation taking only the read lock while the host reads, the
commit waiting for the write lock, and the write lock stalling a tick; pause and
resume; activation while paused; and the profile surviving activation.

## Known gap

The Godot adapter is **not yet wired to `atlas_rt::sim`**. It still runs its own
load flow: `AtlasRtView` owns the `WorldUpdateJob`, calls `plan_load` and
`plan_clear` against its own `world_chunks`, uploads the palette, and submits
the batch itself. There is no `Activation` on the adapter's path, and no
`set_move`, `press_jump`, or `set_paused` binding.

The wayfinder's Godot adapter contract check reviewed that shape and concluded
`atlas_rt::sim` needs no change for it. That conclusion was reached by reading
the adapter, not by connecting it, and the following six points are recorded
here as the shape the wiring must take when it happens. None of them is
implemented.

- The frame path is `AtlasRtView::process(delta)` under `PROCESS_MODE_ALWAYS`, so elapsed and input flow while the tree is paused and stop when frames stop. That is the clock rule the thread boundary in [0012](0012-simulation-thread-and-clock-ownership.md) asks for, satisfiable without a second clock.
- An edit command carries the raw validated micro-chunk plus a single-cell variant for the ray, and the simulation diffs and applies it at commit. `submit_batch` returns true for validated and queued, never applied, which is all a GDScript caller can know.
- Activation carries the `World` plus the loader's snapshots. The host uploads the palette first, the simulation plans the batch under the write lock at the swap, and a `clear` is activation with an empty `World` and no snapshots.
- `InputSample::from_local` applies the view builder's x negation on the rotation, so local +x is the drawn frame's +x and both hosts pass keys straight through with D as +1 and no per-host fixup. A plain yaw rotation was rejected: it puts local +x on physical right, which is screen left in this frame, so strafing runs backwards against the view.
- The adapter stamps the jump edge from `Instant::now()` and sends it once, and the sticky merge holds it until a tick consumes it. Key bindings stay in the project, so no action name moves into Rust.
- The camera is written from interpolated feet plus the profile's eye offset, in one place. Scene-side positioning was rejected because it puts a second eye offset in the scene that can drift from the profile.

The adapter's `world_chunks` mirror goes away when the wiring happens and the
simulation becomes the only planner. Until then there are two planners, and the
one-owner rule covers the simulation's path alone.

## Consequences

- The standalone host drives the simulation today, and the simulation is testable with no renderer present.
- `ViewInterpolation` is library code at `atlas_rt::host` rather than a module under the standalone host's `app`, so the adapter imports the one type instead of copying the alpha formula.
- A rule that needs to know what the renderer did has no channel to ask through. That is the point.

## Status

accepted (2026-09-30). Promoted from the physics wayfinder's decisions 06, 07,
and 12, which were resolved against a prototype and had no earlier ADR.
