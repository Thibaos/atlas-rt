mod common;

use std::thread;
use std::time::{Duration, Instant};

use atlas_rt::render::region::queue::RendererInput;
use atlas_rt::sim::{Command, Handle, InputSample, PlayerProfile, PlayerState};
use atlas_rt::world::diff::edit::VoxelEdit;
use atlas_rt::world::diff::snapshot::{MicroChunkSnapshot, emit_snapshots};
use glam::{IVec3, Vec2, Vec3};

use common::*;

const EPSILON: f32 = 1.0e-3;

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < EPSILON
}

#[test]
fn frames_before_the_first_activation_run_no_ticks() {
    let (_world, handle) = spawn_sim();

    for _ in 0..4 {
        feed(&handle, period().saturating_mul(5));
    }

    assert_silent(&handle);

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));

    assert!(wait_ready(&handle).grounded);

    feed(&handle, period());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(
        tick.report.ticks, 1,
        "the frames before activation left no owed time"
    );
}

#[test]
fn activation_answers_with_the_batch_before_readiness_and_the_batch_renders() {
    let (_world, handle) = spawn_sim();
    let activation = activation_of(&[set(0, 0, 0, 3), set(1, 0, 0, 3)]);
    let expected = emit_snapshots(&activation.world).unwrap();

    handle.activate(activation);

    let batch = expect_batch(recv_push(&handle));

    assert_eq!(
        batch, expected,
        "the first activation plans the loader's snapshots with nothing to clear"
    );

    // the host uploads the palette before the activation, so the batch is
    // renderable the moment it arrives
    let input = RendererInput::new().unwrap();

    input.submit_batch(batch).unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        !input.packed_regions().unwrap().is_empty(),
        "a batch arriving before readiness still renders"
    );

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(1.0, 1.0, 0.5),
            grounded: true,
        }
    );
}

#[test]
fn activation_leaves_the_new_world_readable_through_the_shared_lock() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(4, 4, 4, 6)]));
    wait_ready(&handle);

    {
        let guard = world.read().unwrap();

        assert_eq!(guard.get_voxel(&IVec3::new(4, 4, 4)), Some(6));
        assert_eq!(guard.voxel_count(), 1);
    }

    handle.activate(activation_of(&[set(8, 8, 8, 2)]));
    wait_ready(&handle);

    let guard = world.read().unwrap();

    assert_eq!(guard.get_voxel(&IVec3::new(8, 8, 8)), Some(2));
    assert_eq!(
        guard.get_voxel(&IVec3::new(4, 4, 4)),
        None,
        "the replaced World's content is gone"
    );
}

#[test]
fn spawn_sits_on_top_of_the_highest_cell_of_the_center_column() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[
        set(0, 0, 0, 1),
        set(3, 4, 3, 1),
        set(2, 1, 2, 1),
    ]));

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(2.0, 5.0, 2.0),
            grounded: true,
        },
        "the wide collider climbs over the cells it now spans until nothing solid touches it"
    );
}

#[test]
fn an_empty_spawn_column_spawns_at_the_roofline() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1), set(3, 0, 0, 1)]));

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(2.0, 1.0, 0.5),
            grounded: true,
        },
        "the feet sit at the roofline and the wide footprint rests on the solid cells beside the empty column"
    );
}

#[test]
fn an_empty_world_spawns_on_the_lattice_floor() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[]));

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(0.0, -2048.0, 0.0),
            grounded: true,
        }
    );
}

#[test]
fn a_spawn_straddling_solid_cells_depenetrates_upward() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[
        set(2, 0, 0, 1),
        set(2, 1, 0, 1),
        set(1, 2, 0, 1),
        set(1, 3, 0, 1),
    ]));

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(2.0, 4.0, 0.5),
            grounded: true,
        },
        "the collider climbs cell by cell until nothing solid touches it"
    );
}

