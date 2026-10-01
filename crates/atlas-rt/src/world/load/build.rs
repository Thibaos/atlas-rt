use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{Mutex, MutexGuard},
};

use dot_vox::{DotVoxData, Voxel};
use rayon::prelude::*;
use rustc_hash::FxBuildHasher;

use super::scene_graph::{SceneGraphTraverser, VoxelPlacement};
use crate::world::{
    BoundsPolicy, World, grid,
    store::sharded::{SHARD_COUNT, ShardedMap, VoxelMap, fold, shard_index},
};

const BUILD_CHUNK: usize = 8_192;

type StagedMap = HashMap<u64, u64, FxBuildHasher>;

pub(in crate::world) fn load(voxel_data: &DotVoxData, policy: BoundsPolicy) -> (World, usize) {
    let mut world = World::default();

    if voxel_data.scenes.is_empty() {
        let direct = voxel_data
            .models
            .iter()
            .map(|model| model.voxels.len())
            .sum();
        world.reserve(direct);
    }

    let mut loader = SceneGraphTraverser {
        world: &mut world,
        policy,
        scene: voxel_data,
        models: vec![],
    };

    let mut clipped = loader.traverse();

    let models = std::mem::take(&mut loader.models);

    let mut placements = Vec::with_capacity(models.len());
    let mut live = 0usize;

    for (translation, rotation, size, voxels) in models {
        let placement = VoxelPlacement::new(translation, rotation, size);

        if policy == BoundsPolicy::Clip && placement.misses_lattice() {
            clipped = clipped.saturating_add(voxels.len());
        } else {
            let attempts = voxels.len() as u64;
            let capacity = placement.in_lattice_capacity(attempts);

            live = live.saturating_add(usize::try_from(capacity).unwrap_or(usize::MAX));
            placements.push((placement, voxels));
        }
    }

    if placements.is_empty() || live == 0 {
        return (world, clipped);
    }

    let (shards, build_clipped) = build(&placements, live, policy);

    clipped = clipped.saturating_add(build_clipped);

    (
        World::from_store(Box::new(ShardedMap::from_shards(shards))),
        clipped,
    )
}

fn build(
    placements: &[(VoxelPlacement, &[Voxel])],
    live: usize,
    policy: BoundsPolicy,
) -> ([VoxelMap; SHARD_COUNT], usize) {
    let per_shard = live / SHARD_COUNT;
    let staged: Vec<Mutex<StagedMap>> = (0..SHARD_COUNT)
        .map(|_| {
            Mutex::new(StagedMap::with_capacity_and_hasher(
                per_shard,
                FxBuildHasher,
            ))
        })
        .collect();

    let clipped = placements
        .par_iter()
        .enumerate()
        .flat_map(|(model_index, (placement, voxels))| {
            let model_base = (model_index as u64).wrapping_mul(1u64 << 40);

            voxels
                .par_chunks(BUILD_CHUNK)
                .enumerate()
                .map(move |(chunk_index, chunk)| {
                    let chunk_base =
                        model_base.wrapping_add(chunk_index.wrapping_mul(BUILD_CHUNK) as u64);

                    (placement, chunk, chunk_base)
                })
        })
        .map(|(placement, chunk, sequence_base)| {
            stage_chunk(placement, chunk, sequence_base, policy, &staged)
        })
        .sum();

    let loaded: Vec<VoxelMap> = staged.into_par_iter().map(unstage_shard).collect();

    let shards = loaded
        .try_into()
        .unwrap_or_else(|_| panic!("staging produced a shard count other than {SHARD_COUNT}"));

    (shards, clipped)
}

fn stage_chunk(
    placement: &VoxelPlacement,
    chunk: &[Voxel],
    sequence_base: u64,
    policy: BoundsPolicy,
    staged: &[Mutex<StagedMap>],
) -> usize {
    let mut routed: Vec<Vec<(u64, u64)>> = (0..SHARD_COUNT).map(|_| Vec::new()).collect();
    let mut sequence = sequence_base;
    let mut clipped = 0usize;

    for voxel in chunk {
        let position = placement.place(*voxel);

        if grid::in_lattice(position) {
            let key = fold(position);
            let value = sequence.wrapping_mul(256) | u64::from(voxel.i);

            match routed.get_mut(shard_index(key)) {
                Some(bucket) => bucket.push((key, value)),
                None => panic!("shard route out of the {SHARD_COUNT} shards"),
            }
        } else {
            match policy {
                BoundsPolicy::Panic => grid::assert_in_lattice(position),
                BoundsPolicy::Clip => clipped = clipped.saturating_add(1),
            }
        }

        sequence = sequence.wrapping_add(1);
    }

    for (staged_map, bucket) in staged.iter().zip(&routed) {
        if bucket.is_empty() {
            continue;
        }

        let mut map = lock_stage(staged_map);

        for (key, value) in bucket {
            match map.entry(*key) {
                Entry::Vacant(entry) => {
                    entry.insert(*value);
                }
                Entry::Occupied(mut entry) => {
                    if *value > *entry.get() {
                        entry.insert(*value);
                    }
                }
            }
        }
    }

    clipped
}

fn lock_stage(staged: &Mutex<StagedMap>) -> MutexGuard<'_, StagedMap> {
    match staged.lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn unstage_shard(staged: Mutex<StagedMap>) -> VoxelMap {
    let map = match Mutex::into_inner(staged) {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };

    map.into_iter()
        .map(|(position, value)| {
            let [material, ..] = value.to_le_bytes();
            (position, material)
        })
        .collect()
}
