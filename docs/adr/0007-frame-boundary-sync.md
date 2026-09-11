# Frame-boundary sync: host-time ordering and the three-slot Delivery ring

Embedded (ADR 0005's transport), no synchronization object crosses the device
boundary in v1, in either direction. Godot's public RenderingDevice API
exposes no external-wait surface: no fence, semaphore, or timeline crosses it
(submit() and sync() refuse on the main instance), so atlas-rt can export a
completion signal that nothing on Godot's side can wait on. Cross-device
order is carried in host time instead. The extension's worker waits its own
submission fence under a bounded wait (about four display frames; two
consecutive timeouts degrade the session to CPU delivery per ADR 0005's
fallback rule, and a timeout on the degraded readback path is a renderer
failure, status failed) and then publishes the slot as finished. The
coordinator wraps the newest published slot at frame_post_draw; the canvas
draw samples it in the next Godot frame.

Rewrites obey a tick rule that stands in for the missing fence: worker frame
N writes Delivery slot N mod 3 and may not touch that slot again before
frame N+3. Soundness rests on Godot's own frame-slot fences, not on vsync
and not on observation: with frame_queue_size = 2 (the default), the stall
at the end of Godot's draw(m+2) waits the fence covering draw(m+1), the draw
that sampled the slot, and that stall completes before _process(m+3)
submits the rewrite. The general rule is R >= Q+1 (ring slots R,
frame_queue_size Q); facts and derivation in
docs/research/godot-frame-loop-pacing.md (its soundness section was
corrected once, from a two-tick margin to R >= Q+1, when the handoff's
one-iteration sampling lag was accounted). At any instant one slot belongs
to the worker and two hold the most recent finished frames, which Godot is
free to sample.

The counting is sound only under pinned conditions, each carried into the
plan: frame_queue_size = 2, probed at extension init (a project setting of 3
would need a fourth slot, so zero-copy degrades to CPU delivery for the
session); threaded RS off (experimental; it defers frame_post_draw to the
render thread and breaks the main-thread handoff); low-processor mode
unsupported on the zero-copy path (structurally safe while producing, since
every wrap requests a redraw and the low-processor branch draws whenever
anything changed; the pause and hidden cases stop submissions per ticket 06, so
their skipped draws are harmless).

Pacing stays Godot's: the embedded path has no present mode (standalone keeps
PresentMode::Immediate unchanged), the extension adds no internal fps cap,
and the plan documents vsync on with engine.max_fps and low-processor mode as
the game's own knobs. The CPU-fallback backend needs no gate: one submission
carries the ray pass and the readback copy into a host-visible buffer, the
same bounded wait applies, the copy-out runs on the worker, and the
ImageTexture update is a main-thread Godot call needing no extra sync, since
Godot owns both ends; the fence covers slot reuse on that path.

## Status

accepted (godot-integration ticket 07, 2026-09-07). Amends ADR 0006 (Delivery
ring widened two -> three slots); ADR 0002's dangling 0007 pointer was
already corrected by ticket 05 to cite 0006.

## Considered Options

- **Two-slot ring on timing alone** (ADR 0006 as written). Rejected: with
  the wrap-at-post_draw handoff the rewrite can precede the sampling
  guarantee by one full iteration, and the window is widest when atlas-rt is
  fast, the acceptance target itself. Sampling an image mid-rewrite is
  undefined behavior (corruption or a driver fault), not a visible glitch.
- **Two-slot ring with post_draw-gated submissions.** Rejected: sound, but the
  worker starts after Godot's draw, so input-to-photon grows to about two
  game frames in the fast case, and frame_queue_size must still be pinned to
  2, so the change buys no independence from the engine setting.
- **GPU-side bridge (exported semaphore or timeline plus a Godot-side
  wait).** Unbuilt: no public RD wait surface exists
  (godot-rd-external-texture-semantics.md finding 5); gated on upstream
  #11567 (explicit semaphores) plus a public external-wait or
  texture-sharing surface (#13969, #15210). The hook-enabled external
  semaphore/fence extensions (ADR 0005) keep it reachable; the gate is
  recorded in the watch list (ticket 09).

## Consequences

- The Delivery ring is three 16F slots, amending 0006 ("two images (a 2-slot
  ring)"): about 66 MB more VRAM at 4K and one more export/import pair on
  the init/resize path, which is init-path code already.
- frame_queue_size is a zero-copy precondition, probed at extension init
  against rendering/rendering_device/vsync/frame_queue_size; a project
  setting of 3 degrades the session to CPU delivery.
- Latency: input marshaled at _process(N) is visible at the end of iteration
  N+1 when the worker keeps pace (one game frame plus the overlapped worker
  frame time); the async submission keeps the game's rate independent of the
  renderer's worst frame.
- Vsync, present-queue depth, and the 3-image swapchain play no part in
  soundness: the binding stall is Godot's per-slot fence on every draw, and
  queued presents are backed by already-submitted command buffers.
- The CPU fallback's delivery interface is unchanged (one output interface,
  two backends, ADR 0005); its slot reuse needs no gate.
- The exit transition remains the release point (0006); publish is
  fence-backed under the bounded wait. The 4K60 budget graduates to its own
  ticket (integration overhead vs the renderer's own frame time).
