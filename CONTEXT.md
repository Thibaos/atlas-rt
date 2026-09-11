# atlas-rt

A real-time voxel renderer using hardware ray tracing (Vulkan RT pipelines,
vulkano). Renders sparse voxel worlds loaded from .vox files.

## Language

**World**:
The scene loaded from a .vox file: a sparse set of occupied voxels, held in a
flat map keyed by global coordinates, internally sharded 64 ways by hash
route, plus a 256-color palette. Worlds are loaded once at startup today.
_Avoid_: Scene, level, map

**Palette**:
A 256-entry RGBA8 color table from the .vox file mapping material indices to
display colors; kept sRGB-encoded end to end. The ray pass converts a hit's
entry to linear for the display path. GPU-side: a bindless vec4[256] storage
buffer.
_Avoid_: Color table, LUT

**Material index**:
The per-voxel u8 the voxel pool carries beside the Occupancy mask: the
Palette entry the voxel paints with. There is no surface property table, so
the renderer shades from the Palette alone.
_Avoid_: material id, MATL, material system

**Normal**:
The geometric surface normal at a voxel hit: the face the DDA's march
entered the committed voxel through (the entered face, never a neighbor
average). The DDA intersection shader knows it exactly as the last
Amanatides-Woo step axis, but reportIntersectionEXT carries only (t, 8-bit
hitKind) and the payload is opaque to intersection shaders, so the closest
hit reconstructs it from the hit point (the reported t is the cell-entry
boundary crossing): p[a] is an integer, within epsilon, exactly on the
crossed axis. Ties (edge/corner entries) break to the first axis in x, y, z
order, the DDA's own preference order. A camera embedded in a voxel (the
t_min commit, no crossed face) gets the camera-facing direction instead.
Object space == world space up to the translation instance transform.
Carried in the ray payload; the Normal debug Render mode paints it as a
heatmap (x red, y green, z blue).
_Avoid_: facet normal, interpolated normal (no raster interpolants exist)

**Micro-chunk**:
The renderer's 8x8x8 render/acceleration-structure unit, tightly wrapped to
occupied voxels (owner requirement; named by rendering-core ticket 03). One
AABB per non-empty micro-chunk. That AABB is the trimmed hull (tight occupied
bounds), not the full 8x8x8 cell box.
_Avoid_: cell

**Region**:
The renderer's grouping of Micro-chunks that share one acceleration-structure
build: 32^3 micro-chunks (256^3 voxels). The TLAS holds one
instance per region; a region's structure exists only while it holds >=1
non-empty Micro-chunk.
_Avoid_: Super-chunk, block

## Voxel storage

**Voxel pool**:
The renderer's GPU-side storage for voxel data, organized per Region: for
each non-empty Micro-chunk, one Occupancy mask plus the material indices of
the occupied voxels. Built by the renderer from the world's Micro-chunk
snapshots; the world never writes it.
_Avoid_: Voxel buffer, voxel data store

**Occupancy mask**:
The 512-bit presence bitmap of a Micro-chunk: one bit per voxel, set iff
the voxel is occupied. The mask, not a sentinel index, defines which
voxels exist (palette index 0 is a real color). Material indices hang off
it.
_Avoid_: Bitmask, presence bitmap

## Renderer input

**Snapshot**:
The unit of change the world hands the renderer: a Micro-chunk's global
coords, 64-byte Occupancy mask, and u8 material indices. Create, update,
and removal are the same message. An emptied Micro-chunk re-snapshots with
a zero mask.
_Avoid_: Edit message, delta

**Change queue**:
The renderer's inbound queue of Snapshots plus its dirty-region set; the
world enqueues, the renderer drains. Coalescing is last-wins per
Micro-chunk.
_Avoid_: Event bus, message bus

**World load**:
The unit of world supply: one .vox source, clipped to the lattice, its
Micro-chunks queued as Snapshots and its Palette loaded with them. A clear
empties the loaded world; a load after a clear replaces it. The pipeline
outlives loads.
_Avoid_: world streaming (the later incremental form), level (a game-side
concept)

**Resident region**:
A Region holding at least one non-empty Micro-chunk: it owns a BLAS, a
voxel pool, and a TLAS instance. It becomes resident on its first non-empty
Micro-chunk and leaves residency when the last one empties.
_Avoid_: Active region, loaded region

**Dirty region**:
A Resident region whose content changed since the last rebuild, queued for
a rebuild.
_Avoid_: Changed region

## Ray tracing

**Ray tracing pipeline**:
The full pipeline-based hardware ray tracing mechanism (ray generation, miss,
closest-hit, intersection shaders, shader binding table) via vulkano.
atlas-rt's only acceleration mechanism.
_Avoid_: Ray query (below)

**Ray query**:
The inline hardware ray-intersection mechanism (wgpu's ray queries / Vulkan
ray-query) used without a dedicated pipeline. Not used in atlas-rt; reference
term only.
_Avoid_: Ray tracing pipeline

**DDA**:
The renderer's voxel-resolution algorithm: a ray marches cell-by-cell through
the 8x8x8 Micro-chunk lattice (Amanatides-Woo), rejecting empty cells against
the Occupancy mask and committing the first occupied one.
_Avoid_: voxel ray march, ray walk

