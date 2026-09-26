use std::mem;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::error;

use crate::world::World;
use crate::world::material::PhysicalMaterialTable;
use crate::world::update::{
    batch::{TrackedCoords, plan_load},
    edit::{VoxelEdit, edit_world},
    snapshot::MicroChunkSnapshot,
};

use super::input::{InputSample, PlayerState};
use super::profile::PlayerProfile;
use super::scheduler::Scheduler;
use super::spawn::{floor_pose, grounded, pose};

/// One frame of host time and buffered input, one edit command, one World
/// handover, or the shutdown that ends the loop.
enum Message {
    Frame {
        elapsed: Duration,
        sample: InputSample,
    },
    Command(Command),
    Activation(Box<Activation>),
    Shutdown,
}

/// The `World` handover.
///
/// The loaded `World` with the snapshots and tracked set that describe what
/// the renderer already holds. The sim plans the snapshots under its write
/// lock, so it owns tracking from here on.
#[derive(Debug)]
pub struct Activation {
    pub world: World,
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub tracked: TrackedCoords,
    pub materials: PhysicalMaterialTable,
}

/// A host edit resolved at the next commit, against the `World` then active.
#[derive(Debug)]
pub enum Command {
    Edits(Vec<VoxelEdit>),
}

/// What the sim hands back: an activation batch, the readiness reply, or one
/// push per update that ran ticks.
#[derive(Debug)]
pub enum Push {
    ActivationBatch(Vec<MicroChunkSnapshot>),
    Ready { player: PlayerState },
    Tick(TickEnd),
}

/// One update's tick-end payload, sent once per update that ran ticks.
#[derive(Debug)]
pub struct TickEnd {
    pub player: PlayerState,
    pub remainder: Duration,
    pub snap: bool,
    pub report: UpdateReport,
}

/// What one update did: ticks run, batches committed in tick order, ticks
/// past the catch-up cap, and the summed evaluation and commit times.
#[derive(Debug)]
pub struct UpdateReport {
    pub ticks: u32,
    pub batches: Vec<Vec<MicroChunkSnapshot>>,
    pub discarded: u64,
    pub tick_time: Duration,
    pub commit_time: Duration,
}

/// Spawns the sim thread against `world` and returns the host's handle to it.
/// The sim takes no ticks until an activation arrives.
///
/// # Errors
///
/// Returns an error when the operating system refuses the thread.
pub fn spawn(world: Arc<RwLock<World>>, profile: PlayerProfile) -> Result<Handle> {
    let (host, inbox) = mpsc::channel();
    let (pushes, outbox) = mpsc::channel();

    let shared = Arc::clone(&world);
    let runtime = Runtime::new(shared, profile, pushes);

    let thread = thread::Builder::new()
        .name(String::from("atlas-sim"))
        .spawn(move || runtime.run(&inbox))
        .context("failed to spawn the simulation thread")?;

    Ok(Handle::new(world, host, outbox, thread))
}

/// The host's end of the boundary: the shared World, frames, commands and
/// activations in, readiness and tick-end pushes out. Dropping it shuts the
/// sim thread down and waits for it.
pub struct Handle {
    world: Arc<RwLock<World>>,
    host: mpsc::Sender<Message>,
    pushes: mpsc::Receiver<Push>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// The shared World, for host queries under the read lock. The sim reads
    /// it to evaluate and takes the write lock to commit or to activate.
    #[must_use]
    pub const fn world(&self) -> &Arc<RwLock<World>> {
        &self.world
    }

    /// Sends one frame of elapsed time and buffered input. The sim advances
    /// only while frames arrive.
    pub fn frame(&self, elapsed: Duration, sample: InputSample) {
        self.send(Message::Frame { elapsed, sample });
    }

    /// Queues an edit command for the next commit.
    pub fn command(&self, command: Command) {
        self.send(Message::Command(command));
    }

    /// Hands over a loaded World. The host uploads the activation batch's
    /// palette before calling this. The sim answers with the batch and then
    /// readiness, and keeps ticking the current World until it arrives.
    pub fn activate(&self, activation: Activation) {
        self.send(Message::Activation(Box::new(activation)));
    }

    /// Waits for the next push, readiness included.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when no push arrives in time and
    /// [`RecvTimeoutError::Disconnected`] once the sim thread has stopped.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Push, RecvTimeoutError> {
        self.pushes.recv_timeout(timeout)
    }

    /// Takes the next push without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] when nothing is queued and
    /// [`TryRecvError::Disconnected`] once the sim thread has stopped.
    pub fn try_recv(&self) -> Result<Push, TryRecvError> {
        self.pushes.try_recv()
    }

    const fn new(
        world: Arc<RwLock<World>>,
        host: mpsc::Sender<Message>,
        pushes: mpsc::Receiver<Push>,
        thread: JoinHandle<()>,
    ) -> Self {
        Self {
            world,
            host,
            pushes,
            thread: Some(thread),
        }
    }

    fn send(&self, message: Message) {
        if self.host.send(message).is_err() {
            error!("atlas_rt: the simulation thread has stopped");
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.host.send(Message::Shutdown).is_err() {
            error!("atlas_rt: the simulation thread stopped before the shutdown");
        }

        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            error!("atlas_rt: the simulation thread panicked");
        }
    }
}

