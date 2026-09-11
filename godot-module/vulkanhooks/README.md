# vulkanhooks engine module

A Godot engine module that installs a `VulkanHooks` implementation whose only
active behavior is appending `VK_KHR_external_memory_win32`,
`VK_KHR_external_semaphore_win32`, and `VK_KHR_external_fence_win32` to the
assembled `VkDeviceCreateInfo` at device creation. No queue-family count is
bumped; the transport decision keeps atlas-rt on its own device.

## Files

- `config.py`: restricts `can_build` to `platform == "windows"`.
- `SCsub`: clones `env_modules` and adds `*.cpp` to `env.modules_sources`.
- `register_types.{h,cpp}`: at `MODULE_INITIALIZATION_LEVEL_SERVERS`, one
  `memnew(ExternalMemoryHooks())`. The `VulkanHooks` base constructor is the
  only singleton installer (first construction wins); the object is never
  freed, because the destructor clears the singleton and rendering teardown
  still consults it.
- `external_memory_hooks.{h,cpp}`: the `VulkanHooks` subclass. The base class
  interface was verified line-by-line against `drivers/vulkan/vulkan_hooks.h`
  of the pinned tag.

## Hook behavior, verified against tag 4.7.2-stable

- Device creation: `RenderingDeviceDriverVulkan::_initialize_device()` assembles
  `VkDeviceCreateInfo` and, when `VulkanHooks::get_singleton()` is non-null,
  calls `create_vulkan_device()` instead of `vkCreateDevice`. The hook copies
  the driver-create-info fields, appends the three extension names, and calls
  `vkCreateDevice` itself. The stock `pQueuePriorities` arrays are
  driver-owned static storage and are left untouched.
- Physical device: with a hook active, `RenderingContextDriverVulkan
  ::_initialize_devices()` requires `get_physical_device()` to return a valid
  `VkPhysicalDevice`; there is no fallback to full enumeration. The hook
  enumerates devices itself, prefers one exposing all three extensions, and
  falls back to the first device otherwise.
- Instance creation: with a hook active, `_create_vulkan_instance()` defers to
  the hook. The hook calls `vkCreateInstance` with the passed create info and
  stores the resulting instance for the device query. No instance-level
  external-memory extensions are needed: the external capabilities API is core
  in Vulkan 1.1 and Godot targets `VK_API_VERSION_1_2`.
- `set_direct_queue_family_and_index`, `use_fragment_density_offsets`,
  `get_fragment_density_offsets`, and `use_subsampled_images` are no-ops / `false`.

## Build recipe (Windows)

Toolchain: Visual Studio 2022 with the MSVC v143 x86_64 build tools component
and Windows 11 SDK 10.0.22621.0 or newer; Python 3.9+; SCons 4.4+
(`python -m pip install scons`). D3D12 dependencies are irrelevant to this
module; pass `d3d12=no` if their setup is a problem.

1. Clone Godot at the pinned tag, commit `ed1daf0b` (the `4.7` maintenance
   branch is three commits ahead and its version string is 4.7.3-rc; do not
   build it for this deliverable):

       git clone https://github.com/godotengine/godot.git godot
       cd godot
       git checkout 4.7.2-stable   # resolves to ed1daf0b

2. Install the module in-tree by copying this directory to
   `godot/modules/vulkanhooks/` (registration files must sit at the module top
   level). Out-of-tree use also works via `custom_modules=<path>`, but in-tree
   is what the recipe below assumes.

3. From cmd or PowerShell (not an MSYS2/MinGW shell), inside `godot/`:

       scons platform=windows target=template_release arch=x86_64

   SCons auto-detects the Visual Studio install. To edit in a generated
   solution instead, add `vsproj=yes`.

4. Expected artifact:

       bin/godot.windows.template_release.x86_64.exe

## Rebuild policy

Rebuild iff the pinned tag (`4.7.2-stable`, commit `ed1daf0b`) or the module
source changed. Otherwise reuse `bin/godot.windows.template_release.x86_64.exe`.

## Verification status

Verified by direct fetch at the pinned tag: `drivers/vulkan/vulkan_hooks.h`,
`drivers/vulkan/vulkan_hooks.cpp`, the hook call sites in
`drivers/vulkan/rendering_context_driver_vulkan.cpp`
(`_initialize_devices`, `_create_vulkan_instance`), and
`drivers/vulkan/rendering_device_driver_vulkan.cpp` (device-create branch).
The module has not been compiled: the Godot engine checkout does not exist in
this repo, so the first build above is the compile check.
