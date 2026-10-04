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

use std::time::{Duration, Instant};

use glam::IVec3;

use crate::world::diff::snapshot::MicroChunkSnapshot;
use crate::world::grid::MICRO_CHUNK_LENGTH;

use super::runtime::SnapshotFold;

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
