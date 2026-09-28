use std::mem;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use glam::{IVec3, Vec2};
use tracing::error;

use crate::world::World;
use crate::world::diff::{
    batch::{TrackedCoords, plan_load},
    edit::{MicroChunkEdit, VoxelEdit, edit_world},
    snapshot::MicroChunkSnapshot,
};
use crate::world::grid::in_lattice;
use crate::world::material::PhysicalMaterialTable;

use super::input::{InputSample, PlayerState};
use super::physics::controller::Controller;
use super::physics::field::Field;
use super::physics::queue::UpdateQueue;
use super::physics::rules::{self, ParityPolicy};
use super::physics::spawn::{floor_pose, pose};
use super::profile::PlayerProfile;
use super::scheduler::Scheduler;

/// One frame of host time and buffered input, one pause change, one edit
/// command, one World handover, or the shutdown that ends the loop.
enum Message {
    Frame {
        elapsed: Duration,
        sample: InputSample,
    },
    Paused(bool),
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
/// Commands commit in arrival order, each diffed against the World the
/// earlier ones left.
#[derive(Debug)]
pub enum Command {
    Cell(VoxelEdit),
    MicroChunk(MicroChunkEdit),
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
/// The parity policy fixes how a grain breaks a diagonal tie for the life of
/// the sim.
///
/// # Errors
///
/// Returns an error when the operating system refuses the thread.
pub fn spawn(
    world: Arc<RwLock<World>>,
    profile: PlayerProfile,
    parity: ParityPolicy,
) -> Result<Handle> {
    let (host, inbox) = mpsc::channel();
    let (pushes, outbox) = mpsc::channel();

    let shared = Arc::clone(&world);
    let runtime = Runtime::new(shared, profile, parity, pushes);

    let thread = thread::Builder::new()
        .name(String::from("atlas-sim"))
        .spawn(move || runtime.run(&inbox))
        .context("failed to spawn the simulation thread")?;

    Ok(Handle::new(world, host, outbox, thread))
}

/// The host's end of the boundary: the shared World, frames, pause changes,
/// commands and activations in, readiness and tick-end pushes out. Dropping it
/// shuts the sim thread down and waits for it.
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

    /// Freezes or resumes the sim's clock. A pause drops the owed time and
    /// the buffered jump edge; frames owe nothing while it holds, and the
    /// resume starts from no accumulated time.
    pub fn set_paused(&self, paused: bool) {
        self.send(Message::Paused(paused));
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
    parity: ParityPolicy,
    materials: PhysicalMaterialTable,
    queue: UpdateQueue,
    tracked: TrackedCoords,
    commands: Vec<Command>,
    rule_edits: Vec<VoxelEdit>,
    scheduler: Scheduler,
    player: PlayerState,
    controller: Controller,
    movement: Vec2,
    pushes: mpsc::Sender<Push>,
    active: bool,
    paused: bool,
    snap_pending: bool,
}

impl Runtime {
    fn new(
        world: Arc<RwLock<World>>,
        profile: PlayerProfile,
        parity: ParityPolicy,
        pushes: mpsc::Sender<Push>,
    ) -> Self {
        let scheduler = Scheduler::new(profile.tick_period());

        Self {
            world,
            profile,
            parity,
            materials: PhysicalMaterialTable::default(),
            queue: UpdateQueue::default(),
            tracked: TrackedCoords::default(),
            commands: Vec::new(),
            rule_edits: Vec::new(),
            scheduler,
            player: floor_pose(),
            controller: Controller::new(),
            movement: Vec2::ZERO,
            pushes,
            active: false,
            paused: false,
            snap_pending: false,
        }
    }

    fn run(mut self, inbox: &mpsc::Receiver<Message>) {
        while let Ok(message) = inbox.recv() {
            match message {
                Message::Frame { elapsed, sample } => self.on_frame(elapsed, sample),
                Message::Paused(paused) => self.on_paused(paused),
                Message::Command(command) => self.on_command(command),
                Message::Activation(activation) => self.on_activation(*activation),
                Message::Shutdown => break,
            }
        }
    }

    const fn on_paused(&mut self, paused: bool) {
        self.paused = paused;

        if paused {
            self.scheduler.reset();
            self.controller.discard_jump();
            self.movement = Vec2::ZERO;
        }
    }

    fn on_frame(&mut self, elapsed: Duration, sample: InputSample) {
        if !self.active || self.paused {
            return;
        }

        if let Some(edge) = sample.jump_edge {
            self.controller.buffer_jump(edge);
        }

        self.movement = sample.movement;

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
            self.tick(&mut report);
        }

        let push = Push::Tick(TickEnd {
            player: self.player,
            remainder: self.scheduler.remainder(),
            snap,
            report,
        });

        let _ = self.pushes.send(push);
    }

    /// One tick: the player moves and the voxel rules drain the queue under
    /// the read lock, then the pending edits commit under the write lock.
    fn tick(&mut self, report: &mut UpdateReport) {
        let evaluated = self.evaluate();
        let (committed, batch) = self.commit();

        report.tick_time = report.tick_time.saturating_add(evaluated);
        report.commit_time = report.commit_time.saturating_add(committed);

        if let Some(snapshots) = batch {
            report.batches.push(snapshots);
        }
    }

