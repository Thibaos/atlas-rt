//! What a tick's drain hands the World: the chunks the overlay moved enough
//! cells in are written whole, and the moves outside them go as per-cell edits.

use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::world::World;
use crate::world::budget::cell_budget;
use crate::world::diff::batch::{Batch, TrackedCoords, plan_edit};
use crate::world::diff::edit::{
    EditError, VoxelEdit, apply_edits, check_cell_budget, edit_world, projected_count,
};
use crate::world::diff::snapshot::MicroChunkSnapshot;
use crate::world::grid::{MICRO_CHUNK_LENGTH, grid_origin};
use crate::world::micro::{self, MICRO_BYTES, MICRO_CELLS};

use super::physics::field::{ChunkHeld, Overlay};
use super::runtime::SnapshotFold;

/// The moves a chunk serves above which the commit writes it whole: about
/// 250 ns for one entry write against about 106 ns for one move, so four moves
/// are the cheapest a chunk can be left to the per-cell path with.
pub(in crate::sim) const REWRITE_THRESHOLD: usize = 4;

/// Applies one drain's moves to `world` and returns the batch the tick
/// submits.
///
/// Every chunk that served more moves than `threshold` is written whole from
/// `overlay`, and every edit inside such a chunk is dropped, because the entry
/// already holds that move. A chunk at or under the threshold keeps its edits.
/// The two snapshot lists fold into one, and the tracked set is planned once
/// over the folded list.
///
/// # Errors
///
/// Returns an [`EditError`] when the batch would carry the World past its cell
/// budget, before anything is written, and when an edit or an entry lies
/// outside the lattice.
pub(in crate::sim) fn commit(
    world: &mut World,
    edits: &[VoxelEdit],
    overlay: &Overlay,
    threshold: usize,
    tracked: &TrackedCoords,
) -> Result<Batch, EditError> {
    let mut rewrites = rewritten(overlay, threshold);

    if rewrites.is_empty() {
        return edit_world(world, edits, tracked);
    }

    let (touched, remaining) = split(edits, &rewrites);

    refuse_over_budget(world, &remaining, &rewrites)?;

    for (origin, snapshot) in &rewrites {
        world.write_entry(*origin, snapshot.chunk.as_ref())?;
    }

    let mut edited = apply_edits(world, &remaining)?.into_iter();
    let mut ordered = Vec::with_capacity(touched.len().saturating_add(rewrites.len()));

    for origin in touched {
        if let Some(snapshot) = rewrites.remove(&origin) {
            ordered.push(snapshot);
        } else if let Some(snapshot) = edited.next() {
            ordered.push(snapshot);
        }
    }

    // A translated chunk emits no edit, so the first-touch order above never
    // reaches it. Its entry is its own change, and last in the list.
    let mut translated: Vec<IVec3> = rewrites.keys().copied().collect();

    translated.sort_unstable_by_key(IVec3::to_array);

    for origin in translated {
        if let Some(snapshot) = rewrites.remove(&origin) {
            ordered.push(snapshot);
        }
    }

    debug_assert!(
        rewrites.is_empty(),
        "every rewritten chunk reaches the batch"
    );

    let mut folded = SnapshotFold::default();

    folded.merge(ordered);

    Ok(plan_edit(folded.into_vec(), tracked))
}

/// The touched chunks that served more moves than `threshold`, each with the
/// snapshot its whole-entry write leaves behind.
fn rewritten(overlay: &Overlay, threshold: usize) -> FxHashMap<IVec3, MicroChunkSnapshot> {
    let mut rewrites: FxHashMap<IVec3, MicroChunkSnapshot> = FxHashMap::default();

    for (origin, held) in overlay.chunks() {
        if held.moves() <= threshold {
            continue;
        }

        rewrites.insert(origin, snapshot(origin, held));
    }

    rewrites
}

/// A chunk as the entry the commit writes: its mask and the material of each
/// occupied cell in ascending cell order, which is the order the store's entry
/// keeps them in.
fn snapshot(origin: IVec3, held: &ChunkHeld) -> MicroChunkSnapshot {
    let mask: [u8; MICRO_BYTES] = *held.mask();
    let mut materials = Vec::with_capacity(micro::occupied_count(&mask));

    for cell in 0..MICRO_CELLS {
        if let Some(material) = held.material(cell) {
            materials.push(material);
        }
    }

    MicroChunkSnapshot {
        global_coords: origin,
        chunk: micro::owned_chunk(origin, mask, materials),
    }
}

