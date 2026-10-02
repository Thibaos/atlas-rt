# Sharded world map for parallel load

World's voxel map is sharded for parallel construction: 64 hash maps, one per
shard, each entry keyed by the bijective 36-bit fold of the voxel's position
(three biased 12-bit axis fields, x high), every fold input gated by the
in-lattice check. A golden-multiply fold of that key routes each voxel to its
shard; FxHasher hashes the maps themselves. World::load stages placements
across threads, per model in 8192-voxel chunks: each chunk routes
sequence-tagged records into per-shard mutex-guarded staged maps, and the
maximum sequence (model order high, file order low) wins, reproducing the
serial path's last-write-wins. A final per-shard strip rehash converts each
staged map into the resident material-only map. The scene graph traverser
borrows model voxels as slices instead of cloning them.

## Status

accepted (load-performance ticket 06, 2026-09-07). Amended (region-backed
voxel store, 2026-10-01): the sharded map is superseded as the World's
resident storage. `World` becomes a flat table of 4096 Region slots indexed by
the 12-bit region id, and parallel load partitions by region id instead of by
hash route, so there is one owner thread per Region and no mutex array. The
sharded map survives as an oracle gated behind the non-default `map-oracle`
feature, kept for the differential tests, not as the resident store. The
decision this
ADR recorded, that insert contention and last-write-wins are resolved without a
global lock, still stands: partitioning by region satisfies it as well as
routing by hash did. Sequence tags survive inside a Region, because two
placements writing one cell still need the later to win.

Three of the consequences below are void, and each says so in place. The
per-shard `reserve` split and its instruction to ticket 07 have no referent once
the shards are gone, and the per-shard strip pass is replaced by a per-Region
one. The staged and resident coexistence consequence is removed rather than
revised: staging writes into the Region's own blob, so peak load memory no
longer holds two copies of the World. That is the largest single memory win of
the change and the reason the load stage no longer needs a pre-write reserve
sized from the in-lattice attempt count.

## Considered Options

- **Keep serial insertion**. Rejected: 5.4s of world_new on bistro, and the
  ticket exists to parallelize that stage.
- **Naive union into one map**. Rejected: merging per-thread maps re-inserts
  every voxel once, about a second at bistro scale. That eats the parallel win
  and funnels every insert through one lock.
- **Sharded map with staged sequence-tag merge**. Chosen: routing spreads
  inserts over 64 maps, staged merging resolves overlapping writes without a
  global lock, and the strip pass is the only full rehash. Bistro world_new
  drops from 5.4s to ~0.9s.
- **Clone model voxels into the loader**. Rejected: the clone was ~1.8GB at
  bistro scale. The traverser hands out borrowed voxel slices instead.

## Consequences

- World's public surface is unchanged: callers pass and receive IVec3
  coordinates; the packed fold key, shard routing, and staged maps are
  internal.
- Iteration order (iter_voxels, voxel_bounds) is shard-major and unordered
  within a shard; content comparisons must be order-insensitive. (Void as of
  2026-10-01: iteration is by Region id, then Micro-chunk ordinal, then cell
  index, which is deterministic, so order-insensitive comparison is no longer
  required.)
- reserve distributes capacity per shard (additional / 64 each); ticket 07
  touches the same reserve site and must keep the per-shard split. (Void as of
  2026-10-01: no shards and no per-shard reserve site remain. Ticket 07 must
  not look for one.)
- Staged maps (sequence-tagged values) and resident maps coexist during the
  strip pass, so peak load memory holds both; staged maps are consumed
  shard-by-shard as they strip. (Void as of 2026-10-01: staging writes into
  the Region's own blob, so there is no second copy of the World during load.)
- Material indices must fit a byte, which the .vox format guarantees.
