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
staged map into the resident material-only map. SceneGraphTraverser borrows
model voxels as slices instead of cloning them.

## Status

accepted (load-performance ticket 06, 2026-09-07)

## Considered Options

- **Keep serial insertion**. Rejected: 5.4s of world_new on bistro, and the
  ticket exists to parallelize that stage.
- **Naive union into one map**. Rejected: merging per-thread maps re-inserts
  every voxel once, about a second at bistro scale, which eats the parallel
  win and funnels every insert through one lock.
- **Sharded map with staged sequence-tag merge**. Chosen: routing spreads
  inserts over 64 maps, staged merging resolves overlapping writes without a
  global lock, and the strip pass is the only full rehash. Bistro world_new
  drops 5.4s to ~0.9s.
- **Clone model voxels into the loader**. Rejected: the clone was ~1.8GB at
  bistro scale; the traverser hands out borrowed voxel slices instead.

## Consequences

- World's public surface is unchanged: callers pass and receive IVec3
  coordinates; the packed fold key, shard routing, and staged maps are
  internal.
- Iteration order (iter_voxels, voxel_bounds) is shard-major and unordered
  within a shard; content comparisons must be order-insensitive.
- reserve distributes capacity per shard (additional / 64 each); ticket 07
  touches the same reserve site and must keep the per-shard split.
- Staged maps (sequence-tagged values) and resident maps coexist during the
  strip pass, so peak load memory holds both; staged maps are consumed
  shard-by-shard as they strip.
- Material indices must fit a byte, which the .vox format guarantees.
