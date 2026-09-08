# Godot 4.7.2 frame loop pacing: swapchain depth, vsync blocking, and RenderingDevice frames in flight

Facts for the v1 CPU-fence bridge and its paced rewrite gate (ADR 0005 follow-up): which
backpressure mechanisms in Godot 4.7.2 actually bound how long a canvas draw that samples the
extension's Texture2DRD can still be executing on Godot's GPU. The soundness question this
settles: can Godot's GPU be >= 2 frames behind its CPU, in execution terms, when the extension's
worker thread rewrites a ring slot two coordinator ticks after Godot last sampled that slot?

Sources and method. Tag verified as 4.7.2-stable (version.py fetched from
raw.githubusercontent.com reports major 4, minor 7, patch 2, status stable). No Godot checkout
existed in or near the workspace, so every Godot file cited below was fetched complete from
raw.githubusercontent.com/godotengine/godot/4.7.2-stable/<path> into the workspace scratch
directory .tmp-godot-472/ and read in full, so every line number is complete-file and no
web_fetch 100k-byte truncation scopes any claim here. Layout note: 4.7.2 has no
drivers/vulkan/vulkan_context.cpp and no DisplayServerVulkan; the swapchain acquire/present
machinery lives in the RenderingDevice driver
(drivers/vulkan/rendering_device_driver_vulkan.cpp) behind RenderingDevice, the display core is
servers/display/, and the RD core is servers/rendering/ (consistent with
godot-rd-external-texture-semantics.md). Vulkan claims cite the local spec checkout at
docs/vulkan/chapters/ (chapter prose; the two claims used here are wsi.adoc text, no generated
includes involved). GitHub blob URLs for cited lines take the form
https://github.com/godotengine/godot/blob/4.7.2-stable/<path>#L<first>.

## Findings

1. **The swapchain requests the same image count for every present mode; only the present mode
differs.** The setting is `display/window/vsync/vsync_mode` (GLOBAL_DEF_BASIC at main/main.cpp
L2839, default DisplayServerEnums::VSYNC_ENABLED; doc default "1" at
doc/classes/ProjectSettings.xml L1107; `--disable-vsync` forces VSYNC_DISABLED at main/main.cpp
L2840-2842). The int maps onto VSyncMode 0 Disabled, 1 Enabled, 2 Adaptive, 3 Mailbox
(servers/display/display_server_enums.h L266-269; the comment at L263 pins the enum to the
setting). At startup the value flows Main::setup to DisplayServer::create (main/main.cpp L3368)
into the DisplayServerWindows constructor (platform/windows/display_server_windows.cpp L8320)
through window_set_vsync_mode (L4981-4985) to RenderingContextDriver::window_set_vsync_mode
(servers/rendering/rendering_context_driver.cpp L62-67) and surface_set_vsync_mode, which stores
the mode on the Surface and flags needs_resize (drivers/vulkan/rendering_context_driver_vulkan.cpp
L970-974). At swapchain build the driver maps the mode to VkPresentModeKHR: Mailbox to
VK_PRESENT_MODE_MAILBOX_KHR, Adaptive to VK_PRESENT_MODE_FIFO_RELAXED_KHR, Enabled to
VK_PRESENT_MODE_FIFO_KHR, Disabled to VK_PRESENT_MODE_IMMEDIATE_KHR, falling back to FIFO with a
warning when the requested mode is unsupported
(drivers/vulkan/rendering_device_driver_vulkan.cpp L3743-3769; the same fallback is documented at
ProjectSettings.xml L1110). The image count is mode-independent:
minImageCount = MAX(desired, surface_capabilities.minImageCount), clamped to maxImageCount when
maxImageCount > 0 (driver L3772-3777, assigned at L3816), where desired is
RenderingDevice::_get_swap_chain_desired_count() = MAX(2, project setting
rendering/rendering_device/vsync/swapchain_image_count) (servers/rendering/rendering_device.cpp
L5356-5358), default 3 with allowed range 2-4 (core/config/project_settings.cpp L1824;
ProjectSettings.xml L3393). So with defaults Godot requests 3 images for FIFO, FIFO_RELAXED,
MAILBOX, and IMMEDIATE alike. The swapchain is built lazily: swap_chain_create makes an empty
holder (driver L3662-3669), and the real VkSwapchain is created in swap_chain_resize on the first
screen_prepare_for_drawing acquire that reports resize_required (rendering_device.cpp L5377-5421,
resize at L5404; driver acquire returns resize_required while vk_swapchain is null at L3994-3998).
A runtime vsync change flags needs_resize the same way, so the swapchain is rebuilt with the same
count formula (rendering_context_driver_vulkan.cpp L970-974; rendering_device.cpp L5400-5404).
Adaptive and Mailbox exist only under Forward+ and Mobile, not the GL Compatibility method
(ProjectSettings.xml L1112).

