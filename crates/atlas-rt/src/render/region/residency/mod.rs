pub mod apply;
pub mod decision;

use std::sync::Arc;

use anyhow::Context;
use dot_vox::DotVoxData;
use glam::IVec3;
use vulkano::{
    DeviceSize, Packed24_8,
    acceleration_structure::{AabbPositions, AccelerationStructure, AccelerationStructureInstance},
    buffer::{Buffer, BufferContents, BufferCreateInfo, BufferUsage, Subbuffer},
    memory::allocator::{AllocationCreateInfo, DeviceLayout, MemoryTypeFilter},
};
use vulkano_taskgraph::{
    Id,
    descriptor_set::{AccelerationStructureId, BindlessContext, StorageBufferId},
    resource::HostAccessType,
};

use crate::{
    render::{
        accel,
        context::RenderContext,
        pipeline::task::{default_scene, production_raygen},
        region::{
            alloc::{AllocStats, FreeLists, PendingFrees},
            pack::{REGION_COUNT, RegionData},
            queue::RendererInput,
            rebuild::{RebuildLogEntry, RebuildPlan},
        },
    },
    world::{
        grid::{REGION_LENGTH, region_id},
        palette::get_effective_palette,
    },
};

struct ResidentRegion {
    pool_buffer_id: Id<Buffer>,
    pool_capacity: u64,
    aabb_buffer_id: Id<Buffer>,
    aabb_capacity: u32,
    blas: Arc<AccelerationStructure>,
    blas_storage_size: u64,
}

struct SceneBuffers {
    camera: Id<Buffer>,
    scene: Id<Buffer>,
    palette: Id<Buffer>,
    region_table: Id<Buffer>,
    aabb_table: Id<Buffer>,
    instance: Id<Buffer>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub became_resident: Vec<IVec3>,
    pub left_resident: Vec<IVec3>,
    pub dirty: Vec<IVec3>,
    pub blas_replaced: Vec<IVec3>,
    pub tlas_rebuilt: bool,
    pub rebuild_log: Vec<RebuildLogEntry>,
    pub instance_count_before: usize,
    pub instance_count: usize,
}

#[derive(Clone, Copy)]
pub struct RegionBindingsIds {
    pub camera_buffer: Id<Buffer>,
    pub scene_buffer: Id<Buffer>,
    pub region_table_storage: StorageBufferId,
    pub camera_storage: StorageBufferId,
    pub scene_storage: StorageBufferId,
    pub palette_storage: StorageBufferId,
    pub acceleration_structure: AccelerationStructureId,
    pub aabb_table_storage: StorageBufferId,
    pub instance_buffer: Id<Buffer>,
}

pub struct RegionStore {
    pub bindings: RegionBindingsIds,
    palette_buffer_id: Id<Buffer>,
    region_table_buffer_id: Id<Buffer>,
    aabb_table_buffer_id: Id<Buffer>,
    instances: Vec<AccelerationStructureInstance>,
    resident_ids: Vec<u32>,
    tlas: Arc<AccelerationStructure>,
    tlas_storage_size: u64,
    tlas_initialized: bool,
    regions: Vec<Option<ResidentRegion>>,
    table_addresses: Vec<u64>,
    free: FreeLists,
    pending_free: PendingFrees,
    dummy_blas: Arc<AccelerationStructure>,
    alloc_stats: AllocStats,
}

impl RegionStore {
    /// # Errors
    ///
    /// Returns an error if `upload_default_globals`, `create_bindings`, store `ensure_tlas_initialized` or `write_aabb_table` failed
    pub fn new_empty(gpu: &RenderContext) -> anyhow::Result<Self> {
        let buffers = create_scene_buffers(gpu)?;
        let (tlas, tlas_storage_size) = create_tlas(gpu, buffers.instance)?;
        let dummy_blas = create_dummy_blas(gpu)?;

        upload_default_globals(gpu, &buffers)?;

        let bindings = create_bindings(gpu, &buffers, &tlas)?;

        let mut store = Self {
            bindings,
            palette_buffer_id: buffers.palette,
            region_table_buffer_id: buffers.region_table,
            aabb_table_buffer_id: buffers.aabb_table,
            instances: static_instances()?,
            resident_ids: Vec::new(),
            tlas,
            tlas_storage_size,
            tlas_initialized: false,
            regions: (0..REGION_COUNT).map(|_| None).collect(),
            table_addresses: vec![0; REGION_COUNT],
            free: FreeLists::default(),
            pending_free: PendingFrees::default(),
            dummy_blas,
            alloc_stats: AllocStats::default(),
        };

        store.ensure_tlas_initialized(gpu)?;
        store.write_aabb_table(gpu, buffers.aabb_table)?;

        Ok(store)
    }

