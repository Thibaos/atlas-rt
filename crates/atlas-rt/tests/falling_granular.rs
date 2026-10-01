//! The falling-granular rules and the update queue, driven through the sim's
//! tick boundary: worlds are asserted cell by cell and queue contents never
//! are, because the queue is Simulation state.

mod common;

use atlas_rt::render::region::queue::RendererInput;
use atlas_rt::sim::{Handle, ParityPolicy};
use atlas_rt::world::diff::edit::VoxelEdit;
use atlas_rt::world::diff::snapshot::{MicroChunkSnapshot, emit_snapshots};
use glam::{IVec3, Vec3};

use common::*;

/// A nine by nine floor at y = 0, the ground every scene here stands on.
fn floor() -> Vec<VoxelEdit> {
    let mut edits = Vec::new();

    for x in 0..=8 {
        for z in 0..=8 {
            edits.push(set(x, 0, z, 1));
        }
    }

    edits
}

fn grains(handle: &Handle) -> Vec<IVec3> {
    let guard = handle.world().read().unwrap();
    let mut cells: Vec<IVec3> = guard
        .iter_voxels()
        .filter(|(_, voxel)| *voxel == GRAIN)
        .map(|(position, _)| position)
        .collect();

    cells.sort_unstable_by_key(|cell| cell.to_array());

    cells
}

/// Feeds one whole period, requires exactly one tick, and hands back the
/// batches that tick submitted.
fn one_tick(handle: &Handle) -> Vec<Vec<MicroChunkSnapshot>> {
    let tick = run_tick(handle);

    assert_eq!(tick.report.ticks, 1, "one frame owes one tick");

    tick.report.batches
}

/// A renderer input holding the World as it stands, so the batches that
/// follow update it one tick at a time.
fn seeded(handle: &Handle) -> RendererInput {
    let input = RendererInput::new().unwrap();

    submit(
        &input,
        &emit_snapshots(&handle.world().read().unwrap()).unwrap(),
    );

    input
}

fn submit(input: &RendererInput, snapshots: &[MicroChunkSnapshot]) {
    input.submit_batch(snapshots.iter().cloned()).unwrap();
    input.wait_until_idle().unwrap();
}

/// One tick's handoff: the batch has to carry every micro-chunk the tick
/// changed, and applying it has to leave the renderer at the sim's World.
fn handoff(
    handle: &Handle,
    input: &RendererInput,
    before: &[MicroChunkSnapshot],
    batch: &[MicroChunkSnapshot],
) {
    let after = emit_snapshots(&handle.world().read().unwrap()).unwrap();
    let carried: Vec<IVec3> = batch
        .iter()
        .map(|snapshot| snapshot.global_coords)
        .collect();

    for snapshot in &after {
        let changed = before
            .iter()
            .find(|old| old.global_coords == snapshot.global_coords)
            .is_none_or(|old| old != snapshot);

        assert!(
            !changed || carried.contains(&snapshot.global_coords),
            "the batch omits the changed micro-chunk {:?}",
            snapshot.global_coords
        );
    }

    submit(input, batch);
    assert_geometry(&input.packed_regions().unwrap(), &after);
}

#[test]
fn a_grain_falls_straight_down_while_the_column_under_it_is_open() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(6, 6, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    let input = seeded(&handle);

    for y in [5, 4, 3, 2, 1] {
        let before = emit_snapshots(&handle.world().read().unwrap()).unwrap();
        let batches = one_tick(&handle);

        assert_eq!(batches.len(), 1, "a moving grain commits one batch");
        handoff(
            &handle,
            &input,
            &before,
            batches.first().expect("one batch"),
        );
        assert_eq!(grains(&handle), vec![IVec3::new(6, y, 6)]);
    }

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the settled grain submits nothing"
    );
    assert!(tick.report.commit_time.is_zero(), "nothing left to commit");
    assert_eq!(grains(&handle), vec![IVec3::new(6, 1, 6)]);
}

#[test]
fn two_due_ticks_submit_two_batches_in_tick_order() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(6, 6, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    let input = seeded(&handle);

    feed(&handle, part(9, 4));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 2, "two whole periods are owed two ticks");
    assert_eq!(tick.report.batches.len(), 2, "one batch per due tick");

    for batch in &tick.report.batches {
        submit(&input, batch);
    }

    let after = emit_snapshots(&handle.world().read().unwrap()).unwrap();

    assert_geometry(&input.packed_regions().unwrap(), &after);
    assert_eq!(
        grains(&handle),
        vec![IVec3::new(6, 4, 6)],
        "the grain fell one cell per tick"
    );
}

