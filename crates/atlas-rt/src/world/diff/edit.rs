use std::{error::Error, fmt, fmt::Display};

use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::world::{
    World,
    budget::cell_budget,
    diff::{
        batch::{Batch, TrackedCoords, plan_edit},
        snapshot::MicroChunkSnapshot,
    },
    grid::{MICRO_CHUNK_LENGTH, grid_origin, in_lattice, region_index_in_lattice, region_index_of},
};

pub const MICRO_EDGE: usize = MICRO_CHUNK_LENGTH as usize;
pub const MICRO_AREA: usize = MICRO_EDGE * MICRO_EDGE;
pub const MICRO_CELLS: usize = MICRO_EDGE * MICRO_AREA;
pub const MICRO_BYTES: usize = MICRO_CELLS / MICRO_EDGE;

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

impl VoxelEdit {
    /// Whether the edit disagrees with `world`: a `Set` of the material the
    /// cell already holds and a `Clear` of an empty cell agree and resolve
    /// as no-ops. An edit outside the lattice always disagrees, so
    /// validation still gets its turn.
    #[must_use]
    pub fn disagrees_with(&self, world: &World) -> bool {
        if !in_lattice(self.position) {
            return true;
        }

        let held = world.get_voxel(&self.position);

        match self.change {
            VoxelChange::Set(material) => held != Some(material),
            VoxelChange::Clear => held.is_some(),
        }
    }
}

/// A Micro-chunk as a host commands it.
///
/// The origin, the occupancy mask, and the material indices of the occupied
/// cells in ascending cell-index order. Diffed against the World at commit,
/// so a later write never derives a no-op against a World that lacks an
/// earlier one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroChunkEdit {
    pub origin: IVec3,
    pub mask: [u8; MICRO_BYTES],
    pub materials: Vec<u8>,
}

impl MicroChunkEdit {
    /// The edits that turn `world`'s copy of this Micro-chunk into the raw
    /// chunk, in ascending cell-index order. A cell already holding what the
    /// chunk asks for contributes nothing.
    ///
    /// # Errors
    ///
    /// Returns an [`EditError`] naming the origin when it sits outside the
    /// lattice or off the Micro-chunk grid, and when the materials do not
    /// match the mask's occupancy.
    pub fn diff(&self, world: &World) -> Result<Vec<VoxelEdit>, EditError> {
        validate_chunk(self)?;

        let mut edits = Vec::new();
        let mut next_material = 0usize;

        for index in 0..MICRO_CELLS {
            let occupied = self
                .mask
                .get(index / MICRO_EDGE)
                .is_some_and(|byte| byte & (1u8 << (index % MICRO_EDGE)) != 0);

            let incoming = if occupied {
                let material = self.materials.get(next_material).copied();
                next_material = next_material.saturating_add(1);
                material
            } else {
                None
            };

            let position = self.origin.saturating_add(cell_offset(index));
            let current = world.get_voxel(&position);

            match (incoming, current) {
                (Some(material), Some(existing)) if material == existing => {}
                (Some(material), _) => edits.push(VoxelEdit {
                    position,
                    change: VoxelChange::Set(material),
                }),
                (None, Some(_)) => edits.push(VoxelEdit {
                    position,
                    change: VoxelChange::Clear,
                }),
                (None, None) => {}
            }
        }

        Ok(edits)
    }
}

/// The chunk-wide checks a diff needs before it may read the World: the
/// origin sits in the lattice on the Micro-chunk grid, which puts every cell
/// it covers in the lattice too, and the materials match the occupancy.
fn validate_chunk(chunk: &MicroChunkEdit) -> Result<(), EditError> {
    let position = chunk.origin;

    if !in_lattice(position) {
        return Err(EditError::rejected(position, EditReason::OutsideLattice));
    }

    if grid_origin(position, MICRO_CHUNK_LENGTH) != position {
        return Err(EditError::rejected(position, EditReason::NotChunkOrigin));
    }

    let occupied: usize = chunk
        .mask
        .iter()
        .map(|byte| byte.count_ones() as usize)
        .sum();

    if occupied != chunk.materials.len() {
        return Err(EditError::rejected(
            position,
            EditReason::MaterialCount {
                occupied,
                given: chunk.materials.len(),
            },
        ));
    }

    Ok(())
}

