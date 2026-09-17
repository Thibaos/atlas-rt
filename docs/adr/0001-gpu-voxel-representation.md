# GPU voxel pools and in-shader voxel resolution

The renderer stores voxels in one GPU buffer pool per Region, addressed through
BDA. Each non-empty Micro-chunk has a 64-byte Occupancy mask and
popcount-compacted u8 material bytes, located through a u32 offset table.

The intersection shader resolves voxel hits. Micro-chunk AABBs use absolute
region-local coordinates and are trimmed to occupied bounds, so the hit position
identifies the micro-chunk and cell. The DDA commits a hit with
`reportIntersectionEXT(t, material_index)`. The 8-bit `hitKind` carries the
material index, leaving closest-hit to look up the palette entry and the payload
to store `{color, t}`.

## Status

Accepted, rendering-core ticket 04, 2026-08-10.

## Considered options

- **Textures / bindless sampled images per micro-chunk.** Rejected. Discrete
  palette indices need no filtering, and sparse micro-chunks need indirection
  anyway.
- **Dense fixed 512B material slabs per micro-chunk.** Rejected. They waste about
  8x the memory on sparse micro-chunks. One popcount per committed hit costs
  little; material sampling is not the bottleneck.
- **Material via SBT record offset or payload.** Rejected. The SBT record offset
  cannot vary per primitive within a region. The payload would carry data
  closest-hit does not need.
- **Full 8^3 hulls.** Rejected by the owner. Trimmed hulls avoid intersection-shader
  invocations for rays that miss a sparse micro-chunk's occupied sub-volume.
  See Teardown finding 3.

## Consequences

- Voxel edits rebuild the whole region pool and region BLAS. Compacted block
  sizes change with popcount, so there is no in-place patching.
- f32 precision concerns are limited to ray origins, directions, and instance
  transforms. Region-local coordinates are <= 256, narrowing ticket 05's
  precision question.
- The world supplies Micro-chunk snapshots `{coords, mask, materials}`. The
  renderer owns the region lattice. Ticket 07 defines the change path.
