//! Timings and regression budgets for the simulation's per-tick work.
//!
//! `snapshot_fold_timings`, release, Windows, 2026-10-04, AMD Ryzen 7 9800X3D:
//! one tick's snapshot fold at the full-lattice scale, about 1M snapshots over
//! half as many Micro-chunks, so half the folds are the last write on a chunk
//! already in the batch. Issue 08 measured the linear scan this replaced at
//! 470.9 s and the map at 21.9 ms over its own 987k snapshots.
//!
//! | snapshots | chunks  | fold   | budget |
//! | --------- | ------- | ------ | ------ |
//! | 1,048,576 | 524,288 | 84.1ms | 250 ms |
//!
//! The budget is the measured figure with headroom for run-to-run variance.
//! The fold is linear in the snapshots, so the budget only has to separate it
//! from the quadratic scan it replaced: any long tick now stays in the
//! hundreds of milliseconds rather than the minutes.
//!
//! `generation_first_tick_timings` drives a generated surface's own granular
//! queue under the cap and reports the grains, the moves and the settle. It
//! sits here because counting moves needs the drain, and the World job's push
//! carries tick timings and Snapshots rather than the edits a tick produced.

use std::time::{Duration, Instant};

use glam::IVec3;

use crate::world::World;
use crate::world::diff::batch::TrackedCoords;
use crate::world::diff::edit::edit_world;
use crate::world::diff::snapshot::{MicroChunkSnapshot, emit_snapshots};
use crate::world::generation::{GenerationParams, generate};
use crate::world::grid::MICRO_CHUNK_LENGTH;
use crate::world::load::progress::Progress;
use crate::world::material::PhysicalMaterialTable;

use super::physics::queue::UpdateQueue;
use super::physics::rules::drain;
use super::runtime::SnapshotFold;
use super::{MAX_CELLS_PER_TICK, ParityPolicy};

const FOLD_BUDGET: Duration = Duration::from_millis(250);
const FOLD_SNAPSHOTS: usize = 1 << 20;
const FOLD_CHUNKS: usize = FOLD_SNAPSHOTS / 2;
const CHUNK_AXIS: usize = 512;
const CHUNK_PLANE: usize = CHUNK_AXIS * CHUNK_AXIS;

fn snapshot(chunk: usize, material: u8) -> MicroChunkSnapshot {
    let edge = i32::try_from(MICRO_CHUNK_LENGTH).unwrap_or_default();
    let half = i32::try_from(CHUNK_AXIS)
        .unwrap_or_default()
        .saturating_mul(edge)
        .saturating_div(2);
    let axis = |value: usize| {
        i32::try_from(value)
            .unwrap_or_default()
            .saturating_mul(edge)
            .saturating_sub(half)
    };
    let origin = IVec3::new(
        axis(chunk % CHUNK_AXIS),
        axis((chunk / CHUNK_PLANE) % CHUNK_AXIS),
        axis((chunk / CHUNK_AXIS) % CHUNK_AXIS),
    );
    let mut mask = [0u8; 64];

    if let Some(first) = mask.first_mut() {
        *first = 1;
    }

    MicroChunkSnapshot {
        global_coords: origin,
        mask,
        materials: vec![material],
    }
}

fn batch() -> Vec<MicroChunkSnapshot> {
    (0..FOLD_SNAPSHOTS)
        .map(|index| {
            let material = u8::try_from(index % 256).unwrap_or_default();

            snapshot(index % FOLD_CHUNKS, material)
        })
        .collect()
}

#[test]
#[ignore = "bench: cargo test --release snapshot_fold_timings -- --ignored --nocapture"]
fn snapshot_fold_timings() {
    let snapshots = batch();
    let mut fold = SnapshotFold::default();

    let started = Instant::now();
    fold.merge(snapshots);
    let elapsed = started.elapsed();

    assert_eq!(
        fold.into_vec().len(),
        FOLD_CHUNKS,
        "each Micro-chunk reaches the renderer once"
    );
    println!(
        "{FOLD_SNAPSHOTS} snapshots over {FOLD_CHUNKS} chunks folded in {elapsed:.3?} \
         (budget {FOLD_BUDGET:.3?})"
    );
    assert!(
        elapsed <= FOLD_BUDGET,
        "the fold took {elapsed:.3?}, over budget {FOLD_BUDGET:.3?}"
    );
}

const SURFACE_SEED: u64 = 0x5EED_1234;
const SURFACE_FOOTPRINTS: [i32; 4] = [512, 1024, 2048, 4096];

/// Caps the settle, so a surface that never empties its queue cannot hold the
/// bench.
const SETTLE_TICKS: usize = 20_000;

/// One generated surface's grains, its first tick's moves, and its settle.
struct SurfaceRun {
    footprint: i32,
    voxels: usize,
    grains: usize,
    first_moves: usize,
    moves: usize,
    settle_ticks: Option<usize>,
    settle: Duration,
}

