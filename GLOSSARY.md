# atlas-rt

A real-time voxel renderer using hardware ray tracing (Vulkan RT pipelines,
vulkano). Renders sparse voxel worlds loaded from .vox files.

## Language

**World**:
The scene loaded from a .vox file: a sparse set of occupied voxels, held as a
flat table of 4096 Region slots indexed by region id, plus a 256-color palette.
A Region stores its content per Micro-chunk as an Occupancy mask followed by the
Material indices of that Micro-chunk's occupied cells, which is the same shape
the Voxel pool uses. The single source of truth for voxel content: a load
constructs one, the Voxel edit path is the only later mutation, and the renderer
holds only packed regions built from its Snapshots.
_Avoid_: Scene, level, map

**Lattice**:
The bounded coordinate space the World lives in: half-open, ±2048 per axis.
A load clips to it, and outside it the player is blocked while a voxel-rule
destination counts as unavailable.
_Avoid_: bounds, world extent

**Voxel edit**:
A caller's unit of change: a position and a change, `Set` with a u8 material
index or `Clear`. A batch is validated first and all-or-nothing, applied to the
World in input order with last write wins, then compiled on the world side into
one Snapshot per touched Micro-chunk. The renderer never receives a Voxel edit.
_Avoid_: edit message, brush (a message is a Snapshot; a brush is a
cursor-driven generator of edits, out of scope and not a Generation)

**Palette**:
A 256-entry RGBA8 color table mapping Material indices to display colors,
supplied by a World load's .vox source or by a Generation's Vocabulary, and
kept sRGB-encoded end to end. The ray pass converts a hit's entry to linear for
the display path. The source RGBA alpha remains part of the Palette, and an
optional material alpha can reduce it during loading. GPU-side: a bindless
vec4[256] storage buffer.
_Avoid_: Color table, LUT

**Material index**:
The per-voxel u8 the voxel pool carries beside the Occupancy mask: the
Palette entry the voxel paints with. MagicaVoxel `MATL` IDs are one-based
metadata over these entries. The renderer consumes only `MATL._alpha` and folds
it into the effective Palette during loading. Other material properties are
out of scope.
_Avoid_: material property table, physical glass, material system

**Material alpha**:
An optional MagicaVoxel `MATL._alpha` value for a one-based material ID. It
multiplies the Palette alpha to produce effective Transparency. Missing or
invalid values fall back to the Palette alpha.
_Avoid_: opacity (inverted), transparency (the effective result), refraction

**Transparency**:
The effective compositing coverage after combining Palette alpha with optional
Material alpha. The nearest surface with `0 < coverage < 1` blends over the
first opaque surface or the Background behind it, one layer deep, so a
transparent surface further back never appears. Coverage 0 is fully
see-through and never intersects. No transparent surface casts a shadow,
whatever its coverage. Voxel mode only.
_Avoid_: opacity (inverted), glass (a material), alpha blending (the blend
equation, not the property)

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
The 8x8x8 unit the World stores by and the renderer traces, tightly wrapped to
occupied voxels (owner requirement; named by rendering-core ticket 03). One
AABB per non-empty micro-chunk. That AABB is the trimmed hull (tight occupied
bounds), not the full 8x8x8 cell box. The Occupancy mask, not a sentinel index,
decides which of its 512 cells exist.
_Avoid_: cell

**Region**:
The unit both the World stores by and the renderer traces: 32^3 micro-chunks
(256^3 voxels). The TLAS holds one instance per region and a region's structure
exists only while it holds >=1 non-empty Micro-chunk. In the World, one region
slot holds an index over its micro-chunks plus a blob of their Occupancy masks
and Materials, and a region that has never held a voxel costs nothing.
_Avoid_: Super-chunk, block

## Voxel storage

**Voxel pool**:
The renderer's GPU-side storage for voxel data, organized per Region: for
each non-empty Micro-chunk, one Occupancy mask plus the material indices of
the occupied voxels. Built by the renderer from the world's Micro-chunk
snapshots; the world never writes it. The World stores per-Micro-chunk content
in this same shape, so the two sides of the boundary need no translation step.
_Avoid_: Voxel buffer, voxel data store

