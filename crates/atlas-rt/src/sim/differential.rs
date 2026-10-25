//! The drain's differential harness: one scene built twice, each side driven
//! through the real drain and commit under one policy, compared after every
//! tick. `Runs` aliases the per-cell pass until the run translation lands, so
//! the cases prove the harness itself before they prove a fast path.
//!
//! The commit is compared the same way: one scene drains the same way twice,
//! once with every move applied as a per-cell edit and once with every touched
//! chunk written whole from the overlay.

use glam::IVec3;

use crate::world::World;
use crate::world::diff::batch::TrackedCoords;
use crate::world::diff::edit::{VoxelChange, VoxelEdit};
use crate::world::diff::snapshot::MicroChunkSnapshot;
use crate::world::grid::{MICRO_CHUNK_LENGTH, grid_origin};
use crate::world::load::progress::Progress;
use crate::world::material::{PhysicalMaterialTable, parse_override};
use crate::world::sand_ball;

use super::commit::{REWRITE_THRESHOLD, commit};
use super::physics::field::Overlay;
use super::physics::queue::UpdateQueue;
use super::physics::rules::{DrainPolicy, Drained, ParityPolicy, drain};

const GRAIN: u8 = 2;
const OTHER_GRAIN: u8 = 3;
const PILLAR: u8 = 1;

/// The solid slab a sphere rests on. The sphere's lowest cell starts at y = 5,
/// so it falls 12 cells before it lands.
const FLOOR_Y: i32 = -8;

fn table() -> PhysicalMaterialTable {
    parse_override(&format!("material {GRAIN} falling_granular solid=true"))
        .unwrap_or_else(|rejections| panic!("{rejections:?}"))
}

/// Both granular materials, so the drain reads a table where the rule alone
/// does not name the material.
fn two_grain_table() -> PhysicalMaterialTable {
    parse_override(&format!(
        "material {GRAIN} falling_granular solid=true\n\
         material {OTHER_GRAIN} falling_granular solid=true"
    ))
    .unwrap_or_else(|rejections| panic!("{rejections:?}"))
}

/// One scene built twice, so both policies start from the same World, the same
/// table and the same queue.
struct Scene {
    worlds: [World; 2],
    table: PhysicalMaterialTable,
    granular_cells: Option<Vec<IVec3>>,
}

impl Scene {
    fn new(build: impl Fn() -> World, table: PhysicalMaterialTable) -> Self {
        Self::of([build(), build()], table)
    }

    fn of(worlds: [World; 2], table: PhysicalMaterialTable) -> Self {
        Self {
            worlds,
            table,
            granular_cells: None,
        }
    }

    fn seeded(mut self, granular_cells: Vec<IVec3>) -> Self {
        self.granular_cells = Some(granular_cells);

        self
    }

    fn sides(self) -> [Side; 2] {
        let Self {
            worlds,
            table,
            granular_cells,
        } = self;

        worlds.map(|world| {
            let mut queue = UpdateQueue::default();

            queue.seed(&world, &table, granular_cells.as_deref());

            Side {
                world,
                table,
                queue,
                tracked: TrackedCoords::default(),
                snapshots: Vec::new(),
                processed: 0,
            }
        })
    }
}

/// One policy's run of a scene: its own World, queue, tracked set, the snapshot
/// list the last commit planned, and the cell count the last drain reported.
struct Side {
    world: World,
    table: PhysicalMaterialTable,
    queue: UpdateQueue,
    tracked: TrackedCoords,
    snapshots: Vec<MicroChunkSnapshot>,
    processed: usize,
}

impl Side {
    /// One tick in the shape `Runtime::commit` drives: drain, then commit the
    /// drain's moves with the tracked set the last batch carried forward.
    fn serve(&mut self, policy: DrainPolicy, threshold: usize) -> Drained {
        let drained = drain(
            &self.world,
            &self.table,
            &mut self.queue,
            ParityPolicy::Alternate,
            policy,
        );

        self.processed = drained.processed;
        self.commit(&drained.edits, &drained.overlay, threshold);

        drained
    }

    fn commit(&mut self, edits: &[VoxelEdit], overlay: &Overlay, threshold: usize) {
        if edits.is_empty() {
            self.snapshots = Vec::new();

            return;
        }

        let batch = commit(&mut self.world, edits, overlay, threshold, &self.tracked)
            .unwrap_or_else(|error| panic!("the tick's edits must apply: {error}"));

        self.tracked = batch.tracked;
        self.snapshots = batch.snapshots;
    }

    fn cells(&self) -> Vec<(IVec3, u8)> {
        let mut cells: Vec<(IVec3, u8)> = self.world.iter_voxels().collect();

        cells.sort_unstable_by_key(|(position, _)| position.to_array());

        cells
    }

