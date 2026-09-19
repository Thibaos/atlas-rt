use std::{error::Error, fmt, fmt::Display};

use glam::IVec3;
use rustc_hash::FxHashSet;

use crate::world::{
    World,
    grid::{MICRO_CHUNK_LENGTH, grid_origin, in_lattice, region_index_in_lattice, region_index_of},
    update::{
        batch::{Batch, TrackedCoords, plan_edit},
        snapshot::MicroChunkSnapshot,
    },
};

const MICRO_EDGE: usize = 8;
const MICRO_AREA: usize = MICRO_EDGE * MICRO_EDGE;
const MICRO_CELLS: usize = MICRO_EDGE * MICRO_AREA;
const MICRO_BYTES: usize = MICRO_CELLS / MICRO_EDGE;

// the probe walk below indexes cells by hand, so the edge must be the lattice's
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
const _: [(); 1] = [(); (MICRO_CHUNK_LENGTH == MICRO_EDGE as u32) as usize];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoxelChange {
    Set(u8),
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoxelEdit {
    pub position: IVec3,
    pub change: VoxelChange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditReason {
    OutsideLattice,
    RegionOutsideLattice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditError {
    position: IVec3,
    reason: EditReason,
}

impl Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { position, reason } = self;

        match reason {
            EditReason::OutsideLattice => write!(f, "voxel {position} is outside the lattice"),
            EditReason::RegionOutsideLattice => {
                write!(
                    f,
                    "the region holding voxel {position} is outside the lattice"
                )
            }
        }
    }
}

impl Error for EditError {}

/// Mutates `world`, compiles every Micro-chunk the edits touch, and returns the
/// snapshots to submit with the tracked set they leave behind.
///
/// The edits are validated from the starting world before any of them is
/// applied, so a rejected batch leaves the world, the tracked set, and the
/// snapshots exactly as they were.
///
/// # Errors
///
/// Returns an [`EditError`] naming the position when an edit lies outside the
/// voxel lattice or its Micro-chunk lies outside the renderer lattice.
pub fn edit_world(
    world: &mut World,
    edits: &[VoxelEdit],
    tracked: &TrackedCoords,
) -> Result<Batch, EditError> {
    for edit in edits {
        validate(edit)?;
    }

    for edit in edits {
        match edit.change {
            VoxelChange::Set(material) => world.set_voxel(edit.position, material),
            VoxelChange::Clear => world.clear_voxel(edit.position),
        }
    }

    let snapshots: Vec<MicroChunkSnapshot> = chunks_touched(edits)
        .iter()
        .map(|origin| compile_chunk(world, *origin))
        .collect();

    Ok(plan_edit(snapshots, tracked))
}

fn validate(edit: &VoxelEdit) -> Result<(), EditError> {
    let fail = |reason| {
        Err(EditError {
            position: edit.position,
            reason,
        })
    };

    if !in_lattice(edit.position) {
        return fail(EditReason::OutsideLattice);
    }

    // today's voxel lattice implies the region lattice, so this arm is
    // unreachable; `submit_batch` asserts on an out-of-lattice region, so the
    // guard stays for the day the two bounds diverge
    if !region_index_in_lattice(region_index_of(edit.position)) {
        return fail(EditReason::RegionOutsideLattice);
    }

    Ok(())
}

/// The Micro-chunks the edits touch, in first-touch order and deduplicated.
fn chunks_touched(edits: &[VoxelEdit]) -> Vec<IVec3> {
    let mut seen: FxHashSet<IVec3> = FxHashSet::default();
    let mut touched: Vec<IVec3> = Vec::with_capacity(edits.len());

    for edit in edits {
        let origin = grid_origin(edit.position, MICRO_CHUNK_LENGTH);

        if seen.insert(origin) {
            touched.push(origin);
        }
    }

    touched
}

/// The chunk's content read back out of `world`, cell index `x + 8y + 64z` from
/// the origin, which is the order `ChunkBuf::into_snapshot` emits materials in.
fn compile_chunk(world: &World, origin: IVec3) -> MicroChunkSnapshot {
    let mut mask = [0u8; MICRO_BYTES];
    let mut materials = Vec::new();

    for index in 0..MICRO_CELLS {
        let offset = IVec3::new(
            i32::try_from(index % MICRO_EDGE).unwrap_or(0),
            i32::try_from((index / MICRO_EDGE) % MICRO_EDGE).unwrap_or(0),
            i32::try_from(index / MICRO_AREA).unwrap_or(0),
        );

        let Some(material) = world.material_at(&origin.saturating_add(offset)) else {
            continue;
        };

        let byte = index / MICRO_EDGE;

        if let Some(bits) = mask.get_mut(byte) {
            *bits |= 1u8 << (index % MICRO_EDGE);
        }

        materials.push(material);
    }

    MicroChunkSnapshot {
        global_coords: origin,
        mask,
        materials,
    }
}

#[cfg(test)]
mod tests {
    use glam::{IVec3, UVec3};

    use crate::world::{
        World,
        grid::{MICRO_CHUNK_LENGTH, grid_origin},
        placement_differential::{Rng, u8_below},
        update::{
            batch::TrackedCoords,
            snapshot::{MicroChunkSnapshot, emit_snapshots, tests::random_world},
        },
    };

    use super::{
        EditError, EditReason, MICRO_AREA, MICRO_BYTES, MICRO_CELLS, MICRO_EDGE, VoxelChange,
        VoxelEdit, edit_world,
    };

    #[allow(clippy::as_conversions)]
    const CHUNK: i32 = MICRO_CHUNK_LENGTH as i32;

    fn set(x: i32, y: i32, z: i32, material: u8) -> VoxelEdit {
        edit(x, y, z, VoxelChange::Set(material))
    }

    fn clear(x: i32, y: i32, z: i32) -> VoxelEdit {
        edit(x, y, z, VoxelChange::Clear)
    }

    fn edit(x: i32, y: i32, z: i32, change: VoxelChange) -> VoxelEdit {
        VoxelEdit {
            position: IVec3::new(x, y, z),
            change,
        }
    }

    /// One occupied cell at the origin, in the given material.
    fn cell(origin: IVec3, material: u8) -> MicroChunkSnapshot {
        let mut mask = [0u8; MICRO_BYTES];
        mask[0] = 1;

        MicroChunkSnapshot {
            global_coords: origin,
            mask,
            materials: vec![material],
        }
    }

    /// Rebuilds the snapshot from the world, in ascending material order, and
    /// requires it to equal what the snapshot holds.
    fn assert_self_consistent(world: &World, snapshot: &MicroChunkSnapshot) {
        let mut mask = [0u8; MICRO_BYTES];
        let mut materials = Vec::new();

        for index in 0..MICRO_CELLS {
            let local = UVec3::new(
                u32::try_from(index % MICRO_EDGE).unwrap_or(0),
                u32::try_from((index / MICRO_EDGE) % MICRO_EDGE).unwrap_or(0),
                u32::try_from(index / MICRO_AREA).unwrap_or(0),
            )
            .as_ivec3();

            let Some(material) = world.material_at(&snapshot.global_coords.saturating_add(local))
            else {
                continue;
            };

            if let Some(bits) = mask.get_mut(index / MICRO_EDGE) {
                *bits |= 1u8 << (index % MICRO_EDGE);
            }

            materials.push(material);
        }

        let rebuilt = MicroChunkSnapshot {
            global_coords: snapshot.global_coords,
            mask,
            materials,
        };

        assert_eq!(
            snapshot, &rebuilt,
            "chunk {} does not describe the world's content",
            snapshot.global_coords
        );
    }

    /// The chunk's compiled snapshot next to the one the emitter produces.
    fn compiled_against_emitted(
        world: &World,
        batch: &super::Batch,
        origin: IVec3,
    ) -> Option<(Option<MicroChunkSnapshot>, Option<MicroChunkSnapshot>)> {
        let compiled = batch
            .snapshots
            .iter()
            .find(|snapshot| snapshot.global_coords == origin)
            .cloned();

        let emitted = emit_snapshots(world)
            .ok()?
            .into_iter()
            .find(|snapshot| snapshot.global_coords == origin);

        Some((compiled, emitted))
    }

    /// Whether the compiled chunk is what the emitter holds for it. An empty
    /// chunk with no emitted snapshot counts as a match: that is a removal.
    fn matches_emit(compiled: &MicroChunkSnapshot, emitted: Option<&MicroChunkSnapshot>) -> bool {
        match emitted {
            Some(emitted) => compiled == emitted,
            None => compiled.occupied_count() == 0,
        }
    }

    fn assert_snapshot_matches(
        compiled: &MicroChunkSnapshot,
        emitted: Option<&MicroChunkSnapshot>,
        context: &str,
    ) {
        assert!(
            matches_emit(compiled, emitted),
            "{context}: chunk {} does not match the emitter's snapshot",
            compiled.global_coords
        );
    }

    fn assert_matches_emit(world: &World, batch: &super::Batch, context: &str) {
        let oracle = emit_snapshots(world).unwrap_or_else(|error| {
            panic!("{context}: the world did not emit: {error}");
        });

        for snapshot in &batch.snapshots {
            let emitted = oracle
                .iter()
                .find(|expected| expected.global_coords == snapshot.global_coords);

            assert_snapshot_matches(snapshot, emitted, context);
        }
    }

    #[test]
    fn a_set_creates_the_chunk_it_lands_in() {
        let mut world = World::default();
        let batch = edit_world(&mut world, &[set(0, 0, 0, 5)], &TrackedCoords::default()).unwrap();

        assert_eq!(batch.snapshots, vec![cell(IVec3::ZERO, 5)]);
        assert_eq!(
            batch.tracked,
            [IVec3::ZERO].into_iter().collect(),
            "the created chunk is tracked"
        );
    }

    #[test]
    fn clearing_the_last_voxel_of_a_chunk_emits_a_cleared_snapshot() {
        let mut world = World::default();
        let origin = IVec3::ZERO;
        let tracked: TrackedCoords = [origin].into_iter().collect();

        let batch = edit_world(&mut world, &[set(1, 2, 3, 7)], &tracked).unwrap();

        assert!(batch.tracked.contains(&origin));

        let batch = edit_world(&mut world, &[clear(1, 2, 3)], &batch.tracked).unwrap();

        assert_eq!(batch.snapshots, vec![MicroChunkSnapshot::cleared(origin)]);
        assert!(batch.tracked.is_empty(), "the emptied chunk is dropped");
        assert_eq!(world.voxel_count(), 0);
    }

    #[test]
    fn a_clear_of_a_position_the_world_never_held_clears_the_chunk() {
        let mut world = World::default();
        let batch = edit_world(&mut world, &[clear(0, 0, 0)], &TrackedCoords::default()).unwrap();

        assert_eq!(
            batch.snapshots,
            vec![MicroChunkSnapshot::cleared(IVec3::ZERO)],
            "the renderer drops a stale chunk on the empty snapshot"
        );
        assert!(batch.tracked.is_empty());
    }

    #[test]
    fn the_last_write_of_a_position_wins_in_input_order() {
        let origin = IVec3::new(24, -16, 8);
        let mut world = World::default();

        let batch =
            edit_world(&mut world, &[set(24, -16, 8, 9)], &TrackedCoords::default()).unwrap();

        assert_eq!(batch.snapshots, vec![cell(origin, 9)]);
        assert_eq!(batch.tracked, [origin].into_iter().collect());

        let batch = edit_world(
            &mut world,
            &[set(24, -16, 8, 9), clear(24, -16, 8)],
            &batch.tracked,
        )
        .unwrap();

        assert_eq!(
            batch.snapshots,
            vec![MicroChunkSnapshot::cleared(origin)],
            "set then clear leaves the chunk empty"
        );
        assert_eq!(world.voxel_count(), 0, "set then clear leaves it clear");

        let batch = edit_world(
            &mut world,
            &[clear(24, -16, 8), set(24, -16, 8, 3)],
            &batch.tracked,
        )
        .unwrap();

        assert_eq!(batch.snapshots, vec![cell(origin, 3)]);
        assert_eq!(batch.tracked, [origin].into_iter().collect());
        assert_eq!(world.get_voxel(&origin), Some(&3));
    }

    #[test]
    fn an_out_of_lattice_edit_rejects_the_whole_batch() {
        let mut world = World::default();
        world.set_voxel(IVec3::ZERO, 4);

        let tracked: TrackedCoords = [IVec3::ZERO].into_iter().collect();
        let before = emit_snapshots(&world).unwrap();

        // the tracked set is borrowed, so a rejection cannot change it
        let error: EditError =
            edit_world(&mut world, &[set(1, 0, 0, 8), set(2048, 0, 0, 8)], &tracked).unwrap_err();

        assert_eq!(
            error.to_string(),
            "voxel [2048, 0, 0] is outside the lattice",
            "the error names the offending position"
        );
        assert_eq!(world.voxel_count(), 1, "nothing was mutated");
        assert!(world.get_voxel(&IVec3::new(1, 0, 0)).is_none());
        assert_eq!(emit_snapshots(&world).unwrap(), before);
    }

    #[test]
    fn the_offending_position_is_named_in_the_error() {
        // the region arm is unreachable through `apply` while the voxel lattice
        // is the tighter bound, so the wording is checked on the error itself
        let error = EditError {
            position: IVec3::new(-1, 2, -3),
            reason: EditReason::RegionOutsideLattice,
        };

        assert_eq!(
            error.to_string(),
            "the region holding voxel [-1, 2, -3] is outside the lattice"
        );
    }

    #[test]
    fn materials_come_out_in_ascending_cell_index() {
        let mut world = World::default();

        let batch = edit_world(
            &mut world,
            &[
                set(0, 0, 1, 3),
                set(0, 1, 0, 4),
                set(0, 0, 0, 1),
                set(7, 0, 0, 2),
            ],
            &TrackedCoords::default(),
        )
        .unwrap();

        assert_eq!(batch.snapshots.len(), 1);

        let Some(snapshot) = batch.snapshots.first() else {
            panic!("the batch must carry the touched chunk");
        };

        assert_eq!(
            snapshot.materials,
            vec![1, 2, 4, 3],
            "the materials at cell indices 0, 7, 8 and 64, in that order"
        );
        assert_eq!(snapshot.occupied_count(), 4);
        assert_eq!(
            snapshot.mask.first().copied(),
            Some(0b1000_0001),
            "cells 0 and 7 land in the first mask byte"
        );
        assert_eq!(
            snapshot.mask.get(8).copied(),
            Some(1),
            "cell 64 lands in the ninth mask byte"
        );
    }

    #[test]
    fn a_batch_touching_many_chunks_pushes_one_snapshot_each() {
        let mut world = World::default();

        let batch = edit_world(
            &mut world,
            &[set(0, 0, 0, 1), set(8, 0, 0, 2), clear(16, 0, 0)],
            &TrackedCoords::default(),
        )
        .unwrap();

        assert_eq!(
            batch.snapshots.len(),
            3,
            "one snapshot per touched chunk, deduplicated"
        );
        assert_eq!(
            batch
                .snapshots
                .iter()
                .map(|snapshot| snapshot.global_coords)
                .collect::<Vec<_>>(),
            vec![
                IVec3::new(0, 0, 0),
                IVec3::new(8, 0, 0),
                IVec3::new(16, 0, 0),
            ],
            "first-touch order, which is the order the caller submits"
        );
        assert_eq!(batch.tracked.len(), 2, "the cleared chunk is not tracked");
    }

    fn random_edits(rng: &mut Rng, chunks: &[IVec3]) -> Vec<VoxelEdit> {
        let count = rng.below(12).saturating_add(1);

        (0..count)
            .map(|_| {
                let origin = chunks[rng.below(chunks.len() as u64) as usize];
                let local = UVec3::new(
                    u32::try_from(rng.below(8)).unwrap_or(0),
                    u32::try_from(rng.below(8)).unwrap_or(0),
                    u32::try_from(rng.below(8)).unwrap_or(0),
                )
                .as_ivec3();

                let change = if rng.below(4) == 0 {
                    VoxelChange::Clear
                } else {
                    VoxelChange::Set(u8_below(rng, 256))
                };

                VoxelEdit {
                    position: origin + local,
                    change,
                }
            })
            .collect()
    }

    /// Cluster origins well inside the lattice, so every cell of every chunk is
    /// a legal edit position.
    fn chunk_origins(rng: &mut Rng, world: &World) -> Vec<IVec3> {
        let mut chunks: Vec<IVec3> = world
            .iter_voxels()
            .map(|(position, _)| grid_origin(position, MICRO_CHUNK_LENGTH))
            .collect();
        chunks.sort_unstable_by_key(IVec3::to_array);
        chunks.dedup();

        while chunks.len() < 3 {
            let origin = IVec3::new(
                (rng.below(3) as i32 - 1).saturating_mul(CHUNK),
                (rng.below(3) as i32 - 1).saturating_mul(CHUNK),
                (rng.below(3) as i32 - 1).saturating_mul(CHUNK),
            );

            if !chunks.contains(&origin) {
                chunks.push(origin);
            }
        }

        chunks.truncate(8);
        chunks.sort_unstable_by_key(IVec3::to_array);
        chunks
    }

    #[test]
    fn compiled_snapshots_match_the_emitter_on_randomized_edits() {
        let mut rng = Rng::new(0x00ED_1701);

        for case in 0..48u32 {
            let mut world = random_world(&mut rng);
            let chunks = chunk_origins(&mut rng, &world);
            let edits = random_edits(&mut rng, &chunks);
            let context = format!("case {case}");

            // Some tracked chunks the edits never touch, so the set has to
            // survive a batch as well as follow one.
            let tracked: TrackedCoords = chunks
                .iter()
                .copied()
                .step_by(2)
                .chain([IVec3::new(64, 64, 64)])
                .collect();

            let batch = edit_world(&mut world, &edits, &tracked).unwrap();

            let mut compiled: Vec<IVec3> = batch
                .snapshots
                .iter()
                .map(|snapshot| snapshot.global_coords)
                .collect();
            compiled.sort_unstable_by_key(IVec3::to_array);

            let mut touched: Vec<IVec3> = edits
                .iter()
                .map(|edit| grid_origin(edit.position, MICRO_CHUNK_LENGTH))
                .collect();
            touched.sort_unstable_by_key(IVec3::to_array);
            touched.dedup();

            assert_eq!(
                compiled, touched,
                "{context}: the compiled chunks are not the touched chunks"
            );

            for snapshot in &batch.snapshots {
                assert_self_consistent(&world, snapshot);
            }

            let mut expected_tracked = tracked.clone();

            for snapshot in &batch.snapshots {
                if snapshot.occupied_count() == 0 {
                    expected_tracked.remove(&snapshot.global_coords);
                } else {
                    expected_tracked.insert(snapshot.global_coords);
                }
            }

            assert_eq!(
                batch.tracked, expected_tracked,
                "{context}: the tracked set does not follow from the snapshots"
            );

            assert_matches_emit(&world, &batch, &context);
        }
    }

    #[test]
    fn a_perturbed_snapshot_stops_matching_the_world() {
        let mut world = World::default();

        let batch = edit_world(
            &mut world,
            &[set(0, 0, 0, 3), set(7, 0, 0, 4)],
            &TrackedCoords::default(),
        )
        .unwrap();

        let Some((Some(compiled), Some(emitted))) =
            compiled_against_emitted(&world, &batch, IVec3::ZERO)
        else {
            panic!("the case needs a compiled and an emitted chunk");
        };

        assert!(
            matches_emit(&compiled, Some(&emitted)),
            "the unperturbed compile has to match"
        );

        let mut shifted = compiled.clone();

        if let Some(material) = shifted.materials.first_mut() {
            *material = material.wrapping_add(1);
        }

        assert!(
            !matches_emit(&shifted, Some(&emitted)),
            "one material shifted by a slot must fail the comparison"
        );

        let mut dropped = compiled;
        dropped.materials.pop();

        assert!(
            !matches_emit(&dropped, Some(&emitted)),
            "one material dropped must fail the comparison"
        );
    }

    #[test]
    fn compiling_a_settled_world_is_stable() {
        let mut rng = Rng::new(0x5E77_1ED);
        let mut world = random_world(&mut rng);
        let chunks = chunk_origins(&mut rng, &world);
        let edits = random_edits(&mut rng, &chunks);

        let first = edit_world(&mut world, &edits, &TrackedCoords::default()).unwrap();
        let second = edit_world(&mut world, &edits, &first.tracked).unwrap();

        assert_eq!(
            first.snapshots, second.snapshots,
            "the compile reads the world, not the edits"
        );
    }
}
