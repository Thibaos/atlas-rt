use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use atlas_rt::render::region::feed::RendererInput;
use atlas_rt::render::region::pack::{RegionData, pack_regions};
use atlas_rt::sim::{
    self, Activation, Command, Handle, InputSample, PlayerProfile, PlayerState, Push, TickEnd,
};
use atlas_rt::world::World;
use atlas_rt::world::material::PhysicalMaterialTable;
use atlas_rt::world::update::batch::TrackedCoords;
use atlas_rt::world::update::edit::{VoxelChange, VoxelEdit, edit_world};
use atlas_rt::world::update::snapshot::{MicroChunkSnapshot, emit_snapshots};
use glam::{IVec3, Vec3};

const TIMEOUT: Duration = Duration::from_secs(2);
const SETTLE: Duration = Duration::from_millis(150);

fn spawn_sim() -> (Arc<RwLock<World>>, Handle) {
    let world = Arc::new(RwLock::new(World::default()));
    let handle =
        sim::spawn(Arc::clone(&world), PlayerProfile::default()).expect("the sim must spawn");

    (world, handle)
}

fn period() -> Duration {
    PlayerProfile::default().tick_period()
}

fn part(numerator: u64, denominator: u64) -> Duration {
    let scaled = u128::from(period().as_nanos())
        .checked_mul(u128::from(numerator))
        .and_then(|product| product.checked_div(u128::from(denominator)))
        .expect("the fraction must divide");

    Duration::from_nanos(u64::try_from(scaled).expect("the fraction fits a Duration"))
}

fn feed(handle: &Handle, elapsed: Duration) {
    handle.frame(elapsed, InputSample::default());
}

fn set(x: i32, y: i32, z: i32, material: u8) -> VoxelEdit {
    VoxelEdit {
        position: IVec3::new(x, y, z),
        change: VoxelChange::Set(material),
    }
}

/// A loaded `World` with the snapshots and tracked set its edits produced.
fn activation_of(edits: &[VoxelEdit]) -> Activation {
    let mut world = World::default();
    let batch = edit_world(&mut world, edits, &TrackedCoords::default()).unwrap();

    Activation {
        world,
        snapshots: batch.snapshots,
        tracked: batch.tracked,
        materials: PhysicalMaterialTable::default(),
    }
}

fn held(handle: &Handle, position: IVec3) -> Option<u32> {
    let guard = handle.world().read().unwrap();

    guard.get_voxel(&position).copied()
}

/// Waits for readiness, taking the activation batch that comes first.
fn wait_ready(handle: &Handle) -> PlayerState {
    let deadline = Instant::now() + TIMEOUT;

    loop {
        match handle.try_recv() {
            Ok(Push::Ready { player }) => return player,
            Ok(Push::ActivationBatch(_)) => continue,
            Ok(Push::Tick(tick)) => panic!("a tick arrived before readiness: {tick:?}"),
            Err(TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "the sim never answered with readiness"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(TryRecvError::Disconnected) => panic!("the sim thread stopped"),
        }
    }
}

fn recv_push(handle: &Handle) -> Push {
    handle.recv_timeout(TIMEOUT).expect("the sim must push")
}

fn expect_batch(push: Push) -> Vec<MicroChunkSnapshot> {
    match push {
        Push::ActivationBatch(batch) => batch,
        push => panic!("expected an activation batch, got {push:?}"),
    }
}

fn expect_tick(push: Push) -> TickEnd {
    match push {
        Push::Tick(tick) => tick,
        push => panic!("expected a tick end, got {push:?}"),
    }
}

fn assert_geometry(actual: &[RegionData], expected: &[MicroChunkSnapshot]) {
    let want = pack_regions(expected).unwrap();

    assert_eq!(actual.len(), want.len(), "resident region count");

    for (got, want) in actual.iter().zip(&want) {
        assert_eq!(got.region_index, want.region_index);
        assert_eq!(
            got.blocks.len(),
            want.blocks.len(),
            "region {} block byte count",
            got.region_index
        );
        assert!(
            got.blocks == want.blocks,
            "region {} blocks differ from the direct pack of the expected snapshots",
            got.region_index
        );
        assert_eq!(got.aabbs, want.aabbs, "region {} aabbs", got.region_index);
    }
}

/// Forwards one report batch through the renderer input and requires the
/// regions it packs to equal what the held World emits.
fn forward(handle: &Handle, batch: &[MicroChunkSnapshot]) {
    let input = RendererInput::new().unwrap();

    input.submit_batch(batch.iter().cloned()).unwrap();
    input.wait_until_idle().unwrap();

    let guard = handle.world().read().unwrap();
    let expected = emit_snapshots(&guard).unwrap();

    assert_geometry(&input.packed_regions().unwrap(), &expected);
}

/// Requires the sim to hold every push back for `SETTLE` of wall time.
fn assert_silent(handle: &Handle) {
    thread::sleep(SETTLE);

    match handle.try_recv() {
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => panic!("the sim thread stopped"),
        Ok(push) => panic!("expected no push, got {push:?}"),
    }
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

        assert_eq!(guard.get_voxel(&IVec3::new(4, 4, 4)), Some(&6));
        assert_eq!(guard.voxel_count(), 1);
    }

    handle.activate(activation_of(&[set(8, 8, 8, 2)]));
    wait_ready(&handle);

    let guard = world.read().unwrap();

    assert_eq!(guard.get_voxel(&IVec3::new(8, 8, 8)), Some(&2));
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
            feet: Vec3::new(2.0, 2.0, 2.0),
            grounded: true,
        }
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
            grounded: false,
        },
        "the feet rest on an empty column, so the fall begins"
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
        (player.feet.y - 2046.2).abs() < 0.01,
        "the climb stops at the ceiling, feet at {}",
        player.feet.y
    );
    assert_eq!(player.feet.x, 0.5);
    assert_eq!(player.feet.z, 0.5);
    assert!(player.grounded, "a buried player still rests on content");
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

    handle.command(Command::Edits(vec![set(5, 5, 5, 3)]));
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
    handle.command(Command::Edits(vec![set(7, 7, 7, 5)]));
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

    handle.command(Command::Edits(vec![set(1, 1, 1, 4)]));
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

    handle.command(Command::Edits(vec![set(2, 2, 2, 5)]));
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
