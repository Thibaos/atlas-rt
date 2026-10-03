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
index was not worth adding to avoid that. The Region store keeps one entry per
Micro-chunk, so the compile now copies that entry instead. `compile_chunk` in
`world/diff/edit.rs` reads the 64-byte mask and the compacted materials in one
pass, at about 100 ns per touched Micro-chunk on the `edit_path_timings` fixture
(2026-10-02), against about 10.5 µs for the probes. An entryless Micro-chunk
falls back to the 512 probes.

A second index beside the storage still gains nothing. The Region store already
holds one entry per Micro-chunk, and reading it directly is the fix the probes
needed; the per-cell `rank` scan is off the compile path and, after ticket 15,
out of `iter_voxels`, where a running counter replaces it, and word-based behind
`get_voxel`. The reopen trigger is
left standing exactly as ADR 0009 recorded it: scattered 10,000-plus edits per
frame, or fills compiling more than roughly 1,500 chunks in one frame. The
compile no longer gates it. At 100 ns per chunk the 1,500-chunk fill is about
0.15 ms, so the barrier to the trigger is the remaining work per edit and the
storage cost, not the compile.

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
now shares. Amended (region-backed voxel store ticket 13, 2026-10-02) to record
the compile as it was installed then. Amended again (ticket 14, 2026-10-02): the
entry-copy compile is installed, at about 100 ns per touched Micro-chunk, and
the 512-probe path is the fallback for an entryless Micro-chunk in the Region
store. Amended (ticket 15, 2026-10-02):
`rank` was a byte scan from zero on every call. `iter_voxels` now carries its
rank as it walks, and random-access `rank` reads the 8-byte word holding the
cell. `get_voxel` fell from 20.4 to 10.3 ns and `iter_voxels` from 18.4 to 3.0
ns per voxel on the dense fixture; bistro's emission fell from 4.119 to 2.781 s.
Amended (map-oracle removal, 2026-10-03): the sharded map, the `map-oracle`
feature, and the differential tests that ran against it are deleted, so the
Region store is the only voxel store and an entryless Micro-chunk is the only
probe fallback. [0004](0004-sharded-world-map.md) records the deletion.

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
  memory cost is the reason for the change, and the map has no Micro-chunk entry
  to copy, so its compile stays probe-bound.
- **Dense fixed 512-byte slabs per Micro-chunk**. Rejected by
  [0001](0001-gpu-voxel-representation.md), which this decision adopts rather
  than reopens: a slab wastes about 8x on a sparse Micro-chunk.
- **Return to hashing for sparse Worlds**. Rejected: the sparse case is real
  but the regression is bounded and measured, and a denser index shape addresses
  it without giving up the arithmetic read path.
- **A stored per-entry rank prefix**. Considered when the compile's 512 probes
  read back through the mask's per-cell `rank` scan (ticket 15). Rejected: the
  running counter in `iter_voxels` and the word popcount in random-access `rank`
  remove the scan without storage, and a prefix over the eight 64-bit words is 16
  bytes per entry, which pushes the full 576-byte entry past the free list's
  class ceiling and adds about 2.8% to a full Region's blob. Storage is what this
  layout exists to save.

## Consequences

- The World and the Voxel pool now hold the same content in the same shape, so
  no translation step sits between them.
- The voxel read type narrows to `u8`: `get_voxel` returns `Option<u8>` and
  `iter_voxels` yields `(IVec3, u8)`.
- The compile copies the touched Micro-chunk's entry: `compile_chunk` reads the
  Region store's 64-byte mask and compacted materials, about 100 ns per chunk
  against the 512 `get_voxel` probes' about 10.5 µs. An entryless Micro-chunk
  falls back to the probes.
- A touched but barely filled Region costs 128 KiB of index, which is the one
  case the layout handles worse than a map tuned for scatter. The ignored asset
  tests record where the repository's content sits against it.
- [0004](0004-sharded-world-map.md)'s sharded map is deleted (map-oracle
  removal, 2026-10-03). The Region store is the only voxel store, so its tests
  check it against independent references rather than against the map this
  decision replaces.
