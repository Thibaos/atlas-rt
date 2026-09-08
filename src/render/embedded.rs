use std::sync::Arc;

use anyhow::Context;
use glam::{Mat4, camera::lh::proj::vulkan::perspective};
use vulkano::{
    image::{Image, ImageCreateInfo, ImageLayout, ImageType, ImageUsage, view::ImageView},

};
use vulkano_taskgraph::{
    Id, QueueFamilyType,
    descriptor_set::StorageImageId,
    graph::{CompileInfo, ExecutableTaskGraph, ResourceMap, TaskGraph},
    resource::{AccessTypes, ImageLayoutType},
};

use vulkano::memory::DeviceMemory;

use crate::render::{
    context::RenderContext,
    pipeline::FrameInput,
    delivery::{DELIVERY_FORMAT, DeliveryRing, SLOT_COUNT, bind_slot, delivery_memory},
    region::{
        feed::RendererInput,
        residency::RegionStore,
        task::{RegionRenderContext, RegionRenderTask, RenderMode, default_scene, production_raygen},
    },
};

const PROJ_NEAR: f32 = 0.01;
const PROJ_FAR: f32 = 10000.0;

#[derive(Clone, Copy, Debug)]
pub struct PublishedSlot {
    pub slot: usize,
    pub extent: [u32; 2],
    pub frame: usize,
}

pub struct EmbeddedPipeline {
    delivery: DeliveryRing,
    virtual_slots: Vec<Id<Image>>,
    storage_ids: Vec<StorageImageId>,
    task_graph: ExecutableTaskGraph<RegionRenderContext>,
    region: RegionRenderContext,
    store: RegionStore,
    input: RendererInput,
    frame: usize,
}

#[allow(clippy::as_conversions, clippy::cast_precision_loss)]
fn projection(input: &Mat4, fov: f32, extent: [u32; 2]) -> production_raygen::Camera {
    let [width, height] = extent;
    let aspect = match (width, height) {
        (0, _) | (_, 0) => 1.0,
        _ => width as f32 / height as f32,
    };

    let fov = if fov > 0.0 { fov } else { crate::render::pipeline::DEFAULT_FOV };

    let proj = perspective(fov, aspect, PROJ_NEAR, PROJ_FAR);

    production_raygen::Camera {
        proj_inverse: proj.inverse().to_cols_array_2d(),
        view_inverse: input.inverse().to_cols_array_2d(),
    }
}

impl EmbeddedPipeline {
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

        let storage_ids = (0..SLOT_COUNT)
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
            .collect::<anyhow::Result<Vec<_>>>()?;

        let region = RegionRenderContext {
            camera: production_raygen::Camera {
                proj_inverse: [[0.0; 4]; 4],
                view_inverse: [[0.0; 4]; 4],
            },
            scene: default_scene(),
            swapchain_storage_image_ids: Vec::new(),
            color_image_id: storage_ids
                .first()
                .copied()
                .context("no delivery slots")?,
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
            frame: 0,
        })
    }

    pub fn upload_palette(
        &self,
        gpu: &RenderContext,
        colors: [[f32; 4]; 256],
    ) -> anyhow::Result<()> {
        self.store.upload_palette(gpu, colors)
    }

    pub fn input(&self) -> &RendererInput {
        &self.input
    }

    pub fn extent(&self) -> [u32; 2] {
        self.delivery.extent()
    }

    pub fn slot_image(
        &self,
        gpu: &RenderContext,
        slot: usize,
    ) -> anyhow::Result<Arc<Image>> {
        self.delivery.image(&gpu.resources, slot)
    }

    pub fn slot_memory(&self, gpu: &RenderContext, slot: usize) -> anyhow::Result<Arc<DeviceMemory>> {
        let physical = self.delivery.physical_id(slot)?;

        delivery_memory(&gpu.resources, physical)
    }

    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    pub fn run_frame(
        &mut self,
        gpu: &RenderContext,
        input: &FrameInput,
    ) -> anyhow::Result<Option<PublishedSlot>> {
        let extent = [input.extent[0], input.extent[1]];

        if extent[0] == 0 || extent[1] == 0 {
            return Ok(None);
        }

        if extent != self.delivery.extent() {
            self.delivery.recreate(gpu, extent)?;
        }

        gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

        self.store.apply(gpu, &self.input)?;

        self.region.mode = input.render_mode;
        self.region.delta_time = input.delta_time;
        self.region.render_extent = extent;
        self.region.camera = projection(&input.view, input.fov, extent);

        let slot = bind_slot(self.frame);

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
            slot: bind_slot(self.frame),
            extent,
            frame: self.frame,
        };

        self.frame = self.frame.wrapping_add(1);

        Ok(Some(published))
    }
}