**Occupancy mask**:
The 512-bit presence bitmap of a Micro-chunk: one bit per voxel, set iff
the voxel is occupied. The mask, not a sentinel index, defines which
voxels exist (palette index 0 is a real color). Material indices hang off
it.
_Avoid_: Bitmask, presence bitmap

**Micro-chunk entry**:
A Micro-chunk's stored form: its 64-byte Occupancy mask followed by the
material indices of its occupied cells in ascending cell order. The World stores
one per non-empty Micro-chunk and the Voxel pool holds the same shape, so a
Snapshot carries one unchanged.
_Avoid_: Chunk blob, chunk record

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

**Tracked coordinates**:
The caller-owned set of Micro-chunk origins the renderer holds content for
after the last submitted batch. A load, clear, or edit returns the set its
batch leaves behind. The set is not derivable from the World, because the
World runs ahead of the renderer between a submit and the frame that applies
the batch.
_Avoid_: occupied chunks, world chunks (those describe the World, which runs
ahead)

**World supply**:
The one way a World is obtained: a World load reading a .vox source or a
Generation making voxels from a Seed. It takes the source, the cell budget and a
Progress, and returns the World with its Palette, its Physical material table,
its Snapshots, its granular cells and its clipped count, or an error naming the
source. It is synchronous and touches no renderer, so the caller chooses the
thread.
_Avoid_: loader, world source (the bytes a load reads, not the supply)

**World load**:
A World supply from one .vox source, clipped to the lattice, its Micro-chunks
queued as Snapshots and its Palette loaded with them. A clear empties the
loaded world; a load after a clear replaces it. The pipeline outlives loads.
_Avoid_: world streaming (the later incremental form), level (a game-side
concept)

**World job**:
The one World supply or clear in flight, run off the main thread: a load's read
and parse or a Generation's voxel production, then the world build and snapshot
emission, whose output is plain data. A job is refused rather than queued while
another is in flight, and it completes when the renderer has taken the batch
carrying it, not when the background work finishes. The tracked coordinate set
stays on the main thread, which is what assembles the ordered batch.
_Avoid_: task, async load, request

**Status**:
What the host reads to drive the loading overlay and the load buttons: no
world and no job in flight, a job in flight, a world resident, or the failure
of the last job with its error string. `Empty` is distinct from `Ready`
because the menu has to tell an unloaded view from a loaded one, and only the
buttons start jobs.
_Avoid_: state, phase, progress

**Resident region**:
A Region holding at least one non-empty Micro-chunk: it owns a BLAS, a
voxel pool, and a TLAS instance. It becomes resident on its first non-empty
Micro-chunk and leaves residency when the last one empties.
_Avoid_: Active region, loaded region

**Dirty region**:
A Resident region whose content changed since the last rebuild, queued for
a rebuild.
_Avoid_: Changed region

## World generation

**Generation**:
A World supply that makes voxels from a Seed instead of reading a .vox source:
one bounded World, produced in a single pass off the main thread, replacing a
resident World the way a World load does. It supplies its own Palette and
Physical material table.
_Avoid_: procedural load (a Generation is not a load), terrain (one feature of
a Generation, not the whole)

**Seed**:
The integer that fixes a generated World: one Seed gives one World, voxel for
voxel and Snapshot for Snapshot, on every machine and every build. A Seed's
features draw the noise they are built from out of it, so a Seed that reaches no
noise fixes nothing.
_Avoid_: random value, noise seed (the Seed fixes every feature, not one)

**Vocabulary**:
The fixed set of materials a generated World draws on: its Material indices
with their Palette colors, their Physical material rules, and the feature tags
that separate a Seed's features.
_Avoid_: material list, material system

**Height field**:
The ground surface of a generated World: one quantized level per column, from
which the fill's top and its material layering follow. It is coherent noise that
varies with the Seed and the column, so neighbouring columns are correlated
rather than independent draws of their own, and the level is the same on every
machine and every build.
_Avoid_: heightmap (a sampled asset), terrain (the whole content)

