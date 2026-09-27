//! The sim harness every integration test drives: spawn, activate, feed
//! frames, and take the pushes in the order the sim sends them.

#![allow(dead_code)]

use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use atlas_rt::sim::{
    self, Activation, Handle, InputSample, PlayerProfile, PlayerState, Push, TickEnd,
};
use atlas_rt::world::World;
use atlas_rt::world::material::PhysicalMaterialTable;
use atlas_rt::world::update::batch::TrackedCoords;
use atlas_rt::world::update::edit::{VoxelChange, VoxelEdit, edit_world};
use glam::IVec3;

pub const TIMEOUT: Duration = Duration::from_secs(2);
pub const SETTLE: Duration = Duration::from_millis(150);

pub fn spawn_sim() -> (Arc<RwLock<World>>, Handle) {
    spawn_sim_with(PlayerProfile::default())
}

pub fn spawn_sim_with(profile: PlayerProfile) -> (Arc<RwLock<World>>, Handle) {
    let world = Arc::new(RwLock::new(World::default()));
    let handle = sim::spawn(Arc::clone(&world), profile).expect("the sim must spawn");

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
    let mut world = World::default();
    let batch = edit_world(&mut world, edits, &TrackedCoords::default()).unwrap();

    Activation {
        world,
        snapshots: batch.snapshots,
        tracked: batch.tracked,
        materials: PhysicalMaterialTable::default(),
    }
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