#[test]
fn a_falling_column_stays_coherent_and_spreads_into_a_row_at_the_floor() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(6, 3, 6, GRAIN));
    edits.push(set(6, 4, 6, GRAIN));
    edits.push(set(6, 5, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(6, 2, 6),
            IVec3::new(6, 4, 6),
            IVec3::new(6, 5, 6)
        ],
        "only the grain with an open column under it moves: a queued grain below holds the rest in place"
    );

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(6, 1, 6),
            IVec3::new(6, 3, 6),
            IVec3::new(6, 5, 6)
        ]
    );

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(6, 1, 6),
            IVec3::new(6, 2, 6),
            IVec3::new(6, 4, 6)
        ]
    );

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(6, 1, 6),
            IVec3::new(6, 3, 6),
            IVec3::new(7, 1, 6)
        ],
        "the grain that gained a settled support slides off it"
    );

    for _ in 0..4 {
        one_tick(&handle);
    }

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(6, 1, 6),
            IVec3::new(7, 1, 6)
        ],
        "three grains rest side by side on the floor"
    );

    let tick = run_tick(&handle);

    assert!(tick.report.batches.is_empty(), "the row has settled");
}

#[test]
fn a_grain_slides_a_diagonal_when_the_cell_below_holds_it() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(6, 1, 6, 1));
    edits.push(set(6, 2, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    let batches = one_tick(&handle);

    assert_eq!(batches.len(), 1);
    assert_eq!(
        grains(&handle),
        vec![IVec3::new(7, 1, 6)],
        "the even column prefers the positive x diagonal"
    );
    assert_eq!(
        held(&handle, IVec3::new(6, 1, 6)),
        Some(1),
        "the pillar under the grain stays"
    );

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the grain that landed on the floor settles"
    );
}

#[test]
fn a_corner_squeeze_is_sealed_at_the_grains_own_height() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(6, 1, 6, 1));
    edits.push(set(5, 2, 6, 1));
    edits.push(set(7, 2, 6, 1));
    edits.push(set(6, 2, 5, 1));
    edits.push(set(6, 2, 7, 1));
    edits.push(set(6, 2, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    for _ in 0..3 {
        let tick = run_tick(&handle);

        assert!(
            tick.report.batches.is_empty(),
            "every diagonal below is open, so only the sealed flanks hold the grain"
        );
        assert_eq!(grains(&handle), vec![IVec3::new(6, 2, 6)]);
    }
}

#[test]
fn diagonal_ties_follow_the_parity_policy() {
    let scene = || {
        let mut edits = floor();

        edits.push(set(6, 1, 6, 1));
        edits.push(set(6, 2, 6, GRAIN));
        edits.push(set(7, 1, 3, 1));
        edits.push(set(7, 2, 3, GRAIN));

        granular_activation_of(&edits)
    };

    let (_world, alternate) = spawn_sim_parity(ParityPolicy::Alternate);

    alternate.activate(scene());
    wait_ready(&alternate);
    one_tick(&alternate);

    assert_eq!(
        grains(&alternate),
        vec![IVec3::new(6, 1, 3), IVec3::new(7, 1, 6)],
        "x = 6 is even and takes the positive x diagonal, x = 7 is odd and takes the negative one"
    );

    let (_world, near) = spawn_sim_parity(ParityPolicy::AlwaysNegative);

    near.activate(scene());
    wait_ready(&near);
    one_tick(&near);

    assert_eq!(
        grains(&near),
        vec![IVec3::new(5, 1, 6), IVec3::new(6, 1, 3)],
        "every column takes the negative x diagonal under AlwaysNegative"
    );
}

#[test]
fn a_diagonal_on_the_z_axis_follows_the_same_parity_ties() {
    for (parity, expected) in [
        (ParityPolicy::Alternate, IVec3::new(7, 1, 7)),
        (ParityPolicy::AlwaysNegative, IVec3::new(7, 1, 5)),
    ] {
        let (_world, handle) = spawn_sim_parity(parity);
        let mut edits = floor();

        edits.push(set(7, 1, 6, 1));
        edits.push(set(7, 2, 6, GRAIN));
        handle.activate(granular_activation_of(&edits));
        wait_ready(&handle);

        one_tick(&handle);

        assert_eq!(
            grains(&handle),
            vec![expected],
            "x plus z is odd here, so z leads and ties on z"
        );

        let tick = run_tick(&handle);

        assert!(
            tick.report.batches.is_empty(),
            "the grain has settled on the floor"
        );
    }
}

#[test]
fn the_first_claim_wins_and_the_loser_retries_the_next_tick() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.retain(|edit| edit.position != IVec3::new(7, 0, 6));
    edits.push(set(6, -1, 6, 1));
    edits.push(set(7, -1, 6, 1));
    edits.push(set(8, -1, 6, 1));
    edits.push(set(4, 1, 6, 1));
    edits.push(set(6, 1, 6, 1));
    edits.push(set(4, 2, 6, GRAIN));
    edits.push(set(6, 2, 6, GRAIN));
    edits.push(set(7, 1, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(6, 2, 6),
            IVec3::new(7, 0, 6)
        ],
        "the first claim moved in, the loser kept its cell, and the blocker fell away"
    );

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(7, 0, 6),
            IVec3::new(7, 1, 6)
        ],
        "the loser retried into the diagonal its blocker vacated"
    );

    one_tick(&handle);

    let tick = run_tick(&handle);

    assert!(tick.report.batches.is_empty(), "every grain has settled");
    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(7, 0, 6),
            IVec3::new(7, 1, 6)
        ]
    );
}

