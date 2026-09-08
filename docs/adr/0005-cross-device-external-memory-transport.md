# Godot transport contract: cross-device external memory through a VulkanHooks module

For the Windows v1 Godot-embedded path, atlas-rt keeps its own VkInstance,
VkDevice, and all queues; the renderer's device and queue setup is identical
standalone and embedded. The frame image is allocated on atlas-rt's device
with Win32-exportable memory, exported as an opaque Win32 handle (the one
vulkano gap, closed by a ~10-line raw call through the loaded
khr_external_memory_win32 fn group), imported into Godot's device, bound to a
shadow RawImage created there, and wrapped by
RenderingDevice.texture_create_from_extension. The extension adopts Godot's
device with vulkano only to run that import and shadow-image creation. A
VulkanHooks engine module in a custom 4.7.2 build appends
VK_KHR_external_memory_win32, VK_KHR_external_semaphore_win32, and
VK_KHR_external_fence_win32 to Godot's device creation and mutates nothing
else. The same transport runs hook-free on stock Linux with FD handles:
vulkano exports FDs natively and stock Linux Godot enables
VK_KHR_EXTERNAL_MEMORY_FD, so zero-copy on stock engines exists there today,
and Windows stays hook-gated.

## Status

accepted (godot-integration ticket 04, 2026-09-07). Same-device adoption is
unbuilt, a watch item gated on the upstream spare-queue change; the CPU-copy
path remains the fallback behind the same output interface.

## Considered Options

- **Same-device adoption: vulkano adopts Godot's VkDevice and owns spare
  queues at index >= 1.** Rejected: it rewrites the renderer's device path
  (create_device's min-flags family selection and one-queue-per-family
  create-infos do not map onto Godot's families); the dedicated compute
  family can be fully claimed by Godot, which claims index 0 of every
  eligible family and where the hook's bump is capped at the family's
  queueCount, so the acceleration-structure compute queue lands on
  graphics-family spares or vanishes; the adoption create-infos must mirror
  how Godot assembles its device, which drifts across engine versions; and
  Godot's RD state tracker cannot see atlas-rt's writes from adopted queues,
  so the layout-handoff convention is needed here too.
- **CPU copy as the primary path.** Rejected: readback plus ImageTexture
  upload costs ~2 GB/s one-way host traffic at 4K60, ~4 GB/s counting
  readback and upload; kept as the fallback behind the same output
  interface, never the performance path.
- **DLL injection or waiting for upstream.** Ruled out at charter: the
  VulkanHooks seam is compile-in and unreachable from GDExtension (research
  finding 11); the custom build plus module is the accepted cost.

## Consequences

- Two devices live side by side. A renderer failure's blast radius stays
  inside the extension; Godot's device state is untouched by atlas-rt and
  vice versa.
- The hook's only mutation is appending three extension strings; the
  create-info passes through otherwise (ticket 02). Distribution: players
  receive the exported game on the custom 4.7.2 template binary; extension
  artifacts and api.json stay vanilla. Logistics resolved by map ticket 08:
  the game preset references the custom template (a missing one is a hard
  export error), the build is local and CI-free, and v1 shipping is
  dev-box only, with public release gated on upstream landing the
  device-extension setting.
- Sync at v1 is a CPU-fence bridge at the frame boundary; Godot's RD has no
  external-wait API under any transport. The hook-enabled external semaphore
  and fence extensions keep the GPU-side bridge open for later (ticket 07).
- The frame image's memory carries Win32 external export; the layout
  convention between atlas-rt's frame image and Godot's shadow image, which
  share one allocation as two image objects, is set by the output-contract
  ADR (ticket 05), which supersedes ADR 0002 for the embedded path and fixes
  0002's dangling 0007 pointer.
- CPU fallback triggers: the extension probes once at init (Vulkan RD
  backend; the three extensions present in Godot's enabled extensions;
  export, import, and shadow-image creation succeed) and picks the delivery
  backend for the session. Runtime transport failure (device lost,
  allocation or import error) degrades to CPU at a frame boundary, logged
  once, never mid-frame. A project setting (atlas_rt/transport = auto |
  zero_copy | cpu) forces CPU for overhead measurement and fallback
  debugging. A forced zero_copy that misses the probe fails loudly (status
  failed plus an error log); forced selections never degrade silently
  (ticket 08).
