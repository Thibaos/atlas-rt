use anyhow::Context;
use dot_vox::DotVoxData;
use glam::{Mat4, camera::lh::proj::vulkan::perspective};
use std::sync::Arc;

use vulkano::{
    VulkanError,
    image::{ImageFormatInfo, ImageLayout, ImageUsage, view::ImageView},
    swapchain::{PresentMode, Surface, SurfaceInfo, Swapchain, SwapchainCreateInfo},
};
use vulkano_taskgraph::{
    Id, QueueFamilyType,
    descriptor_set::{BindlessContext, StorageImageId},
    graph::{CompileInfo, ExecutableTaskGraph, ExecuteError, ResourceMap, TaskGraph},
    resource::{AccessTypes, ImageLayoutType, Resources},
};
use winit::{dpi::PhysicalSize, window::Window};

use crate::render::{
    context::{MIN_SWAPCHAIN_IMAGES, RenderContext},
    region::{
        feed::RendererInput,
        residency::RegionStore,
        task::{
            RegionRenderContext, RegionRenderTask, RenderMode, default_scene, production_raygen,
        },
    },
};
use crate::world::{World, snapshot::emit_snapshots};

pub const DEFAULT_FOV: f32 = std::f32::consts::FRAC_PI_2;
const PROJ_NEAR: f32 = 0.01;
const PROJ_FAR: f32 = 10000.0;

pub struct FrameInput {
    pub view: Mat4,
    pub extent: [u32; 2],
    pub fov: f32,
    pub resized: bool,
    pub render_mode: RenderMode,
    pub delta_time: f32,
}

pub struct FramePipeline {
    window: Arc<Window>,
    swapchain_id: Id<Swapchain>,
    virtual_swapchain_id: Id<Swapchain>,
    swapchain_storage: Vec<StorageImageId>,
    recreate_swapchain: bool,
    task_graph: ExecutableTaskGraph<RegionRenderContext>,
    region: RegionRenderContext,
    input: RendererInput,
    store: RegionStore,
}

fn bindless_context(resources: &Resources) -> anyhow::Result<&BindlessContext> {
    resources
        .bindless_context()
        .context("bindless context not found")
}

fn swapchain_storage_views(
    resources: &Resources,
    swapchain_id: Id<Swapchain>,
) -> anyhow::Result<Vec<StorageImageId>> {
    let bcx = bindless_context(resources)?;
    let swapchain = resources.swapchain(swapchain_id);

    swapchain
        .images()
        .iter()
        .map(|image| {
            let view = ImageView::new_default(image)?;

            Ok(bcx
                .global_set()
                .add_storage_image(view, ImageLayout::General))
        })
        .collect()
}

fn create_swapchain(
    gpu: &RenderContext,
    surface: &Arc<Surface>,
    window_size: PhysicalSize<u32>,
) -> anyhow::Result<Id<Swapchain>> {
    let surface_capabilities = gpu
        .device
        .physical_device()
        .surface_capabilities(surface, &SurfaceInfo::default())?;

    let (image_format, image_color_space) = gpu
        .device
        .physical_device()
        .surface_formats(surface, &SurfaceInfo::default())?
        .into_iter()
        .find(|(format, _)| {
            gpu.device
                .physical_device()
                .image_format_properties(&ImageFormatInfo {
                    format: *format,
                    usage: ImageUsage::STORAGE | ImageUsage::COLOR_ATTACHMENT,
                    ..ImageFormatInfo::default()
                })
                .is_ok_and(|i| i.is_some())
        })
        .context("no swapchain format supports storage and color attachment usage")?;

    let swapchain_id = gpu.resources.create_swapchain(
        surface,
        &SwapchainCreateInfo {
            present_mode: PresentMode::Immediate,
            min_image_count: surface_capabilities
                .min_image_count
                .max(MIN_SWAPCHAIN_IMAGES),
            image_format,
            image_extent: window_size.into(),
            image_usage: ImageUsage::STORAGE | ImageUsage::COLOR_ATTACHMENT,
            image_color_space,
            composite_alpha: surface_capabilities
                .supported_composite_alpha
                .into_iter()
                .next()
                .context("surface does not support composite alpha")?,
            ..SwapchainCreateInfo::default()
        },
    )?;

    Ok(swapchain_id)
}

