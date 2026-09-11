# Godot integration plan: atlas-rt embedded

This document assembles the locked decisions of the wayfinder map in
.scratch/godot-integration (tickets 01-10, 12; assembled by ticket 11).
The detail lives in ADRs 0005-0007 and the closed tickets; here it is
written so implementation can start with no open decisions. Evidence
base: docs/research/godot-renderer-integration.md findings 1-11, not
re-litigated. Terminology follows CONTEXT.md (Delivery, Publish, Wrap,
backend, Init probe).

## Goal and acceptance

Embed atlas-rt as the full-viewport renderer of a Godot 4.7.2 game on
Windows: zero-copy delivery at 4K60 through a custom engine build
(VulkanHooks module), runtime voxel edits through the Snapshot queue,
CPU delivery as the fallback behind the same output interface. The
renderer's own 4K fps workstream (ray-pass milliseconds) is a separate
effort; this plan owns the integration overhead only.

Acceptance (ticket 12): embedded holds 4K60 with >= 15% frame-time
headroom over the standalone baseline on the dev box, same world, vsync
on. The CPU backend is exempt from the 60 Hz acceptance; it is degraded
mode.

## Components

1. VulkanHooks engine module. A module in a custom Godot build from tag
   4.7.2-stable (ed1daf0b; not the 4.7 branch, which is 3 commits ahead
   at 4.7.3-rc). Installed at MODULE_INITIALIZATION_LEVEL_SERVERS, so a
   constructed hook subclass exists before instance and device creation
   (order verified against Main::setup2 and the OpenXR prior art in
   modules/openxr/extensions/platform). Its only mutation: appending
   VK_KHR_external_memory_win32, VK_KHR_external_semaphore_win32, and
   VK_KHR_external_fence_win32 to the assembled VkDeviceCreateInfo. No
   queue bump: the transport decision (ADR 0005) keeps atlas-rt on its
   own device. The external capabilities are core in Vulkan 1.1 and
   Godot targets 1.2, so no instance-side additions. v1 consumes only
   external_memory_win32; the semaphore/fence pair keeps the GPU-side
   upgrade (ADR 0007) reachable. Build: VS2022 + SCons,
   scons platform=windows target=template_release arch=x86_64, artifact
   bin/godot.windows.template_release.x86_64.exe.
2. GDExtension crate (godot-rust 0.5.5, default features; api-4-7 is
   opt-in and not needed here). Owns the coordinator, the worker, the
   atlas-rt pipeline, and every GPU call on atlas-rt's device. All six
   seam methods are bound at the 4.6 default API level:
   RD.texture_create_from_extension, RD.get_driver_resource,
   RS.get_rendering_device, RS.texture_rd_create,
   RS.texture_get_native_handle, and Texture2Drd (gdext's casing; no
   shim needed). It opens atlas-rt's internals via pub mod render; pub
   mod world; with item-level pub grants on the touched items. The
   display encode is not the extension's: the host applies it (see the
   color-pipeline bullet under Transport and fallback).
3. Atlas-rt headless mode. An embedded pipeline constructor with no
   window, no swapchain, no DotVoxData, no World; FrameInput gains
   extent, fov, and an explicit render-mode request (the standalone
   next-mode cycle bool goes away; Tab writes the same field). The
   world and its consumption are decoupled from pipeline creation
   (ADR 0003's world-streaming side stays out of scope).

Standalone behavior is unchanged by construction: the new fields have
standalone defaults, the Delivery ring replaces the swapchain only in
the embedded path, and the free-fly player controller stays
standalone-only.

## Transport and fallback (ADR 0005)

Cross-device external memory. atlas-rt keeps its own instance, device,
and all queues in both modes; the acceleration-structure compute family
is untouched. The Delivery image lives on atlas-rt's device with
Win32-exportable memory; vulkano's single export gap (DeviceMemory
exports FDs only at the pin, git master 70e3169) is closed by a ~10-line
raw call through the loaded khr_external_memory_win32 fn group. The
extension adopts Godot's device with vulkano
(Instance/Device::from_handle_borrowed; adoption validates nothing, the
create-infos declare what the hook actually enabled, and only
queue_family_index values are consumed) to run the Win32 import and the
shadow-image creation, then wraps via RD.texture_create_from_extension.