#[test]
fn a_spawn_with_no_clear_position_stays_buried() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 2046, 0, 1), set(0, 2047, 0, 1)]));

    let player = wait_ready(&handle);

    assert!(
        (player.feet.y - 2030.0).abs() < 0.01,
        "the climb stops at the ceiling, feet at {}",
        player.feet.y
    );
    assert_eq!(player.feet.x, 0.5);
    assert_eq!(player.feet.z, 0.5);
    assert!(!player.grounded, "the row under the buried feet is empty");
}

#[test]
fn a_clear_is_an_activation_with_an_empty_world_and_still_ticks() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 4)]));
    wait_ready(&handle);

    handle.activate(activation_of(&[]));

    assert_eq!(
        expect_batch(recv_push(&handle)),
        vec![MicroChunkSnapshot::cleared(IVec3::ZERO)],
        "the outgoing world's only tracked chunk is cleared"
    );

    assert!(wait_ready(&handle).grounded);

    feed(&handle, period());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(
        tick.report.ticks, 1,
        "a clear is an activation, so it ticks"
    );
}

#[test]
fn a_load_in_flight_keeps_ticking_keeping_commands_and_its_activation_resets_time() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    feed(&handle, part(9, 10));
    assert_silent(&handle);

    handle.command(Command::Cell(set(5, 5, 5, 3)));
    feed(&handle, part(2, 10));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1);
    assert_eq!(tick.report.batches.len(), 1, "the command committed");
    forward(&handle, tick.report.batches.first().expect("one batch"));
    assert_eq!(held(&handle, IVec3::new(5, 5, 5)), Some(3));

    feed(&handle, period());

    let tick = expect_tick(recv_push(&handle));

    assert!(tick.report.commit_time.is_zero(), "no command to commit");
    assert!(tick.report.batches.is_empty());

    feed(&handle, part(9, 10));
    assert_silent(&handle);

    // the load request is sent only once the current World has run down to
    // less than a tick, so a tick here would prove the reset did not happen
    handle.command(Command::Cell(set(7, 7, 7, 5)));
    handle.activate(activation_of(&[set(1, 1, 1, 2)]));

    assert!(wait_ready(&handle).grounded);

    feed(&handle, part(9, 10));
    assert_silent(&handle);

    feed(&handle, part(2, 10));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1, "the activation reset the accumulator");
    assert_eq!(tick.report.batches.len(), 1, "the queued command survived");
    assert!(tick.snap, "the first update after an activation snaps");
    assert_eq!(held(&handle, IVec3::new(7, 7, 7)), Some(5));
    assert_eq!(held(&handle, IVec3::new(1, 1, 1)), Some(2));
    assert_eq!(
        held(&handle, IVec3::new(5, 5, 5)),
        None,
        "the previous World's edit went with its World"
    );
    assert_eq!(held(&handle, IVec3::new(0, 0, 0)), None);
}

#[test]
fn an_update_pushes_once_with_report_remainder_and_snap() {
    let (_world, handle) = spawn_sim();

    handle.command(Command::Cell(set(1, 1, 1, 4)));
    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    feed(&handle, period().saturating_mul(3));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 3);
    assert_eq!(tick.report.discarded, 0);
    assert_eq!(tick.report.batches.len(), 1);
    forward(&handle, tick.report.batches.first().expect("one batch"));
    assert!(tick.snap);
    assert!(tick.remainder.is_zero());
    assert_silent(&handle);

    feed(&handle, period().saturating_add(part(1, 2)));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1);
    assert_eq!(tick.report.discarded, 0);
    assert!(tick.report.batches.is_empty());
    assert!(tick.report.commit_time.is_zero());
    assert!(!tick.snap);
    assert_eq!(tick.remainder, part(1, 2));
    assert_silent(&handle);
}

/// The queue depth the host's log reads: the report carries what the last
/// tick left queued, and a settled scene reports nothing left.
#[test]
fn the_report_carries_the_cells_the_last_tick_left_queued() {
    let (_world, handle) = spawn_sim();
    let mut edits = Vec::new();

    for x in 0..=8 {
        for z in 0..=8 {
            edits.push(set(x, 0, z, 1));
        }
    }

    edits.push(set(6, 6, 6, GRAIN));

    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    let falling = run_tick(&handle);

    assert!(
        falling.report.queued > 0,
        "the moving grain stays queued: {}",
        falling.report.queued
    );

    let settled = (0..8)
        .map(|_| run_tick(&handle))
        .find(|tick| tick.report.queued == 0)
        .expect("the settled grain leaves the queue empty");

    assert_eq!(settled.report.ticks, 1);

    let resting = run_tick(&handle);

    assert_eq!(
        resting.report.queued, 0,
        "a settled queue stays empty until an edit wakes it"
    );
}

