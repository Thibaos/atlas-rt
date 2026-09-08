# Output contract: Delivery images embedded, Composite unchanged standalone

Standalone is unchanged from the implemented behavior (correcting ADR 0002's
stale text): the ray pass writes the linear HDR color image
(R16G16B16A16_SFLOAT, bindless storage write); in Voxel mode the Composite
node reads it and writes the presentable swapchain image (ACES fit at a fixed
identity exposure, gamma 2.2, one-LSB display dither); the debug Render modes
Hull and Normal write the swapchain directly and bypass Composite.

Embedded (ADR 0005's transport), the Delivery set is the ray pass's color
output itself: three images (a 3-slot ring, widened from two by ADR 0007),
each allocated on atlas-rt's device with Win32-exportable memory and
exported through 0005's shim. The ray
pass writes the frame's slot in every Render mode (debug modes paint it raw,
as they paint the swapchain standalone); the embedded taskgraph has no
Composite node. The color pipeline lives engine-side: the extension's
full-rect canvas shader implements Composite (ACES, identity exposure, gamma
2.2, dither at the 8-bit canvas store) sampling the zero-copy Texture2DRD or
the CPU-fallback ImageTexture (readback of the same 16F image, uploaded as
FORMAT_RGBAH), so both delivery backends produce one identical picture. The
shader's mode uniform passes the debug paints through unprocessed; the
extension owns the render-mode request and the uniform, so the two cannot
drift. The look is fixed in v1: the shader takes uniforms carrying today's
constants, and no gameplay-facing knobs are exposed (a small frame-loop API
addition if ever asked).

Shadow image: created on Godot's device with parameters identical to the
Delivery image (format, extent, samples, tiling, mip and layer counts, and
usage: STORAGE | SAMPLED | TRANSFER_SRC on both) so the aliasing rules make
the two image objects one layout range and the memory requirements provably
match the one shared allocation; same-format view, no MUTABLE_FORMAT_BIT.
Godot validates nothing itself: texture_create_from_extension copies the
caller's type, format, samples, dims, and usage unchecked and wraps only a
view, so the contract carries the constraints. Exported memory's initial
layout is UNDEFINED.

Layout convention (the one 0005 deferred): per cycle on the zero-copy path,
atlas-rt enters with an UNDEFINED → General barrier on its image (contents
discarded; every pixel is rewritten), writes in General, and exits with a
General → SHADER_READ_ONLY_OPTIMAL barrier before the release signal. Godot's
tracker transitions the wrapped image once on first sample (UNDEFINED →
SHADER_READ_ONLY_OPTIMAL) and persists the usage across frames, so its
first-use barrier lands on the already-OPTIMAL range as a no-op and steady
state has zero transitions. Storage writes require General and sampling
accepts SHADER_READ_ONLY_OPTIMAL, which is why the range cannot simply pin
General: Godot's public tracker has no knob to sample in General and always
transitions on first use. The CPU-fallback readback copies in General before
the exit transition (SHADER_READ_ONLY_OPTIMAL is an invalid copy source); the
CPU path skips the exit transition. Only one Vulkan instance touches the
allocation at a time; the cross-device ordering itself is the sync design's
work (CPU-fence bridge at v1 per 0005), and since no fence, semaphore, or
timeline crosses Godot's public RD API, the ring's rewrite gate is the tick
rule ADR 0007 proves.

Extent: the host viewport is the authority; on a resize the Delivery images
are destroyed and recreated with the Frame images, re-exporting, re-importing,
and re-wrapping (init-path code reused); zero-extent frames are skipped, as
standalone.

## Status

accepted (godot-integration ticket 05, 2026-09-07; amended by ticket 07 the
same day). Supersedes ADR 0002 and corrects its dangling 0007 pointer
(ADR 0007 now exists, as the sync contract). Amended by ADR 0007: the
Delivery ring is three slots and the rewrite gate is 0007's tick rule.
Refines 0005's consequences with the ring count, the identical
shadow-image parameters, and the layout convention.

## Considered Options

- **Composite stays in atlas-rt; display-ready RGBA8 delivery.** Rejected:
  the color pipeline was wanted engine-side, where the game can adjust it
  without touching atlas-rt. Accepted costs: delivery bandwidth doubles
  (about 66 MB per frame per slot at 4K, 16F readback on the fallback) and
  the debug bypass moves into the shader as a mode uniform.
- **Stock Environment tonemap does the color work.** Rejected: sampled
  content reaches canvas draws with no engine-side color conversion, so an
  extension-side pass is needed regardless (encoding, dither placement at
  the 8-bit store, debug bypass); stock tools could at most layer on top.
  Verified: the tonemap pass is the last step of the 3D render and reads
  only the internal 3D buffer, so canvas pixels never enter it; the one
  route to canvas content is the BG_CANVAS Environment background, which is
  itself a reason to reject (see Consequences). See
  docs/research/godot-rd-external-texture-semantics.md finding 3 and
  docs/research/godot-color-pipeline-reach.md.
- **Single Delivery image with a strict handshake.** Rejected: no fence,
  semaphore, or timeline crosses Godot's public RD API (submit() and sync()
  refuse on the main instance), so "safe to rewrite" is unobservable; the
  ring decouples the loops and the handshake only gates the flip.
- **Fixed-max allocation, sub-rect rendering.** Rejected: wastes VRAM and
  scales sampling UVs; the re-import dance on resize is init-path code at
  window-resize rate.
- **Separate copy node color → Delivery.** Rejected: an extra full-frame
  copy; the color images take the transport roles directly.

## Consequences

- Composite exists twice: the taskgraph node (standalone) and the canvas
  shader (embedded), same math. The ACES fit comes from one shared include
  where both shader dialects accept it; if the include systems do not
  overlap, the duplication is deliberate and this paragraph is its record.
- The Delivery image's barriers are managed explicitly around the task
  graph's auto-tracked accesses (entry discard, exit to
  SHADER_READ_ONLY_OPTIMAL on the zero-copy path); the graph's own tracker
  must not be relied on for the shared allocation's layout.
- HDR 2D stays off: with it on, draws convert to linear and the present
  blit re-encodes sRGB, a second pipeline on top of the shader's. The RT
  viewport's Environment must not use BG_CANVAS either: that background
  mode renders canvas layers into the 3D framebuffer and runs the full
  tonemap pass over them, re-tonemapping the Delivery image's shaded
  output. Both configs are documented in godot-color-pipeline-reach.md.
- The shader encodes into the sRGB-convention canvas target the SDR
  swapchain displays (B8G8R8A8 or R8G8B8A8 UNORM with SRGB_NONLINEAR); an
  _SRGB view would auto-decode at sample and display unencoded, so it is
  never used. 16F has no _SRGB variant, so the zero-copy and fallback
  textures sample identically in every mode.
- Debug Render modes on the CPU fallback read back the raw paint and pass
  through the same shader; identical treatment to zero-copy.
- The sync design inherits: two slots, no public completion fence, the exit
  transition as the release point, and a paced (not proven) rewrite gate
  until its soundness argument lands.