2. **With vsync on, the main thread blocks on acquire inside RS::draw; the cap on
submitted-but-not-complete GPU work comes from a per-frame fence wait in swap_buffers, not from
present.** Iteration order in Main::iteration (main/main.cpp L4921): main loop process (L5062),
RenderingServer::sync (L5076-5077), RenderingServer::draw(wants_present, scaled_step) (L5089,
L5093), OS::add_frame_delay (L5177-5178). draw() emits frame_pre_draw and runs _draw inline on
the calling thread unless the experimental threaded RS is enabled
(servers/rendering/rendering_server_default.cpp L443-453; main/main.cpp L3517-3521). Inside
_draw, draw_viewports renders each viewport's canvas (servers/rendering/renderer_viewport.cpp
L714, inside _draw_viewport L338, called at L902 and L929) and then blits window viewports to
their screens (L979-984); RendererCompositorRD::blit_render_targets_to_screen calls
RD.screen_prepare_for_drawing (servers/rendering/renderer_rd/renderer_compositor_rd.cpp L42-51),
which acquires the swapchain image ("After submitting work, acquire the swapchain image(s)",
servers/rendering/rendering_device.cpp L5380, acquire at L5399) through
swap_chain_acquire_framebuffer calling vkAcquireNextImageKHR with timeout UINT64_MAX
(driver L3988-4026), which per spec blocks until an image is acquired
(docs/vulkan/chapters/VK_KHR_swapchain/wsi.adoc L1101-1105). That acquire is the vsync blocking
point, and it sits inside RS::draw, during the blit phase, not inside present. Present is queued,
not waited on: _execute_frame submits the frame's command buffer(s) with
command_queue_execute_and_present (rendering_device.cpp L8187-8212; driver L3186, vkQueueSubmit at
L3263) and the present rides the same submission (rendering_device.cpp L8166-8169) as
vkQueuePresentKHR (driver L3319-3322), unless a separate present queue family exists
(rendering_device.cpp L8190, L8204-8208). The execution-latency cap is the frame-slot fence:
RenderingDevice keeps frame_count slots, frame_count = MAX(2, project setting
rendering/rendering_device/vsync/frame_queue_size), default 2, range 2-3 (rendering_device.cpp
L8366-8369, frames.resize at L8443; core/config/project_settings.cpp L1823). Each slot's fence is
signaled at its submission (L8199, fence_signaled at L8202) and waited when the slot is reused:
swap_buffers advances frame = (frame + 1) % frames.size() and calls _begin_frame (L7877-7881),
which stalls for that slot first (L8048-8051 into _stall_for_frame L8214-8220, driver fence_wait
to vkWaitForFences with VK_TRUE and UINT64_MAX at
drivers/vulkan/rendering_device_driver_vulkan.cpp L3050-3054). Inference, derived from the cited
code: with the default 2 slots, the CPU cannot start recording frame j+1 (inside draw(j)'s
swap_buffers) before frame j-1's GPU work is complete, so while the CPU is inside frame k+2's
_process at most frame k+1's draw work can be submitted-but-not-complete, and frame k's command
buffer is guaranteed complete. With frame_queue_size = 3 the cap during _process(k+2) is 2, frames
k and k+1 both possibly pending. Engine.max_fps and low-processor mode pace the CPU between
iterations (core/os/os.cpp L708-741, dynamic delay = MAX(low-processor sleep, 1000000/max_fps)
when max_fps > 0 and not in the editor; max_fps default 0 at core/config/engine.h L68) and so only
shrink the chance the GPU accumulates a backlog; they do not lower the frame bound. Low-processor
mode can also skip draw() entirely when nothing changed (main/main.cpp L5085-5091); a skipped draw
submits nothing but also runs no stall, so a previously submitted frame stays pending until the
next draw() that reuses its slot (inference). Present-queue depth adds display latency only: the
FIFO queue drains one present per vblank (docs/vulkan/chapters/VK_KHR_surface/wsi.adoc L2507-2515)
and acquire has a forward-progress rule against holding more than S - M images at once
(VK_KHR_swapchain/wsi.adoc L1125-1142); MAILBOX keeps a single-entry queue whose next request
replaces the prior entry and frees its image (VK_KHR_surface/wsi.adoc L2497-2506); IMMEDIATE
applies requests immediately with possible tearing (L2492-2496). Queued presents are not extra
unexecuted draw work, since their command buffers were already submitted and are covered by the
fence bound (inference).

