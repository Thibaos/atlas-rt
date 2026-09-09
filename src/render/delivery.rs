use anyhow::Context;
use std::mem::MaybeUninit;
use std::sync::Arc;
use vulkano::{
    VulkanError,
    ash::vk,
    device::DeviceOwned,
    format::Format,
    image::{Image, ImageCreateInfo, ImageType, ImageUsage, sys::RawImage},
    memory::{
        DedicatedAllocation, ExternalMemoryHandleType, ExternalMemoryHandleTypes,
        MemoryAllocateInfo, MemoryPropertyFlags, MemoryRequirements, ResourceMemory,
    },
    VulkanObject,
};

pub use vulkano::memory::DeviceMemory;

use vulkano_taskgraph::{Id, resource::Resources};

use crate::render::context::RenderContext;

pub const SLOT_COUNT: usize = 3;
pub const DELIVERY_FORMAT: Format = Format::R16G16B16A16_SFLOAT;

fn delivery_usage() -> ImageUsage {
    ImageUsage::STORAGE | ImageUsage::SAMPLED | ImageUsage::TRANSFER_SRC
}

pub struct DeliveryRing {
    physical: Vec<Id<Image>>,
    extent: [u32; 2],
}

pub fn bind_slot(frame: usize) -> usize {
    frame % SLOT_COUNT
}

fn slot_extent(extent: [u32; 2]) -> [u32; 3] {
    [extent[0], extent[1], 1]
}

fn delivery_create_info(extent: [u32; 2]) -> ImageCreateInfo<'static> {
    ImageCreateInfo {
        image_type: ImageType::Dim2d,
        format: DELIVERY_FORMAT,
        extent: slot_extent(extent),
        usage: delivery_usage(),
        external_memory_handle_types: export_handle_types(),
        ..ImageCreateInfo::default()
    }
}

fn exportable_memory_type_indices(
    gpu: &RenderContext,
    requirements: &MemoryRequirements,
) -> Vec<u32> {
    let types = &gpu.device.physical_device().memory_properties().memory_types;

    types
        .iter()
        .enumerate()
        .filter_map(|(index, memory_type)| {
            let index = u32::try_from(index).ok()?;

            (memory_type
                .property_flags
                .intersects(MemoryPropertyFlags::DEVICE_LOCAL)
                && requirements.memory_type_bits & 1_u32.checked_shl(index).unwrap_or(0) != 0)
                .then(|| index)
        })
        .collect()
}

fn export_handle_types() -> ExternalMemoryHandleTypes {
    if cfg!(windows) {
        ExternalMemoryHandleTypes::OPAQUE_WIN32
    } else if cfg!(unix) {
        ExternalMemoryHandleTypes::OPAQUE_FD
    } else {
        ExternalMemoryHandleTypes::empty()
    }
}

fn allocate_slot(gpu: &RenderContext, extent: [u32; 2]) -> anyhow::Result<Arc<Image>> {
    let raw = RawImage::try_new(&gpu.device, &delivery_create_info(extent))?;
    let requirements = raw
        .memory_requirements()
        .iter()
        .copied()
        .next()
        .context("delivery image reports no memory requirements")?;

    let mut memory = None;

    for index in exportable_memory_type_indices(gpu, &requirements) {
        let allocate_info = MemoryAllocateInfo {
            allocation_size: requirements.layout.size(),
            memory_type_index: index,
            dedicated_allocation: Some(DedicatedAllocation::Image(&raw)),
            export_handle_types: export_handle_types(),
            ..MemoryAllocateInfo::default()
        };

        match DeviceMemory::try_allocate(&gpu.device, &allocate_info) {
            Ok(allocated) => {
                memory = Some(allocated);

                break;
            }
            Err(_) => continue,
        }
    }

    let memory =
        memory.context("no device-local memory type accepted the dedicated delivery allocation")?;

    let image = raw
        .try_bind_memory([ResourceMemory::new_dedicated(memory)])
        .map_err(|(err, _raw, _allocations)| anyhow::anyhow!("{:?}", err))?;

    Ok(Arc::new(image))
}

#[cfg(windows)]
pub fn export_win32_handle(memory: &DeviceMemory) -> anyhow::Result<vk::HANDLE> {
    let info_vk = vk::MemoryGetWin32HandleInfoKHR::default()
        .memory(memory.handle())
        .handle_type(ExternalMemoryHandleType::OpaqueWin32.into());

    let fns = memory.device().fns();
    let mut output = MaybeUninit::uninit();

    unsafe {
        (fns.khr_external_memory_win32.get_memory_win32_handle_khr)(
            memory.device().handle(),
            &info_vk,
            output.as_mut_ptr(),
        )
    }
    .result()
    .map_err(VulkanError::from)?;

    Ok(unsafe { output.assume_init() })
}

#[cfg(not(windows))]
pub fn export_win32_handle(_memory: &DeviceMemory) -> anyhow::Result<std::ffi::c_void> {
    anyhow::bail!("win32 export requires windows")
}

pub fn delivery_memory(resources: &Resources, physical_id: Id<Image>) -> anyhow::Result<Arc<DeviceMemory>> {
    let image = resources.image(physical_id).image().clone();

    match image.memory() {
        vulkano::image::ImageMemory::Normal(memories) => memories
            .iter()
            .next()
            .map(|memory| memory.device_memory().clone())
            .context("delivery image has no bound memory"),
        _ => anyhow::bail!("delivery image is not backed by bound memory"),
    }
}

impl DeliveryRing {
    pub fn new(gpu: &RenderContext, extent: [u32; 2]) -> anyhow::Result<Self> {
        let physical = (0..SLOT_COUNT)
            .map(|_| {
                let image = allocate_slot(gpu, extent)?;
                Ok(gpu.resources.add_image(image))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self { physical, extent })
    }

    pub fn recreate(&mut self, gpu: &RenderContext, extent: [u32; 2]) -> anyhow::Result<()> {
        if extent == self.extent {
            return Ok(());
        }

        let mut batch = gpu.resources.create_deferred_batch();

        for id in &self.physical {
            batch.destroy_image(*id);
        }

        batch.enqueue();

        *self = Self {
            physical: (0..SLOT_COUNT)
                .map(|_| {
                    let image = allocate_slot(gpu, extent)?;
                    Ok(gpu.resources.add_image(image))
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
            extent,
        };

        Ok(())
    }

    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }

    pub fn image(&self, resources: &Resources, slot: usize) -> anyhow::Result<Arc<Image>> {
        let id = self
            .physical
            .get(slot)
            .context("delivery slot out of range")?;

        Ok(resources.image(*id).image().clone())
    }

    pub fn physical_id(&self, slot: usize) -> anyhow::Result<Id<Image>> {
        self.physical
            .get(slot)
            .copied()
            .context("delivery slot out of range")
    }
}

#[cfg(test)]
mod tests {
    use super::{SLOT_COUNT, bind_slot};

    #[test]
    fn consecutive_frames_write_distinct_slots() {
        for frame in 0..9 {
            assert_ne!(bind_slot(frame), bind_slot(frame + 1));
        }
    }

    #[test]
    fn a_slot_is_untouched_for_two_frames_after_write() {
        for frame in 0..300 {
            let slot = bind_slot(frame);

            assert_ne!(slot, bind_slot(frame + 1));
            assert_ne!(slot, bind_slot(frame + 2));
            assert_eq!(slot, bind_slot(frame + SLOT_COUNT));
        }
    }
}