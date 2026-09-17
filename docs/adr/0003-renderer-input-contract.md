# Renderer input contract: snapshots, change queue, BLAS residency

The world supplies Micro-chunk snapshots containing global coordinates, a
64-byte Occupancy mask, and u8 material indices. Create, update, and removal use
the same message. An emptied Micro-chunk supplies a zero mask, and the last
snapshot for each Micro-chunk wins.

The renderer owns the region lattice and derives region ids from global
coordinates. Each origin-aligned region contains 256^3 voxels. The v1 extent of
±2048 per axis gives 16^3 = 4096 regions, exactly the 12-bit region-id budget.
A CPU-side mirror of each region supplies the data for whole-pool repacking.

A content edit rebuilds only the region's BLAS, in place. Its device address
stays stable, so the TLAS rebuilds only when a region becomes empty or non-empty
and its instance is removed or created. Instance transforms and custom region
indices are static; masks are always 0xFF. There is no instance-level culling.
The hardware TLAS rejects regions per ray.

Rebuilds are ordered between the trace that consumes the old data and the next
trace: pool upload, BLAS build, then TLAS build if residency changed. The
original decision used ordered taskgraph nodes; the amendments below describe
the implemented host-side sequencing. This ordering makes in-place rebuilds
race-free without double-buffered acceleration structures or an atomic flip.
The worker only drains and packs CPU data.

Free lists manage region BLAS and pool-buffer memory. A region becomes resident
on its first non-empty Micro-chunk and leaves when its last Micro-chunk empties.
Memory can be returned and reused only after the rebuild sequence that removes
the referencing instance has executed. Streaming uses batches of snapshots.

## Status

accepted (rendering-core ticket 07, 2026-08-10). Amended (grilling
session, 2026-08-21): `RegionStore` owns the consume cycle:
`new` and `apply` are the only drain points of the dirty-region set,
and the app calls `apply` unconditionally each frame. The "ordered
taskgraph nodes" above is realized as host-side sequencing between
frames (graphics flight idle → compute-flight rebuild graph → next
trace), not as nodes of one taskgraph. The race-freedom argument holds
through the flight waits.

Amended (frame-sequence consolidation, 2026-08-30): the per-frame `apply`
call moved from the app into `FramePipeline::run_frame`; the drain-point
contract stands: `new` and `apply` remain the only drain points of the
dirty-region set, applied unconditionally each frame.

## Considered Options

- **TLAS rebuild on every content edit**. Rejected: the instance references the BLAS by device address, stable across in-place rebuilds; a per-edit TLAS rebuild is needless work.
- **Per-voxel delta messages**. Rejected: non-idempotent, demands renderer-side incremental mask maintenance, contradicts the wholesale region pool rebuild (ADR 0001).
- **World-derived region ids / pull-diff interface**. Rejected: duplicates lattice constants or couples the renderer to the world's change tracking; the renderer owns the lattice and the world stays region-agnostic.
- **Camera-driven frustum culling via instance masks**. Rejected (owner): per-frame instance changes break change-driven rebuilds; the hardware TLAS already rejects per-ray. Revises 03's "region-granularity culling".
- **Double-buffered back AS + flip atomic worker**. Rejected for rebuilds (revises 03's mechanics): in-place rebuilds of objects the front frame traces cannot be gated by wait-for-frame-advance; ordered taskgraph nodes give correctness without the double buffer or the flip.
- **Chunk-level change events / visibility flag**. Rejected: no chunk-level events; chunk-visibility semantics are the world side's concern, expressed as snapshots if at all.

## Consequences

- The world side (loading/editing/streaming, implemented later) calls `submit_microchunk` / `submit_batch`; enqueue-only, any thread, never blocks on GPU.
- Content edits cost one in-place region BLAS rebuild; TLAS rebuilds only on region residency transitions; rebuild GPU time is inline in one frame (measured by ticket 06); startup is a one-shot pre-loop build.
- Contract violations between world and renderer surface as rendering artifacts; the reference tracer that exercised the contract directly (ticket 06 / ADR 0003) was removed with the validation teardown (2026-08-27).
- Region BLAS + pool memory churn under streaming is absorbed by free lists; reuse ordering prevents use-after-free.
- 03's "region-granularity frustum culling" and "async double-buffered worker (back AS + flip)" are revised by this ADR.