#[test]
fn an_update_runs_at_most_five_ticks_and_reports_the_discard() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    feed(&handle, period().saturating_mul(12));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 5, "catch-up is capped at five ticks");
    assert_eq!(tick.report.discarded, 7, "the rest is reported, not run");
    assert!(tick.remainder.is_zero());
    assert_silent(&handle);
}

#[test]
fn ticks_only_run_while_frames_arrive() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    feed(&handle, part(9, 10));
    assert_silent(&handle);

    feed(&handle, part(2, 10));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1, "only the frames' time is owed");
}

#[test]
fn the_write_lock_stalls_a_tick_until_the_host_releases_it() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    let held_lock = world.write().unwrap();

    // evaluation needs the read lock, which the host's write lock excludes
    feed(&handle, period());
    assert_silent(&handle);

    drop(held_lock);

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1);
}

#[test]
fn commit_waits_for_the_write_lock_while_the_host_reads() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    let held_lock = world.read().unwrap();

    handle.command(Command::Cell(set(2, 2, 2, 5)));
    feed(&handle, period());

    thread::sleep(Duration::from_millis(300));

    if let Ok(push) = handle.try_recv() {
        panic!("the commit cannot finish while the host holds the read lock: {push:?}");
    }

    drop(held_lock);

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1);
    assert_eq!(tick.report.batches.len(), 1);
    assert!(
        !tick.report.commit_time.is_zero(),
        "the commit window carries the lock wait, measured {:?}",
        tick.report.commit_time
    );
    assert_eq!(held(&handle, IVec3::new(2, 2, 2)), Some(5));
}

#[test]
fn evaluation_takes_only_the_read_lock_and_pushes_while_the_host_reads() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    let held_lock = world.read().unwrap();

    feed(&handle, period());

    let tick = expect_tick(
        handle
            .recv_timeout(TIMEOUT)
            .expect("the tick must reach the host while the host reads"),
    );

    assert_eq!(tick.report.ticks, 1);
    assert!(
        tick.report.commit_time.is_zero(),
        "no queued command means the write lock is never taken"
    );
    assert!(tick.player.grounded);

    drop(held_lock);
}

/// The marker command is queued behind the backlog, so it commits only if the
/// sim drains every queued frame instead of stopping on the shutdown flag.
/// The marker staying uncommitted is how the test sees that the drop did not
/// wait for the backlog.
#[test]
fn a_queued_frame_backlog_does_not_delay_the_join() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    // far more frames than the sim can reach before the handle drops
    const BACKLOG: usize = 100_000;

    for _ in 0..BACKLOG {
        feed(&handle, period());
    }

    handle.command(Command::Cell(set(5, 5, 5, 3)));
    feed(&handle, period());

    drop(handle);

    let guard = world.read().unwrap();

    assert_eq!(
        guard.get_voxel(&IVec3::new(5, 5, 5)),
        None,
        "the join drained the queued frames past the marker instead of stopping promptly"
    );
}

/// A frame owes five ticks, and a held write lock stalls its first evaluation.
/// The drop raises the flag while that tick is stuck; once the lock is freed
/// the sim must stop between ticks, so the grain falls one cell, not five.
#[test]
fn a_shutdown_during_a_multi_tick_frame_stops_between_ticks() {
    let (world, handle) = spawn_sim();

    handle.activate(granular_activation_of(&[
        set(6, 0, 6, 1),
        set(6, 6, 6, GRAIN),
    ]));
    wait_ready(&handle);

    let held = world.write().unwrap();

    feed(&handle, period().saturating_mul(5));

    let dropper = thread::spawn(move || drop(handle));

    thread::sleep(Duration::from_millis(50));
    drop(held);
    dropper.join().unwrap();

    let guard = world.read().unwrap();

    assert_eq!(
        guard.get_voxel(&IVec3::new(6, 5, 6)),
        Some(GRAIN),
        "the one tick in flight when the flag rose ran"
    );
    assert_eq!(
        guard.get_voxel(&IVec3::new(6, 4, 6)),
        None,
        "the ticks behind the flag's check did not run"
    );
}

