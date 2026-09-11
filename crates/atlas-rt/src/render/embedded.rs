use std::sync::Arc;

use anyhow::Context;
use glam::{Mat4, camera::lh::proj::vulkan::perspective};
use vulkano::image::{Image, ImageCreateInfo, ImageLayout, ImageType, ImageUsage, view::ImageView};
use vulkano_taskgraph::{
    Id, QueueFamilyType,
    descriptor_set::StorageImageId,
    graph::{CompileInfo, ExecutableTaskGraph, ResourceMap, TaskGraph},
    resource::{AccessTypes, ImageLayoutType},
};

use vulkano::memory::DeviceMemory;
use vulkano::{Handle, VulkanObject};

use crate::render::{
    context::RenderContext,
    delivery::{DELIVERY_FORMAT, DeliveryRing, SLOT_COUNT, bind_slot, delivery_memory},
    pipeline::FrameInput,
    region::{
        feed::RendererInput,
        residency::{ApplyReport, RegionStore},
        task::{
            RegionRenderContext, RegionRenderTask, RenderMode, default_scene, production_raygen,
        },
    },
};

const PROJ_NEAR: f32 = 0.01;
const PROJ_FAR: f32 = 10000.0;

pub const REWRITE_GATE_TICKS: u64 = 3;

#[derive(Clone, Copy, Debug, Default)]
pub struct WrapTimes {
    pub wrapped_at: [Option<u64>; SLOT_COUNT],
    pub tick: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct PublishedSlot {
    pub slot: usize,
    pub extent: [u32; 2],
    pub version: u64,
}

pub struct EmbeddedPipeline {
    delivery: DeliveryRing,
    virtual_slots: Vec<Id<Image>>,
    storage_ids: Vec<StorageImageId>,
    task_graph: ExecutableTaskGraph<RegionRenderContext>,
    region: RegionRenderContext,
    store: RegionStore,
    input: RendererInput,
    content_version: u64,
    frame: usize,
}

/// Whether a store apply left the content a frame draws different: a region
/// entered, left, or took new Snapshots. The report's rebuild log and TLAS flags
/// describe the build, not the content, so they cannot answer this.
const fn content_changed(report: &ApplyReport) -> bool {
    !report.became_resident.is_empty() || !report.left_resident.is_empty() || !report.dirty.is_empty()
}
#[allow(clippy::as_conversions, clippy::cast_precision_loss)]
fn projection(input: &Mat4, fov: f32, extent: [u32; 2]) -> production_raygen::Camera {
    let [width, height] = extent;
    let aspect = match (width, height) {
        (0, _) | (_, 0) => 1.0,
        _ => width as f32 / height as f32,
    };

    let fov = if fov > 0.0 {
        fov
    } else {
        crate::render::pipeline::DEFAULT_FOV
    };

    let proj = perspective(fov, aspect, PROJ_NEAR, PROJ_FAR);

    production_raygen::Camera {
        proj_inverse: proj.inverse().to_cols_array_2d(),
        view_inverse: input.inverse().to_cols_array_2d(),
    }
}

/// # Errors
///
/// Returns an error if any delivery slot is empty, or image view creation failed
fn slot_storage_ids(
    gpu: &RenderContext,
    delivery: &DeliveryRing,
) -> anyhow::Result<Vec<StorageImageId>> {
    (0..SLOT_COUNT)
        .map(|slot| {
            let physical = delivery.physical_id(slot)?;
            let image = gpu.resources.image(physical).image().clone();
            let view = ImageView::new_default(&image)?;

            let bcx = gpu
                .resources
                .bindless_context()
                .context("bindless context not found")?;

            Ok::<StorageImageId, anyhow::Error>(
                bcx.global_set()
                    .add_storage_image(view, ImageLayout::General),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{REWRITE_GATE_TICKS, WrapTimes, content_changed, gated_slot};
    use crate::render::delivery::SLOT_COUNT;
    use crate::render::region::residency::ApplyReport;

    fn wrap_times(wrapped_at: [Option<u64>; SLOT_COUNT], tick: u64) -> WrapTimes {
        WrapTimes { wrapped_at, tick }
    }

    #[test]
    fn never_wrapped_slots_start_eligible() {
        assert_eq!(gated_slot(0, &wrap_times([None; SLOT_COUNT], 0)), Some(0));
    }

    #[test]
    fn the_ring_slot_waits_full_gate_even_fresh() {
        let wrap_times = wrap_times([Some(0), None, None], REWRITE_GATE_TICKS - 1);

        assert_eq!(gated_slot(0, &wrap_times), Some(1));
        assert_eq!(gated_slot(1, &wrap_times), Some(1));
    }

    #[test]
    fn the_gate_opens_on_the_third_tick() {
        let wrap_times = wrap_times([Some(0), None, None], REWRITE_GATE_TICKS);

        assert_eq!(gated_slot(0, &wrap_times), Some(0));
    }

    #[test]
    fn every_recently_wrapped_slot_skips_the_frame() {
        let wrap_times = wrap_times([Some(9), Some(9), Some(9)], 10);

        assert_eq!(gated_slot(1, &wrap_times), None);
    }

    #[test]
    fn an_empty_apply_report_leaves_the_content_alone() {
        assert!(!content_changed(&ApplyReport::default()));
    }

    #[test]
    fn a_region_that_entered_left_or_took_snapshots_counts_as_a_content_change() {
        let coords = glam::IVec3::new(0, 0, 0);
        let cases = [
            ApplyReport {
                became_resident: vec![coords],
                ..ApplyReport::default()
            },
            ApplyReport {
                left_resident: vec![coords],
                ..ApplyReport::default()
            },
            ApplyReport {
                dirty: vec![coords],
                ..ApplyReport::default()
            },
        ];

        for report in cases {
            assert!(content_changed(&report), "{report:?}");
        }
    }

    #[test]
    fn a_rebuild_without_a_region_moving_is_not_a_content_change() {
        let report = ApplyReport {
            tlas_rebuilt: true,
            instance_count_before: 3,
            instance_count: 3,
            ..ApplyReport::default()
        };

        assert!(
            !content_changed(&report),
            "a TLAS rebuild for content that did not move is not a new world to show"
        );
    }
}

fn gated_slot(bind: usize, wrap_times: &WrapTimes) -> Option<usize> {
    let eligible = |slot: usize| -> bool {
        wrap_times
            .wrapped_at
            .get(slot)
            .copied()
            .flatten()
            .is_none_or(|wrap| {
                wrap.checked_add(REWRITE_GATE_TICKS)
                    .is_some_and(|res| wrap_times.tick >= res)
            })
    };

    if eligible(bind) {
        return Some(bind);
    }

    let mut fallback_slot = None;
    let mut oldest_wrap = None;

    for (slot, wrap) in wrap_times.wrapped_at.iter().enumerate() {
        if !eligible(slot) {
            continue;
        }

        fallback_slot.get_or_insert(slot);

        match (wrap, oldest_wrap) {
            (Some(tick), Some((_, oldest))) if *tick < oldest => {
                oldest_wrap = Some((slot, *tick));
            }
            (Some(tick), None) => oldest_wrap = Some((slot, *tick)),
            _ => {}
        }
    }

    oldest_wrap.map_or(fallback_slot, |(slot, _)| Some(slot))
}

impl EmbeddedPipeline {
    /// # Errors
    ///
    /// Returns an error if:
    ///   - Region store creation failed
    ///   - Renderer input creation failed
    ///   - Shader loading failed
    ///   - Region render task creation failed
    ///   - Image storage is empty
    pub fn new(gpu: &RenderContext, extent: [u32; 2]) -> anyhow::Result<Self> {
        let delivery = DeliveryRing::new(gpu, extent)?;
        let store = RegionStore::new_empty(gpu)?;
        let input = RendererInput::new()?;

        let raygen = unsafe { production_raygen::load(&gpu.device)? }
            .entry_point("main")
            .context("main entry point not found for raygen shader")?;

        let mut task_graph = TaskGraph::new(&gpu.resources);

        let virtual_slots = (0..SLOT_COUNT)
            .map(|_| {
                task_graph.add_image(&ImageCreateInfo {
                    image_type: ImageType::Dim2d,
                    format: DELIVERY_FORMAT,
                    usage: ImageUsage::STORAGE,
                    ..ImageCreateInfo::default()
                })
            })
            .collect::<Vec<_>>();

        let rt_pass = RegionRenderTask::new(gpu, &store, None, &raygen)?;
        let instance_buffer_id = rt_pass.instance_buffer_id();

        let mut rt_node = task_graph.create_task_node("Render", QueueFamilyType::Graphics, rt_pass);

        rt_node.buffer_access(
            instance_buffer_id,
            AccessTypes::RAY_TRACING_SHADER_ACCELERATION_STRUCTURE_READ,
        );

        for virtual_slot in &virtual_slots {
            rt_node.image_access(
                *virtual_slot,
                AccessTypes::RAY_TRACING_SHADER_STORAGE_WRITE,
                ImageLayoutType::General,
            );
        }

        rt_node.build();

        let task_graph = unsafe {
            task_graph.compile(&CompileInfo {
                queues: &[&gpu.graphics_queue],
                flight_id: gpu.graphics_flight_id,
                ..CompileInfo::default()
            })
        }?;

        let storage_ids = slot_storage_ids(gpu, &delivery)?;

        let region = RegionRenderContext {
            camera: production_raygen::Camera {
                proj_inverse: [[0.0; 4]; 4],
                view_inverse: [[0.0; 4]; 4],
            },
            scene: default_scene(),
            swapchain_storage_image_ids: Vec::new(),
            color_image_id: storage_ids.first().copied().context("no delivery slots")?,
            delta_time: 0.0,
            mode: RenderMode::default(),
            render_extent: extent,
        };

        Ok(Self {
            delivery,
            virtual_slots,
            storage_ids,
            task_graph,
            region,
            store,
            input,
            content_version: 0,
            frame: 0,
        })
    }

    /// # Errors
    ///
    /// Returns an error is store upload failed
    pub fn upload_palette(
        &self,
        gpu: &RenderContext,
        colors: [[f32; 4]; 256],
    ) -> anyhow::Result<()> {
        self.store.upload_palette(gpu, colors)
    }

    pub const fn input(&self) -> &RendererInput {
        &self.input
    }

    pub const fn extent(&self) -> [u32; 2] {
        self.delivery.extent()
    }

    /// The version the frames produced from now on carry until content changes
    /// again. The host records it to gate delivery while a world goes away.
    pub const fn content_version(&self) -> u64 {
        self.content_version
    }

    /// # Errors
    ///
    /// Returns an error if delivery image fetch failed
    pub fn slot_image(&self, gpu: &RenderContext, slot: usize) -> anyhow::Result<Arc<Image>> {
        self.delivery.image(&gpu.resources, slot)
    }

    /// # Errors
    ///
    /// Returns an error if store upload failed
    pub fn slot_memory(
        &self,
        gpu: &RenderContext,
        slot: usize,
    ) -> anyhow::Result<Arc<DeviceMemory>> {
        let physical = self.delivery.physical_id(slot)?;

        delivery_memory(&gpu.resources, physical)
    }

    /// # Errors
    ///
    /// Returns an error if store image fetch failed
    pub fn slot_image_handle(&self, gpu: &RenderContext, slot: usize) -> anyhow::Result<u64> {
        let image = self.slot_image(gpu, slot)?;

        Ok(image.handle().as_raw())
    }

    /// # Errors
    ///
    /// Returns an error if:
    ///   - Flight waiting failed
    ///   - Delivery recreation failed
    ///   - `slot_storage_ids` failed
    ///   - Store `apply` failed
    ///   - Current slot is invalid
    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    pub fn run_frame(
        &mut self,
        gpu: &RenderContext,
        input: &FrameInput,
        wrap_times: &WrapTimes,
    ) -> anyhow::Result<Option<PublishedSlot>> {
        let extent = [input.extent[0], input.extent[1]];

        if extent[0] == 0 || extent[1] == 0 {
            return Ok(None);
        }

        if extent != self.delivery.extent() {
            gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

            let mut batch = gpu.resources.create_deferred_batch();

            for storage_id in &self.storage_ids {
                batch.destroy_storage_image(*storage_id);
            }

            batch.enqueue();

            self.delivery.recreate(gpu, extent)?;
            self.storage_ids = slot_storage_ids(gpu, &self.delivery)?;
        }

        gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

        if content_changed(&self.store.apply(gpu, &self.input)?) {
            self.content_version = self.content_version.wrapping_add(1);
        }

        self.region.mode = input.render_mode;
        self.region.delta_time = input.delta_time;
        self.region.render_extent = extent;
        self.region.camera = projection(&input.view, input.fov, extent);

        let bind = bind_slot(self.frame);

        let Some(slot) = gated_slot(bind, wrap_times) else {
            return Ok(None);
        };

        self.region.color_image_id = self
            .storage_ids
            .get(slot)
            .copied()
            .context("delivery slot out of range")?;

        let mut map = ResourceMap::new(&self.task_graph)?;

        for (index, virtual_slot) in self.virtual_slots.iter().enumerate() {
            map.insert(*virtual_slot, self.delivery.physical_id(index)?)?;
        }

        unsafe {
            self.task_graph.execute(map, &self.region, move || {})?;
        }

        let published = PublishedSlot {
            slot,
            extent,
            version: self.content_version,
        };

        self.frame = self.frame.wrapping_add(1);

        Ok(Some(published))
    }
}
