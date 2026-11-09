# Clustered geometry and partitioned TLAS

> Verification date: 2026-11-06. Sources: the DXR functional spec part 2 v0.38
> (2026-09-08), `VK_NV_cluster_acceleration_structure` revision 4 (2025-07-16),
> `VK_NV_partitioned_acceleration_structure` revision 1 (2025-01-09), the
> Microsoft Agility SDK 1.722 preview post (2026-10-28), and the atlas-rt tree.

## Executive answer

The proposal contains two separate features. Clustered Geometry replaces a
triangle BLAS with reusable clusters of triangles. Partitioned TLAS replaces a
monolithic TLAS with persistent instances grouped into partitions, so a frame can
rebuild only the partitions that changed.

The premise in the question holds for PTLAS, not for Clustered Geometry. PTLAS
does segment TLAS updates and does allow an instance count far above what a
full rebuild per frame would tolerate. Clustered Geometry only changes how a
triangle BLAS is built.

atlas-rt cannot use Clustered Geometry. A CLAS contains triangles, with a limit
of 256 triangles and 256 unique vertex indices, and a BLAS cannot mix triangles
with the procedural AABBs that atlas-rt traces. Converting the voxel hulls to
triangles would add geometry, add memory, and keep or worsen the in-shader
resolution that already works.

In the current design, atlas-rt can use PTLAS but the possible gain is under one
millisecond per dirty frame and zero on every other frame. The TLAS holds one
instance per region, at most 4096. It rebuilds only on frames that apply region
changes. The measured per-region update, which includes a TLAS rebuild, is
0.6 ms on an RTX 3070. PTLAS would add a persistent instance list, partition
bookkeeping, a new descriptor path, and a vendor-specific code path to save a
fraction of that.

The earlier voxel-as-instance design changes this question. It put one TLAS
instance per voxel over a shared box BLAS, with a cap of 2^20 instances and
nearest-chunk culling. Its frame path was hardware ray-triangle against the box
BLAS, with no intersection shader and no DDA, and its full TLAS rebuild is the
cost that pushed the project to per-region BLASes.
PTLAS targets exactly that cost. Against the current design it is not worth
implementing. Against a return to per-voxel instances it is the enabling
feature. The trade then is the old frame-time win against the instance memory,
partition limits, trace cost, and NVIDIA-only API.

Recommendation: do not implement Clustered Geometry. Do not implement PTLAS for
the current design. If the per-voxel design is revisited, prototype PTLAS and
measure frame time, partition rebuild time, and peak memory before committing.

## The two features

### Clustered Geometry

DXR calls it Clustered Geometry and defines three object types:

- **CLAS**: a cluster-level acceleration structure built from triangles. The DXR
  limit is 256 triangles and 256 unique vertex indices per CLAS. A CLAS cannot be
  placed directly in a TLAS.
- **Cluster template**: a partially built CLAS without vertex positions, used to
  instantiate many CLAS cheaply when only the positions change.
- **Cluster BLAS**: a BLAS assembled from references to CLAS. It can be traced
  and instanced like a normal BLAS.

The point is to avoid rebuilding a large triangle BLAS when an animated or
streamed mesh changes. A cluster that did not change is reused by reference, and
a changed cluster is rebuilt or instantiated from a template. NVIDIA's Vulkan
sample states the intended use plainly: cluster geometry is for meshes that
change frequently, and a traditional triangle BLAS is still recommended for
static content because it can trace faster.

### Partitioned TLAS

A PTLAS keeps a persistent instance list separate from the instances that rays
can see. Instances are assigned to partitions. The build has two stages: build
each partition from its instances, then combine the partitions into one
structure that rays traverse like a TLAS. A rebuild can touch only the partitions
that changed.

The spec adds several details that matter for atlas-rt:

- A **global partition** holds instances that behave as if each were in its own
  partition. It is intended for instances that move every frame.
- **Partition translation** lets a partition carry a translation vector that can
  be updated without rebuilding the partition. This is for world-origin shifts,
  not for streaming.