#[test]
fn pausing_stops_accumulation_and_resume_starts_from_an_empty_accumulator() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    feed(&handle, part(9, 10));
    assert_silent(&handle);

    handle.set_paused(true);

    // six whole ticks and a half sit owed while paused, and none of it runs
    feed(
        &handle,
        period().saturating_mul(6).saturating_add(part(1, 2)),
    );
    assert_silent(&handle);

    handle.set_paused(false);

    // the held ninth came back with the resume, so this frame would owe a
    // full tick if the pause had kept it
    feed(&handle, part(2, 10));
    assert_silent(&handle);

    feed(&handle, period());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(
        tick.report.ticks, 1,
        "resume starts with an empty accumulator"
    );
    assert_eq!(tick.report.discarded, 0);
    assert_eq!(tick.remainder, part(2, 10));
    assert_silent(&handle);
}

#[test]
fn time_owed_while_paused_is_neither_run_nor_reported_as_a_drop() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    handle.set_paused(true);
    feed(&handle, period().saturating_mul(9));
    assert_silent(&handle);
    handle.set_paused(false);

    feed(&handle, period().saturating_mul(12));

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 5, "catch-up is capped at five ticks");
    assert_eq!(
        tick.report.discarded, 7,
        "the paused frames owed nothing to discard"
    );
    assert!(tick.remainder.is_zero());
    assert_silent(&handle);
}

#[test]
fn activation_while_paused_stays_paused_until_the_host_resumes() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    handle.set_paused(true);
    handle.activate(activation_of(&[set(3, 3, 3, 1)]));

    assert!(wait_ready(&handle).grounded);

    feed(&handle, period().saturating_mul(4));
    assert_silent(&handle);

    handle.set_paused(false);
    feed(&handle, period());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1, "the activation kept the pause");
}

#[test]
fn the_profile_survives_activation_and_a_different_profile_needs_a_new_sim() {
    let profile = PlayerProfile::new(0.5, 0.5, 2.0, 1.7, 3.5, 18.0, 7.0, 1.0, 0.1, 60.0)
        .unwrap_or_else(|error| panic!("{error}"));
    let (_world, handle) = spawn_sim_with(profile);

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    // one 60 Hz period owes one tick; a sim that fell back to the 30 Hz
    // default would owe none
    handle.frame(profile.tick_period(), InputSample::default());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 1);
    assert!(tick.remainder.is_zero());

    handle.activate(activation_of(&[set(2, 2, 2, 1)]));
    wait_ready(&handle);

    handle.frame(profile.tick_period(), InputSample::default());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(
        tick.report.ticks, 1,
        "the swap left the controller's profile in place"
    );
    assert!(tick.remainder.is_zero());
}

/// A floor wide enough for three ticks of a walk to stay on it.
fn walk_floor() -> Vec<VoxelEdit> {
    (0..=7)
        .flat_map(|x| (0..=4).map(move |z| set(x, 0, z, 1)))
        .collect()
}

/// A floor with a pillar under the spawn column, so the player spawns
/// grounded with clear air above it.
fn pillar_room() -> Vec<VoxelEdit> {
    let mut edits: Vec<VoxelEdit> = (0..=4).map(|x| set(x, 0, 2, 1)).collect();

    edits.push(set(2, 3, 2, 1));

    edits
}

fn pressed() -> InputSample {
    InputSample {
        movement: Vec2::ZERO,
        jump_edge: Some(Instant::now()),
    }
}

fn keys(movement: Vec2) -> InputSample {
    InputSample {
        movement,
        jump_edge: None,
    }
}

