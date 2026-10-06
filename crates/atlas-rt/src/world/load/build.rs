use dot_vox::{DotVoxData, Rotation, Voxel};
use glam::{IVec3, UVec3};
use rayon::prelude::*;

use super::scene_graph::{SceneGraphTraverser, VoxelPlacement};
use crate::world::{
    World, grid,
    store::region::{Region, RegionStore},
};

type PlacedModel<'scene> = (IVec3, Rotation, UVec3, &'scene [Voxel]);

/// One model placement and the Region-index box its positions can reach.
struct Batch<'scene> {
    placement: VoxelPlacement,
    voxels: &'scene [Voxel],
    region_bounds: (IVec3, IVec3),
}

/// A build before its Region blobs are allocated: the placements left to
/// write, the clips already counted, and the cells the build will attempt.
enum Plan<'scene> {
    Batches {
        batches: Vec<Batch<'scene>>,
        clipped: usize,
        attempts: usize,
    },
    SceneLess {
        scene: &'scene DotVoxData,
        attempts: usize,
    },
}

/// Places every model's voxels and reports the clipped count, refusing a
/// placement count above `budget` before any Region blob is allocated.
///
/// A scene graph is partitioned by Region: the 4096 Region slots are split
/// across worker threads, each slot has exactly one owner thread, and a Region
/// is written into its own blob rather than into a staged copy. Placements are
/// walked in serial order inside every Region, so a later placement at a shared
/// cell still wins. The in-lattice attempt count is summed before any Region
/// blob is allocated, and a load above `budget` is refused with that count
/// before the allocation.
///
/// # Errors
///
/// Returns the cell count the build would attempt when it is above `budget`.
pub(in crate::world) fn load(
    voxel_data: &DotVoxData,
    budget: usize,
) -> Result<(World, usize), usize> {
    let plan = Plan::new(voxel_data);
    let attempts = plan.attempts();

    if attempts > budget {
        return Err(attempts);
    }

    Ok(plan.into_world())
}

/// The same placement with no cell budget, for a constructor that reads none.
pub(in crate::world) fn load_unbudgeted(voxel_data: &DotVoxData) -> (World, usize) {
    Plan::new(voxel_data).into_world()
}

impl<'scene> Plan<'scene> {
    fn new(voxel_data: &'scene DotVoxData) -> Self {
        if voxel_data.scenes.is_empty() {
            let attempts = voxel_data
                .models
                .iter()
                .map(|model| model.voxels.len())
                .fold(0usize, usize::saturating_add);

            return Self::SceneLess {
                scene: voxel_data,
                attempts,
            };
        }

        let (clipped, models) = collect_models(voxel_data);
        let mut clipped = clipped;
        let mut attempts = 0u64;
        let mut batches: Vec<Batch<'_>> = Vec::new();

        for (translation, rotation, size, voxels) in models {
            let placement = VoxelPlacement::new(translation, rotation, size);

            if placement.misses_lattice() {
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

        clipped = clipped.saturating_add(clipped_voxels(&batches));

        Self::Batches {
            batches,
            clipped,
            attempts: usize::try_from(attempts).unwrap_or(usize::MAX),
        }
    }

    const fn attempts(&self) -> usize {
        match self {
            Self::Batches { attempts, .. } | Self::SceneLess { attempts, .. } => *attempts,
        }
    }

    fn into_world(self) -> (World, usize) {
        match self {
            Self::SceneLess { scene, .. } => {
                let mut world = World::empty();
                let mut loader = SceneGraphTraverser {
                    world: &mut world,
                    scene,
                    models: Vec::new(),
                };

                let clipped = loader.traverse();
                drop(loader);

                (world, clipped)
            }
            Self::Batches {
                batches,
                clipped,
                attempts,
            } => {
                if batches.is_empty() || attempts == 0 {
                    return (World::empty(), clipped);
                }

                let world = World::from_store(Box::new(build_region(&batches)));

                (world, clipped)
            }
        }
    }
}

fn collect_models(voxel_data: &DotVoxData) -> (usize, Vec<PlacedModel<'_>>) {
    let mut world = World::empty();
    let mut loader = SceneGraphTraverser {
        world: &mut world,
        scene: voxel_data,
        models: Vec::new(),
    };

    let clipped = loader.traverse();

    (clipped, std::mem::take(&mut loader.models))
}

/// Counts the voxels that leave the lattice, in parallel over placements. A
/// placement whose Region bounds are inside the lattice contributes nothing.
fn clipped_voxels(batches: &[Batch<'_>]) -> usize {
    batches
        .par_iter()
        .map(|batch| {
            if region_bounds_in_lattice(batch.region_bounds.0, batch.region_bounds.1) {
                return 0;
            }

            batch
                .voxels
                .iter()
                .filter(|voxel| !grid::in_lattice(batch.placement.place(**voxel)))
                .count()
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

fn region_contains(region: &IVec3, min: &IVec3, max: &IVec3) -> bool {
    region.cmpge(*min).all() && region.cmple(*max).all()
}

fn region_bounds_in_lattice(min: IVec3, max: IVec3) -> bool {
    grid::region_index_in_lattice(min) && grid::region_index_in_lattice(max)
}