- Instances can carry **explicit bounding boxes**, so a BLAS pointer can be
  swapped for an LOD variant without a rebuild.
- Build arguments are read from GPU memory. The DX12 API is
  `ExecuteIndirectRTASOperations`; the Vulkan extension exposes multi-indirect
  build commands.

Trace cost depends on partition layout. Fewer, larger partitions trace better
and rebuild slower. More, smaller partitions rebuild faster and trace worse.
Overlapping partition bounds hurt traversal, the same way overlapping instances
do in a normal TLAS.

## DX12 and Vulkan status

| Capability | DXR, Agility SDK 1.722 preview | Vulkan | atlas-rt fit |
| --- | --- | --- | --- |
| Clustered geometry | CLAS, cluster templates, Cluster BLAS, 256 triangles and 256 vertices per CLAS | `VK_NV_cluster_acceleration_structure`, same object model, limits queried from `maxTrianglesPerCluster` and `maxVerticesPerCluster` | None. atlas-rt has no triangles. |
| Partitioned TLAS | Persistent instances, partitions, global partition, partition translation, explicit AABBs | `VK_NV_partitioned_acceleration_structure`, same model, `maxPartitionCount` queried | Possible, small gain at 4096 instances. |
| Indirect AS builds | `ExecuteIndirectRTASOperations`, inputs in GPU memory | Multi-indirect commands in both NV extensions | Little gain. atlas-rt builds are CPU-planned and rare. |
| Hardware | All RTX GPUs, Radeon RX 9000, Arc B-series and Core Ultra 200V, per Microsoft's table | NVIDIA only. No AMD or Intel Vulkan extension found. | The test RTX 3070 exposes both NV extensions. The integrated Radeon does not. |
| Status | Preview in Agility SDK 1.722, Windows Developer Mode required | Vendor extensions, not ratified, first submitted 2025-01-23 | No stable cross-vendor target. |

The Vulkan cluster extension requires a SPIR-V extension and a separate
`VkRayTracingPipelineClusterAccelerationStructureCreateInfoNV` in the pipeline
create chain, in addition to the AS build changes. The partitioned extension
requires a new descriptor type,
`VK_DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV`,
and a descriptor write through `VkWriteDescriptorSetPartitionedAccelerationStructureNV`.
Neither is in the Vulkan core specification or in a KHR extension as of the
verification date.

## atlas-rt baseline

The current design is described in `docs/research/voxel-rt-cost-model.md` and
`docs/adr/0001-gpu-voxel-representation.md`. The numbers below are from the
current tree.

- A region is 256³ voxels, split into 8³ micro-chunks. A region has 32,768
  micro-chunk slots.
- The lattice is 16³ regions, 4096 regions total.
- Each resident region owns one BLAS. The BLAS contains one AABB per non-empty
  micro-chunk, at most 32,768 AABBs.
- The TLAS has exactly one instance per region, with a static instance table of
  4096 entries. The instance custom index is the region id.
- The church scene has 112 resident regions and 222,791 non-empty
  micro-chunks. Its load applies 112 uploads, 112 BLAS builds, and one TLAS
  build in 39 ms of apply time, after 104 ms of CPU pack time.
- A single-region edit takes 0.6 ms of apply time for the widest region, which
  holds 4,908 micro-chunks. That path uploads one region, rebuilds one BLAS, and
  rebuilds the TLAS. The decision code marks the TLAS dirty on every region
  update because a BLAS build invalidates the top-level structure that references
  it.
- Rebuilds run synchronously on the graphics queue. The TLAS is always built
  with `BuildAccelerationStructureMode::Build`, even though its flags include
  `ALLOW_UPDATE`.
- There is no compaction, no async compute overlap, no GPU culling, and no
  serialization.

## The abandoned voxel-as-instance design

The initial renderer used a different acceleration structure. It matters because
it is the baseline PTLAS was designed for, and it is not the baseline in the
current tree.

- One TLAS instance per voxel. `Chunks::to_instances` at commit `52e435e`
  (`src/world/chunk.rs`) builds an `AccelerationStructureInstance` for each
  voxel, with the voxel position and scale in the transform.
