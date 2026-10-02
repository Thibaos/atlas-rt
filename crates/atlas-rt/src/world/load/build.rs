use dot_vox::DotVoxData;
use glam::IVec3;

use super::scene_graph::{SceneGraphTraverser, VoxelPlacement};
use crate::world::{
    BoundsPolicy, StoreKind, World, grid,
    store::region::{micro_chunk_key, region_slot},
};

/// Places every model's voxels into a serial store of the requested kind.
///
/// The scene-graph path stages each Region's voxels before writing them in
/// Micro-chunk ordinal and cell order, so each Micro-chunk is filled before
/// the next and its entry grows in place. A later placement at a shared cell
/// overwrites an earlier one because the stable stage keeps the scene order.
pub(in crate::world) fn load(
    voxel_data: &DotVoxData,
    policy: BoundsPolicy,
    store: StoreKind,
) -> (World, usize) {
    let mut world = World::empty(store);
    let clipped = load_into(&mut world, voxel_data, policy);

    (world, clipped)
}

pub(in crate::world) fn load_into(
    world: &mut World,
    voxel_data: &DotVoxData,
    policy: BoundsPolicy,
) -> usize {
    let mut loader = SceneGraphTraverser {
        world,
        policy,
        scene: voxel_data,
        models: vec![],
    };

    let clipped = loader.traverse();
    let models = std::mem::take(&mut loader.models);
    drop(loader);

    if models.is_empty() {
        return clipped;
    }

    let mut staged: Vec<Vec<(IVec3, u8)>> = (0..grid::REGION_COUNT).map(|_| Vec::new()).collect();
    let mut staged_clipped = 0usize;

    for (translation, rotation, size, voxels) in models {
        let placement = VoxelPlacement::new(translation, rotation, size);

        if policy == BoundsPolicy::Clip && placement.misses_lattice() {
            staged_clipped = staged_clipped.saturating_add(voxels.len());
            continue;
        }

        for voxel in voxels {
            let position = placement.place(*voxel);

            if grid::in_lattice(position) {
                if let Some(bucket) = staged.get_mut(region_slot(position)) {
                    bucket.push((position, voxel.i));
                }
            } else {
                match policy {
                    BoundsPolicy::Panic => grid::assert_in_lattice(position),
                    BoundsPolicy::Clip => staged_clipped = staged_clipped.saturating_add(1),
                }
            }
        }
    }

    for bucket in &mut staged {
        write_bucket(world, bucket);
    }

    clipped.saturating_add(staged_clipped)
}

fn write_bucket(world: &mut World, bucket: &mut Vec<(IVec3, u8)>) {
    if bucket.is_empty() {
        return;
    }

    bucket.sort_by_key(|(position, _)| micro_chunk_key(*position));

    for (position, material) in bucket.drain(..) {
        world.set_voxel(position, material);
    }
}