#[test]
fn the_three_cells_above_a_vacated_cell_wake_up() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.retain(|edit| edit.position != IVec3::new(6, 0, 6));
    edits.push(set(6, -1, 6, 1));
    edits.push(set(7, 1, 6, 1));
    edits.push(set(7, 1, 5, 1));
    edits.push(set(7, 1, 7, 1));
    edits.push(set(8, 1, 6, 1));
    edits.push(set(6, 1, 6, GRAIN));
    edits.push(set(7, 2, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![IVec3::new(6, 0, 6), IVec3::new(7, 2, 6)],
        "the grain over the hole fell and the grain over the pillar sealed every diagonal"
    );

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![IVec3::new(6, 0, 6), IVec3::new(6, 1, 6)],
        "the sealed grain woke from the diagonal neighbor of the vacated cell and took it"
    );

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the stack rests: every diagonal below the top grain is the floor"
    );
}

#[test]
fn blocking_derives_from_queue_membership() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(4, 4, 4, GRAIN));
    handle.activate(granular_activation_of(&edits));

    let player = wait_ready(&handle);

    assert_eq!(player.feet, Vec3::new(4.5, 5.0, 4.5));
    assert!(
        !player.grounded,
        "the queued grain under the feet does not hold the player"
    );

    let tick = (0..20)
        .map(|_| run_tick(&handle))
        .last()
        .expect("twenty ticks run");

    assert_eq!(grains(&handle), vec![IVec3::new(4, 1, 4)]);
    assert_eq!(
        tick.player.feet,
        Vec3::new(4.5, 2.0, 4.5),
        "the settled grain holds the player one cell up"
    );
    assert!(tick.player.grounded);
}

#[test]
fn a_grain_settles_on_an_occupied_cell_that_is_not_solid() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor();

    edits.push(set(6, 1, 6, NON_BLOCKING));
    edits.push(set(6, 2, 6, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the cell below is neither solid nor a settled grain, so no diagonal is tried"
    );
    assert_eq!(grains(&handle), vec![IVec3::new(6, 2, 6)]);
}

#[test]
fn a_grain_at_the_lattice_edge_settles_as_against_a_solid() {
    let (_world, handle) = spawn_sim();
    let edits = vec![
        set(2047, 0, 0, 1),
        set(2046, 0, 0, 1),
        set(2047, 0, 1, 1),
        set(2047, 0, -1, 1),
        set(2047, 1, 0, GRAIN),
        set(0, -2048, 0, GRAIN),
    ];

    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    for _ in 0..3 {
        let tick = run_tick(&handle);

        assert!(
            tick.report.batches.is_empty(),
            "a destination outside the lattice is unavailable, so both grains settle"
        );
    }

    assert_eq!(
        grains(&handle),
        vec![IVec3::new(0, -2048, 0), IVec3::new(2047, 1, 0)],
        "no grain left the lattice"
    );
}

#[test]
fn every_activation_reseeds_the_queue() {
    let scene = || {
        let mut edits = floor();

        edits.push(set(6, 6, 6, GRAIN));

        granular_activation_of(&edits)
    };

    let (_world, handle) = spawn_sim();

    handle.activate(scene());
    wait_ready(&handle);
    one_tick(&handle);

    assert_eq!(grains(&handle), vec![IVec3::new(6, 5, 6)]);

    handle.activate(scene());
    wait_ready(&handle);

    let batches = one_tick(&handle);

    assert_eq!(
        batches.len(),
        1,
        "the new World's grain moved on its first tick"
    );
    assert_eq!(
        grains(&handle),
        vec![IVec3::new(6, 5, 6)],
        "the queue the new World starts from holds every grain of that World"
    );
}

#[test]
fn two_identical_90_tick_sequences_hash_the_same_under_both_parity_policies() {
    let scene = || {
        let mut edits = floor();

        edits.push(set(2, 1, 2, 1));
        edits.push(set(5, 1, 5, 1));
        edits.push(set(7, 1, 3, 1));

        for cell in [
            (2, 2, 2),
            (5, 2, 5),
            (7, 2, 3),
            (4, 3, 4),
            (4, 4, 4),
            (4, 5, 4),
            (1, 4, 1),
            (1, 5, 1),
            (6, 2, 6),
            (6, 3, 6),
            (3, 6, 3),
            (8, 4, 8),
            (0, 3, 7),
            (8, 7, 0),
        ] {
            edits.push(set(cell.0, cell.1, cell.2, GRAIN));
        }

        granular_activation_of(&edits)
    };

    for parity in [ParityPolicy::Alternate, ParityPolicy::AlwaysNegative] {
        let mut runs = [0u64; 2];

        for run in &mut runs {
            let (_world, handle) = spawn_sim_parity(parity);

            handle.activate(scene());
            wait_ready(&handle);

            let rested = world_hash(&handle);

            for _ in 0..90 {
                one_tick(&handle);
            }

            let settled = world_hash(&handle);

            assert_ne!(
                settled, rested,
                "the scene has to churn for a hash match to mean anything"
            );

            *run = settled;
        }

        assert_eq!(
            runs[0], runs[1],
            "two 90-tick sequences under {parity:?} hash the same"
        );
    }
}