struct Runtime {
    world: Arc<RwLock<World>>,
    profile: PlayerProfile,
    materials: PhysicalMaterialTable,
    tracked: TrackedCoords,
    commands: Vec<Vec<VoxelEdit>>,
    scheduler: Scheduler,
    player: PlayerState,
    pushes: mpsc::Sender<Push>,
    active: bool,
    snap_pending: bool,
}

impl Runtime {
    fn new(world: Arc<RwLock<World>>, profile: PlayerProfile, pushes: mpsc::Sender<Push>) -> Self {
        let scheduler = Scheduler::new(profile.tick_period());

        Self {
            world,
            profile,
            materials: PhysicalMaterialTable::default(),
            tracked: TrackedCoords::default(),
            commands: Vec::new(),
            scheduler,
            player: floor_pose(),
            pushes,
            active: false,
            snap_pending: false,
        }
    }

    fn run(mut self, inbox: &mpsc::Receiver<Message>) {
        while let Ok(message) = inbox.recv() {
            match message {
                Message::Frame { elapsed, sample } => self.on_frame(elapsed, sample),
                Message::Command(command) => self.on_command(command),
                Message::Activation(activation) => self.on_activation(*activation),
                Message::Shutdown => break,
            }
        }
    }

    fn on_frame(&mut self, elapsed: Duration, _sample: InputSample) {
        if !self.active {
            return;
        }

        self.scheduler.advance(elapsed);

        let (ticks, discarded) = self.scheduler.take_due();

        if ticks == 0 {
            return;
        }

        let snap = mem::take(&mut self.snap_pending);
        let mut report = UpdateReport {
            ticks,
            batches: Vec::new(),
            discarded,
            tick_time: Duration::ZERO,
            commit_time: Duration::ZERO,
        };

        for _ in 0..ticks {
            let (grounded, evaluated) = self.evaluate();
            let (committed, batch) = self.commit();

            report.tick_time = report.tick_time.saturating_add(evaluated);
            report.commit_time = report.commit_time.saturating_add(committed);
            self.player.grounded = grounded;

            if let Some(snapshots) = batch {
                report.batches.push(snapshots);
            }
        }

        let push = Push::Tick(TickEnd {
            player: self.player,
            remainder: self.scheduler.remainder(),
            snap,
            report,
        });

        let _ = self.pushes.send(push);
    }

    fn on_command(&mut self, command: Command) {
        let Command::Edits(edits) = command;

        self.commands.push(edits);
    }

    fn on_activation(&mut self, activation: Activation) {
        let Activation {
            world,
            snapshots,
            tracked,
            materials,
        } = activation;

        let outgoing = mem::take(&mut self.tracked);
        let shared = Arc::clone(&self.world);

        let (planned, player) = {
            let mut guard = shared.write().unwrap_or_else(PoisonError::into_inner);

            *guard = world;

            let planned = plan_load(snapshots, &outgoing);
            let player = pose(&guard, self.profile, &materials);

            drop(guard);

            (planned, player)
        };

        self.tracked = planned.tracked;
        self.tracked.extend(tracked);
        self.materials = materials;
        self.player = player;
        self.scheduler.reset();
        self.active = true;
        self.snap_pending = true;

        let _ = self.pushes.send(Push::ActivationBatch(planned.snapshots));
        let _ = self.pushes.send(Push::Ready { player });
    }

    /// Reads the ground under the player under the read lock alone.
    fn evaluate(&self) -> (bool, Duration) {
        let started = Instant::now();
        let shared = Arc::clone(&self.world);

        let grounded = {
            let guard = shared.read().unwrap_or_else(PoisonError::into_inner);

            grounded(&guard, self.player.feet, self.profile, &self.materials)
        };

        (grounded, started.elapsed())
    }

    /// Applies the queued commands under the write lock and emits their
    /// snapshots. The measured window starts before the lock is taken, so it
    /// carries any wait for a host reader.
    fn commit(&mut self) -> (Duration, Option<Vec<MicroChunkSnapshot>>) {
        if self.commands.is_empty() {
            return (Duration::ZERO, None);
        }

        let edits: Vec<VoxelEdit> = self.commands.iter().flatten().copied().collect();
        self.commands.clear();

        let started = Instant::now();
        let shared = Arc::clone(&self.world);

        let outcome = {
            let mut guard = shared.write().unwrap_or_else(PoisonError::into_inner);

            edit_world(&mut guard, &edits, &self.tracked)
        };

        let elapsed = started.elapsed();

        debug_assert!(
            outcome.is_ok(),
            "an edit left the lattice between validation and commit: {outcome:?}"
        );

        match outcome {
            Ok(batch) => {
                self.tracked = batch.tracked;

                let pushed = (!batch.snapshots.is_empty()).then_some(batch.snapshots);

                (elapsed, pushed)
            }
            Err(error) => {
                error!("atlas_rt: dropped a failed edit batch: {error}");

                (elapsed, None)
            }
        }
    }
}