impl FramePipeline {
    /// # Errors
    ///
    /// Returns an error if:
    ///   - `emit_snapshots` failed
    ///   - Input `submit_batch` failed
    ///   - `RegionStore::new` failed
    ///   - `Surface::from_window` failed
    ///   - `create_swapchain` failed
    ///   - Swapchain storage view creation failed
    ///   - Raygen shader loading failed
    ///   - `RegionRenderTask::new` failed
    pub fn new(
        gpu: &RenderContext,
        window: Arc<Window>,
        voxel_data: &DotVoxData,
        world: &World,
    ) -> anyhow::Result<Self> {
        let input = RendererInput::new()?;
        input.submit_batch(emit_snapshots(world)?)?;

        let store = RegionStore::new(gpu, voxel_data, &input)?;
        let surface = Surface::from_window(&gpu.instance, &window)?;
        let window_size = window.inner_size();
        let swapchain_id = create_swapchain(gpu, &surface, window_size)?;

        let mut task_graph = TaskGraph::new(&gpu.resources);

        let virtual_swapchain_id = task_graph.add_swapchain(&SwapchainCreateInfo::default());

        let swapchain_storage = swapchain_storage_views(&gpu.resources, swapchain_id)?;

        let raygen = unsafe { production_raygen::load(&gpu.device)? }
            .entry_point("main")
            .context("main entry point not found for raygen shader")?;

        let rt_pass = RegionRenderTask::new(gpu, &store, Some(virtual_swapchain_id), &raygen)?;
        let instance_buffer_id = rt_pass.instance_buffer_id();

        let mut rt_node = task_graph.create_task_node("Render", QueueFamilyType::Graphics, rt_pass);
        rt_node.image_access(
            virtual_swapchain_id.current_image_id(),
            AccessTypes::RAY_TRACING_SHADER_STORAGE_WRITE,
            ImageLayoutType::General,
        );
        rt_node.buffer_access(
            instance_buffer_id,
            AccessTypes::RAY_TRACING_SHADER_ACCELERATION_STRUCTURE_READ,
        );
        rt_node.build();

        let task_graph = unsafe {
            task_graph.compile(&CompileInfo {
                queues: &[&gpu.graphics_queue],
                present_queue: Some(&gpu.graphics_queue),
                flight_id: gpu.graphics_flight_id,
                ..CompileInfo::default()
            })
        }?;

        let region = RegionRenderContext {
            camera: production_raygen::Camera {
                proj_inverse: [[0.0; 4]; 4],
                view_inverse: [[0.0; 4]; 4],
            },
            scene: default_scene(),
            swapchain_storage_image_ids: swapchain_storage.clone(),
            color_image_id: StorageImageId::INVALID,
            delta_time: 0.0,
            mode: RenderMode::default(),
            render_extent: [0, 0],
        };

        Ok(Self {
            window,
            swapchain_id,
            virtual_swapchain_id,
            swapchain_storage,
            recreate_swapchain: false,
            task_graph,
            region,
            input,
            store,
        })
    }

    fn recreate_if_needed(&mut self, gpu: &RenderContext) -> anyhow::Result<bool> {
        if !self.recreate_swapchain {
            return Ok(false);
        }

        let window = self.window.clone();

        self.swapchain_id = gpu
            .resources
            .recreate_swapchain(self.swapchain_id, |create_info| SwapchainCreateInfo {
                image_extent: window.inner_size().into(),
                ..create_info.clone()
            })?;

        let mut batch = gpu.resources.create_deferred_batch();

        for storage_id in &self.swapchain_storage {
            batch.destroy_storage_image(*storage_id);
        }

        batch.enqueue();

        self.swapchain_storage = swapchain_storage_views(&gpu.resources, self.swapchain_id)?;
        self.region
            .swapchain_storage_image_ids
            .clone_from(&self.swapchain_storage);

        self.recreate_swapchain = false;

        Ok(true)
    }

    /// # Errors
    ///
    /// Returns an error if flight waiting or frame execution failed
    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    pub fn run_frame(&mut self, gpu: &RenderContext, input: &FrameInput) -> anyhow::Result<()> {
        self.recreate_swapchain |= input.resized;

        let extent = self.window.inner_size();
        let plan = frame_plan(self.recreate_swapchain, extent.width, extent.height);

        if plan.recreate {
            self.recreate_if_needed(gpu)?;
        }

        if !plan.execute {
            return Ok(());
        }

        gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

        self.store.apply(gpu, &self.input)?;

        self.region.mode = input.render_mode;

        let aspect = extent.width as f32 / extent.height as f32;
        let proj = perspective(input.fov, aspect, PROJ_NEAR, PROJ_FAR);
        self.region.camera = production_raygen::Camera {
            proj_inverse: proj.inverse().to_cols_array_2d(),
            view_inverse: input.view.inverse().to_cols_array_2d(),
        };

        self.region.delta_time = input.delta_time;

        self.execute()?;

        Ok(())
    }

    fn execute(&mut self) -> anyhow::Result<()> {
        let mut map = ResourceMap::new(&self.task_graph)?;
        map.insert(self.virtual_swapchain_id, self.swapchain_id)?;

        let window = self.window.clone();

        let result = unsafe {
            self.task_graph
                .execute(map, &self.region, move || window.pre_present_notify())
        };

        if let Err(ExecuteError::Swapchain {
            error: VulkanError::OutOfDate,
            ..
        }) = result
        {
            self.recreate_swapchain = true;
            return Ok(());
        }

        result?;

        Ok(())
    }
}

struct FramePlan {
    recreate: bool,
    execute: bool,
}

const fn frame_plan(recreate_requested: bool, width: u32, height: u32) -> FramePlan {
    let drawable = width > 0 && height > 0;

    FramePlan {
        recreate: drawable && recreate_requested,
        execute: drawable,
    }
}

#[must_use]
pub const fn next_render_mode(mode: RenderMode) -> RenderMode {
    match mode {
        RenderMode::Voxel => RenderMode::Hull,
        RenderMode::Hull => RenderMode::Normal,
        RenderMode::Normal => RenderMode::Voxel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recreate_plans_a_recreate() {
        let plan = frame_plan(true, 1920, 1080);

        assert!(plan.recreate);
        assert!(plan.execute);
    }

    #[test]
    fn zero_extent_skips_the_frame_including_recreate() {
        let plan = frame_plan(true, 0, 1080);

        assert!(!plan.recreate);
        assert!(!plan.execute);
    }

    #[test]
    fn ordinary_frame_executes_without_recreate() {
        let plan = frame_plan(false, 1920, 1080);

        assert!(!plan.recreate);
        assert!(plan.execute);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn mode_cycle_returns_to_voxel() {
        assert_eq!(next_render_mode(RenderMode::Voxel), RenderMode::Hull);
        assert_eq!(next_render_mode(RenderMode::Hull), RenderMode::Normal);
        assert_eq!(next_render_mode(RenderMode::Normal), RenderMode::Voxel);
    }
}
