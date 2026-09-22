# World-side voxel edit contract

Callers edit voxels through one primitive, `edit_world`, which takes a slice of
voxel-level edits, mutates the `World`, and returns a `Batch`: one Micro-chunk
snapshot per touched chunk plus the tracked coordinate set after they apply.
The renderer's message granularity does not change. Folding happens on the
world side, so [0003](0003-renderer-input-contract.md)'s rejection of
per-voxel delta messages still holds.

The `World` is the single source of truth for voxel content. `edit_world` and a
load that constructs a fresh `World` are the only writers; `World::set_voxel`
and `World::clear_voxel` stay crate-visible so no second writer exists. Each
path's host keeps the `World` for the lifetime of the loaded world, which the
Godot view did not do before; the renderer keeps only packed regions built from
snapshots. The CPU reference tracer drives from `World::get_voxel`
([0008](0008-validation-reference-tracer.md)), so an edit that lands in the
`World` is visible to both the GPU frame and the reference, while a
renderer-only patch would make the reference compare a pre-edit scene against a
post-edit frame.

Changes are a two-variant enum, `Set(u8)` and `Clear`, not an optional
material. `None` reads as "do nothing here" as easily as "clear here", and the
two must not be confusable at a call site. Materials are `u8` because the
Palette has 256 entries, the snapshot carries bytes, and
[0004](0004-sharded-world-map.md) requires material indices to fit a byte;
`World` stores `u32` and widens on write.

Validation runs first and is all-or-nothing. An out-of-lattice position, or a
position whose Micro-chunk would fall outside the region lattice, rejects the
whole batch with an `EditError` naming the offending edit. Nothing is mutated
and nothing is submitted. This is a caller-supplied edit crossing a crate
boundary, not an internal invariant, so it returns an error rather than
panicking as `World::assert_in_lattice` does. The primitive must never produce
a snapshot that `RendererInput::submit_batch` rejects, because that path
asserts.

The compile reads the world back per touched chunk: 512 `get_voxel` probes,
mask bits for the occupied cells, materials in ascending cell order, and a
cleared snapshot for a chunk left empty. `World` keeps no chunk occupancy
index. `edit_path_timings` put the on-thread cost at about 10 µs per touched
chunk, so the measured fallback, a locked or worker-owned `World` plus a chunk
index, was not taken. It reopens if scattered 10,000-plus edits per frame or
fills compiling more than roughly 1,500 chunks in one frame become real
targets.

The tracked coordinate set is caller-owned and updated by the primitive. It
means what the renderer holds after the last submitted batch, and the `World`
runs ahead of the renderer between a submit and the frame that applies it, so
deriving the set from the `World` would answer a different question in that
window and would need the chunk index that does not exist.

A touched region rebuilds wholesale
([0001](0001-gpu-voxel-representation.md)), so edits per frame are bounded by
the regions dirtied per frame times the per-region rebuild, not by edit count.

Out of scope by decision: palette mutation, backpressure or edit refusal when
the renderer trails the `World`, a resync path for a divergence, and brush or
shape generators.

## Status

accepted (voxel-edits ticket 06, 2026-09-22)

## Considered Options

- **Per-voxel messages to the renderer**. Rejected: non-idempotent, demands
  renderer-side incremental mask maintenance, and contradicts the wholesale
  region pool rebuild
  ([0001](0001-gpu-voxel-representation.md)), the same rejection
  [0003](0003-renderer-input-contract.md) already records.
- **Renderer-only patch that leaves the `World` stale**. Rejected: paint,
  raycast, and the reference tracer
  ([0008](0008-validation-reference-tracer.md)) all read the `World`, so a
  patch that only reaches the renderer makes them disagree with the frame, and
  a reference run that applies an edit between two captured frames would
  report the deliberate edit as a divergence.
- **Public per-voxel setter on `World`**. Rejected: a second writer outside
  the primitive would skip validation, skip compilation, and leave the tracked
  set and the change queue unaware of the edit. The setters stay
  crate-visible for `edit_world` alone.
- **Chunk occupancy index in `World`**. Rejected: the measured cost above meets
  the bar, and the region repack that follows the first edit in a region
  dominates the probes the index would save. The fallback and its trigger
  condition are recorded above.
- **Shape generators in the primitive** (`fill_box`, `fill_sphere`, ray
  fills). Rejected: they are pure functions over positions that expand to the
  same set and clear edits, and putting them in the primitive couples brush
  policy to the mutation and compile path.

## Consequences

- [0003](0003-renderer-input-contract.md)'s "world side, implemented later"
  now exists for edits; its Status points here.
- The Godot view holds a `World` for the lifetime of the loaded world, which it
  did not before. Its `submit_microchunk` and `submit_batch` keep their
  GDScript surface but diff incoming chunks against the `World` and express
  them as edits, so no raw snapshot path remains.
- Material changes are index changes. Repainting with a color the `.vox` never
  defined needs a palette path that does not exist yet.
- The reference tracer's run that edits between two captured frames is
  coherent: the edit lands in the `World`, so the GPU frame and the reference
  see the same scene.
