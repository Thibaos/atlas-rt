//! Randomized differential and property tests over both World stores.
//!
//! Compiled only under `map-oracle`, since the comparison needs the sharded map
//! beside the Region store. Every case runs the same input through both stores
//! and names the failing case in the assertion.

use dot_vox::DotVoxData;
use glam::IVec3;

use crate::world::{
    StoreKind, World,
    diff::{
        batch::TrackedCoords,
        edit::{VoxelChange, VoxelEdit, edit_world, projected_count},
        snapshot::{MicroChunkSnapshot, emit_snapshots},
    },
    grid::{MICRO_CHUNK_LENGTH, grid_origin},
    test_support::{Rng, assert_worlds_agree, random_specs, rotation_bytes, u8_below},
};

use super::{ModelSpec, scene_fixture};

const LOAD_SEED: u64 = 0x0701_10AD;
const DIRECT_SEED: u64 = 0x0702_DA7A;
const EDIT_SEED: u64 = 0x0703_ED17;
const EMIT_SEED: u64 = 0x0704_3A17;
const PROPERTY_SEED: u64 = 0x0705_9A0F;
const PROJECTION_SEED: u64 = 0x0706_9A0F;

fn scattered_position(rng: &mut Rng) -> IVec3 {
    let mut axis = || i32::try_from(rng.below(512)).unwrap_or(0).wrapping_sub(256);

    IVec3::new(axis(), axis(), axis())
}

/// A cell in one of 27 Micro-chunks clustered near the origin, so edits land in
/// shared Regions and exercise growth, rank, and the free list.
fn clustered_position(rng: &mut Rng) -> IVec3 {
    let chunk = IVec3::new(
        i32::try_from(rng.below(3)).unwrap_or(0).wrapping_sub(1),
        i32::try_from(rng.below(3)).unwrap_or(0).wrapping_sub(1),
        i32::try_from(rng.below(3)).unwrap_or(0).wrapping_sub(1),
    );
    let cell = IVec3::new(
        i32::try_from(rng.below(8)).unwrap_or(0),
        i32::try_from(rng.below(8)).unwrap_or(0),
        i32::try_from(rng.below(8)).unwrap_or(0),
    );

    chunk.saturating_mul(IVec3::splat(8)).saturating_add(cell)
}

fn random_specs_mixed(rng: &mut Rng, rotations: &[u8]) -> Vec<ModelSpec> {
    let first = rotations[rng.below(rotations.len() as u64) as usize];
    let mut specs = random_specs(rng, first);

    let second = rotations[rng.below(rotations.len() as u64) as usize];
    specs.extend(random_specs(rng, second));

    specs
}

/// Both stores built from the same in-memory scene: clipped counts and every
/// observable have to agree.
fn assert_loads_agree(data: &DotVoxData, case: &str) {
    let (region, region_clipped) = World::new_clipped_with_store(data, StoreKind::Region);
    let (map, map_clipped) = World::new_clipped_with_store(data, StoreKind::Map);

    assert_eq!(
        region_clipped, map_clipped,
        "{case}: region and map clipped counts"
    );
    assert_worlds_agree(&region, &map, case);
}

#[test]
fn randomized_loads_agree_across_stores() {
    let mut rng = Rng::new(LOAD_SEED);
    let rotations = rotation_bytes();

    for (index, rotation) in rotations.iter().enumerate() {
        for case in 0..8u32 {
            let specs = random_specs(&mut rng, *rotation);
            let data = scene_fixture(&specs);
            let context = format!(
                "load seed {LOAD_SEED:#x} rotation #{index} {rotation:#010b} case {case}: specs {specs:?}"
            );

            assert_loads_agree(&data, &context);
        }
    }

    for case in 0..48u32 {
        let specs = random_specs_mixed(&mut rng, &rotations);
        let data = scene_fixture(&specs);
        let context =
            format!("load seed {LOAD_SEED:#x} mixed rotations case {case}: specs {specs:?}");

        assert_loads_agree(&data, &context);
    }
}

fn fold_bounds(positions: impl Iterator<Item = IVec3>) -> Option<(IVec3, IVec3)> {
    positions.fold(None, |bounds, position| {
        Some(match bounds {
            Some((min, max)) => (min.min(position), max.max(position)),
            None => (position, position),
        })
    })
}