3. **frame_post_draw is emitted after the frame's command submission and present are queued, and
the canvas draw sampling a Texture2DRD is submitted inside that same draw() call.** Chain:
Main::iteration calls RenderingServer::draw (main/main.cpp L5093), RenderingServerDefault::draw
emits frame_pre_draw then runs _draw (servers/rendering/rendering_server_default.cpp L443-453),
_draw runs draw_viewports (L109; canvas item rendering at renderer_viewport.cpp L714, swapchain
blit at L979-984), then RSG::rasterizer->end_frame(p_swap_buffers) (L115) is
RendererCompositorRD::end_frame calling RD.swap_buffers(p_present)
(servers/rendering/renderer_rd/renderer_compositor_rd.cpp L135-137), where _end_frame flattens the
command graph into the primary command buffer (rendering_device.cpp L8122-8123;
RenderingDeviceGraph::end at servers/rendering/rendering_device_graph.cpp L2583) and _execute_frame
submits it and queues the present (rendering_device.cpp L8187-8212), then the frame index advances
and the next slot begins (L7877-7881). Back in _draw, _run_post_draw_steps runs (L130-134) and
emits frame_post_draw (rendering_server_default.cpp L229). So the signal fires after the whole
frame's graph commands, including the canvas draws that sample the Texture2DRD and the final blit,
have been submitted to the GPU and the present queued. Under the threaded RS the emit is deferred
to the render thread (L130-131), a mode the engine marks experimental (main/main.cpp L3517-3521).

4. **The command graph defers execution to the end of the same frame; cross-frame pipelining is
bounded by per-slot fences that block the main thread on every draw(), independently of vsync.**
draw_graph.begin() clears per-frame graph state at the start of each slot (rendering_device.cpp
L8071-8073; rendering_device_graph.cpp L1757-1784); every RD draw, compute, and copy call during
the frame is recorded into the graph and nothing reaches the GPU until _end_frame's
draw_graph.end(...) produces the command buffer (rendering_device.cpp L8122-8123) and
_execute_frame submits it (L8187-8212). The graph carries no fences (no fence references in
servers/rendering/rendering_device_graph.h); fences live one per frame slot in
RenderingDevice::frames (servers/rendering/rendering_device.h L1813-1821, slots vector L1851).
The slot count is the frame_queue_size setting (rendering_device.cpp L8366-8369, default 2, range
2-3), and slot reuse forces a CPU fence wait every frame (L7877-7881 to L8048-8051 to L8214-8220,
vkWaitForFences at drivers/vulkan/rendering_device_driver_vulkan.cpp L3050-3054). So yes: the
bounded frames in flight themselves block the main thread, on every draw(), and the bound caps GPU
execution lag independently of vsync, because the stall sits on the slot-reuse path regardless of
present mode or present-queue depth. Transfer workers keep separate fences for staging work
(rendering_device.cpp L7202, L7295, L7409) and are not part of the canvas draw path.

