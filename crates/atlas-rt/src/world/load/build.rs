use dot_vox::{DotVoxData, Rotation, Voxel};
use glam::{IVec3, UVec3};
use rayon::prelude::*;

use super::scene_graph::{SceneGraphTraverser, VoxelPlacement};
use crate::world::{
    BoundsPolicy, StoreKind, World, grid,
    store::region::{Region, RegionStore},
};

type PlacedModel<'scene> = (IVec3, Rotation, UVec3, &'scene [Voxel]);

/// One model placement and the Region-index box its positions can reach.
struct Batch<'scene> {
    placement: VoxelPlacement,
    voxels: &'scene [Voxel],
    region_bounds: (IVec3, IVec3),
}

/// Places every model's voxels and reports the clipped count.
///
/// A scene graph is partitioned by Region: the 4096 Region slots are split
/// across worker threads, each slot has exactly one owner thread, and a Region
/// is written into its own blob rather than into a staged copy. Placements are
/// walked in serial order inside every Region, so a later placement at a shared
/// cell still wins. The in-lattice attempt count is summed before any Region
/// blob is allocated, and a load above `budget` is refused with that count
/// before the allocation.
pub(in crate::world) fn load(
    voxel_data: &DotVoxData,
    policy: BoundsPolicy,
    store: StoreKind,
    budget: usize,
) -> Result<(World, usize), usize> {
    if voxel_data.scenes.is_empty() {
        return load_without_scene(voxel_data, policy, store, budget);
    }

    let (traverse_clipped, models) = collect_models(voxel_data, policy);
    let mut clipped = traverse_clipped;
    let mut attempts = 0u64;
    let mut batches: Vec<Batch<'_>> = Vec::new();

    for (translation, rotation, size, voxels) in models {
        let placement = VoxelPlacement::new(translation, rotation, size);

        if policy == BoundsPolicy::Clip && placement.misses_lattice() {
            clipped = clipped.saturating_add(voxels.len());

            continue;
        }

        attempts = attempts.saturating_add(placement.in_lattice_capacity(voxels.len() as u64));
        batches.push(Batch {
            region_bounds: placement.region_bounds(),
            placement,
            voxels,
        });
    }

    clipped = clipped.saturating_add(clipped_voxels(&batches, policy));

    let attempts = usize::try_from(attempts).unwrap_or(usize::MAX);

    if batches.is_empty() || attempts == 0 {
        return Ok((World::empty(store), clipped));
    }

    if attempts > budget {
        return Err(attempts);
    }

    let world = match store {
        StoreKind::Region => World::from_store(Box::new(build_region(&batches))),
        #[cfg(feature = "map-oracle")]
        StoreKind::Map => build_map(batches),
    };

    Ok((world, clipped))
}

/// The no-scene path: every model is inserted straight through the traverser.
fn load_without_scene(
    voxel_data: &DotVoxData,
    policy: BoundsPolicy,
    store: StoreKind,
    budget: usize,
) -> Result<(World, usize), usize> {
    let attempts = voxel_data
        .models
        .iter()
        .map(|model| model.voxels.len())
        .fold(0usize, usize::saturating_add);

    if attempts > budget {
        return Err(attempts);
    }

    let mut world = World::empty(store);
    let mut loader = SceneGraphTraverser {
        world: &mut world,
        policy,
        scene: voxel_data,
        models: Vec::new(),
    };

    let clipped = loader.traverse();
    drop(loader);

    Ok((world, clipped))
}

fn collect_models(voxel_data: &DotVoxData, policy: BoundsPolicy) -> (usize, Vec<PlacedModel<'_>>) {
    let mut world = World::empty(StoreKind::Region);
    let mut loader = SceneGraphTraverser {
        world: &mut world,
        policy,
        scene: voxel_data,
        models: Vec::new(),
    };

    let clipped = loader.traverse();

    (clipped, std::mem::take(&mut loader.models))
}

/// Counts the voxels that leave the lattice, or asserts they do not, in
/// parallel over placements. A placement whose Region bounds are inside the
/// lattice contributes nothing.
fn clipped_voxels(batches: &[Batch<'_>], policy: BoundsPolicy) -> usize {
    batches
        .par_iter()
        .map(|batch| {
            if region_bounds_in_lattice(batch.region_bounds.0, batch.region_bounds.1) {
                return 0;
            }

            match policy {
                BoundsPolicy::Panic => {
                    for voxel in batch.voxels {
                        grid::assert_in_lattice(batch.placement.place(*voxel));
                    }

                    0
                }
                BoundsPolicy::Clip => batch
                    .voxels
                    .iter()
                    .filter(|voxel| !grid::in_lattice(batch.placement.place(**voxel)))
                    .count(),
            }
        })
        .sum()
}

/// Builds the Region store with one owner thread per Region slot. Each thread
/// scans the placements whose bounds cover its slot and writes the in-lattice
/// voxels that land in the slot, in placement order. No two threads write one
/// Region, so the writes need no lock.
fn build_region(batches: &[Batch<'_>]) -> RegionStore {
    let mut store = RegionStore::default();

    store
        .slots_mut()
        .par_iter_mut()
        .enumerate()
        .for_each(|(slot, slot_region)| {
            let region_index = grid::region_index_from_id(u32::try_from(slot).unwrap_or(0));

            for batch in batches {
                let (min, max) = batch.region_bounds;

                if !region_contains(&region_index, &min, &max) {
                    continue;
                }

                for voxel in batch.voxels {
                    let position = batch.placement.place(*voxel);

                    if grid::in_lattice(position) && grid::region_index_of(position) == region_index
                    {
                        slot_region
                            .get_or_insert_with(Region::new)
                            .set(position, voxel.i);
                    }
                }
            }
        });

    store.recount();

    store
}

#[cfg(feature = "map-oracle")]
fn build_map(batches: Vec<Batch<'_>>) -> World {
    let mut world = World::empty(StoreKind::Map);

    for batch in batches {
        for voxel in batch.voxels {
            let position = batch.placement.place(*voxel);

            if grid::in_lattice(position) {
                world.set_voxel(position, voxel.i);
            }
        }
    }

    world
}

fn region_contains(region: &IVec3, min: &IVec3, max: &IVec3) -> bool {
    region.cmpge(*min).all() && region.cmple(*max).all()
}

fn region_bounds_in_lattice(min: IVec3, max: IVec3) -> bool {
    grid::region_index_in_lattice(min) && grid::region_index_in_lattice(max)
}