/// The same randomized writes applied to both stores. `clustered` chooses
/// shared Micro-chunks so the writes exercise compaction and the free list;
/// `clears` mixes clears into the sequence. The third value is the written
/// extremes, which equal the content's extremes only when `clears` is false.
fn build_worlds(
    rng: &mut Rng,
    clustered: bool,
    clears: bool,
) -> (World, World, Option<(IVec3, IVec3)>) {
    let mut region = World::empty(StoreKind::Region);
    let mut map = World::empty(StoreKind::Map);
    let mut written = Vec::new();
    let writes = 200 + rng.below(800) as usize;

    for _ in 0..writes {
        let position = if clustered {
            clustered_position(rng)
        } else {
            scattered_position(rng)
        };

        if clears && rng.below(4) == 0 {
            region.clear_voxel(position);
            map.clear_voxel(position);
        } else {
            let material = u8_below(rng, 256);

            region.set_voxel(position, material);
            map.set_voxel(position, material);
        }

        written.push(position);
    }

    (region, map, fold_bounds(written.into_iter()))
}

fn randomized_worlds(rng: &mut Rng, clustered: bool) -> (World, World) {
    let (region, map, _) = build_worlds(rng, clustered, true);

    (region, map)
}

#[test]
fn randomized_direct_writes_agree_across_stores() {
    let mut rng = Rng::new(DIRECT_SEED);

    for case in 0..32u32 {
        let clustered = case % 2 == 1;
        let (region, map) = randomized_worlds(&mut rng, clustered);
        let context =
            format!("direct-write seed {DIRECT_SEED:#x} case {case} (clustered: {clustered})");

        assert_worlds_agree(&region, &map, &context);
    }
}

/// Micro-chunk origins that cover the world's content and at least three fresh
/// chunks, all well inside the lattice so every cell of every chunk is a legal
/// edit position.
fn chunk_origins(rng: &mut Rng, world: &World) -> Vec<IVec3> {
    let mut chunks: Vec<IVec3> = world
        .iter_voxels()
        .map(|(position, _)| grid_origin(position, MICRO_CHUNK_LENGTH))
        .collect();

    chunks.sort_unstable_by_key(IVec3::to_array);
    chunks.dedup();

    while chunks.len() < 3 {
        let origin = IVec3::new(
            (rng.below(3) as i32 - 1).saturating_mul(MICRO_CHUNK_LENGTH as i32),
            (rng.below(3) as i32 - 1).saturating_mul(MICRO_CHUNK_LENGTH as i32),
            (rng.below(3) as i32 - 1).saturating_mul(MICRO_CHUNK_LENGTH as i32),
        );

        if !chunks.contains(&origin) {
            chunks.push(origin);
        }
    }

    chunks.sort_unstable_by_key(IVec3::to_array);
    chunks
}

fn random_mixed_edits(rng: &mut Rng, origins: &[IVec3]) -> Vec<VoxelEdit> {
    let count = rng.below(24).saturating_add(8) as usize;

    (0..count)
        .map(|_| {
            let origin = origins[rng.below(origins.len() as u64) as usize];
            let local = IVec3::new(
                rng.below(MICRO_CHUNK_LENGTH as u64) as i32,
                rng.below(MICRO_CHUNK_LENGTH as u64) as i32,
                rng.below(MICRO_CHUNK_LENGTH as u64) as i32,
            );

            let change = if rng.below(3) == 0 {
                VoxelChange::Clear
            } else {
                VoxelChange::Set(u8_below(rng, 256))
            };

            VoxelEdit {
                position: origin.saturating_add(local),
                change,
            }
        })
        .collect()
}

#[test]
fn randomized_edit_batches_agree_across_stores() {
    let mut rng = Rng::new(EDIT_SEED);

    for case in 0..32u32 {
        let clustered = case % 2 == 1;
        let (mut region, mut map) = randomized_worlds(&mut rng, clustered);
        let origins = chunk_origins(&mut rng, &region);
        let edits = random_mixed_edits(&mut rng, &origins);

        let tracked: TrackedCoords = origins
            .iter()
            .copied()
            .step_by(2)
            .chain([IVec3::new(64, 64, 64)])
            .collect();

        let context =
            format!("edit-batch seed {EDIT_SEED:#x} case {case} (clustered: {clustered})");

        let region_batch = edit_world(&mut region, &edits, &tracked)
            .unwrap_or_else(|error| panic!("{context}: region batch rejected: {error}"));
        let map_batch = edit_world(&mut map, &edits, &tracked)
            .unwrap_or_else(|error| panic!("{context}: map batch rejected: {error}"));

        assert_eq!(
            region_batch.snapshots, map_batch.snapshots,
            "{context}: compiled Snapshots"
        );
        assert_eq!(
            region_batch.tracked, map_batch.tracked,
            "{context}: Tracked sets"
        );
        assert_worlds_agree(&region, &map, &context);
    }
}