**t pre-pass**:
A candidate primary-visibility optimization under evaluation: a coarse
(lower-resolution) ray pass records each tile's nearest-hit t, which the
full-resolution pass then uses to skip nearer empty space. Named by its
mechanism, a lower-res t pass, not an effect.
_Avoid_: beam (classic beam tracing is secondary-ray cone tracing, out of
scope), depth pre-pass (implies a raster depth buffer this renderer lacks)

**Procedural sky**:
The analytic Background: a piecewise-linear radiance gradient in
μ = cos(elevation), knots at ground/horizon/zenith (all positive),
evaluated by the miss shader. No assets, no Sun disk.
_Avoid_: skybox, environment map (a sampled asset; the Procedural sky is
analytic), sky

**Background**:
The radiance produced where no geometry is hit (the miss shader's output):
the Procedural sky. Rays that leave the loaded world hit nothing and report
the Background color. The ray pass's t-range equals the camera's near/far,
so Background also appears beyond the far plane.
_Avoid_: empty space ("empty" is a property of the sparse world,
not a place)

**Void**:
The space outside the loaded world; rays there hit nothing and report the
Background color.
_Avoid_: Sky, empty space

## Display path

**Composite**:
The color pipeline that exposes the ray pass's radiance for display: the ACES
curve at a fixed identity exposure, gamma, and a one-LSB display dither. It is
the host's, not the engine's: the ray pass stores raw linear radiance and a
host canvas shader over the Delivery image applies the curve. The shader gates
it to Voxel; debug Render modes paint directly and bypass it.
_Avoid_: post-processing (beyond exposure/tonemap, out of scope), final pass,
eye adaptation (the exposure is a constant, not a meter), color grade

## Frame lifecycle

**Frame images**:
The renderer's extent-bound image set. Embedded, these are the Delivery
images; standalone there are none, and the ray pass stores into the
swapchain's bindless storage views instead.
_Avoid_: render targets, trace-pass images, G-buffer

**Delivery image**:
The renderer's exported output in the embedded path: the ray pass's color
Frame images themselves, three of them (a ring), each carrying
Win32-exportable memory; the host imports each as a shadow image and samples
it. Standalone has none (the swapchain plays the role).
_Avoid_: shared image, handoff texture, export image

**Delivery backend**:
How a published Delivery image reaches the host in the embedded path:
zero-copy, where the frame image's memory is exported and the host device
maps it through a shadow image, or CPU delivery. Picked once per session
by the Init probe; one set of Frame images serves both (ADR 0005).
_Avoid_: transport mode, delivery path

**Init probe**:
The one-time capability test at extension startup that picks the Delivery
backend for the session: Vulkan RD backend, VK_KHR_external_memory_win32
among Godot's enabled extensions, export, import, and shadow-image
creation all succeeding. Capability is the whole engine-build check;
engine build identity is only a provenance log line.
_Avoid_: handshake, capability check, startup probe

**CPU delivery**:
The fallback Delivery backend: the finished Delivery image is read back to
a host-visible buffer and uploaded as an ImageTexture. Correct on any
engine; about two 66 MB PCIe crossings per frame at 4K. Entered on a
failed Init probe or a runtime failure at a frame boundary.
_Avoid_: fallback mode, software rendering

**Publish**:
The worker marking a Delivery slot finished: the frame's submission completed
under the bounded fence wait, covering the exit transition (zero-copy) or the
readback copy (CPU fallback). The coordinator wraps only published slots.
_Avoid_: commit, flush, signal

**Wrap**:
The coordinator adopting the newest published Delivery slot as the sampled
texture at frame_post_draw. Pause freezes by finding nothing new to wrap; the
canvas keeps sampling the previous Wrap.
_Avoid_: swap, flip, handoff

**Rewrite gate**:
The rule that a Delivery slot is rewritten only three coordinator ticks after
its Wrap. Godot's own frame-slot stall makes the count sound at
frame_queue_size 2; it stands in for a cross-device fence, which cannot exist
through Godot's public RD API.
_Avoid_: pacing gate, fence gate

**Frame input**:
What the app reports to the renderer each frame: the player's view
(transform and fov), the render extent, and a render-mode request. The
renderer derives the projection from the view's fov and the extent.
_Avoid_: camera update, render parameters

## Render mode

**Render mode**:
What the ray pass paints each pixel with: surface identity (`Voxel`, `Hull`) or
a diagnostic quantity (the `Normal` heatmap).
`Voxel` (default): the
DDA commits the surface voxel, painted with its Palette entry.
`Hull`: each
Micro-chunk's trimmed AABB is the surface, colored by a coordinate hash, with
no DDA. The diagnostic modes are debug-build-only.
_Avoid_: shading mode, visualization mode

**Normal (Render mode)**:
A diagnostic Render mode (debug builds): each pixel is colored by its hit's
geometric Normal, -1..1 mapped to 0..1 per channel, voxel faces paint by
their axis (x red, y green, z blue; + side bright, - side dark), background
gray. Traces the DDA hit group like Voxel; the normal rides the payload.
_Avoid_: normal map visualization (a texture-space concept)
