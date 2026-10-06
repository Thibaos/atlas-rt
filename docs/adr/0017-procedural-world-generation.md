# A generated World is a World supply fixed by a Seed

atlas-rt could obtain a World only by loading a `.vox` file. A second World
supply now builds one from a program: a Generation is bounded by the Lattice,
runs in one pass off the main thread, and is asked for with a Seed and an
extent. One Seed fixes one World completely, voxel for voxel and Snapshot
for Snapshot, on every machine and every build. The `.vox` load is unchanged
and remains the other supply.

The design stays small. The store gains one write and one read, and neither
changes the World's format: the generator fills a Micro-chunk and hands over
its Micro-chunk entry in a single call, and the emitter reads entries back
instead of re-encoding voxels. The World remains the single source of truth and
the renderer's content is derived from it.

[0016](0016-world-microchunk-storage-format.md) is what makes this possible.
Because a Micro-chunk's stored form is already the shape a Generation produces
and a Snapshot carries, both the write and the read can be at entry
granularity rather than at cell granularity.

## Status

accepted (2026-10-03). Amended (2026-10-04) after measurement: a Generation
writes Micro-chunk entries rather than cells, and `emit_snapshots` reads entries
rather than walking voxels. Both replace a decision taken in the first draft,
and both are recorded under Consequences with their measured cost. Widens
**World load**, **World job** and **Palette** in `GLOSSARY.md`, adds its terms
under **World generation**, and adds **Micro-chunk entry**. The rejected-float
option below is the one the Height field took, in fixed point, recorded by
[0019](0019-integer-gradient-noise-height-field.md). Amended (2026-10-06): the
standalone binary generates by default, and `--world` keeps the load.

## Considered Options

- **A `.vox` used as a stamp library**, with the generator choosing a position
  and rotation per model. Rejected: a model is one shape, so a structure with
  per-part materials and per-variant shapes needs a model for every
  combination. Tiered structures multiply structures, parts, tiers and
  variants, and that library is authored by hand.
- **A Region-parallel build**, one owner thread per Region slot as the `.vox`
  loader does. Rejected: an entry write takes the fill to about 1.4 seconds at
  the full Lattice, which does not buy a partition.
- **A cell write per voxel**, the first draft's decision. Rejected on
  measurement: the store's own bookkeeping is about 46.5 of the 47.5 ns a cell
  write costs, against a 1.0 ns floor for visiting the cells and choosing their
  materials, and an entry write costs 1.3 ns. The saving is 36 times the fill.
- **Float noise.** Rejected: the pinning promise would then rest on the
  toolchain's floating-point behavior rather than on the Seed alone. Integer
  hashing keeps the promise a property of the Seed.
- **One seed consumed as a sequential stream in feature order.** Rejected: the
  height at one column would then depend on how many columns were visited
  before it, which forbids parallel generation permanently and makes a World
  depend on the generator's revision. Fixed per-feature tags keep a later
  feature from moving the Worlds of the features before it.
- **The caller supplies the Palette and the Physical material table.**
  Rejected: neither is constructible from outside the crate, and a fixed
  in-code Vocabulary is what makes a material's index, color and rule one edit
  in one place.

## Consequences

- **World load** is no longer the only World supply. **World job** covers a
  third request beside load and clear.
- `progress::Stage` gains `Generate`, and the pipeline branches: a load runs
  Read, Parse, Build, Emit, and a Generation runs Generate, Build, Emit.
- The store gains a Micro-chunk entry write. Its default is the per-cell path,
  so the sharded map oracle is unchanged, and the Region store overrides it with
  one block allocation and one mask-plus-materials copy. Writing over an entry
  that already exists frees the block it held.
- The store gains a Micro-chunk entry enumeration, in Region, then Micro-chunk
  ordinal order. Its default reproduces the existing voxel walk and bucketing,
  so the oracle and the Region store stay comparable, and `emit_snapshots` reads
  through it.
- The per-voxel record buffer in `emit_snapshots` is gone, and with it the
  8.6 GB transient the first draft reserved at the full Lattice. The bucket
  flush that the first draft needed is no longer required, and the emitter keeps
  its final sort so its output order is unchanged.
- A Generation bypasses `budget::cell_budget()`, which refuses a load past its
  budget before allocating. A generated World has no budget by request, and the
  load path stays the budget's only guard.
- Measured on a 512x512 extent of 16,897,321 voxels in 49,151 Micro-chunks,
  in release, then scaled by 64 to the full Lattice. Filling cell by cell costs
  47.5 ns per cell, filling by entry 1.3, emitting by voxel 13.4 and emitting by
  entry 0.7. End to end that is about 66 seconds against about 2.2 seconds, of
  which the World is about 1.21 GB and the Snapshots about 1.4 GB.
- The extent is a parameter rather than a constant, so a development run
  generates a small World.
- The standalone binary generates by default. A run with no `--seed` and no
  `--world` draws a random Seed and logs it, so each run opens on a different
  World and the logged Seed repeats that World. `--world` still loads a file,
  and `--extent` now bounds the default Generation as well as a pinned one.