/// The cell at `index` of the `x + 8y + 64z` walk a Micro-chunk's mask and
/// materials both follow.
fn cell_offset(index: usize) -> IVec3 {
    IVec3::new(
        i32::try_from(index % MICRO_EDGE).unwrap_or(0),
        i32::try_from((index / MICRO_EDGE) % MICRO_EDGE).unwrap_or(0),
        i32::try_from(index / MICRO_AREA).unwrap_or(0),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditReason {
    OutsideLattice,
    RegionOutsideLattice,
    NotChunkOrigin,
    MaterialCount { occupied: usize, given: usize },
}

/// Why a Voxel edit batch was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Rejected { position: IVec3, reason: EditReason },
    OverBudget { count: usize, limit: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditError(Refusal);

impl EditError {
    const fn rejected(position: IVec3, reason: EditReason) -> Self {
        Self(Refusal::Rejected { position, reason })
    }

    const fn over_budget(count: usize, limit: usize) -> Self {
        Self(Refusal::OverBudget { count, limit })
    }
}

impl Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Refusal::Rejected { position, reason } => match reason {
                EditReason::OutsideLattice => write!(f, "voxel {position} is outside the lattice"),
                EditReason::RegionOutsideLattice => {
                    write!(
                        f,
                        "the region holding voxel {position} is outside the lattice"
                    )
                }
                EditReason::NotChunkOrigin => {
                    write!(f, "voxel {position} is not a Micro-chunk origin")
                }
                EditReason::MaterialCount { occupied, given } => {
                    write!(
                        f,
                        "the mask marks {occupied} cells but carries {given} materials"
                    )
                }
            },
            Refusal::OverBudget { count, limit } => write!(
                f,
                "the batch would leave {count} cells, above the cell budget of {limit}"
            ),
        }
    }
}

impl Error for EditError {}