- One shared BLAS, built from a 12-triangle box (`triangles_from_box` at the same
  commit, `src/world/voxel.rs`). Every instance references that one BLAS.
- A hard cap of `MAX_INSTANCE_COUNT = 2^20` instances, filled by sorting chunks
  by camera distance and taking the nearest. The church's 14.3M voxels were never
  all in the TLAS at once.
- No procedural intersection shader for the voxel geometry. The hardware traced
  the triangles of the shared box BLAS.

That design maps each voxel to one instance, so the TLAS size is the visible
voxel count. The current design maps each non-empty 8³ micro-chunk to one AABB
primitive, so the TLAS size is the region count. The trade is frame time against
update cost and memory.

## Clustered Geometry does not apply

The blocker is geometry type, not performance.

A CLAS is built from triangles. atlas-rt's BLAS is built from `AabbPositions`
and the hit is resolved by an intersection shader that runs a slab test and an
Amanatides-Woo DDA over the 8³ occupancy mask. Vulkan and DXR both forbid mixing
triangle geometry and AABB geometry in one BLAS, so adopting clusters would mean
a second acceleration structure, a second hit path, and a second set of shaders.
The test RTX 3070 reports `maxVerticesPerCluster` and `maxTrianglesPerCluster`
of 256, matching the DXR limit.

The numbers make the conversion unattractive even if the mix were allowed:

- A micro-chunk hull is a box: 12 triangles, 8 vertices. That fits one CLAS, but
  the cluster is still just a box. The shader still has to run the DDA to find
  voxels inside it, so the cluster adds build machinery and removes nothing.
- Emitting the voxel faces instead would remove the DDA, but an 8³ micro-chunk
  can expose up to 1,536 faces in a checkerboard pattern, which is six times the
  256-triangle CLAS limit. The church would need millions of triangles in place
  of 222,791 AABBs.
- The input alone grows about tenfold per hull: 24 bytes for an AABB against
  roughly 240 bytes for 12 triangles with 8 shared vertices. The current church
  AABB input is about 5.1 MiB.

The Microsoft demo numbers, 1.2 FPS to 79 FPS and 788 ms to 3.11 ms of
acceleration-structure work, come from a scene that rebuilds a large animated
triangle BLAS every frame. atlas-rt does not build every frame, and its 112 BLAS
builds average about 0.35 ms each in the church load. The demo says nothing
about procedural AABB geometry.

The case where CLAS would help, a deforming triangle mesh, does not exist in
atlas-rt. Voxel edits change occupancy counts, which already forces a BLAS
rebuild, and the rebuild is cheap because the geometry is boxes.

## Partitioned TLAS against the current design

The mechanism matches the premise. A PTLAS can hold many more instances than a
per-frame monolithic rebuild would tolerate, and a frame can rebuild only the
partitions that changed. The gap is between the mechanism and atlas-rt's actual
numbers.

- **Instance count**: in the current design the TLAS is bounded by regions, not
  by micro-chunks. The maximum is 4096, and the church load uses 112. PTLAS
  targets tens of thousands to millions of instances. At 112 instances, the
  whole TLAS build is already under the 0.6 ms measured for one region update,
  because that update includes a TLAS build.
- **Cadence**: the TLAS is rebuilt only when an apply changes residency or
  rebuilds a BLAS. A frame with no region work does no acceleration-structure
  work at all. PTLAS cannot improve a frame that already does nothing.
- **Where the time goes**: in the church load, 39 ms covers 112 uploads and 112
  BLAS builds plus the TLAS. The TLAS is one build of 112 instances and is
  bounded by the 0.6 ms single-update figure, so it is under 2% of the load.
  PTLAS cannot reduce the BLAS builds or the uploads, which dominate.
- **Memory**: PTLAS adds a persistent instance list, a second copy of instance
  data in the worst case, and partition structures. At 4096 instances the
  instance list alone is 256 KiB at 64 bytes per instance. That is small, but it
  is an increase, and the current TLAS storage is already sized for the maximum
  instance count.