    /// # Errors
    ///
    /// Returns an error if effective-palette construction, `Self::new_empty`,
    /// packing regions, or store rebuild fails.
    pub fn new(
        gpu: &RenderContext,
        voxel_data: &DotVoxData,
        input: &RendererInput,
    ) -> anyhow::Result<Self> {
        let palette = get_effective_palette(voxel_data)?;
        let mut store = Self::new_empty(gpu)?;

        store.upload_palette(gpu, palette.map(|color| color.to_array()))?;

        input.wait_until_idle()?;

        let packs: Vec<(IVec3, Option<RegionData>)> = input
            .packed_regions()?
            .into_iter()
            .map(|region| (region.region_index, Some(region)))
            .collect();

        let report = store.rebuild(gpu, packs)?;

        debug_assert!(
            report.left_resident.is_empty(),
            "the initial batch only creates residency"
        );

        Ok(store)
    }

    /// # Errors
    ///
    /// See `upload_palette_colors`
    pub fn upload_palette(
        &self,
        gpu: &RenderContext,
        colors: [[f32; 4]; 256],
    ) -> anyhow::Result<()> {
        upload_palette_colors(gpu, self.palette_buffer_id, &colors)
    }

    fn write_aabb_table(
        &self,
        gpu: &RenderContext,
        aabb_table_buffer_id: Id<Buffer>,
    ) -> anyhow::Result<()> {
        let mut bdas = vec![0u64; REGION_COUNT];

        for (id, region) in self.regions.iter().enumerate() {
            if let Some(region) = region {
                *bdas
                    .get_mut(id)
                    .context(format!("bda slot {id} out of range"))? = gpu
                    .resources
                    .buffer(region.aabb_buffer_id)
                    .buffer()
                    .device_address()
                    .get();
            }
        }

        unsafe {
            vulkano_taskgraph::execute(
                &gpu.transfer_queue,
                &gpu.resources,
                gpu.graphics_flight_id,
                |_cbf, tcx| {
                    tcx.write_buffer::<production_raygen::AabbTable>(aabb_table_buffer_id, ..)
                        .bdas
                        .copy_from_slice(&bdas);
                    Ok(())
                },
                [(aabb_table_buffer_id, HostAccessType::Write)],
                [],
                [],
            )?;
        }

        gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

        Ok(())
    }

    fn ensure_tlas_initialized(&mut self, gpu: &RenderContext) -> anyhow::Result<()> {
        if self.tlas_initialized {
            return Ok(());
        }

        let mut plan = RebuildPlan::default();
        self.plan_tlas_build(gpu, &mut plan, 1)?;

        self.rebuild_with_plan(gpu, plan)
    }

    #[must_use]
    pub fn blases(&self) -> Vec<Arc<AccelerationStructure>> {
        self.regions
            .iter()
            .filter_map(|region| region.as_ref().map(|region| region.blas.clone()))
            .collect()
    }

    pub(crate) const fn region_table_buffer_id(&self) -> Id<Buffer> {
        self.region_table_buffer_id
    }

    pub(crate) fn tlas(&self) -> Arc<AccelerationStructure> {
        self.tlas.clone()
    }
}

fn static_instances() -> anyhow::Result<Vec<AccelerationStructureInstance>> {
    let mut out = vec![AccelerationStructureInstance::default(); REGION_COUNT];

    for x in -8..8 {
        for y in -8..8 {
            for z in -8..8 {
                let index = IVec3::new(x, y, z);
                let id = usize::try_from(region_id(index))?;
                let region_length = REGION_LENGTH.cast_signed();
                let origin = IVec3::new(
                    x.strict_mul(region_length),
                    y.strict_mul(region_length),
                    z.strict_mul(region_length),
                )
                .as_vec3()
                .to_array();

                *out.get_mut(id)
                    .context(format!("instance slot {id} out of range"))? =
                    AccelerationStructureInstance {
                        transform: [
                            [1.0, 0.0, 0.0, origin[0]],
                            [0.0, 1.0, 0.0, origin[1]],
                            [0.0, 0.0, 1.0, origin[2]],
                        ],
                        instance_custom_index_and_mask: Packed24_8::new(region_id(index), 0xFF),
                        acceleration_structure_reference: 0,
                        ..AccelerationStructureInstance::default()
                    };
            }
        }
    }

    Ok(out)
}