/// Queues `frames` as one burst the sim cannot start on until every frame is
/// in its inbox. The World write lock is held across the activation and the
/// frames, so the sim's activation blocks until the whole burst is queued;
/// the lock is the harness's way of driving a backlog without sleeping.
/// Returns the handle once readiness arrived, and the pose it spawned on.
fn burst(scene: &[VoxelEdit], frames: &[(Duration, InputSample)]) -> (Handle, PlayerState) {
    let (world, handle) = spawn_sim();
    let locked = world.write().unwrap();

    handle.activate(activation_of(scene));

    for &(elapsed, sample) in frames {
        handle.frame(elapsed, sample);
    }

    drop(locked);

    let spawn = wait_ready(&handle);

    (handle, spawn)
}

#[test]
fn a_burst_of_queued_frames_coalesces_into_one_update() {
    let (handle, _) = burst(&[set(0, 0, 0, 1)], &[(period(), InputSample::default()); 3]);

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(
        tick.report.ticks, 3,
        "the burst's three periods are one update"
    );
    assert_eq!(tick.report.discarded, 0);
    assert!(
        tick.remainder.is_zero(),
        "the burst divided into whole ticks with nothing left over"
    );
    assert_silent(&handle);
}

#[test]
fn a_burst_past_the_catch_up_cap_reports_the_discarded_ticks() {
    let (handle, _) = burst(
        &[set(0, 0, 0, 1)],
        &[(period(), InputSample::default()); 12],
    );

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 5, "catch-up is capped at five ticks");
    assert_eq!(
        tick.report.discarded, 7,
        "the rest of the burst is reported, not run"
    );
    assert!(tick.remainder.is_zero());
    assert_silent(&handle);
}

#[test]
fn a_burst_applies_the_newest_movement_sample() {
    const DISTANCE: f32 = 40.0 / 30.0;

    let (handle, spawn) = burst(
        &walk_floor(),
        &[
            (period(), keys(Vec2::X)),
            (period(), keys(Vec2::X)),
            (period(), keys(Vec2::NEG_X)),
        ],
    );

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 3, "the whole burst is one update");
    assert!(
        near(tick.player.feet.x, spawn.feet.x - 3.0 * DISTANCE),
        "only the newest movement ran, so three ticks walked west from {}: {}",
        spawn.feet.x,
        tick.player.feet.x
    );
    assert_eq!(
        tick.player.feet.y, spawn.feet.y,
        "the walk stayed on the floor instead of leaving it"
    );
    assert!(tick.player.grounded);
}

#[test]
fn a_jump_edge_inside_a_coalesced_burst_still_launches() {
    let (handle, spawn) = burst(
        &pillar_room(),
        &[
            (period(), pressed()),
            (period(), InputSample::default()),
            (period(), InputSample::default()),
        ],
    );

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 3);
    assert!(
        near(tick.player.feet.y, spawn.feet.y + 7.333333),
        "the press on the first frame of the run launched three ticks of arc from {}: {}",
        spawn.feet.y,
        tick.player.feet.y
    );
    assert_eq!(tick.player.feet.x, spawn.feet.x);
    assert_eq!(tick.player.feet.z, spawn.feet.z);
}

#[test]
fn a_command_between_frames_ends_the_coalesced_run() {
    let (world, handle) = spawn_sim();
    let locked = world.write().unwrap();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    feed(&handle, period());
    handle.command(Command::Cell(set(5, 5, 5, 3)));
    feed(&handle, period());

    drop(locked);

    wait_ready(&handle);

    let before = expect_tick(recv_push(&handle));

    assert_eq!(before.report.ticks, 1, "the command ended the frame run");
    assert!(
        before.report.batches.is_empty(),
        "the command had not arrived when the first run ticked"
    );

    let after = expect_tick(recv_push(&handle));

    assert_eq!(after.report.ticks, 1, "the second run keeps its own time");
    assert_eq!(
        after.report.batches.len(),
        1,
        "the command committed in the second update"
    );
    assert_eq!(held(&handle, IVec3::new(5, 5, 5)), Some(3));
    assert_silent(&handle);
}