- **Granularity**: PTLAS would make millions of instances cheaper to update, but
  it does not make finer instancing free. One instance per micro-chunk would put
  222,791 instances in the church TLAS and 134,217,728 slots in the full lattice.
  One instance per voxel would be larger still. A cheaper update does not fix the
  per-instance memory or the instance overlap.

One real use case remains: if the region count ever grows well past 4096, or if
dynamic objects are added as individual instances, the TLAS build could become
measurable. A PTLAS with spatial partitions would then let a changed region
rebuild one partition instead of the whole structure. That is a future condition,
not the current tree.

### Against the voxel-as-instance design

PTLAS addresses the update blocker directly. Instances are persistent and
grouped into spatial partitions. A local edit rebuilds the partition that holds
the changed voxels, not the whole TLAS. The full rebuild that made the old
design impractical is the operation PTLAS removes. The gain is largest when the
change is local. If camera movement changes which voxels are instanced, every
partition that gains or loses instances is rebuilt.

It does not remove the other costs:

- **Instance memory**: 64 bytes per instance. At the old 2^20 cap that is
  64 MiB. At the church's full 14.3M voxels it is about 900 MB. The current AABB
  input for the church is about 5.1 MiB, and the voxel pools are separate.
  PTLAS adds bookkeeping on top of the instance list; it does not compress it.
- **Partition sizing**: the DXR guidance favors partitions of roughly 100 to
  1,000 instances. At 2^20 instances that is 1,048 to 10,485 partitions. The
  church at 14.3M would need 14,300 to 143,000. The test RTX 3070 reports
  `maxPartitionCount` of 16,777,215, so the count fits. More partitions can
  still cost trace time.
- **Initial build**: the first build of every partition still costs a TLAS build
  for its instances. PTLAS amortizes updates, not the first scene build.
- **Trace cost**: partitions add a level to the structure and can hurt traversal.
  The old design's frame-time win has to survive that.
- **Portability**: Vulkan PTLAS is NVIDIA-only. A per-voxel path built on it
  would not run on AMD or Intel without a second path.

The experiment that settles it has three measurements: frame time for the old
per-voxel path and the current region path on the same scene, PTLAS rebuild time
for the old path's instance count and partition size, and peak memory. The
frame-time win and the update cost are both unknown without those numbers.

## Migration cost

The feature is not available through the current dependencies.

- `ash 0.38.0+1.3.281` exposes Vulkan 1.3.281 headers, which predate both NV
  extensions. The types, commands, and descriptor enum are absent.
- The pinned vulkano commit has no PTLAS wrapper. The task graph's bindless
  global set has no binding for the partitioned descriptor type, so the
  descriptor would have to be written by hand outside the resource map.
- The build command is multi-indirect and takes its arguments from GPU memory.
  The current rebuild graph records fixed tasks and tracks buffers through the
  task graph. A PTLAS build would need raw command recording and manual barriers,
  or a task graph extension.
- A non-NVIDIA GPU needs a fallback, so the TLAS code would keep both paths.
- Driver support is confirmed on the test machine. The RTX 3070 reports driver
  621.1.0.0, Vulkan 1.4.357, `VK_NV_cluster_acceleration_structure` revision 4,
  and `VK_NV_partitioned_acceleration_structure` revision 1. The integrated
  Radeon does not expose either, which is the portability problem in practice.

The cluster path has the same dependency and driver problems, plus the geometry
conversion described above.

## Recommendation

1. Do not implement Clustered Geometry. There are no triangles to cluster, and
   converting the voxel hulls would increase memory and keep the same hit path.
2. Do not implement PTLAS for the current design. The TLAS cost is under 0.6 ms
   per dirty frame by the single-update timing, and zero on clean frames.
3. If the voxel-as-instance design is on the table, measure its frame-time win
   first. Render the same scene through the old path and the current path. If
   the win is small, PTLAS does not change the decision.