On stock Linux the same transport runs hook-free: vulkano exports FDs
natively and stock Linux Godot enables the FD external-memory path, so
Windows is the only hook-gated platform.

Init probe, once at extension init on the Vulkan RD backend: Godot's
enabled extensions include khr_external_memory_win32; vulkano export,
import, and shadow-image creation succeed. Any miss selects CPU
delivery for the session. Runtime transport failure degrades to CPU at
a frame boundary, logged once, never mid-frame. Two consecutive
bounded-wait timeouts on the zero-copy path degrade the session too
(sync section); two on the degraded readback path is a renderer failure
(status failed).

Project setting atlas_rt/transport = auto | zero_copy | cpu. Forced
zero_copy that misses the probe fails loudly (status failed, error
log), never a silent copy-path measurement. Forced cpu exists for
overhead measurement and fallback debugging.

## Output contract (ADR 0006; supersedes ADR 0002)

The Delivery set is the ray pass's own color output: three 16F images
(the third slot is the rewrite-gate amendment from ADR 0007, about
66 MB more at 4K) on atlas-rt's device, Win32-exportable, usage
STORAGE | SAMPLED | TRANSFER_SRC, imported as shadow images with
identical parameters on Godot's device and wrapped same-format.

- The ray pass writes the frame's slot in every Render mode; debug
  modes paint it raw and the host's composite shader gates its curve to
  Voxel through its mode uniform. Neither the embedded nor the
  standalone taskgraph has a Composite node: the engine stores raw
  linear radiance and the host encodes it.
- The color pipeline (ACES, identity exposure, gamma 2.2, dither) lives
  in the host's full-rect canvas shader, sampling a Texture2Drd
  (zero-copy) or a fallback ImageTexture (16F readback, FORMAT_RGBAH).
  Both backends produce one identical picture. Fixed look in v1:
  uniforms carry today's constants, no gameplay-facing knobs. Its
  `target_linear` uniform must match the project's 2D color space, and
  nothing on the engine side can detect a mismatch.
- Layout per cycle: entry UNDEFINED, transition to General, write in
  General, exit to SHADER_READ_ONLY_OPTIMAL before the release signal.
  Godot samples SHADER_READ_ONLY_OPTIMAL steady state; the CPU readback
  copies in General before the exit transition. The CPU path skips the
  exit transition.
- Extent follows the host viewport; recreate on resize re-exports and
  re-imports (the worker recreates the images between frames and
  re-exports; the coordinator imports and re-wraps at the next
  boundary, reusing the init path).

## Sync and pacing (ADR 0007)

No synchronization object crosses the device boundary in v1, in either
direction: Godot's public RD API has no external-wait surface (its
submit()/sync() refuse on the main instance). Order is host-time, built
from three ordinary waits.

- The worker waits its own submission fence after the ray pass. Bounded
  by the named constant below (constants section).
- Godot waits its own per-slot stall inside every RS::draw (its
  frames-in-flight machinery, independent of us).
- The rewrite gate replaces the missing fence: worker frame N writes
  slot N mod 3 and may not touch it before frame N+3.

Frame-boundary contract: a Delivery slot is safe to sample from the
moment the coordinator wraps it until the worker's frame N+3 begins;
the closing side is Godot's own machinery. At frame_queue_size 2 the
end-of-draw stall fences the sampling draw complete before the next
kick, with a full iteration of margin. Invariant: at any instant one
slot is being written, two hold the most recent finished frames and are
free to sample.