/// One capped tick of the runtime's own commit path: `drain` produces the
/// edits, `edit_world` compiles them against the renderer's tracked set, and
/// the batch's tracked set carries on. An empty batch commits nothing, as
/// `Runtime::commit` does. Returns how many cells moved.
fn commit_tick(
    world: &mut World,
    table: &PhysicalMaterialTable,
    queue: &mut UpdateQueue,
    tracked: &mut TrackedCoords,
) -> usize {
    let edits = drain(
        world,
        table,
        queue,
        ParityPolicy::Alternate,
        MAX_CELLS_PER_TICK,
    );
    let moves = edits.len() / 2;

    if edits.is_empty() {
        return 0;
    }

    let batch = edit_world(world, &edits, tracked)
        .unwrap_or_else(|error| panic!("the tick's edits must apply: {error}"));

    *tracked = batch.tracked;

    moves
}

/// Generates one footprint, seeds the queue and the tracked set the way an
/// activation does, and drives the runtime's capped commit until the queue is
/// empty.
fn measure_surface(footprint: i32) -> SurfaceRun {
    let generated = generate(
        &Progress::generate_path(),
        GenerationParams::new(SURFACE_SEED, IVec3::splat(footprint)),
    )
    .unwrap_or_else(|error| panic!("the {footprint} footprint must generate: {error}"));

    let grains = generated.granular_cells.len();
    let voxels = generated.world.voxel_count();
    let table = generated.materials;
    let mut world = generated.world;
    let mut queue = UpdateQueue::default();
    let snapshots = emit_snapshots(&world)
        .unwrap_or_else(|error| panic!("the {footprint} snapshots must emit: {error}"));
    let mut tracked: TrackedCoords = snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();

    drop(snapshots);
    queue.seed(&world, &table, Some(&generated.granular_cells));

    let started = Instant::now();
    let mut ticks = 0usize;
    let mut moves = 0usize;
    let mut first_moves = 0usize;

    while queue.iter().next().is_some() && ticks < SETTLE_TICKS {
        let this_tick = commit_tick(&mut world, &table, &mut queue, &mut tracked);

        if ticks == 0 {
            first_moves = this_tick;
        }

        moves = moves.saturating_add(this_tick);
        ticks = ticks.saturating_add(1);
    }

    let settle = started.elapsed();
    let settle_ticks = queue.iter().next().is_none().then_some(ticks);

    SurfaceRun {
        footprint,
        voxels,
        grains,
        first_moves,
        moves,
        settle_ticks,
        settle,
    }
}

fn print_surface_run(run: &SurfaceRun) {
    let share = 100.0 * run.first_moves as f64 / run.grains.max(1) as f64;
    let ticks = run
        .settle_ticks
        .map_or_else(|| format!(">{SETTLE_TICKS}"), |ticks| ticks.to_string());

    println!(
        "{:>9} {:>12} {:>10} {:>11} {:>6.1}% {:>12} {:>11.3?} {:>12}",
        run.footprint,
        run.voxels,
        run.grains,
        run.first_moves,
        share,
        ticks,
        run.settle,
        run.moves
    );
}

/// The generated surface's first tick and its settle: the Falling granular
/// cells a Generation writes, the cells the first capped tick moves, and the
/// ticks and wall time a capped drain takes to empty the queue. The queue is
/// seeded from the generator's own list and the tracked set from the emitted
/// Snapshots, and the drain is the simulation's own under `MAX_CELLS_PER_TICK`,
/// so the figures are the cap's. The grain count and the moves are
/// world-generation issue 08's figures, re-taken on the coherent field.
#[test]
#[ignore = "bench: cargo test --release generation_first_tick_timings -- --ignored --nocapture (ATLAS_BENCH_SURFACE_FOOTPRINT pins one footprint edge)"]
fn generation_first_tick_timings() {
    let footprints: Vec<i32> = std::env::var("ATLAS_BENCH_SURFACE_FOOTPRINT").map_or_else(
        |_| SURFACE_FOOTPRINTS.to_vec(),
        |edge| {
            let footprint = edge
                .parse::<i32>()
                .unwrap_or_else(|error| panic!("the footprint edge must be an integer: {error}"));

            vec![footprint]
        },
    );

    println!(
        "cap           {MAX_CELLS_PER_TICK} cells per tick, {SETTLE_TICKS} ticks at most before the queue is reported undrained"
    );
    println!(
        "{:>9} {:>12} {:>10} {:>11} {:>7} {:>12} {:>11} {:>12}",
        "footprint", "voxels", "grains", "first moves", "share", "settle ticks", "settle", "moves"
    );

    for footprint in footprints {
        print_surface_run(&measure_surface(footprint));
    }
}