    fn on_command(&mut self, command: Command) {
        self.commands.push(command);
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

        self.materials = materials;

        let (planned, player) = {
            let mut guard = shared.write().unwrap_or_else(PoisonError::into_inner);

            *guard = world;

            self.queue.seed(&guard, &self.materials);

            let planned = plan_load(snapshots, &outgoing);
            let player = pose(&guard, self.profile, &self.materials, &self.queue);

            drop(guard);

            (planned, player)
        };

        self.tracked = planned.tracked;
        self.tracked.extend(tracked);
        self.player = player;
        self.scheduler.reset();
        self.controller.reset();
        self.active = true;
        self.snap_pending = true;

        let _ = self.pushes.send(Push::ActivationBatch(planned.snapshots));
        let _ = self.pushes.send(Push::Ready { player });
    }

    /// Runs one tick's movement on the player and one drain of the rule
    /// queue on the same read lock, so the rules read one immutable view of
    /// the World for the whole tick and hand back a pending edit list. The
    /// window starts before the lock is taken, so it carries any wait for a
    /// host reader and stamps the jump deadline.
    fn evaluate(&mut self) -> Duration {
        let started = Instant::now();
        let shared = Arc::clone(&self.world);

        let guard = shared.read().unwrap_or_else(PoisonError::into_inner);

        let field = Field::new(&guard, &self.materials, &self.queue);

        self.controller.advance(
            &field,
            &mut self.player,
            self.profile,
            self.movement,
            started,
        );

        let outcome = rules::drain(&field, self.parity);

        drop(guard);

        self.queue = outcome.queue;
        self.rule_edits = outcome.edits;

        started.elapsed()
    }

    /// Applies the rule edits as one batch and then every queued command as
    /// its own batch, in arrival order, all under one write lock, so a later
    /// command diffs against the World the earlier ones left. The batches of
    /// a tick merge into one snapshot list, the last snapshot per
    /// Micro-chunk winning. A failed rule batch reseeds the queue from the
    /// World it left behind; a failed command is logged and dropped. The
    /// measured window starts before the lock is taken, so it carries any
    /// wait for a host reader.
    fn commit(&mut self) -> (Duration, Option<Vec<MicroChunkSnapshot>>) {
        if self.rule_edits.is_empty() && self.commands.is_empty() {
            return (Duration::ZERO, None);
        }

        let started = Instant::now();
        let shared = Arc::clone(&self.world);
        let mut merged: Vec<MicroChunkSnapshot> = Vec::new();

        {
            let mut guard = shared.write().unwrap_or_else(PoisonError::into_inner);
            let rules = mem::take(&mut self.rule_edits);

            if !rules.is_empty() {
                let outcome = edit_world(&mut guard, &rules, &self.tracked);

                if outcome.is_err() {
                    self.queue.seed(&guard, &self.materials);
                }

                debug_assert!(
                    outcome.is_ok(),
                    "a rule edit left the lattice between validation and commit: {outcome:?}"
                );

                match outcome {
                    Ok(batch) => {
                        self.tracked = batch.tracked;
                        merge_snapshots(&mut merged, batch.snapshots);
                    }
                    Err(error) => error!("atlas_rt: dropped a failed rule batch: {error}"),
                }
            }

            for command in mem::take(&mut self.commands) {
                self.apply_command(&mut guard, command, &mut merged);
            }

            drop(guard);
        }

        let elapsed = started.elapsed();
        let pushed = (!merged.is_empty()).then_some(merged);

        (elapsed, pushed)
    }

    /// Diffs one command against the World it will land in and applies what
    /// the diff asks for, folding the snapshots into `merged`. A command
    /// that fails validation, or that changes nothing, leaves the World,
    /// the tracked set, and the queue alone.
    fn apply_command(
        &mut self,
        guard: &mut World,
        command: Command,
        merged: &mut Vec<MicroChunkSnapshot>,
    ) {
        let edits = match command {
            Command::Cell(edit) => {
                if !edit.disagrees_with(guard) {
                    return;
                }

                vec![edit]
            }
            Command::MicroChunk(chunk) => match chunk.diff(guard) {
                Ok(edits) => edits,
                Err(error) => {
                    error!("atlas_rt: dropped an invalid command: {error}");

                    return;
                }
            },
        };

        if edits.is_empty() {
            return;
        }

        match edit_world(guard, &edits, &self.tracked) {
            Ok(batch) => {
                self.tracked = batch.tracked;
                merge_snapshots(merged, batch.snapshots);

                for edit in &edits {
                    self.wake(edit.position);
                }
            }
            Err(error) => error!("atlas_rt: dropped a failed command batch: {error}"),
        }
    }

    /// Queues the edited cell and the three above it, so the rule queue sees
    /// what a host edit moved without scanning the World. Cells above the
    /// lattice are skipped.
    fn wake(&mut self, cell: IVec3) {
        for height in 0..4 {
            let woken = cell.with_y(cell.y.saturating_add(height));

            if in_lattice(woken) {
                self.queue.insert(woken);
            }
        }
    }
}

/// Folds one batch's snapshots into the tick's list, keeping the last
/// snapshot per Micro-chunk so a chunk edited twice reaches the renderer in
/// its final state.
fn merge_snapshots(merged: &mut Vec<MicroChunkSnapshot>, snapshots: Vec<MicroChunkSnapshot>) {
    for snapshot in snapshots {
        if let Some(slot) = merged
            .iter_mut()
            .find(|earlier| earlier.global_coords == snapshot.global_coords)
        {
            *slot = snapshot;
        } else {
            merged.push(snapshot);
        }
    }
}
