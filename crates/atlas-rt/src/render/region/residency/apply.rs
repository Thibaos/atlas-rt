use std::sync::Arc;

use anyhow::Context;
use glam::IVec3;
use vulkano::{
    Packed24_8,
    acceleration_structure::{AabbPositions, AccelerationStructure, AccelerationStructureInstance},
    buffer::{Buffer, Subbuffer},
};
use vulkano_taskgraph::Id;

use crate::render::{
    accel,
    context::RenderContext,
    region::{
        alloc::{
            BlasAllocation, FreedBlas, FreedPool, PoolAllocation, allocate_blas, allocate_pool,
        },
        pack::{REGION_COUNT, RegionData},
        queue::RendererInput,
        rebuild::{BlasBuild, RebuildGraph, RebuildPlan, RegionUpload, TlasBuild},
        residency::decision::{RegionEffect, RegionSlot, decide},
    },
};

use super::{ApplyReport, RegionStore, ResidentRegion};

impl RegionStore {
    pub(super) fn plan_tlas_build(
        &self,
        gpu: &RenderContext,
        plan: &mut RebuildPlan,
        instance_count: u32,
    ) -> anyhow::Result<()> {
        let instance_buffer = Subbuffer::new(
            gpu.resources
                .buffer(self.bindings.instance_buffer)
                .buffer()
                .clone(),
        )
        .cast_aligned::<AccelerationStructureInstance>();

        let sizes = accel::tlas_build_sizes(gpu, &instance_buffer, instance_count)?;

        debug_assert!(
            self.tlas_storage_size >= sizes.acceleration_structure_size,
            "in-place TLAS build for {instance_count} instances exceeds the stable storage"
        );

        plan.instances = Some(self.packed_instance_prefix()?);

        plan.tlas = Some(TlasBuild {
            instance_count,
            scratch: accel::allocate_scratch(gpu, sizes.build_scratch_size)?,
        });

        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if packing regions failed
    pub fn apply(
        &mut self,
        gpu: &RenderContext,
        input: &RendererInput,
    ) -> anyhow::Result<ApplyReport> {
        let dirty = input.take_dirty_regions();

        if dirty.is_empty() {
            return Ok(ApplyReport::default());
        }

        let packs: Vec<(IVec3, Option<RegionData>)> = dirty
            .iter()
            .map(|&region| Ok((region, input.packed_region(region)?)))
            .collect::<anyhow::Result<_>>()?;

        self.rebuild(gpu, packs)
    }

    pub(super) fn rebuild(
        &mut self,
        gpu: &RenderContext,
        packs: Vec<(IVec3, Option<RegionData>)>,
    ) -> anyhow::Result<ApplyReport> {
        let slots: Vec<Option<RegionSlot>> = self
            .regions
            .iter()
            .map(|region| {
                region.as_ref().map(|region| RegionSlot {
                    pool_capacity: region.pool_capacity,
                    aabb_capacity: region.aabb_capacity,
                })
            })
            .collect();

        let instance_count_before = self.resident_ids.len();
        let decision = decide(&slots, &self.resident_ids, packs)?;
        self.resident_ids = decision.resident_ids;

        let mut report = ApplyReport {
            instance_count_before,
            became_resident: decision.became_resident,
            left_resident: decision.left_resident,
            dirty: decision.dirty,
            blas_replaced: decision.blas_replaced,
            tlas_rebuilt: decision.tlas_dirty,
            ..ApplyReport::default()
        };

        let mut plan = RebuildPlan::default();

        for (id, effect) in decision.effects {
            match effect {
                RegionEffect::Ignore => {}

                RegionEffect::Enter {
                    pool_bytes,
                    aabbs,
                    pack,
                } => {
                    self.enter_region(gpu, &mut plan, id, pool_bytes, aabbs, pack)?;
                }

                RegionEffect::Exit {
                    retire_pool,
                    retire_blas,
                } => {
                    self.exit_region(id, retire_pool, retire_blas)?;
                }

                RegionEffect::Update {
                    pool_bytes,
                    aabbs,
                    retire_pool,
                    retire_blas,
                    pack,
                } => {
                    self.update_region(
                        gpu,
                        &mut plan,
                        id,
                        pool_bytes,
                        aabbs,
                        retire_pool,
                        retire_blas,
                        pack,
                    )?;
                }
            }
        }

        if decision.table_changed {
            let addresses: [u64; REGION_COUNT] = self
                .table_addresses
                .clone()
                .try_into()
                .ok()
                .context("region address table length differs from REGION_COUNT")?;
            plan.table = Some(addresses);
        }

        if decision.tlas_dirty {
            self.plan_tlas_build(
                gpu,
                &mut plan,
                u32::try_from(self.resident_ids.len().max(1))?,
            )?;
        }

        report.rebuild_log = plan.log()?;

        if plan.is_empty() {
            report.instance_count = self.resident_ids.len();
            return Ok(report);
        }

        self.rebuild_with_plan(gpu, plan)?;

        if report.tlas_rebuilt {
            self.write_aabb_table(gpu, self.aabb_table_buffer_id)?;
        }

        report.instance_count = self.resident_ids.len();

        Ok(report)
    }

    fn enter_region(
        &mut self,
        gpu: &RenderContext,
        plan: &mut RebuildPlan,
        id: u32,
        pool_bytes: u64,
        aabbs: u32,
        pack: RegionData,
    ) -> anyhow::Result<()> {
        let region_index = pack.region_index;

        let pool = allocate_pool(gpu, &mut self.free, &mut self.alloc_stats, pool_bytes)?;
        let blas_alloc = allocate_blas(gpu, &mut self.free, &mut self.alloc_stats, aabbs)?;

        let aabb_buffer = Subbuffer::new(
            gpu.resources
                .buffer(blas_alloc.aabb_buffer_id)
                .buffer()
                .clone(),
        )
        .cast_aligned::<AabbPositions>();

        let (blas, blas_storage_size) =
            resolve_blas_storage(gpu, &aabb_buffer, aabbs, &blas_alloc)?;

        plan.uploads.push(RegionUpload {
            region_index,
            pool_buffer_id: pool.buffer_id,
            pool_bytes: pack.blocks,
            aabb_buffer_id: blas_alloc.aabb_buffer_id,
            aabbs: pack.aabbs,
        });

        plan.blas_builds.push(plan_blas_build(
            gpu,
            region_index,
            blas_alloc.aabb_buffer_id,
            &aabb_buffer,
            aabbs,
            blas.clone(),
            blas_storage_size,
            true,
        )?);

        let address = gpu
            .resources
            .buffer(pool.buffer_id)
            .buffer()
            .device_address()
            .get();

        self.set_instance_reference(id, &blas)?;
        self.set_table_address(id, address)?;

        *self
            .regions
            .get_mut(usize::try_from(id)?)
            .context(format!("region {id} out of range"))? = Some(ResidentRegion {
            pool_buffer_id: pool.buffer_id,
            pool_capacity: pool.capacity,
            aabb_buffer_id: blas_alloc.aabb_buffer_id,
            aabb_capacity: blas_alloc.aabb_capacity,
            blas,
            blas_storage_size,
        });

        Ok(())
    }

    fn exit_region(&mut self, id: u32, retire_pool: u64, retire_blas: u32) -> anyhow::Result<()> {
        let region = self
            .regions
            .get_mut(usize::try_from(id)?)
            .context(format!("region {id} out of range"))?
            .take()
            .context(format!("region {id} is not resident"))?;

        self.set_table_address(id, 0)?;

        self.pending_free.pools.push(FreedPool {
            buffer_id: region.pool_buffer_id,
            capacity: retire_pool,
        });

        self.pending_free.blas.push(FreedBlas {
            aabb_buffer_id: region.aabb_buffer_id,
            aabb_capacity: retire_blas,
            blas: region.blas.clone(),
            blas_storage_size: region.blas_storage_size,
        });

        Ok(())
    }

    fn update_region(
        &mut self,
        gpu: &RenderContext,
        plan: &mut RebuildPlan,
        id: u32,
        pool_bytes: u64,
        aabbs: u32,
        retire_pool: Option<u64>,
        retire_blas: Option<u32>,
        pack: RegionData,
    ) -> anyhow::Result<()> {
        let region_index = pack.region_index;

        let blas_replacement = if retire_blas.is_some() {
            let alloc = allocate_blas(gpu, &mut self.free, &mut self.alloc_stats, aabbs)?;

            self.replace_blas(id, &alloc)?;

            Some(alloc)
        } else {
            None
        };

        if retire_pool.is_some() {
            let pool = allocate_pool(gpu, &mut self.free, &mut self.alloc_stats, pool_bytes)?;

            self.replace_pool(gpu, id, &pool)?;
        }

        let (pool_id, aabb_id) = self.region_buffer_ids(id)?;

        plan.uploads.push(RegionUpload {
            region_index,
            pool_buffer_id: pool_id,
            pool_bytes: pack.blocks,
            aabb_buffer_id: aabb_id,
            aabbs: pack.aabbs,
        });

        match blas_replacement {
            Some(alloc) => {
                self.plan_replacement_blas_build(gpu, plan, id, region_index, aabbs, &alloc)?;
            }
            None => self.plan_in_place_blas_build(gpu, plan, id, region_index, aabb_id, aabbs)?,
        }

        Ok(())
    }

    fn replace_pool(
        &mut self,
        gpu: &RenderContext,
        id: u32,
        pool: &PoolAllocation,
    ) -> anyhow::Result<()> {
        let region = self
            .regions
            .get_mut(usize::try_from(id)?)
            .context(format!("region {id} out of range"))?
            .as_mut()
            .context(format!("region {id} is not resident"))?;

        self.pending_free.pools.push(FreedPool {
            buffer_id: region.pool_buffer_id,
            capacity: region.pool_capacity,
        });

        region.pool_buffer_id = pool.buffer_id;
        region.pool_capacity = pool.capacity;

        let address = gpu
            .resources
            .buffer(pool.buffer_id)
            .buffer()
            .device_address()
            .get();

        self.set_table_address(id, address)?;

        Ok(())
    }

    fn replace_blas(&mut self, id: u32, alloc: &BlasAllocation) -> anyhow::Result<()> {
        let region = self
            .regions
            .get_mut(usize::try_from(id)?)
            .context(format!("region {id} out of range"))?
            .as_mut()
            .context(format!("region {id} is not resident"))?;

        self.pending_free.blas.push(FreedBlas {
            aabb_buffer_id: region.aabb_buffer_id,
            aabb_capacity: region.aabb_capacity,
            blas: region.blas.clone(),
            blas_storage_size: region.blas_storage_size,
        });

        region.aabb_buffer_id = alloc.aabb_buffer_id;
        region.aabb_capacity = alloc.aabb_capacity;

        Ok(())
    }

    fn region_buffer_ids(&self, id: u32) -> anyhow::Result<(Id<Buffer>, Id<Buffer>)> {
        let region = self
            .regions
            .get(usize::try_from(id)?)
            .context(format!("region {id} out of range"))?
            .as_ref()
            .context(format!("region {id} is not resident"))?;

        Ok((region.pool_buffer_id, region.aabb_buffer_id))
    }

    fn plan_replacement_blas_build(
        &mut self,
        gpu: &RenderContext,
        plan: &mut RebuildPlan,
        id: u32,
        region_index: IVec3,
        aabb_count: u32,
        alloc: &BlasAllocation,
    ) -> anyhow::Result<()> {
        let aabb_buffer =
            Subbuffer::new(gpu.resources.buffer(alloc.aabb_buffer_id).buffer().clone())
                .cast_aligned::<AabbPositions>();

        let (blas, blas_storage_size) = resolve_blas_storage(gpu, &aabb_buffer, aabb_count, alloc)?;

        plan.blas_builds.push(plan_blas_build(
            gpu,
            region_index,
            alloc.aabb_buffer_id,
            &aabb_buffer,
            aabb_count,
            blas.clone(),
            blas_storage_size,
            true,
        )?);

        let region = self
            .regions
            .get_mut(usize::try_from(id)?)
            .context(format!("region {id} out of range"))?
            .as_mut()
            .context(format!("region {id} is not resident"))?;

        region.blas = blas.clone();
        region.blas_storage_size = blas_storage_size;

        self.set_instance_reference(id, &blas)?;

        Ok(())
    }

    fn plan_in_place_blas_build(
        &self,
        gpu: &RenderContext,
        plan: &mut RebuildPlan,
        id: u32,
        region_index: IVec3,
        aabb_id: Id<Buffer>,
        aabb_count: u32,
    ) -> anyhow::Result<()> {
        let (blas, blas_storage_size) = {
            let region = self
                .regions
                .get(usize::try_from(id)?)
                .context(format!("region {id} out of range"))?
                .as_ref()
                .context(format!("region {id} is not resident"))?;

            (region.blas.clone(), region.blas_storage_size)
        };

        let aabb_buffer = Subbuffer::new(gpu.resources.buffer(aabb_id).buffer().clone())
            .cast_aligned::<AabbPositions>();

        plan.blas_builds.push(plan_blas_build(
            gpu,
            region_index,
            aabb_id,
            &aabb_buffer,
            aabb_count,
            blas,
            blas_storage_size,
            false,
        )?);

        Ok(())
    }

    fn set_table_address(&mut self, id: u32, address: u64) -> anyhow::Result<()> {
        *self
            .table_addresses
            .get_mut(usize::try_from(id)?)
            .context(format!("table address slot {id} out of range"))? = address;

        Ok(())
    }

    fn set_instance_reference(
        &mut self,
        id: u32,
        blas: &Arc<AccelerationStructure>,
    ) -> anyhow::Result<()> {
        self.instances
            .get_mut(usize::try_from(id)?)
            .context(format!("instance slot {id} out of range"))?
            .acceleration_structure_reference = blas.device_address().into();

        Ok(())
    }

    pub(super) fn rebuild_with_plan(
        &mut self,
        gpu: &RenderContext,
        plan: RebuildPlan,
    ) -> anyhow::Result<()> {
        let tlas_rebuilds = plan.tlas.is_some();
        let graph = RebuildGraph::new(gpu, self, plan)?;

        graph.execute(gpu)?;

        if tlas_rebuilds {
            self.tlas_initialized = true;
        }

        self.release_pending_frees();

        Ok(())
    }

    fn packed_instance_prefix(&self) -> anyhow::Result<Vec<AccelerationStructureInstance>> {
        packed_prefix(&self.instances, &self.resident_ids, self.dummy_instance())
    }

    fn dummy_instance(&self) -> AccelerationStructureInstance {
        AccelerationStructureInstance {
            instance_custom_index_and_mask: Packed24_8::new(0, 0x00),
            acceleration_structure_reference: self.dummy_blas.device_address().into(),
            ..AccelerationStructureInstance::default()
        }
    }

    fn release_pending_frees(&mut self) {
        self.free.pools.append(&mut self.pending_free.pools);
        self.free.blas.append(&mut self.pending_free.blas);
    }
}

fn resolve_blas_storage(
    gpu: &RenderContext,
    aabb_buffer: &Subbuffer<[AabbPositions]>,
    aabb_count: u32,
    alloc: &BlasAllocation,
) -> anyhow::Result<(Arc<AccelerationStructure>, u64)> {
    match &alloc.as_storage {
        Some((blas, storage_size)) => Ok((blas.clone(), *storage_size)),
        None => accel::create_blas_aabbs_storage(
            aabb_buffer,
            aabb_count,
            &gpu.memory_allocator,
            &gpu.device,
        ),
    }
}

fn plan_blas_build(
    gpu: &RenderContext,
    region_index: IVec3,
    aabb_buffer_id: Id<Buffer>,
    aabb_buffer: &Subbuffer<[AabbPositions]>,
    aabb_count: u32,
    blas: Arc<AccelerationStructure>,
    blas_storage_size: u64,
    fresh: bool,
) -> anyhow::Result<BlasBuild> {
    let sizes = accel::blas_build_sizes(gpu, aabb_buffer, aabb_count)?;

    debug_assert!(
        blas_storage_size >= sizes.acceleration_structure_size,
        "BLAS build for {aabb_count} AABBs exceeds its {blas_storage_size}-byte storage"
    );

    Ok(BlasBuild {
        region_index,
        aabb_buffer_id,
        aabb_count,
        blas,
        scratch: accel::allocate_scratch(gpu, sizes.build_scratch_size)?,
        fresh,
    })
}

fn packed_prefix(
    instances: &[AccelerationStructureInstance],
    resident_ids: &[u32],
    empty_dummy: AccelerationStructureInstance,
) -> anyhow::Result<Vec<AccelerationStructureInstance>> {
    if resident_ids.is_empty() {
        return Ok(vec![empty_dummy]);
    }

    resident_ids
        .iter()
        .map(|&id| {
            let index = usize::try_from(id)?;

            instances
                .get(index)
                .copied()
                .context(format!("resident region {id} has no instance slot"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::render::region::residency::static_instances;

    use super::*;

    #[test]
    fn packed_prefix_rewrites_resident_instances() {
        let instances = static_instances().unwrap();
        let dummy = AccelerationStructureInstance {
            instance_custom_index_and_mask: Packed24_8::new(0, 0x00),
            ..AccelerationStructureInstance::default()
        };

        let prefix = packed_prefix(&instances, &[2, 5, 9], dummy).unwrap();
        assert_eq!(prefix.len(), 3);
        assert_eq!(prefix[0].instance_custom_index_and_mask.low_24(), 2);
        assert_eq!(prefix[1].instance_custom_index_and_mask.low_24(), 5);
        assert_eq!(prefix[2].instance_custom_index_and_mask.low_24(), 9);

        let prefix = packed_prefix(&instances, &[], dummy).unwrap();
        assert_eq!(prefix, vec![dummy]);
    }
}