4. If the win is large, prototype PTLAS before committing to the design. Query
   `maxPartitionCount`, test partitions at roughly 100, 1,000, and 10,000
   instances, and measure build time, trace time, and peak memory on the RTX
   3070.
5. Measure the current TLAS build separately. The bench reports it inside the
   apply time. A timestamp around `BuildTlasTask`, or a bench that builds only
   the TLAS, would replace the bound above with a number.
6. Try TLAS refit before PTLAS for the current design. The TLAS already sets
   `ALLOW_UPDATE` and never uses `Update` mode. A region update keeps the
   instance count and the instance set, so a refit is legal in principle.
   Measure it against the current build.
7. Watch for a KHR or multi-vendor PTLAS, for AMD or Intel Vulkan support, and
   for TLAS instance counts above roughly 10,000. Also watch
   `VK_KHR_device_address_commands`, which is the cross-vendor route to
   GPU-driven build commands, though it does not provide partitions by itself.

## Sources

- DXR functional spec part 2, clustered geometry and PTLAS:
  https://microsoft.github.io/DirectX-Specs/d3d/Raytracing2.html
- DXR functional spec part 1, TLAS and BLAS rules:
  https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html
- Microsoft, Agility SDK 1.722 preview announcement:
  https://devblogs.microsoft.com/directx/clas-ptlas-preview
- Microsoft, Agility SDK 1.620 retail and 1.722 preview release post:
  https://devblogs.microsoft.com/directx/agilitysdk-1-620-retail-and-1-722-preview
- AMD GPUOpen, Agility SDK October 2026 support and the demo numbers:
  https://gpuopen.com/learn/amd-directx12-agility-sdk-october-2026
- `VK_NV_cluster_acceleration_structure` proposal:
  https://docs.vulkan.org/features/latest/features/proposals/VK_NV_cluster_acceleration_structure.html
- `VK_NV_partitioned_acceleration_structure` proposal:
  https://docs.vulkan.org/features/latest/features/proposals/VK_NV_partitioned_acceleration_structure.html
- NVIDIA `vk_animated_clusters` sample and its guidance on static content:
  https://github.com/nvpro-samples/vk_animated_clusters
- Repo: `docs/research/voxel-rt-cost-model.md`, `docs/research/vulkano-ray-tracing-api.md`,
  `crates/atlas-rt/src/render/accel.rs`,
  `crates/atlas-rt/src/render/region/residency/`,
  `crates/atlas-rt/src/render/region/rebuild.rs`,
  `crates/atlas-rt/src/render/region/bench.rs`,
  `crates/atlas-rt/src/world/grid.rs`
- Repo history: commit `52e435e`, the initial per-voxel renderer, for
  `Chunks::to_instances` and `triangles_from_box`.
- Device query: `vulkaninfo` and a small `ash` probe on the RTX 3070,
  2026-11-06, driver 621.1.0.0, Vulkan 1.4.357. It reports both NV extensions,
  `maxPartitionCount` 16,777,215, and cluster limits of 256 vertices and 256
  triangles.

## Gaps

- The TLAS build is not timed on its own. The 0.6 ms bound comes from the
  single-region update, which includes one BLAS build, one upload, and one TLAS
  build.
- The device query covers properties and features only. Partition build time,
  update time, and trace cost on the RTX 3070 are still unmeasured.
- No public benchmark of PTLAS with AABB instances was found. The Microsoft
  numbers cover Clustered Geometry on triangle geometry.
- `maxPartitionCount` on NVIDIA hardware is queryable but not pinned here.
- The old per-voxel renderer's frame-time numbers are not in the current tree.
  The comparison against the current region path has to be rerun.
- No PTLAS prototype exists, so partition build time and trace cost on the RTX
  3070 are unknown.
- The current code rebuilds the TLAS after every BLAS rebuild. The Vulkan spec
  requires the referenced BLAS objects to be valid when the TLAS is accessed, but
  this research did not find an explicit rule that forces a TLAS rebuild after an
  in-place BLAS rebuild. The code comment asserts the need. The refit experiment
  is the way to test it.