fn create_scene_buffers(gpu: &RenderContext) -> anyhow::Result<SceneBuffers> {
    Ok(SceneBuffers {
        camera: create_storage_buffer::<production_raygen::Camera>(gpu)?,
        scene: create_storage_buffer::<production_raygen::Scene>(gpu)?,
        palette: create_storage_buffer::<production_raygen::Palette>(gpu)?,
        region_table: create_storage_buffer::<production_raygen::RegionTable>(gpu)?,
        aabb_table: create_storage_buffer::<production_raygen::AabbTable>(gpu)?,
        instance: create_instance_buffer(gpu)?,
    })
}

fn create_storage_buffer<T: BufferContents>(gpu: &RenderContext) -> anyhow::Result<Id<Buffer>> {
    Ok(gpu.resources.create_buffer(
        &BufferCreateInfo {
            usage: BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_DST,
            ..BufferCreateInfo::default()
        },
        &AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
            ..AllocationCreateInfo::default()
        },
        DeviceLayout::new_sized::<T>(),
    )?)
}

fn create_instance_buffer(gpu: &RenderContext) -> anyhow::Result<Id<Buffer>> {
    let layout = DeviceLayout::new_unsized::<[AccelerationStructureInstance]>(REGION_COUNT as u64)
        .context("device layout for the instance buffer is invalid")?;

    Ok(gpu.resources.create_buffer(
        &BufferCreateInfo {
            usage: BufferUsage::SHADER_DEVICE_ADDRESS
                | BufferUsage::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY,
            ..BufferCreateInfo::default()
        },
        &AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
            ..AllocationCreateInfo::default()
        },
        layout,
    )?)
}

fn create_tlas(
    gpu: &RenderContext,
    instance_buffer_id: Id<Buffer>,
) -> anyhow::Result<(Arc<AccelerationStructure>, u64)> {
    let instance_buffer = Subbuffer::new(gpu.resources.buffer(instance_buffer_id).buffer().clone())
        .cast_aligned::<AccelerationStructureInstance>();

    accel::create_tlas_storage(
        &instance_buffer,
        u32::try_from(REGION_COUNT)?,
        &gpu.memory_allocator,
        &gpu.device,
    )
}

fn upload_default_globals(gpu: &RenderContext, buffers: &SceneBuffers) -> anyhow::Result<()> {
    let default_palette = [[0.0; 4]; 256];

    unsafe {
        vulkano_taskgraph::execute(
            &gpu.transfer_queue,
            &gpu.resources,
            gpu.graphics_flight_id,
            |_cbf, tcx| {
                *tcx.write_buffer::<production_raygen::Palette>(buffers.palette, ..) =
                    production_raygen::Palette {
                        colors: default_palette,
                    };
                *tcx.write_buffer::<production_raygen::Scene>(buffers.scene, ..) = default_scene();
                Ok(())
            },
            [
                (buffers.palette, HostAccessType::Write),
                (buffers.scene, HostAccessType::Write),
            ],
            [],
            [],
        )?;
    }

    gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

    Ok(())
}

/// # Errors
///
/// Returns an error if taskgraph execution or flight waiting failed
fn upload_palette_colors(
    gpu: &RenderContext,
    palette_buffer_id: Id<Buffer>,
    colors: &[[f32; 4]; 256],
) -> anyhow::Result<()> {
    unsafe {
        vulkano_taskgraph::execute(
            &gpu.transfer_queue,
            &gpu.resources,
            gpu.graphics_flight_id,
            |_cbf, tcx| {
                *tcx.write_buffer::<production_raygen::Palette>(palette_buffer_id, ..) =
                    production_raygen::Palette { colors: *colors };
                Ok(())
            },
            [(palette_buffer_id, HostAccessType::Write)],
            [],
            [],
        )?;
    }

    gpu.resources.flight(gpu.graphics_flight_id).wait_idle()?;

    Ok(())
}