/// The projection's count has to equal the count the same edits leave in the
/// World, on both stores: the Region store folds per Micro-chunk mask, the map
/// store falls back to the per-position overlay. Randomized mixed edits with
/// repeats exercise both paths.
#[test]
fn randomized_projections_match_the_applied_count_across_stores() {
    let mut rng = Rng::new(PROJECTION_SEED);

    for case in 0..32u32 {
        let clustered = case % 2 == 1;
        let (mut region, mut map) = randomized_worlds(&mut rng, clustered);
        let origins = chunk_origins(&mut rng, &region);
        let edits = random_mixed_edits(&mut rng, &origins);
        let context =
            format!("projection seed {PROJECTION_SEED:#x} case {case} (clustered: {clustered})");

        let region_projected = projected_count(&region, &edits);
        let map_projected = projected_count(&map, &edits);

        assert_eq!(
            region_projected, map_projected,
            "{context}: projected counts differ across stores"
        );

        let tracked: TrackedCoords = origins.iter().copied().step_by(2).collect();

        let _region = edit_world(&mut region, &edits, &tracked)
            .unwrap_or_else(|error| panic!("{context}: region batch rejected: {error}"));
        let _map = edit_world(&mut map, &edits, &tracked)
            .unwrap_or_else(|error| panic!("{context}: map batch rejected: {error}"));

        assert_eq!(
            region.voxel_count(),
            region_projected,
            "{context}: the projection did not match the applied count"
        );
        assert_eq!(
            map.voxel_count(),
            map_projected,
            "{context}: the map projection did not match the applied count"
        );
    }
}

#[test]
fn randomized_emissions_agree_across_stores() {
    let mut rng = Rng::new(EMIT_SEED);

    for case in 0..32u32 {
        let clustered = case % 2 == 1;
        let (region, map) = randomized_worlds(&mut rng, clustered);
        let context = format!("emission seed {EMIT_SEED:#x} case {case} (clustered: {clustered})");

        let region_snapshots = emit_snapshots(&region)
            .unwrap_or_else(|error| panic!("{context}: region emission: {error}"));
        let map_snapshots =
            emit_snapshots(&map).unwrap_or_else(|error| panic!("{context}: map emission: {error}"));

        assert_eq!(
            region_snapshots, map_snapshots,
            "{context}: emitted Snapshots"
        );
    }
}

/// The cell offsets of a Micro-chunk in the `x + 8y + 64z` order the mask and
/// materials both follow.
fn cell_offset(index: usize) -> IVec3 {
    IVec3::new(
        i32::try_from(index % 8).unwrap_or(0),
        i32::try_from((index / 8) % 8).unwrap_or(0),
        i32::try_from(index / 64).unwrap_or(0),
    )
}

fn assert_properties(world: &World, written: Option<(IVec3, IVec3)>, case: &str) {
    let snapshots =
        emit_snapshots(world).unwrap_or_else(|error| panic!("{case}: emission failed: {error}"));

    let occupied: usize = snapshots
        .iter()
        .map(MicroChunkSnapshot::occupied_count)
        .sum();

    assert_eq!(
        occupied,
        world.voxel_count(),
        "{case}: snapshot occupied counts do not sum to the voxel count"
    );

    for snapshot in &snapshots {
        assert_eq!(
            snapshot.materials.len(),
            snapshot.occupied_count(),
            "{case}: chunk {} material count differs from the mask popcount",
            snapshot.global_coords
        );

        let mut next_material = 0usize;

        for index in 0..512 {
            let set = snapshot.mask[index / 8] & (1u8 << (index % 8)) != 0;
            let position = snapshot.global_coords.saturating_add(cell_offset(index));

            if set {
                let material = snapshot.materials.get(next_material).copied();

                assert_eq!(
                    material,
                    world.get_voxel(&position),
                    "{case}: chunk {} set bit at cell {index} does not carry the world's Material",
                    snapshot.global_coords
                );
                next_material = next_material.saturating_add(1);
            } else {
                assert_eq!(
                    world.get_voxel(&position),
                    None,
                    "{case}: chunk {} unset bit at cell {index} is occupied in the world",
                    snapshot.global_coords
                );
            }
        }
    }

    let iterated = fold_bounds(world.iter_voxels().map(|(position, _)| position));

    assert_eq!(
        iterated, written,
        "{case}: iterated bounds differ from the written extremes"
    );
    assert_eq!(
        world.voxel_bounds(),
        written,
        "{case}: voxel_bounds differs from the written extremes"
    );
}

#[test]
fn randomized_worlds_satisfy_the_properties_on_both_stores() {
    let mut rng = Rng::new(PROPERTY_SEED);

    for case in 0..32u32 {
        let clustered = case % 2 == 1;
        let (region, map, written) = build_worlds(&mut rng, clustered, false);
        let context =
            format!("property seed {PROPERTY_SEED:#x} case {case} (clustered: {clustered})");

        assert_properties(&region, written, &context);
        assert_properties(&map, written, &context);
        assert_worlds_agree(&region, &map, &context);
    }
}