    fn queued(&self) -> Vec<IVec3> {
        self.queue.iter().collect()
    }

    fn tracking(&self) -> Vec<IVec3> {
        let mut tracking: Vec<IVec3> = self.tracked.iter().copied().collect();

        tracking.sort_unstable_by_key(IVec3::to_array);

        tracking
    }

    fn snapshot_origins(&self) -> Vec<IVec3> {
        self.snapshots
            .iter()
            .map(|snapshot| snapshot.global_coords)
            .collect()
    }
}

/// Drives both drain policies over one scene and compares them after every
/// tick.
fn run(scene: Scene, ticks: usize) -> Result<(), String> {
    let [mut per_cell, mut runs] = scene.sides();

    for tick in 1..=ticks {
        per_cell.serve(DrainPolicy::PerCell, REWRITE_THRESHOLD);
        runs.serve(DrainPolicy::Runs, REWRITE_THRESHOLD);
        compare(tick, &per_cell, &runs)?;
    }

    Ok(())
}

fn assert_same(scene: Scene, ticks: usize) {
    if let Err(difference) = run(scene, ticks) {
        panic!("the runs policy diverged from the per-cell path: {difference}");
    }
}

/// Drives one scene's drain twice under one policy, once with a threshold that
/// rewrites no chunk and once with a threshold that rewrites every touched
/// chunk, and compares them after every tick.
fn run_commits(scene: Scene, ticks: usize) -> Result<(), String> {
    let [mut per_cell, mut rewrite] = scene.sides();

    for tick in 1..=ticks {
        per_cell.serve(DrainPolicy::PerCell, usize::MAX);
        rewrite.serve(DrainPolicy::PerCell, 0);
        compare(tick, &per_cell, &rewrite)?;
    }

    Ok(())
}

fn assert_same_commit(scene: Scene, ticks: usize) {
    if let Err(difference) = run_commits(scene, ticks) {
        panic!("the rewrite commit diverged from the per-cell commit: {difference}");
    }
}

/// One tick of both sides: the World's cells and their count, the queue's cells
/// in drain order, the cells the drain evaluated, the tracked set, and the
/// snapshot list in the order the commit planned it.
fn compare(tick: usize, per_cell: &Side, runs: &Side) -> Result<(), String> {
    let cells = per_cell.cells();
    let translated = runs.cells();

    if cells != translated {
        return Err(format!(
            "tick {tick}: the World's voxels differ at {}",
            first_disagreement(&cells, &translated)
        ));
    }

    if per_cell.world.voxel_count() != runs.world.voxel_count() {
        return Err(format!(
            "tick {tick}: the World's voxel count differs: {} against {}",
            per_cell.world.voxel_count(),
            runs.world.voxel_count()
        ));
    }

    let queued = per_cell.queued();
    let translated = runs.queued();

    if queued != translated {
        return Err(format!(
            "tick {tick}: the queue's cells differ at {}",
            first_disagreement(&queued, &translated)
        ));
    }

    if per_cell.processed != runs.processed {
        return Err(format!(
            "tick {tick}: the drained cell count differs: {} against {}",
            per_cell.processed, runs.processed
        ));
    }

    let tracking = per_cell.tracking();
    let translated = runs.tracking();

    if tracking != translated {
        return Err(format!(
            "tick {tick}: the tracked set differs at {}",
            first_disagreement(&tracking, &translated)
        ));
    }

    let origins = per_cell.snapshot_origins();
    let translated = runs.snapshot_origins();

    if origins != translated {
        return Err(format!(
            "tick {tick}: the snapshot list's order differs at {}",
            first_disagreement(&origins, &translated)
        ));
    }

    for (per_cell, runs) in per_cell.snapshots.iter().zip(&runs.snapshots) {
        if per_cell != runs {
            return Err(format!(
                "tick {tick}: the snapshot content at {} differs: {:?} against {:?}",
                per_cell.global_coords,
                per_cell.chunk.mask(),
                runs.chunk.mask()
            ));
        }
    }

    Ok(())
}

/// The first entry the two sides disagree on, or the extra entry of the longer
/// one, so a difference names a cell rather than a length.
fn first_disagreement<T: PartialEq + std::fmt::Debug>(per_cell: &[T], runs: &[T]) -> String {
    let longest = per_cell.len().max(runs.len());

    for index in 0..longest {
        match (per_cell.get(index), runs.get(index)) {
            (Some(per_cell), Some(runs)) if per_cell == runs => {}
            (Some(per_cell), Some(runs)) => {
                return format!(
                    "{per_cell:?} on the per-cell side against {runs:?} on the runs side"
                );
            }
            (Some(only), None) => {
                return format!("{only:?} on the per-cell side, which the runs side lacks");
            }
            (None, Some(only)) => {
                return format!("{only:?} on the runs side, which the per-cell side lacks");
            }
            (None, None) => break,
        }
    }

    String::from("the same cells in the same order")
}