fn bindless_storage_buffer<T>(
    bcx: &BindlessContext,
    buffer_id: Id<Buffer>,
) -> anyhow::Result<StorageBufferId> {
    let size = DeviceSize::try_from(size_of::<T>())?;

    Ok(bcx
        .global_set()
        .create_storage_buffer(buffer_id, 0, Some(size))?)
}

fn create_bindings(
    gpu: &RenderContext,
    buffers: &SceneBuffers,
    tlas: &Arc<AccelerationStructure>,
) -> anyhow::Result<RegionBindingsIds> {
    let bcx = gpu
        .resources
        .bindless_context()
        .context("bindless context not found")?;

    let region_table_storage =
        bindless_storage_buffer::<production_raygen::RegionTable>(bcx, buffers.region_table)?;

    let camera_storage = bindless_storage_buffer::<production_raygen::Camera>(bcx, buffers.camera)?;

    let palette_storage =
        bindless_storage_buffer::<production_raygen::Palette>(bcx, buffers.palette)?;

    let scene_storage = bindless_storage_buffer::<production_raygen::Scene>(bcx, buffers.scene)?;

    let acceleration_structure = bcx.global_set().add_acceleration_structure(tlas.clone());

    let aabb_table_storage =
        bindless_storage_buffer::<production_raygen::AabbTable>(bcx, buffers.aabb_table)?;

    Ok(RegionBindingsIds {
        camera_buffer: buffers.camera,
        scene_buffer: buffers.scene,
        region_table_storage,
        camera_storage,
        scene_storage,
        palette_storage,
        acceleration_structure,
        aabb_table_storage,
        instance_buffer: buffers.instance,
    })
}

fn create_dummy_blas(gpu: &RenderContext) -> anyhow::Result<Arc<AccelerationStructure>> {
    let aabb = AabbPositions {
        min: [1.0e9; 3],
        max: [1.0e9 + 1.0; 3],
    };

    let buffer = Buffer::from_iter(
        &gpu.memory_allocator,
        &BufferCreateInfo {
            usage: BufferUsage::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY
                | BufferUsage::SHADER_DEVICE_ADDRESS,
            ..BufferCreateInfo::default()
        },
        &AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
            ..AllocationCreateInfo::default()
        },
        std::iter::once(aabb),
    )?;

    let result = accel::build_blas_aabbs_fresh(
        &buffer,
        1,
        &gpu.memory_allocator,
        &gpu.device,
        &gpu.compute_queue,
        &gpu.resources,
        gpu.compute_flight_id,
    )?;

    Ok(result.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_ids_fit_12bit_budget_at_full_lattice() {
        let mut seen = std::collections::HashSet::new();

        for x in -8..8 {
            for y in -8..8 {
                for z in -8..8 {
                    let id = region_id(IVec3::new(x, y, z));
                    assert!(id < (1 << 12) as u32, "region id {id} exceeds 12 bits");
                    assert!(seen.insert(id), "region id {id} collides");
                }
            }
        }

        assert_eq!(seen.len(), REGION_COUNT);
    }

    #[test]
    fn static_instance_data_is_lattice_static() {
        let instances = static_instances().unwrap();
        assert_eq!(instances.len(), REGION_COUNT);

        for index in [
            IVec3::new(0, 0, 0),
            IVec3::new(1, 0, 0),
            IVec3::new(-1, 2, 3),
            IVec3::new(7, -8, 0),
        ] {
            let id = region_id(index) as usize;
            let instance = &instances[id];
            let region_length = REGION_LENGTH.cast_signed();
            let origin = IVec3::new(
                index.x.strict_mul(region_length),
                index.y.strict_mul(region_length),
                index.z.strict_mul(region_length),
            )
            .as_vec3()
            .to_array();
            assert_eq!(instance.transform[0], [1.0, 0.0, 0.0, origin[0]]);
            assert_eq!(instance.transform[1], [0.0, 1.0, 0.0, origin[1]]);
            assert_eq!(instance.transform[2], [0.0, 0.0, 1.0, origin[2]]);
            assert_eq!(instance.instance_custom_index_and_mask.low_24(), id as u32);
            assert_eq!(instance.instance_custom_index_and_mask.high_8(), 0xFF);
            assert_eq!(instance.acceleration_structure_reference, 0);
        }
    }
}