/// Mutates `world`, compiles every Micro-chunk the edits touch, and returns the
/// snapshots to submit with the tracked set they leave behind.
///
/// The edits are validated, and the batch is checked against the World's cell
/// budget, from the starting world before any of them is applied, so a refused
/// batch leaves the world, the tracked set, and the snapshots exactly as they
/// were.
///
/// # Errors
///
/// Returns an [`EditError`] naming the position when an edit lies outside the
/// voxel lattice or its Micro-chunk lies outside the renderer lattice, and
/// naming the count when the batch would carry the World past its cell budget.
pub fn edit_world(
    world: &mut World,
    edits: &[VoxelEdit],
    tracked: &TrackedCoords,
) -> Result<Batch, EditError> {
    for edit in edits {
        validate(edit)?;
    }

    let limit = cell_budget();

    if limit != usize::MAX {
        let projected = projected_count(world, edits);

        if projected > limit {
            return Err(EditError::over_budget(projected, limit));
        }
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

/// The World's count after `edits` apply, without mutating it.
///
/// Each touched Micro-chunk folds its edits onto a copy of its Occupancy mask,
/// and the result is the starting count plus the sum of the chunks' popcount
/// deltas. A repeated position is idempotent against the mask. A chunk the
/// store keeps no entry for falls back to a per-position overlay.
pub(in crate::world) fn projected_count(world: &World, edits: &[VoxelEdit]) -> usize {
    let mut projected = world.voxel_count();
    let mut masks: FxHashMap<IVec3, ([u8; MICRO_BYTES], usize)> = FxHashMap::default();
    let mut fallback: FxHashMap<IVec3, bool> = FxHashMap::default();
    let mut fallback_chunks: FxHashSet<IVec3> = FxHashSet::default();

    for edit in edits {
        let origin = grid_origin(edit.position, MICRO_CHUNK_LENGTH);

        if let Some((mask, _)) = masks.get_mut(&origin) {
            set_mask_bit(mask, edit.position.saturating_sub(origin), edit.change);
            continue;
        }

        if fallback_chunks.contains(&origin) {
            fold(&mut fallback, &mut projected, world, edit);
            continue;
        }

        let entry = world.chunk_entry(origin);
        let bytes = entry
            .as_ref()
            .and_then(|entry| <&[u8; MICRO_BYTES]>::try_from(entry.mask).ok());

        if let (Some(entry), Some(bytes)) = (entry.as_ref(), bytes) {
            let mut mask = *bytes;
            let before_cells = entry.materials.len();
            set_mask_bit(&mut mask, edit.position.saturating_sub(origin), edit.change);
            masks.insert(origin, (mask, before_cells));
        } else {
            fallback_chunks.insert(origin);
            fold(&mut fallback, &mut projected, world, edit);
        }
    }

    for (mask, before_cells) in masks.values() {
        let after_cells: usize = mask.iter().map(|byte| byte.count_ones() as usize).sum();
        projected = projected
            .saturating_add(after_cells)
            .saturating_sub(*before_cells);
    }

    projected
}

fn fold(
    pending: &mut FxHashMap<IVec3, bool>,
    projected: &mut usize,
    world: &World,
    edit: &VoxelEdit,
) {
    let occupied = *pending
        .entry(edit.position)
        .or_insert_with(|| world.contains(&edit.position));

    match edit.change {
        VoxelChange::Set(_) => {
            if !occupied {
                *projected = projected.saturating_add(1);
            }

            pending.insert(edit.position, true);
        }
        VoxelChange::Clear => {
            if occupied {
                *projected = projected.saturating_sub(1);
            }

            pending.insert(edit.position, false);
        }
    }
}

fn mask_index(local: IVec3) -> usize {
    let x = usize::try_from(local.x).unwrap_or(0);
    let y = usize::try_from(local.y).unwrap_or(0);
    let z = usize::try_from(local.z).unwrap_or(0);

    x.strict_add(y.strict_mul(MICRO_EDGE))
        .strict_add(z.strict_mul(MICRO_AREA))
}

fn set_mask_bit(mask: &mut [u8; MICRO_BYTES], local: IVec3, change: VoxelChange) {
    let index = mask_index(local);

    if let Some(slot) = mask.get_mut(index / MICRO_EDGE) {
        let bit = 1u8 << (index % MICRO_EDGE);

        match change {
            VoxelChange::Set(_) => *slot |= bit,
            VoxelChange::Clear => *slot &= !bit,
        }
    }
}

/// The refusal-check shapes the decision measured against the installed
/// per-Micro-chunk fold: the original per-position overlay, a sorted fold, and
/// an apply-then-rollback. `budget_projection_timings` compares their counts
/// against [`projected_count`] and times them.
#[cfg(test)]
pub(in crate::world) mod projection_candidates {
    use glam::IVec3;
    use rustc_hash::FxHashMap;

    use crate::world::{
        World,
        diff::edit::{VoxelChange, VoxelEdit},
    };

    use super::fold;

    /// The per-position overlay `edit_world` ran before the per-Micro-chunk fold
    /// was installed.
    pub(in crate::world) fn overlay(world: &World, edits: &[VoxelEdit]) -> usize {
        let mut projected = world.voxel_count();
        let mut pending: FxHashMap<IVec3, bool> = FxHashMap::default();

        for edit in edits {
            fold(&mut pending, &mut projected, world, edit);
        }

        projected
    }

    /// Sorts the edits by position, so the last edit of a position decides its
    /// final occupancy and the World is read once per distinct position.
    pub(in crate::world) fn sorted(world: &World, edits: &[VoxelEdit]) -> usize {
        let mut order: Vec<(IVec3, u32)> = edits
            .iter()
            .enumerate()
            .map(|(index, edit)| (edit.position, u32::try_from(index).unwrap_or(u32::MAX)))
            .collect();

        order.sort_unstable_by_key(|&(position, index)| (position.to_array(), index));

        let mut projected = world.voxel_count();
        let mut start = 0;

        while start < order.len() {
            let position = order[start].0;
            let mut end = start;

            while end < order.len() && order[end].0 == position {
                end += 1;
            }

            let last = edits[order[end - 1].1 as usize].change;
            let occupied = world.contains(&position);

            match last {
                VoxelChange::Set(_) if !occupied => projected = projected.saturating_add(1),
                VoxelChange::Clear if occupied => projected = projected.saturating_sub(1),
                _ => {}
            }

            start = end;
        }

        projected
    }

    /// Applies the batch in place, reads the maintained cell counter, and
    /// restores every touched position from its captured original. The bench
    /// rolls back unconditionally so the World it measures against is
    /// unchanged; production would roll back only on a refusal.
    pub(in crate::world) fn apply_then_rollback(
        world: &mut World,
        edits: &[VoxelEdit],
    ) -> usize {
        let mut originals: Vec<Option<u8>> = Vec::with_capacity(edits.len());

        for edit in edits {
            originals.push(world.get_voxel(&edit.position));

            match edit.change {
                VoxelChange::Set(material) => world.set_voxel(edit.position, material),
                VoxelChange::Clear => world.clear_voxel(edit.position),
            }
        }

        let projected = world.voxel_count();

        for (edit, original) in edits.iter().zip(originals).rev() {
            match original {
                Some(material) => world.set_voxel(edit.position, material),
                None => world.clear_voxel(edit.position),
            }
        }

        projected
    }
}

fn validate(edit: &VoxelEdit) -> Result<(), EditError> {
    let fail = |reason| Err(EditError::rejected(edit.position, reason));

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
pub(in crate::world) fn chunks_touched(edits: &[VoxelEdit]) -> Vec<IVec3> {
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

/// The chunk's content as `world` holds it, cell index `x + 8y + 64z` from the
/// origin, which is the order `ChunkBuf::into_snapshot` emits materials in. A
/// store that keeps one entry per Micro-chunk copies its mask and compacted
/// materials straight out; a store with no entry shape probes every cell.
pub(in crate::world) fn compile_chunk(world: &World, origin: IVec3) -> MicroChunkSnapshot {
    if let Some(entry) = world.chunk_entry(origin) {
        let Ok(mask) = <&[u8; MICRO_BYTES]>::try_from(entry.mask) else {
            return probe_chunk(world, origin);
        };

        return MicroChunkSnapshot {
            global_coords: origin,
            mask: *mask,
            materials: entry.materials.to_vec(),
        };
    }

    probe_chunk(world, origin)
}

/// The compile for a store with no Micro-chunk entry: 512 `get_voxel` probes,
/// one per cell, assembling the mask and the materials in cell order.
pub(in crate::world) fn probe_chunk(world: &World, origin: IVec3) -> MicroChunkSnapshot {
    let mut mask = [0u8; MICRO_BYTES];
    let mut materials = Vec::new();

    for index in 0..MICRO_CELLS {
        let Some(material) = world.get_voxel(&origin.saturating_add(cell_offset(index))) else {
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
        budget::set_cell_budget,
        diff::{
            batch::TrackedCoords,
            snapshot::{MicroChunkSnapshot, emit_snapshots, tests::random_world},
        },
        grid::{MICRO_CHUNK_LENGTH, grid_origin},
        test_support::{Rng, u8_below},
    };

    use super::{
        EditError, EditReason, MICRO_AREA, MICRO_BYTES, MICRO_CELLS, MICRO_EDGE, VoxelChange,
        VoxelEdit, edit_world,
    };

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

            let Some(material) = world.get_voxel(&snapshot.global_coords.saturating_add(local))
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
        assert_eq!(world.get_voxel(&origin), Some(3));
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
    fn a_batch_past_the_budget_is_refused_whole() {
        let mut world = World::default();
        world.set_voxel(IVec3::ZERO, 4);

        let tracked: TrackedCoords = [IVec3::ZERO].into_iter().collect();
        let before = emit_snapshots(&world).unwrap();
        let _budget = set_cell_budget(1);

        let error = edit_world(&mut world, &[set(1, 0, 0, 8)], &tracked).unwrap_err();

        assert_eq!(
            error,
            EditError::over_budget(2, 1),
            "the failure names the projected count and the limit"
        );
        assert!(
            error.to_string().contains("2 cells"),
            "the message names the refused count"
        );
        assert_eq!(world.voxel_count(), 1, "the world was not mutated");
        assert!(world.get_voxel(&IVec3::new(1, 0, 0)).is_none());
        assert_eq!(
            emit_snapshots(&world).unwrap(),
            before,
            "the emission is unchanged"
        );

        // tracked is only borrowed, so a refusal cannot change it
        assert_eq!(tracked, [IVec3::ZERO].into_iter().collect());
    }

    #[test]
    fn a_batch_at_the_budget_applies() {
        let mut world = World::default();
        world.set_voxel(IVec3::ZERO, 4);

        let _budget = set_cell_budget(2);

        let batch = edit_world(&mut world, &[set(1, 0, 0, 8)], &TrackedCoords::default()).unwrap();

        assert_eq!(world.voxel_count(), 2, "the batch lands at the limit");
        assert_eq!(world.get_voxel(&IVec3::new(1, 0, 0)), Some(8));
        assert_eq!(batch.tracked, [IVec3::ZERO].into_iter().collect());
    }

    #[test]
    fn a_batch_that_stays_within_the_budget_by_clearing_applies() {
        let mut world = World::default();
        world.set_voxel(IVec3::ZERO, 4);

        let _budget = set_cell_budget(1);

        let batch = edit_world(
            &mut world,
            &[set(1, 0, 0, 5), clear(0, 0, 0)],
            &TrackedCoords::default(),
        )
        .unwrap();

        assert_eq!(world.voxel_count(), 1, "the clear offsets the set");
        assert_eq!(world.get_voxel(&IVec3::new(1, 0, 0)), Some(5));
        assert!(world.get_voxel(&IVec3::ZERO).is_none());
        assert_eq!(batch.tracked, [IVec3::ZERO].into_iter().collect());
    }

    #[test]
    fn overwrites_and_repeated_positions_project_the_net_count() {
        let mut world = World::default();
        world.set_voxel(IVec3::ZERO, 4);

        let _budget = set_cell_budget(1);

        // the overwrite nets to nothing and the set-then-clear of the fresh
        // cell nets to nothing, so the batch lands at the limit
        let batch = edit_world(
            &mut world,
            &[set(0, 0, 0, 9), set(2, 0, 0, 3), clear(2, 0, 0)],
            &TrackedCoords::default(),
        )
        .unwrap();

        assert_eq!(world.voxel_count(), 1);
        assert_eq!(
            world.get_voxel(&IVec3::ZERO),
            Some(9),
            "the overwrite landed"
        );
        assert!(world.get_voxel(&IVec3::new(2, 0, 0)).is_none());
        assert_eq!(batch.tracked, [IVec3::ZERO].into_iter().collect());
    }

    #[test]
    fn the_offending_position_is_named_in_the_error() {
        // the region arm is unreachable through `apply` while the voxel lattice
        // is the tighter bound, so the wording is checked on the error itself
        let error = EditError::rejected(IVec3::new(-1, 2, -3), EditReason::RegionOutsideLattice);

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
