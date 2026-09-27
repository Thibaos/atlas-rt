//! The sim harness every integration test drives: spawn, activate, feed
//! frames, and take the pushes in the order the sim sends them.

#![allow(dead_code)]

use std::hash::{Hash, Hasher};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use atlas_rt::render::region::feed::RendererInput;
use atlas_rt::render::region::pack::{RegionData, pack_regions};
use atlas_rt::sim::{
    self, Activation, Handle, InputSample, ParityPolicy, PlayerProfile, PlayerState, Push, TickEnd,
};
use atlas_rt::world::World;
use atlas_rt::world::material::{PhysicalMaterialTable, parse_override};
use atlas_rt::world::update::batch::TrackedCoords;
use atlas_rt::world::update::edit::{VoxelChange, VoxelEdit, edit_world};
use atlas_rt::world::update::snapshot::{MicroChunkSnapshot, emit_snapshots};
use glam::IVec3;

pub const TIMEOUT: Duration = Duration::from_secs(2);
pub const SETTLE: Duration = Duration::from_millis(150);

/// The material every falling-granular test writes.
pub const GRAIN: u8 = 2;

/// The material a falling-granular test writes for a cell that is occupied
/// without blocking the player.
pub const NON_BLOCKING: u8 = 3;

pub fn spawn_sim() -> (Arc<RwLock<World>>, Handle) {
    spawn_sim_with(PlayerProfile::default())
}

pub fn spawn_sim_with(profile: PlayerProfile) -> (Arc<RwLock<World>>, Handle) {
    spawn_with(profile, ParityPolicy::default())
}

pub fn spawn_sim_parity(parity: ParityPolicy) -> (Arc<RwLock<World>>, Handle) {
    spawn_with(PlayerProfile::default(), parity)
}

fn spawn_with(profile: PlayerProfile, parity: ParityPolicy) -> (Arc<RwLock<World>>, Handle) {
    let world = Arc::new(RwLock::new(World::default()));
    let handle = sim::spawn(Arc::clone(&world), profile, parity).expect("the sim must spawn");

    (world, handle)
}

pub fn period() -> Duration {
    PlayerProfile::default().tick_period()
}

pub fn part(numerator: u64, denominator: u64) -> Duration {
    let scaled = u128::from(period().as_nanos())
        .checked_mul(u128::from(numerator))
        .and_then(|product| product.checked_div(u128::from(denominator)))
        .expect("the fraction must divide");

    Duration::from_nanos(u64::try_from(scaled).expect("the fraction fits a Duration"))
}

pub fn feed(handle: &Handle, elapsed: Duration) {
    handle.frame(elapsed, InputSample::default());
}

pub fn set(x: i32, y: i32, z: i32, material: u8) -> VoxelEdit {
    VoxelEdit {
        position: IVec3::new(x, y, z),
        change: VoxelChange::Set(material),
    }
}

/// A loaded `World` with the snapshots and tracked set its edits produced.
pub fn activation_of(edits: &[VoxelEdit]) -> Activation {
    activation_with(edits, PhysicalMaterialTable::default())
}

pub fn activation_with(edits: &[VoxelEdit], materials: PhysicalMaterialTable) -> Activation {
    let mut world = World::default();
    let batch = edit_world(&mut world, edits, &TrackedCoords::default()).unwrap();

    Activation {
        world,
        snapshots: batch.snapshots,
        tracked: batch.tracked,
        materials,
    }
}

/// An activation on the falling-granular table: [`GRAIN`] holds and blocks,
/// and [`NON_BLOCKING`] is occupied without blocking the player.
pub fn granular_activation_of(edits: &[VoxelEdit]) -> Activation {
    activation_with(edits, granular_table())
}

pub fn granular_table() -> PhysicalMaterialTable {
    parse_override(&format!(
        "material {GRAIN} falling_granular solid=true\nmaterial {NON_BLOCKING} solid solid=false"
    ))
    .expect("the table must parse")
}

/// Waits for readiness, taking the activation batch that comes first.
pub fn wait_ready(handle: &Handle) -> PlayerState {
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

pub fn recv_push(handle: &Handle) -> Push {
    handle.recv_timeout(TIMEOUT).expect("the sim must push")
}

pub fn expect_tick(push: Push) -> TickEnd {
    match push {
        Push::Tick(tick) => tick,
        push => panic!("expected a tick end, got {push:?}"),
    }
}

/// Requires the sim to hold every push back for `SETTLE` of wall time.
pub fn assert_silent(handle: &Handle) {
    thread::sleep(SETTLE);

    match handle.try_recv() {
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => panic!("the sim thread stopped"),
        Ok(push) => panic!("expected no push, got {push:?}"),
    }
}

/// Feeds exactly one whole period and takes the update it owes.
pub fn run_tick(handle: &Handle) -> TickEnd {
    feed(handle, period());

    expect_tick(recv_push(handle))
}

pub fn held(handle: &Handle, position: IVec3) -> Option<u32> {
    let guard = handle.world().read().unwrap();

    guard.get_voxel(&position).copied()
}

/// The hash of the held World's cells in coordinate order.
pub fn world_hash(handle: &Handle) -> u64 {
    let guard = handle.world().read().unwrap();
    let mut cells: Vec<(IVec3, u32)> = guard.iter_voxels().map(|(p, v)| (p, *v)).collect();

    cells.sort_unstable_by_key(|(position, _)| position.to_array());

    let mut hasher = rustc_hash::FxHasher::default();

    cells.hash(&mut hasher);

    hasher.finish()
}

pub fn expect_batch(push: Push) -> Vec<MicroChunkSnapshot> {
    match push {
        Push::ActivationBatch(batch) => batch,
        push => panic!("expected an activation batch, got {push:?}"),
    }
}

pub fn assert_geometry(actual: &[RegionData], expected: &[MicroChunkSnapshot]) {
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
pub fn forward(handle: &Handle, batch: &[MicroChunkSnapshot]) {
    let input = RendererInput::new().unwrap();

    input.submit_batch(batch.iter().cloned()).unwrap();
    input.wait_until_idle().unwrap();

    let guard = handle.world().read().unwrap();
    let expected = emit_snapshots(&guard).unwrap();

    assert_geometry(&input.packed_regions().unwrap(), &expected);
}