fn world_of(cells: &[(IVec3, u8)]) -> World {
    let mut world = World::default();

    for (position, material) in cells {
        world.set_voxel(*position, *material);
    }

    world
}

/// Five grains in open cells above nothing, so every one of them falls
/// straight down when the drain reaches it.
fn row() -> World {
    let mut cells = Vec::new();

    for x in 0..5 {
        cells.push((IVec3::new(x, 5, 0), GRAIN));
    }

    world_of(&cells)
}

/// Five grains stacked in one column above nothing.
fn column() -> World {
    let mut cells = Vec::new();

    for y in 1..=5 {
        cells.push((IVec3::new(0, y, 0), GRAIN));
    }

    world_of(&cells)
}

/// Six grains apart at y = 5 with nothing under them, so each falls one cell
/// per service and no wake reaches another grain.
fn scattered() -> World {
    let mut cells = Vec::new();

    for x in [0, 4, 8, 12, 16, 20] {
        cells.push((IVec3::new(x, 5, 0), GRAIN));
    }

    world_of(&cells)
}

/// A hand-built sphere of `radius`, centered at `centre_y` and standing above a
/// solid slab at `floor_y`. The shipped sand ball's radius is fixed at 64, so
/// this is the smallest scene that falls as a body.
fn sphere(radius: i32, centre_y: i32, floor_y: i32) -> World {
    let mut cells = Vec::new();

    for x in -radius..=radius {
        for z in -radius..=radius {
            cells.push((IVec3::new(x, floor_y, z), PILLAR));
        }
    }

    for x in -radius..=radius {
        for y in -radius..=radius {
            for z in -radius..=radius {
                if x * x + y * y + z * z <= radius * radius {
                    cells.push((IVec3::new(x, y + centre_y, z), GRAIN));
                }
            }
        }
    }

    world_of(&cells)
}

/// The wall and diagonal fixture: a grain over an open column falls, the next
/// takes the diagonal the fall left open, and the sealed grain takes the cell
/// the fall vacated.
fn wall_and_diagonal() -> World {
    world_of(&[
        (IVec3::new(4, 1, 6), PILLAR),
        (IVec3::new(6, 1, 6), PILLAR),
        (IVec3::new(7, 1, 6), GRAIN),
        (IVec3::new(4, 2, 6), GRAIN),
        (IVec3::new(6, 2, 6), GRAIN),
    ])
}

/// A grain asleep on a grain that moves out from under it in the same tick.
fn stacked_pair() -> World {
    world_of(&[(IVec3::new(0, 2, 0), GRAIN), (IVec3::new(0, 1, 0), GRAIN)])
}

/// The wall and diagonal fixture in two granular materials: the grain of one
/// material takes the diagonal into the other material's column, and the two
/// end the tick stacked on each other.
fn two_materials() -> World {
    world_of(&[
        (IVec3::new(4, 1, 6), PILLAR),
        (IVec3::new(6, 1, 6), PILLAR),
        (IVec3::new(7, 1, 6), GRAIN),
        (IVec3::new(4, 2, 6), GRAIN),
        (IVec3::new(6, 2, 6), OTHER_GRAIN),
        (IVec3::new(5, 3, 6), OTHER_GRAIN),
    ])
}

/// A sphere whose lowest cell sits on a Micro-chunk boundary: radius 3 around
/// y = 3 puts its lowest grain at y = 0.
fn boundary_sphere() -> World {
    sphere(3, 3, FLOOR_Y)
}

/// Three by three by three full Micro-chunks of grains above nothing, so every
/// chunk of the block moves many cells on every tick.
fn dense_block() -> World {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let mut cells = Vec::new();

    for x in 0..3 * edge {
        for y in 0..3 * edge {
            for z in 0..3 * edge {
                cells.push((IVec3::new(x, y + edge, z), GRAIN));
            }
        }
    }

    world_of(&cells)
}

fn lowest_grain(world: &World) -> IVec3 {
    world
        .iter_voxels()
        .filter(|(_, material)| *material == GRAIN)
        .map(|(position, _)| position)
        .min_by_key(|position| (position.y, position.x, position.z))
        .expect("the scene holds grains")
}