/// The first-touch order of the chunks the edits touch, and the edits outside
/// every rewritten chunk.
fn split(
    edits: &[VoxelEdit],
    rewrites: &FxHashMap<IVec3, MicroChunkSnapshot>,
) -> (Vec<IVec3>, Vec<VoxelEdit>) {
    let mut seen: FxHashSet<IVec3> = FxHashSet::default();
    let mut touched: Vec<IVec3> = Vec::new();
    let mut remaining: Vec<VoxelEdit> = Vec::with_capacity(edits.len());

    for edit in edits {
        let origin = grid_origin(edit.position, MICRO_CHUNK_LENGTH);

        if seen.insert(origin) {
            touched.push(origin);
        }

        if !rewrites.contains_key(&origin) {
            remaining.push(*edit);
        }
    }

    (touched, remaining)
}

/// Refuses the whole batch when the remaining edits and the rewrites together
/// would pass the cell budget. A rewrite's delta is its overlay mask's popcount
/// against the entry's, both taken before the write.
fn refuse_over_budget(
    world: &World,
    remaining: &[VoxelEdit],
    rewrites: &FxHashMap<IVec3, MicroChunkSnapshot>,
) -> Result<(), EditError> {
    if cell_budget() == usize::MAX {
        return Ok(());
    }

    let mut added = 0usize;
    let mut removed = 0usize;

    for (origin, snapshot) in rewrites {
        added = added.saturating_add(snapshot.chunk.occupied_count());
        removed = removed.saturating_add(
            world
                .chunk_entry(*origin)
                .map_or(0, |entry| entry.occupied_count()),
        );
    }

    let projected = projected_count(world, remaining)
        .saturating_add(added)
        .saturating_sub(removed);

    check_cell_budget(projected)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::world::budget::set_cell_budget;
    use crate::world::diff::edit::VoxelChange;
    use crate::world::material::PhysicalMaterialTable;
    use crate::world::material::parse_override;

    use super::super::physics::queue::UpdateQueue;
    use super::super::physics::rules::{DrainPolicy, Drained, ParityPolicy, drain};

    const GRAIN: u8 = 2;
    const OTHER_GRAIN: u8 = 3;

    fn table() -> PhysicalMaterialTable {
        parse_override(&format!(
            "material {GRAIN} falling_granular solid=true\n\
             material {OTHER_GRAIN} falling_granular solid=true"
        ))
        .unwrap_or_else(|rejections| panic!("{rejections:?}"))
    }

    fn world_of(cells: &[(IVec3, u8)]) -> World {
        let mut world = World::default();

        for (position, material) in cells {
            world.set_voxel(*position, *material);
        }

        world
    }

    fn drained(world: &World) -> Drained {
        let mut queue = UpdateQueue::default();

        queue.seed(world, &table(), None);

        drain(
            world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            DrainPolicy::PerCell,
        )
    }

    fn cells(world: &World) -> Vec<(IVec3, u8)> {
        let mut cells: Vec<(IVec3, u8)> = world.iter_voxels().collect();

        cells.sort_unstable_by_key(|(position, _)| position.to_array());

        cells
    }

    fn clear(x: i32, y: i32, z: i32) -> VoxelEdit {
        VoxelEdit {
            position: IVec3::new(x, y, z),
            change: VoxelChange::Clear,
        }
    }

    /// Five grains falling inside the first chunk and one inside the second, so
    /// the first crosses the threshold and the second does not.
    fn dense_and_sparse() -> World {
        let mut cells = Vec::new();

        for x in 0..5 {
            cells.push((IVec3::new(x, 1, 0), GRAIN));
        }

        cells.push((IVec3::new(16, 1, 0), GRAIN));

        world_of(&cells)
    }

    /// The moves the tick served in `origin`'s chunk, which is what the
    /// threshold reads.
    fn served(overlay: &Overlay, origin: IVec3) -> usize {
        overlay
            .chunks()
            .find(|(touched, _)| *touched == origin)
            .map_or(0, |(_, held)| held.moves())
    }

    /// The state the tick left in `origin`'s chunk.
    fn touched(overlay: &Overlay, origin: IVec3) -> &ChunkHeld {
        overlay
            .chunk(origin)
            .expect("the tick has to touch the chunk the fixture names")
    }

    #[test]
    fn only_a_chunk_that_serves_more_moves_than_the_threshold_is_rewritten() {
        let world = dense_and_sparse();
        let drained = drained(&world);
        let dense = IVec3::new(0, 0, 0);
        let sparse = IVec3::new(16, 0, 0);

        assert!(
            served(&drained.overlay, dense) > REWRITE_THRESHOLD,
            "the dense chunk serves {} moves",
            served(&drained.overlay, dense)
        );
        assert!(
            served(&drained.overlay, sparse) <= REWRITE_THRESHOLD,
            "the sparse chunk serves {} moves",
            served(&drained.overlay, sparse)
        );

        let rewrites = rewritten(&drained.overlay, REWRITE_THRESHOLD);

        assert_eq!(
            rewrites.keys().copied().collect::<Vec<IVec3>>(),
            vec![dense],
            "only the dense chunk is written whole"
        );
        assert!(
            rewritten(&drained.overlay, 0).contains_key(&sparse),
            "a threshold of zero rewrites every chunk that served a move"
        );
        assert!(
            rewritten(&drained.overlay, usize::MAX).is_empty(),
            "a threshold of usize::MAX rewrites nothing"
        );
    }

    #[test]
    fn a_rewritten_chunk_lists_its_materials_in_ascending_cell_order() {
        let world = world_of(&[
            (IVec3::new(0, 1, 0), OTHER_GRAIN),
            (IVec3::new(5, 1, 0), GRAIN),
            (IVec3::new(0, 2, 0), GRAIN),
        ]);
        let drained = drained(&world);
        let chunk = IVec3::new(0, 0, 0);

        let rewritten = snapshot(chunk, touched(&drained.overlay, chunk));

        assert_eq!(
            rewritten.chunk.materials(),
            &[OTHER_GRAIN, GRAIN, GRAIN],
            "cells 0, 5 and 8 carry those materials, and the list follows the cell order rather than the material order"
        );
        assert_eq!(
            rewritten.chunk.occupied_count(),
            3,
            "the mask marks the three cells the tick left"
        );
    }

    #[test]
    fn a_rewritten_uniform_chunk_repeats_its_marker() {
        let world = world_of(&[
            (IVec3::new(0, 1, 0), GRAIN),
            (IVec3::new(5, 1, 0), GRAIN),
            (IVec3::new(0, 2, 0), GRAIN),
        ]);
        let drained = drained(&world);
        let chunk = IVec3::new(0, 0, 0);

        let rewritten = snapshot(chunk, touched(&drained.overlay, chunk));

        assert_eq!(
            rewritten.chunk.materials(),
            &[GRAIN, GRAIN, GRAIN],
            "a uniform chunk's material list is its marker repeated"
        );
        assert_eq!(rewritten.chunk.occupied_count(), 3);
    }

    #[test]
    fn a_batch_whose_growth_a_chunk_rewrite_carries_is_refused_whole() {
        let grains: Vec<(IVec3, u8)> = (1..=8).map(|y| (IVec3::new(0, y, 0), GRAIN)).collect();
        let mut world = world_of(&grains);
        let before = cells(&world);
        let drained = drained(&world);
        let dense = IVec3::new(0, 0, 0);

        assert_eq!(before.len(), 8, "the column is eight grains");
        assert!(
            served(&drained.overlay, dense) > REWRITE_THRESHOLD,
            "the dense chunk serves {} moves",
            served(&drained.overlay, dense)
        );

        let remaining = [clear(0, 8, 0)];
        let budget = before.len().saturating_sub(1);

        assert_eq!(
            projected_count(&world, &remaining),
            budget,
            "the edits outside the rewritten chunk lose one cell, which the rewrite gains back"
        );

        let restore = set_cell_budget(budget);
        let outcome = commit(
            &mut world,
            &drained.edits,
            &drained.overlay,
            REWRITE_THRESHOLD,
            &TrackedCoords::default(),
        );

        drop(restore);

        let error = outcome.expect_err("the rewrite carries a cell past the budget");

        assert!(
            error.to_string().contains("above the cell budget"),
            "the refusal names the budget: {error}"
        );
        assert_eq!(
            cells(&world),
            before,
            "the refused batch leaves the World exactly as it was"
        );
    }
}
