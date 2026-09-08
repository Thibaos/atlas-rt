# Upstream watch list and re-evaluation criteria - Godot integration

Charter posture: watch and gate. ADR 0005 (cross-device external memory, CPU-fence
bridge) is the standing transport; ADR 0007 records the GPU-side semaphore bridge as
upstream-gated. Everything below is fetched from the GitHub API and master's current
sources, second week of 2026-09. The question for each item is which of three gates it
opens, and whether it reopens the transport decision (ADR 0005) or only changes the
delivery path (how extensions and sync get enabled on a stock engine).

Gates keyed to the plan:

- **G1. Same-device adoption on stock engines.** vulkano adopts Godot's VkDevice with
  queue indices >= 1 per family (research finding 8; ADR 0005 was decided against it
  because stock Godot claims exactly one queue per family, so adoption has no spare
  queues).
- **G2. GPU-side cross-device sync through RenderingDevice.** Transport of a frame's
  synchronization object across the device boundary instead of the host-time CPU-fence
  bridge (ADR 0007's recorded, gated GPU-side path).
- **G3. Hook-free Windows builds.** Cross-device external memory on Windows without a
  custom build appending device extensions. Doubles as the public-distribution gate
  (ticket 08): v1 ships dev-box only; public release of the custom-built game waits on
  G3, since official templates then run the integration.

## PR [#114940](https://github.com/godotengine/godot/pull/114940) - additional device extensions as a project setting

State: **open**, milestone 4.x, last activity 2026-08-12. Author dsh0416 (godot-cef).
Adds `rendering/rendering_device/vulkan/additional_device_extensions` (PackedStringArray,
requested as optional extensions, so failure to support degrades silently) plus
`RenderingDevice.get_device_enabled_extensions()` for the extension to verify what
actually got enabled. Master's extension registration
[rendering_device_driver_vulkan.cpp L562-620](https://github.com/godotengine/godot/blob/master/drivers/vulkan/rendering_device_driver_vulkan.cpp)
still requests no external-memory or external-semaphore extension on any platform, so
not landed. Review pressure is mild: one review (AThousandShips, COMMENTED, 2026-01-14),
no maintainer approval or concern. Momentum is the concern the other way: four
independent real-hardware testimonials between April and August 2026 (D3D11 streaming,
live broadcast, hardware video decode, and an XREAL/GL external-semaphore case in the
2026-08-01 comment), plus a duplicate use case ([#122335](https://github.com/godotengine/godot/pull/122335)) closed in its favor.

- Landed signal: merge commit on `refs/heads/master`, then the setting plus
  `get_device_enabled_extensions` present in a tagged stable release we build the
  template from. The merge alone is not enough for distribution; check
  milestone/backport state until the tag.
- Gate: **G3**, straight off the PR's own mechanism (the hook appends the three win32
  external extensions; the project setting replaces exactly that hand-off). Also the
  distribution gate of ticket 08.
- Re-evaluation scope: **delivery only**. Once landed, the custom build's extension
  work moves from the hook into a project setting; the module's remaining job
  (allocation, import, wrapping on the extension side) is unchanged. ADR 0005 stays
  closed. If the setting also enables external semaphore/fence extensions (nothing in
  the current patch forbids the string list), the raw-Vulkan groundwork under G2
  becomes testable without waiting for #11567, see below.

## Upstream spare-queue change - `max_queue_count_per_family` in `_add_queue_create_info`

State: **no proposal, no PR exists.** Searches over godotengine/godot PRs for
`_add_queue_create_info`, queue-count-per-family, and spare-queue adoption all return
zero open items. Master still pins `const uint32_t max_queue_count_per_family = 1`
([rendering_device_driver_vulkan.cpp L1320-1336](https://github.com/godotengine/godot/blob/master/drivers/vulkan/rendering_device_driver_vulkan.cpp)),
created at index 0 in every graphics/compute/transfer family. This is the least active
watch item because nobody else wants it yet: every current interop project (godot-cef,
NoesisGodotNet, video-decode extensions) wants cross-device memory sharing, not queue
adoption, so the demand that would carry this change is us.

- Landed signal: a merged PR touching `_add_queue_create_info` in
  `drivers/vulkan/rendering_device_driver_vulkan.cpp` that bumps
  `max_queue_count_per_family` above 1 or, better for us, deliberately reserves
  indices >= 1 per family for external consumers. Watch PRs against that file, not the
  proposals repo; this will likely land as a byproduct of an unrelated need before
  anyone files it for interop.
- Gate: **G1**, alone on the list, and the only item that reopens ADR 0005. With spare
  indices available on stock engines, same-device adoption (vulkano adopts Godot's
  device; no external memory, no import shim) undercuts the cross-device transport for
  both Windows and Linux, and the win32 extension append becomes dead weight. Reopen
  the decision if it lands.
- Before that: not a delivery-path issue, an engine-support absence. A custom build
  could claim spare queues by patching the constant, but that is fork-shaped change
  with no benefit over the current transport, and ticket 08 keeps custom builds
  dev-box only.

## Proposal [#13969](https://github.com/godotengine/godot-proposals/issues/13969) - enable additional Vulkan extensions for external memory sharing

State: **open**, 9 comments, last upstream movement 2026-07-03. Filed by dsh0416
2026-01-07 with godot-cef as the motivating project; PR #114940 is its concrete
implementation (the PR body links the proposal). No milestone, no maintainer
endorsement.

- Landed signal: a maintainer-attached milestone on the issue, or maintainer approval
  on #114940 (the proposal's only vehicle). The PR landing closes the proposal by
  implementation, so one watch point covers both.
- Gate: **G3**, via the PR. Same re-evaluation scope as #114940 (delivery).
- Secondary signal: a maintainer redesign on the proposal, i.e. a Godot-side surface
  other than the string-array setting, such as a dedicated texture-sharing API on
  RenderingDevice. That would supersede our raw-Vulkan allocation-and-import block
  with a supported surface; still delivery, still not ADR 0005.

## Proposal [#15210](https://github.com/godotengine/godot-proposals/issues/15210) - enable `VK_KHR_external_memory` (+_win32/_fd) opportunistically for zero-copy sharing

State: **open**, 0 comments, filed 2026-07-17 by Huntk23 (NoesisGodotNet), last updated
2026-07-19. Asks for the same outcome as #13969 by the opposite mechanism: always-on
opportunistic enabling of the external-memory extensions. No discussion, no maintainer
reaction.

- Landed signal: proposal acceptance (milestone, maintainer response), or a PR doing
  always-on enabling in `_initialize_device_extensions`. Treat acceptance as direction
  evidence, not a transport signal; the concrete vehicles remain #114940 and master's
  extension list.
- Gate: **G3** (it would also make the path uniform across platforms rather than
  Linux-only by default). Delivery only; ADR 0005 untouched.

## Proposal [#11567](https://github.com/godotengine/godot-proposals/issues/11567) - explicit semaphores on RenderingDevice

State: **open and dormant**: filed 2025-01-15 by HighCWu, one comment, no movement
since 2025-01-17, no milestone. Searches for an implementing PR (semaphore_create,
semaphore_create_from_extension, explicit-semaphore RD support) return nothing open in
godotengine/godot. The proposal's own plan bundles external-semaphore enabling plus
`semaphore_create_from_extension` alongside `texture_create_from_extension`, i.e. the
same seam shape we use.

- Landed signal: a merged PR adding explicit-semaphore methods on RenderingDevice
  (watch `servers/rendering/rendering_device.h` for `semaphore_create` /
  `semaphore_create_from_extension` / a wait-signal surface taking an external
  handle). Per the 4.7 pattern (finding 5, PR #118377), a ClassDB-bound method is also
  GDExtension-reachable even when marked experimental.
- Gate: **G2**. Public RD has no external-wait surface, so v1 bridges sync with
  host-time CPU fences (ADR 0007); an external-semaphore API on RD is precisely what
  the GPU-side path waits on. Note the partial route: if #114940 lands first, enabling
  `VK_KHR_external_semaphore_fd`/`_win32` through the setting and doing signal/wait
  raw-Vulkan in the extension, keyed off `get_driver_resource()`'s queue handles, may
  deliver G2 before #11567 does. ADR 0007 stays the record either way until that is
  proven on hardware.
- Re-evaluation scope: GPU sync replaces the CPU-fence bridge (ticket 07's path, and
  at most an ADR 0007 amendment), not the transport. A related signal worth more: if
  explicit semaphores arrive framed for foreign-device consumers, as in HighCWu's
  local-rendering-device exchange plan, it matches ours directly.

## Proposal [#11142](https://github.com/godotengine/godot-proposals/issues/11142) - replacing Rendering components from GDExtension

State: **open and stale**: filed 2024-11-12, 8 comments, no activity since 2024-11-26,
no milestone. Core response remains cautious per the research verdict (Calinou: prefer
specific upscaling/frame-generation hooks over whole-renderer replacement).

- Landed signal: implementation, e.g. a PR introducing a `RenderingServerManager` or
  ClassDB-exposing any of the Renderer*mStorage / RendererSceneRender components
  (registration precedent: PR
  [godot#65427](https://github.com/godotengine/godot/pull/65427)'s physics-engine
  registration). Earlier ground signal: a maintainer comment attaching a milestone or
  reopening triage.
- Gate: **none of the three**. Out of scope per the map (fork-only per the research
  verdict; upstream #11142 is a watch-item, not this effort). What it would reopen is
  the deepest question, whether atlas-rt should live inside Godot's renderer framework
  at all rather than behind the interop seam, and it would also weaken the
  renderer-internals-churn input that closed the fork option in the verdict table.
  Re-evaluate the plan destination, ADRs 0002/0005/0006 in that order, only on actual
  code movement on the issue, not on discussion.

## VulkanHooks exposure to modules or extensions

State: **nothing exists.** The compile-in
[VulkanHooks](https://github.com/godotengine/godot/blob/master/drivers/vulkan/vulkan_hooks.h)
seam is merged since before the research run (finding 11) and remains an internal
singleton set only from its own subclass constructor (`vulkan_hooks.cpp` L35-39), not
ClassDB-exported. Master still routes instance creation
([rendering_context_driver_vulkan.cpp L895](https://github.com/godotengine/godot/blob/master/drivers/vulkan/rendering_context_driver_vulkan.cpp))
and device creation (rendering_device_driver_vulkan.cpp L1518-1520) through it, and the
only installer in the tree is OpenXR. Searches for PRs exposing VulkanHooks to modules
or extensions return no open items; the matches are OpenXR feature PRs (#96439,
#89880, #112888) that consume the seam internally.

- Landed signal: a merged PR that either registers the hook install through the module
  API (a module-visible way to install a `VulkanHooks` subclass via
  `VulkanHooks::set_singleton` before instance creation) or ClassDB-exposes the hook
  interface. Minimal prior signal: a maintainer discussion about engine-module vulkan
  hooks beyond OpenXR.
- Gate: an alternative route to **G3** (hook installed by GDExtension rather than our
  compiled-in module) and, via `set_direct_queue_family_and_index`, a strengthening of
  the same-device option, relevant to G1 only in combination with a queue-count
  change. For the current plan our module already owns the hook, so exposure only
  matters for the distribution story in ticket 08.
- Re-evaluation scope: delivery. The transport stays cross-device external memory.

## Watch mechanics

- Watch state, not discussion. For #114940 the only events that change the plan are a
  merge to master and a backport into a tagged release; comments and testimonials
  change neither. The exception worth breaking for: a maintainer rejection or redesign
  would move Windows zero-copy back to hook-only and freeze the ticket-08 distribution
  gate until another vehicle appears.
- The check that outranks PR state: master's `drivers/vulkan/rendering_device_driver_vulkan.cpp`.
  The device-extension list and `max_queue_count_per_family` in that file are ground
  truth for G1 and G3 and settle any PR-state ambiguity.
- No item is close to a gate. #114940 has energy but lacks review; the spare-queue
  change has neither proposal nor PR; #11567 is dormant for 19 months, #11142 for 21.
  Nothing in 2026-09 changed the plan's assumptions: custom build stays the delivery,
  and reopen risks are nil.