Pacing is Godot's: async latest-value, no present mode, no internal fps
cap, no lockstep. _process marshals FrameInput into a latest-value cell
and kicks without blocking; the worker produces at most one frame per
tick and never free-runs; frame_post_draw wraps the newest finished
slot, or nothing when nothing new finished. Godot's knobs apply: vsync
on, engine.max_fps, low-processor mode.

Preconditions, probed at init and pinned here: frame_queue_size = 2
(rendering/rendering_device/vsync/frame_queue_size; a value of 3 would
need a fourth slot, so zero-copy degrades to CPU delivery for the
session), threaded RenderingServer off (it defers frame_post_draw to
the render thread and breaks the main-thread handoff), low-processor
mode unsupported on the zero-copy path (structurally safe while
producing: every wrap queue_redraws; pause and hidden stop kicks
entirely). Input marshaled at _process(N) is visible at the end of
iteration N+1 when the worker keeps pace.

The GPU-side upgrade is recorded, not built: the exit transition would
be accompanied by an exported semaphore Godot's side waits on. The gate
is upstream (proposal #11567 plus a public external-wait or
texture-sharing surface, #13969/#15210); the watch list below tracks
it.

## Frame loop and threading

One Control-derived node, AtlasRtView, is the entire integration point:
coordinator and draw surface. It draws full-rect through its
ShaderMaterial; handoff swaps the sampler parameter, never the
material.

Main thread. _process marshals the Camera3D transform and fov, the
viewport extent, the render mode, and delta time into the latest-value
cell, then kicks the worker. RS.frame_post_draw is the handoff: wrap the
newest published slot (texture_create_from_extension,
RS.texture_rd_create, Texture2Drd RID rotation), queue_redraw. Every
Godot API call lives on the main thread; cross-thread Godot calls panic
under gdext.

Worker thread, one per coordinator tick. Drain edits, apply and rebuild
(ADR 0003 sequencing, no separate rebuild thread in v1), ray pass
writes the Delivery slot, exit transition, publish the finished slot.
Zero Godot calls from the worker.

Shared state: the FrameInput latest-value cell, the Change edit queue
(any-thread enqueue), a resize request flag, the load job
(worker-serialized), and the ring index.

Lifetimes. The pipeline (device, Delivery ring, region store) is
created once at extension init alongside the transport probe and lives
for the session; the world lives for the level, touched only by
clear_world and load_world. Pause or hidden means freeze: kicks stop,
the canvas keeps the last wrapped texture, the handoff finds nothing
new. Shutdown joins the worker after wait_until_idle.

Host API sketch (from ticket 06, unchanged):

    AtlasRtView (Control, full-rect, ShaderMaterial over the current
    Texture2Drd)

      status: AtlasRtStatus              # loading | ready | failed (read-only)
      status_error: String               # populated when failed
      status_backend: Delivery backend   # zero_copy | cpu (read)
      render_mode: AtlasRtRenderMode     # Voxel | Hull | Normal; debug values rejected in release

      set_camera(Camera3D)               # view + fov authority from the next tick
      set_view(origin: Vector3, basis: Basis)   # underneath; camera-less tooling
      load_world(path: String) -> bool   # worker job, returns immediately
      clear_world() -> bool
      submit_microchunk(coords: Vector3i, mask: PackedByteArray,
                        materials: PackedByteArray) -> bool
      submit_batch(edits: Array) -> bool # one Dictionary per edit:
                                         # coords, mask, materials

Per-frame flow: _process marshals and kicks; the worker runs one
atlas-rt frame (drain edits, apply and rebuild, ray pass, exit
transition, publish); frame_post_draw wraps the newest slot and rotates
the Texture2Drd; the host's composite canvas shader draws it, its mode
uniform gating the curve to Voxel and passing debug paints through.

Boundary rule. Every public edit and load entry validates at the
GDScript boundary (coords inside the lattice and multiples of 8; mask
exactly 64 bytes; materials length equals the mask popcount) and
returns a bool plus one push_error on failure. A Rust panic across the
gdext boundary kills the game, so public entries never inherit internal
panic semantics; internal asserts stay for internal misuse.

## World supply

- load_world(path) -> bool: FileAccess read (res:// and user:// survive
  exported pck builds), dot_vox::load_bytes (5.2.0), World::new_clipped
  (always clipped; the flag is removed and the clipped count lands in
  the warning log), emit_snapshots, submit_batch, palette upload (moved
  from construction to load; default_scene() stays construction-time),
  ready. Runs on the worker serialized with frames.
- clear_world() -> bool: a zero-mask snapshot per coordinate of the
  previous world's micro-chunk set; existing coalescing and residency
  rules empty the regions and return memory through the free lists.
- Level flow is clear_world() then load_world(); reload is legal
  (menus, levels). A store-level wholesale clear is the streaming-era
  replacement, named here, not built.
- Status: loading | ready | failed (+ error string, read-only). While
  loading, and until the first ready, the canvas draws a placeholder
  clear color; on failed the placeholder stays and one push_error
  fires. The game decides what to show; no retry in v1.

## Gameplay surface

- Input: zero bridge owed. The camera contract ends at set_camera /
  set_view; camera movement is entirely the game's side; the extension
  reads no input and ships no controller.
- UI: free frame. UI lives in a CanvasLayer with layer >= 1 above the
  view; anything below is encroachment the view may not see. The menu
  state is the freeze: the last wrapped frame behind the menu, dimmed
  by the menu's own drawing; no placeholder menu mode.
- Edits: thin passthrough. submit_microchunk / submit_batch enqueue
  straight into the Change queue on the calling thread, callable at any
  time from any thread including mid-frame; once queued, an edit cannot
  be lost or applied at the wrong time. Coalescing is the queue's
  (last-wins per Micro-chunk); rebuild rides the same frame per ADR
  0003 sequencing. The only throughput constraint is the per-tick
  dirty-region cap from the budget section. Level loads are
  load_world's job, never submit_batch.

## Distribution (ticket 08)

- The game preset's custom_template/release references the custom
  template (a res:// or absolute path; the value is used verbatim). A
  configured-but-missing template fails the export hard, by Godot's
  own hand; the Linux preset's field stays empty so stock-Linux keeps
  official templates. Replacing the official template file is rejected
  (machine-global, clobbered by reinstalls).
- Ship shape: the exported game is the only player-facing binary; v1
  distribution is dev-box only. Public release gates on upstream
  landing the device-extension setting (PR #114940), at which point
  official templates run the integration.
- No custom editor in v1. Dev loops the project from the stock editor
  binary, which runs CPU delivery; zero-copy is exercised through the
  export-run loop. A custom editor is a named follow-up if in-editor
  zero-copy iteration matters.
- No CI. The template is a local scons run, template_release only, tag
  4.7.2-stable. Rebuild iff the pinned tag or the module source
  changed; otherwise reuse the binary.
- Version handshake is probe-only: the init probe's capability checks
  are the whole engine-build gate. Engine.get_version_info() supplies
  provenance for the init log line (custom_build versus official, plus
  the build-time git hash). Identity is logged, never gated.
- Degraded UX is silent-but-instrumented: CPU delivery with no visible
  warning anywhere, one init log line recording the backend, the miss
  reason, and the engine provenance, and a status backend read so the
  game can adapt quality. Rough reference number for CPU delivery at
  4K: two ~66 MB PCIe crossings plus a host memcpy, roughly 10-15 ms
  added per frame, likely near 30 fps. Ticket 12's measurement
  replaces this estimate.

## 4K60 budget (ticket 12)

Integration overhead vs standalone is a measured delta: same dev box,
same 4K world, standalone exe vs embedded build, decomposed into the
named measurements below. The protocol and thresholds are fixed now;
numbers fill in at implementation. No pre-code estimate table.

Implementation must measure these to accept:

1. Standalone 4K fps baseline on the dev box, same world (the
   denominator of every delta).
2. Embedded end-to-end at 4K, vsync on, same world.
3. Wrap cost at frame_post_draw (texture_create_from_extension,
   RS.texture_rd_create, Texture2Drd rotation), measured once to
   confirm the microseconds assumption.
4. CPU-fallback round trip at 4K (readback plus copy-out, ~66 MB each
   way), measured to state its achievable fps; not accepted at 60.
5. Worst-tick edit scenario at the cap below.

Acceptance: embedded holds 4K60 with >= 15% frame-time headroom over
the standalone baseline. CPU delivery is exempt (degraded mode).

Edit-spike bounding. The worker is decoupled from the game frame, so a
rebuild spike cannot break Godot's frame; it only lowers the worker's
publish rate. The bound gameplay respects is a per-tick dirty-region
cap: at most N regions dirtied by submits per coordinator tick, N
derived once per device from the measured per-region BLAS-rebuild ms
such that a worst tick stays inside the standalone frame-time headroom.
Beyond the cap, edits queue and drain over later ticks, coalescing
intact. Edits stay any-time/any-thread; the cap is the only constraint.

Split of ownership. This plan owns the kick/submission cost, the wrap
cost, the three-slot ring effects, the CPU-fallback transport cost, and
the edit-spike cap. The renderer's own workstream keeps ray-pass ms,
the t pre-pass interplay, and the per-edit rebuild strategy beyond the cap.
The sub-line items fixed by ADR 0007 (tick kick, one submission, the
bounded own-fence wait, already signaled in steady state) are treated
as below measurement.

## Upstream watch and gates (ticket 09)

Full table with per-item signals: docs/research/upstream-watch-list.md.
Summary of the gates:

- PR #114940 (device extensions as a project setting): open, active, no
  maintainer approval, strong user momentum. Opens the
  hook-free-Windows gate and doubles as the public-distribution gate.
  Delivery-only; ADR 0005 stays closed either way.
- Upstream spare-queue change (queue indices >= 1 per family): nothing
  exists upstream, no proposal, no PR. The only item that would reopen
  ADR 0005 (same-device adoption on stock engines). Watch PRs against
  the Vulkan driver file, not the PR list.
- Proposals #13969 and #15210 (zero-copy texture sharing): open, no
  maintainer endorsement (#114940 is #13969's implementation vehicle).
  Delivery-only.
- Proposal #11567 (explicit semaphores): open, dormant since 2025-01.
  Opens the GPU-side sync gate at most (an ADR 0007 amendment replacing
  the CPU-fence bridge, never the transport). If #114940 lands first,
  raw-Vulkan external semaphores in the extension could deliver the
  same gate.
- Proposal #11142 (renderer component exposure): open, stale since
  2024-11, opens no gate here; would reopen the deepest question.
  Re-evaluate on code movement, not activity.
- VulkanHooks exposure upstream: nothing exists (still internal,
  OpenXR-only); an alternative delivery-only route to hook-free
  Windows.

Watch mechanic: master's rendering_device_driver_vulkan.cpp (extension
list plus queue-count constant) outranks PR state as ground truth.

## Research index

- docs/research/godot-renderer-integration.md (findings 1-11, base)
- docs/research/vulkano-external-memory-adoption.md (ticket 01)
- docs/research/vulkanhooks-module-mechanics.md (ticket 02)
- docs/research/gdext-seam-coverage.md (ticket 03)
- docs/research/godot-rd-external-texture-semantics.md (ticket 05)
- docs/research/godot-color-pipeline-reach.md (ticket 05)
- docs/research/godot-frame-loop-pacing.md (ticket 07)
- docs/research/upstream-watch-list.md (ticket 09)

ADRs: 0003 (renderer input contract, world sequencing), 0005
(transport), 0006 (offscreen output contract; supersedes 0002), 0007
(frame-boundary sync and pacing).