## Consequences for the ring-slot rewrite question

Corrected 2026-09-07 (godot-integration ticket 07 session): the conclusion below was first
written with a two-tick margin, which was off by one handoff tick. Ticket 06's handoff wraps the
newest finished slot at frame_post_draw(m), so the canvas draw sampling that slot runs in Godot
frame m+1, one iteration after the wrap. The rewrite happens at worker frame m+R. That shift
changes the soundness rule from R >= 2 to R >= Q+1. The finding lines above are unaffected; only
this section's conclusions changed.

Model, from the verified mechanics. In iteration i the coordinator kicks worker frame i at
_process(i). draw(i) records the canvas sampling of the texture wrapped at post_draw(i-1),
submits Godot frame i, and then swap_buffers advances the slot and _begin_frame stalls on the
reused slot's fence, which is frame i+1-Q's (rendering_device.cpp L7877-7881, L8048-8051). So by
the time iteration i's draw returns, Godot frames up to i+1-Q are complete, and the guarantee for
the sampling frame m+1 lands at the end of draw(m+Q). The worker rewrites the sampled slot at its
frame m+R, kicked at _process(m+R), which runs after draw(m+R-1) and before draw(m+R). Sound
rewrite requires _process(m+R) to follow the end of draw(m+Q), that is R >= Q+1 (inference from
the cited lines; the worst case is the fast worker, where the slot wraps at post_draw(m) with m
equal to the producing tick).

Consequences:

- Ring 2 (ADR 0006 as written) at the default frame_queue_size 2 is unsound by one iteration: the
rewrite at _process(m+2) can precede the end-of-draw(m+2) stall that would guarantee the sampling
frame m+1. The window is realistic exactly when atlas-rt is fast, since the worker's rewrite then
completes early in iteration m+2. The worker's own fence does not help; it orders atlas-rt's
device only.
- Ring 3 at frame_queue_size 2 is sound with at least one full iteration of margin, at the cost
of one more 16F slot (about 66 MB at 4K) and one more export/import pair at init.
- frame_queue_size 3 shifts the guarantee to end-of-draw(m+3), so ring 3 races again; the
zero-copy path must pin frame_queue_size to 2. The setting is
rendering/rendering_device/vsync/frame_queue_size (rendering_device.cpp L8366-8369,
core/config/project_settings.cpp L1823, default 2, range 2-3); the extension probes it at init
and degrades to CPU delivery on 3.
- Vsync stays irrelevant: the binding stall sits on the slot-reuse path inside every draw(), and
queued presents are backed by already-submitted command buffers (inference). The 3-image
swapchain caps display lag only, and the bound holds the same with vsync off.
- Break conditions (inference): frame_queue_size 3 (would need R >= 4, out of range); draw()
being skipped while the worker keeps rewriting. The skip is excluded structurally while
producing: every wrap queue_redraws, RenderingServer::has_changed stays true, and the
low-processor branch at main/main.cpp L5087-5091 draws whenever anything changed; the no-kick
pause and hidden cases stop rewrites outright, so their skips are harmless. Threaded RS is a
third exclusion: it defers frame_post_draw to the render thread
(rendering_server_default.cpp L130-131), breaking the main-thread handoff contract.

## Sources note

Godot sources: no local checkout existed (glob for rendering_device_graph.cpp, main/main.cpp, and
version.py over the workspace and its parent found nothing usable), so the files were fetched
complete from tag 4.7.2-stable into .tmp-godot-472/ and read in full; the tag is verified through
version.py (major 4, minor 7, patch 2, status stable). The scratch directory is deletable; every
cited line number is per file and per tag. Vulkan sources: local checkout at docs/vulkan/chapters/
(chapter asciidoc); the acquire-timeout and present-mode claims come from wsi.adoc chapter prose,
so the missing generated includes scope no claim here. Items marked inference: the frame-count
pending-work derivation in finding 2 and the consequences section; the per-mode acquire-blocking
steady states; queued presents adding no unexecuted work; the low-processor skipped-draw pending
window; the break conditions.