/// The moves each of a scene's touched chunks serves on its first drain, which
/// is what the commit's threshold reads.
fn served_moves(build: impl Fn() -> World, table: PhysicalMaterialTable) -> Vec<usize> {
    let world = build();
    let mut queue = UpdateQueue::default();

    queue.seed(&world, &table, None);

    let drained = drain(
        &world,
        &table,
        &mut queue,
        ParityPolicy::Alternate,
        DrainPolicy::PerCell,
    );

    let mut served: Vec<usize> = drained
        .overlay
        .chunks()
        .map(|(_, held)| held.moves())
        .filter(|moves| *moves > 0)
        .collect();

    served.sort_unstable();

    served
}

#[test]
fn a_row_falls_the_same_under_both_policies() {
    assert_same(Scene::new(row, table()), 6);
}

#[test]
fn a_column_falls_the_same_under_both_policies() {
    assert_same(Scene::new(column, table()), 6);
}

#[test]
fn scattered_grains_fall_the_same_under_both_policies() {
    assert_same(Scene::new(scattered, table()), 4);
}

#[test]
fn a_sphere_falls_and_lands_the_same_under_both_policies() {
    assert_same(Scene::new(|| sphere(3, 8, FLOOR_Y), table()), 14);
}

#[test]
fn a_wall_and_a_diagonal_fall_the_same_under_both_policies() {
    assert_same(Scene::new(wall_and_diagonal, table()), 4);
}

#[test]
fn a_settled_grain_woken_by_a_move_falls_the_same_under_both_policies() {
    assert_same(Scene::new(stacked_pair, table()), 4);
}

#[test]
fn two_granular_materials_fall_the_same_under_both_policies() {
    let world = two_materials();

    for material in [GRAIN, OTHER_GRAIN] {
        assert!(
            world.iter_voxels().any(|(_, held)| held == material),
            "the fixture holds a grain of material {material}"
        );
    }

    assert_same(Scene::new(two_materials, two_grain_table()), 4);
}

#[test]
fn a_sphere_whose_lowest_cell_sits_on_a_chunk_boundary_falls_the_same() {
    let lowest = lowest_grain(&boundary_sphere());

    assert_eq!(
        grid_origin(lowest, MICRO_CHUNK_LENGTH),
        lowest,
        "the lowest grain sits on a Micro-chunk boundary, which is the case this covers"
    );

    assert_same(Scene::new(boundary_sphere, table()), 9);
}

#[test]
fn a_dense_block_of_full_chunks_falls_the_same_under_both_policies() {
    let edge = usize::try_from(MICRO_CHUNK_LENGTH).unwrap_or_default();
    let full = edge
        .saturating_mul(edge)
        .saturating_mul(edge)
        .saturating_mul(27);

    assert_eq!(
        dense_block().voxel_count(),
        full,
        "every cell of the block's 27 Micro-chunks holds a grain"
    );

    assert_same(Scene::new(dense_block, table()), 3);
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "the full ball belongs to the standing bench"
)]
fn the_sand_ball_falls_the_same_under_both_policies() {
    let first = sand_ball::build(&Progress::generate_path());
    let second = sand_ball::build(&Progress::generate_path());
    let scene =
        Scene::of([first.world, second.world], first.materials).seeded(first.granular_cells);

    assert_same(scene, 4);
}

#[test]
fn a_dense_block_commits_the_same_with_and_without_chunk_rewrites() {
    let served = served_moves(dense_block, table());

    assert!(!served.is_empty(), "the block's tick moves cells");

    assert!(
        served.iter().all(|moves| *moves > REWRITE_THRESHOLD),
        "every chunk of the block crosses the threshold: {served:?}"
    );

    assert_same_commit(Scene::new(dense_block, table()), 3);
}

#[test]
fn a_scattered_world_commits_the_same_with_and_without_chunk_rewrites() {
    let served = served_moves(scattered, table());

    assert!(!served.is_empty(), "the scattered tick moves cells");

    assert!(
        served.iter().all(|moves| *moves <= REWRITE_THRESHOLD),
        "no chunk of the scattered world crosses the threshold: {served:?}"
    );

    assert_same_commit(Scene::new(scattered, table()), 4);
}

#[test]
fn two_granular_materials_commit_the_same_with_and_without_chunk_rewrites() {
    assert_same_commit(Scene::new(two_materials, two_grain_table()), 4);
}

#[test]
fn a_sphere_falling_and_landing_commits_the_same_with_and_without_chunk_rewrites() {
    assert_same_commit(Scene::new(|| sphere(3, 8, FLOOR_Y), table()), 14);
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "the full ball belongs to the standing bench"
)]
fn the_sand_ball_commits_the_same_with_and_without_chunk_rewrites() {
    let first = sand_ball::build(&Progress::generate_path());
    let second = sand_ball::build(&Progress::generate_path());
    let scene =
        Scene::of([first.world, second.world], first.materials).seeded(first.granular_cells);

    assert_same_commit(scene, 4);
}