**Bedrock**:
The level a generated World's fill stops at: every filled column is solid from
here up to its Height field level, and nothing is generated below it.
_Avoid_: sea level, ground level (the Height field's own zero, not the floor)

**Generation params**:
What a Generation is asked for with: a Seed and an extent, the extent
defaulting to the full Lattice so a development run can generate a small World.
_Avoid_: footprint, world size, config

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

**Applied generation**:
How many batches the renderer has taken delivery of, carried on every
published slot. The frame that applies a pending batch reports one more than
the count read before it was submitted, so the host can tell a frame that
carries its batch from one built earlier, which is what a job completes on.
_Avoid_: content version (that is the delivery gate's), frame counter

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

## Physics

**Physical material table**:
The mapping from Material index to simulation behavior and player solidity. It
is separate from the Palette and may be replaced for a World load.
_Avoid_: physics palette, render material, physical material property table

**Physical material override**:
The optional replacement for a World's Physical material table, supplied
alongside the world source. Presence decides it: an absent override is the
normal case, one invalid record discards the whole file back to the built-in
table, and it never touches the Palette. It is invisible to the player, who
sees the fallback as the world loading normally.
_Avoid_: sidecar (generic), material mod, override file

**Falling granular**:
A voxel rule in which an occupied cell attempts to move into the cell below,
then one downward diagonal cell, when the destination is available. A cell with
no accepted move for a simulation tick is settled and blocks the player.
_Avoid_: sand entity, velocity voxel

**Activation**:
The handover of a World to the Simulation: the sim takes the new World, re-seeds,
places the player, resets timing, and replies readiness. The only reset the
Simulation has.
_Avoid_: world swap, respawn, restart

**Simulation tick**:
One fixed-rate advance of the voxel rules and player movement. Player movement
resolves first, then voxel rules. Input is sampled outside the tick and consumed
by the controller during the tick. A tick drains at most the cell cap of the
Update queue and leaves the rest queued.
_Avoid_: physics frame, render frame

**Update queue**:
The cells the voxel rules have not evaluated yet, held in the order a tick
drains them: ascending y, then x, then z. Settled means absent, so a Falling
granular cell blocks the player only while the queue holds it.
_Avoid_: dirty list, work list

**Cell cap**:
The most queued cells one Simulation tick drains, a fixed count in the
simulation beside the catch-up cap. The cells a tick does not reach stay queued
with their wakes.
_Avoid_: grain budget, work budget

**Input sample**:
The host-captured movement state and at most one pending jump edge supplied to
the simulation between Simulation ticks. A Simulation tick consumes the pending
edge at most once.
_Avoid_: raw input event, key state, input frame

**Pause**:
The host-held state that freezes Simulation time: accumulation stops, the
sub-tick remainder and the pending jump edge are dropped, and frames owe no
ticks until the host resumes. It survives Activation, which resets timing while
the pause holds.
_Avoid_: freeze, halt, suspend

**Player collider**:
The unrotated axis-aligned box used to move the player through the World. Its
position is the center of its feet, and its full height participates in floor,
ceiling, and step checks.
_Avoid_: hitbox, capsule, Godot body

**Player profile**:
The immutable set of physical dimensions and motion values that defines one
controller's behavior. It is independent of the World and remains selected for
the controller's lifetime.
_Avoid_: Player settings, character config

**Step height**:
The whole number of voxel heights the controller may rise during an automatic
grounded step. A step height of zero disables automatic stepping.
_Avoid_: Step size, jump height

**Grounded**:
The controller state that permits a jump or an automatic step: any blocking
cell under the Player collider footprint whose top is within contact
tolerance of the feet rests them, and a one cell gap under the footprint
counts as ground while a two cell gap drops.
_Avoid_: on floor, supported, landed (the arrival, not the state)

**Automatic step**:
The rise a Grounded controller makes over the obstacle it walks into, up to the
Step height, when the rise box above the head and the body box at the destination
are both clear. It needs horizontal contact to fire, and a Falling granular cell
that is settled counts as its surface.
_Avoid_: step-up assist, mantle, vault
