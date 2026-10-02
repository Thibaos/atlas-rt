# The World adopts the renderer's Micro-chunk storage format

The World stores a voxel in the shape the renderer's Voxel pool already uses: a
Region holds one entry per non-empty Micro-chunk, a 64-byte Occupancy mask
followed by that Micro-chunk's material bytes in ascending cell order, located
through a 32,768-entry index of blob offsets. This is
[0001](0001-gpu-voxel-representation.md)'s format, adopted rather than
re-derived, so the World and the Voxel pool now share one layout instead of
holding the same content twice. The voxel read type narrows to `u8` with it,
because the storage holds one material byte per cell.

This ADR records what the change does to the numbers ADR 0009 rested on. The
layout itself is the region-backed store spec, not restated here.

## The superseded compile cost

[0009](0009-world-side-voxel-edit-contract.md) rejected a chunk occupancy index
on a measured figure: the compile read the world back per touched Micro-chunk as
512 `get_voxel` probes, at about 10 µs per touched chunk, and a supplementary
index was not worth adding to avoid that. The Region store did not change how the
compile reads. `compile_chunk` in `world/diff/edit.rs` still walks all 512 cells
and calls `world.get_voxel` on each, exactly as it did against the sharded map.
The spec predicted the compile would become a copy of the Micro-chunk's entry,
but that rewrite was never built.

The figure moved anyway. Measured 2026-10-02 on the `edit_path_timings` fixture,
the probe compile is about 11 µs per touched Micro-chunk for clustered and
scattered edits alike. The compile is probe-bound, and the Region store's probes
cost more than the map's did, so the storage change did not make it cheap. Ticket
14 decides whether to build the entry-copy compile and ticket 15 covers the
`rank` scan behind each probe.

The measured basis is superseded; the conclusion survives it. A second index
beside the storage still gains nothing. The Region store already holds one entry
per Micro-chunk, so the fix is to read that entry directly (ticket 14, which
decides the entry-copy rewrite) or make the per-cell read cheap (ticket 15), not
to maintain a parallel structure. The reopen trigger is left standing exactly as
ADR 0009 recorded it: scattered 10,000-plus edits per frame, or fills compiling
more than roughly 1,500 chunks in one frame. The compile cost is still the
barrier to it, at about 11 µs per touched Micro-chunk.

## The break-even arithmetic

The mask layout costs 64 bytes plus one per occupied cell per Micro-chunk. The
region adds 128 KiB of index charged once per Region, which is 4 bytes per
Micro-chunk. The map costs 19 to 39 bytes per occupied cell, 34 at a full
Lattice. Writing `F` for the fraction of a Region's 32,768 Micro-chunks that are
touched and `c` for the cells in each:

```text
c = (4/F + 64) / (map - 1)
```

At 34 bytes per cell that is about 2 cells when every Micro-chunk is touched, 3
when a tenth are, and 123 when one in a thousand is. The crossing is a function
of scatter, not a single figure. It is also lower per Region than per
Micro-chunk: a touched Region that holds about 4,000 voxels anywhere inside it,
which is 8 full Micro-chunks out of 32,768, already breaks even. The expensive
case is a World whose voxels are spread thin enough that most touched
Micro-chunks hold only a handful of cells, not a World that uses few Regions
densely.

## The savings against the map

A full Region is 32,768 Micro-chunks of 576 bytes, which is 18 MiB of blob plus
128 KiB of index, or 18.125 MiB over 16,777,216 voxels before the Region's share
of the table. That is 1.133 bytes per occupied voxel, and 1.625 on the dense
bench fixture, where each Region holds a 64^3 block, so its 128 KiB index is
charged over 512 Micro-chunks instead of 32,768. Against the map's 34 bytes per
voxel at a full Lattice, the store saves about 32 bytes per occupied voxel, a
factor of roughly thirty. Against the map's range of 19 to 39, the saving is 18
to 38 bytes per voxel. The dense write-only fixture asserts 1.625 and 1.702 for
the live layout and the free list's high-water mark.

## The sparse-case cost

The layout is worse than the map when a Region is touched but barely filled. A
Region holding one voxel costs 128 KiB of index, plus a one-voxel entry of 65
bytes padded to 72. One voxel in every Micro-chunk of a Region is up to 76.5
bytes per voxel, more than twice the map's figure at a full Lattice. The
arithmetic read path has no scan, no branch, and no hash, and that is what costs
here, paid once per touched Region and once per touched Micro-chunk whether or
not the Region is dense. The sparse fixture asserts against a looser padded
bound so the regression is recorded rather than allowed to creep.

The layout is therefore wrong for uniformly scattered noise, and the answer to
that case is a denser index shape, not a return to hashing. The ignored asset
tests over church and bistro are where the density question is read.

## Status

accepted (region-backed voxel store ticket 12, 2026-10-02). Supersedes the
measured compile cost in [0009](0009-world-side-voxel-edit-contract.md) and
cites [0001](0001-gpu-voxel-representation.md) as the format precedent the World
now shares. Amended (region-backed voxel store ticket 13, 2026-10-02): the
entry-copy compile was never built, so the compile still probes and the compile
cost is still the barrier to the reopen trigger; ticket 14 decides the rewrite
and ticket 15 covers the `rank` scan. [0004](0004-sharded-world-map.md)'s sharded
map is amended separately and survives as the differential oracle.

## Considered Options

- **Keep the sharded map as the resident store**. Rejected: 19 to 39 bytes per
  occupied voxel against 1.133 for the same content in the renderer's format,
  with the map's allocation sized from the in-lattice attempt count before a
  single voxel is written.
- **Add a supplementary chunk occupancy index beside the map**. This is the
  option ADR 0009 rejected on the 10 µs probe figure. The measured basis is
  superseded, but the rejection stands: the new layout does not need the index
  and gains nothing from a second structure. The reopen trigger is left as ADR
  0009 recorded it.
- **Keep the map and only change the measured compile cost**. Rejected: the
  memory cost is the reason for the change, and the compile re-measures close to
  the old figure either way.
- **Dense fixed 512-byte slabs per Micro-chunk**. Rejected by
  [0001](0001-gpu-voxel-representation.md), which this decision adopts rather
  than reopens: a slab wastes about 8x on a sparse Micro-chunk.
- **Return to hashing for sparse Worlds**. Rejected: the sparse case is real
  but the regression is bounded and measured, and a denser index shape addresses
  it without giving up the arithmetic read path.

## Consequences

- The World and the Voxel pool now hold the same content in the same shape, so
  no translation step sits between them.
- The voxel read type narrows to `u8`: `get_voxel` returns `Option<u8>` and
  `iter_voxels` yields `(IVec3, u8)`.
- The compile still probes: `compile_chunk` reads all 512 cells of a touched
  Micro-chunk through `world.get_voxel`. The entry-copy rewrite the spec
  predicted was not built, so ADR 0009's reopen trigger is still gated by
  compile cost, about 11 µs per touched Micro-chunk. Tickets 14 and 15 decide
  the fix.
- A touched but barely filled Region costs 128 KiB of index, which is the one
  case the layout handles worse than a map tuned for scatter. The ignored asset
  tests record where the repository's content sits against it.
- [0004](0004-sharded-world-map.md)'s sharded map is retained under the
  non-default `map-oracle` feature as the differential oracle, so every
  behaviour claim is checked against the thing this decision replaces.