#[test]
fn a_dropped_edit_fails_the_comparison() {
    let [mut per_cell, mut runs] = Scene::new(row, table()).sides();

    per_cell.serve(DrainPolicy::PerCell, REWRITE_THRESHOLD);

    let drained = drain(
        &runs.world,
        &runs.table,
        &mut runs.queue,
        ParityPolicy::Alternate,
        DrainPolicy::Runs,
    );
    let mut edits = drained.edits;
    let dropped = edits.pop();

    assert_eq!(
        dropped,
        Some(VoxelEdit {
            position: IVec3::new(4, 4, 0),
            change: VoxelChange::Set(GRAIN),
        }),
        "the fixture's last edit is the fifth grain's destination"
    );

    runs.processed = drained.processed;
    runs.commit(&edits, &Overlay::default(), REWRITE_THRESHOLD);

    let difference = compare(1, &per_cell, &runs)
        .expect_err("a dropped destination must leave the two sides different");

    assert!(
        difference.contains("tick 1"),
        "the difference names its tick: {difference}"
    );
    assert!(
        difference.contains("the World's voxels differ at"),
        "the difference names the field that disagrees: {difference}"
    );
    assert!(
        difference.contains("IVec3(4, 4, 0)"),
        "the difference names the first disagreeing cell: {difference}"
    );
}

#[test]
fn a_queue_the_sides_do_not_share_fails_the_comparison() {
    let [mut per_cell, mut runs] = Scene::new(row, table()).sides();

    per_cell.serve(DrainPolicy::PerCell, REWRITE_THRESHOLD);
    runs.serve(DrainPolicy::Runs, REWRITE_THRESHOLD);

    runs.queue.wake(IVec3::new(9, 9, 9));

    let difference = compare(1, &per_cell, &runs)
        .expect_err("a cell only one side queued must fail the comparison");

    assert!(
        difference.contains("the queue's cells differ at"),
        "the difference names the queue: {difference}"
    );
    assert!(
        difference.contains("IVec3(9, 9, 9)"),
        "the difference names the cell only one side queued: {difference}"
    );
}

#[test]
fn a_drained_cell_count_the_sides_do_not_share_fails_the_comparison() {
    let [mut per_cell, mut runs] = Scene::new(row, table()).sides();

    per_cell.serve(DrainPolicy::PerCell, REWRITE_THRESHOLD);
    runs.serve(DrainPolicy::Runs, REWRITE_THRESHOLD);

    runs.processed = per_cell.processed.saturating_add(1);

    let difference = compare(1, &per_cell, &runs)
        .expect_err("a cell count only one side reported must fail the comparison");

    assert!(
        difference.contains("the drained cell count differs"),
        "the difference names the drained cell count: {difference}"
    );
}

#[test]
fn a_rewritten_chunk_whose_content_the_sides_do_not_share_fails_the_comparison() {
    let [mut per_cell, mut rewrite] = Scene::new(dense_block, table()).sides();

    per_cell.serve(DrainPolicy::PerCell, usize::MAX);
    rewrite.serve(DrainPolicy::PerCell, 0);

    let first = rewrite.snapshots.first_mut();

    assert!(first.is_some(), "the dense block's tick rewrites a chunk");

    if let Some(snapshot) = first {
        snapshot.chunk = crate::world::micro::MicroChunk::empty();
    }

    let difference = compare(1, &per_cell, &rewrite)
        .expect_err("a chunk the two sides hold differently must fail the comparison");

    assert!(
        difference.contains("the snapshot content at"),
        "the difference names the snapshot content: {difference}"
    );
}

#[test]
fn a_tracked_set_the_sides_do_not_share_fails_the_comparison() {
    let [mut per_cell, mut rewrite] = Scene::new(row, table()).sides();

    per_cell.serve(DrainPolicy::PerCell, usize::MAX);
    rewrite.serve(DrainPolicy::PerCell, 0);

    rewrite.tracked.insert(IVec3::new(9, 9, 9));

    let difference = compare(1, &per_cell, &rewrite)
        .expect_err("a tracked chunk only one side holds must fail the comparison");

    assert!(
        difference.contains("the tracked set differs at"),
        "the difference names the tracked set: {difference}"
    );
    assert!(
        difference.contains("IVec3(9, 9, 9)"),
        "the difference names the chunk only one side tracked: {difference}"
    );
}
