use std::collections::hash_map::Entry;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use glam::{IVec3, Vec2};
use rustc_hash::FxHashMap;
use tracing::error;

use crate::world::World;
use crate::world::diff::{
    batch::{TrackedCoords, plan_load},
    edit::{MicroChunkEdit, VoxelChange, VoxelEdit, edit_world},
    snapshot::MicroChunkSnapshot,
};
use crate::world::grid::in_lattice;
use crate::world::material::PhysicalMaterialTable;
use crate::world::rain::Emitter;
use crate::world::vocabulary::Material;

use super::commit::{self, REWRITE_THRESHOLD};
use super::input::{InputSample, PlayerState};
use super::physics::controller::Controller;
use super::physics::field::{Field, Overlay};
use super::physics::queue::UpdateQueue;
use super::physics::rules::{self, DrainPolicy, ParityPolicy};
use super::physics::spawn::{floor_pose, pose};
use super::profile::PlayerProfile;
use super::rain;
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
/// the renderer already holds, and the emitter that rains grains into it while
/// the World is active. The sim plans the snapshots under its write lock, so it
/// owns tracking from here on.
#[derive(Debug)]
pub struct Activation {
    pub world: World,
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub tracked: TrackedCoords,
    pub materials: PhysicalMaterialTable,
    pub granular_cells: Option<Vec<IVec3>>,
    pub emitter: Option<Emitter>,
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
/// tick's end.
#[derive(Debug)]
pub enum Push {
    ActivationBatch(Vec<MicroChunkSnapshot>),
    Ready { player: PlayerState },
    Tick(TickEnd),
}

/// One tick's end, sent as the tick finishes rather than at the end of the
/// update it belongs to, so the host shows every step the World took.
#[derive(Debug)]
pub struct TickEnd {
    pub player: PlayerState,
    pub remainder: Duration,
    pub snap: bool,
    pub report: UpdateReport,
}

/// The grains an emitter planted on one tick, and the ones it could not because
/// their cell already held a grain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Emission {
    pub emitted: usize,
    pub skipped: usize,
}

/// What one tick did: the batch its commits produced, the grains the queue
/// served, the grains the emitter planted, and the two windows it spent.
///
/// `discarded` rides the first tick of an update, the only one that can report
/// what the scheduler dropped before the update ran. `processed` is the queued
/// cells the rules evaluated, moved or not, which is the scene's own size and
/// what the host's log divides by the ticks it reports.
#[derive(Debug)]
pub struct UpdateReport {
    pub batch: Option<Vec<MicroChunkSnapshot>>,
    pub discarded: u64,
    pub processed: usize,
    pub queued: usize,
    pub emission: Emission,
    pub tick_time: Duration,
    pub commit_time: Duration,
}

/// Spawns the sim thread against `world` and returns the host's handle to it.
/// The sim takes no ticks until an activation arrives.
///
/// The parity policy fixes how a grain breaks a diagonal tie for the life of
/// the sim, and the drain policy fixes how each tick serves the queue.
///
/// # Errors
///
/// Returns an error when the operating system refuses the thread.
pub fn spawn(
    world: Arc<RwLock<World>>,
    profile: PlayerProfile,
    parity: ParityPolicy,
    policy: DrainPolicy,
) -> Result<Handle> {
    let (host, inbox) = mpsc::channel();
    let (pushes, outbox) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));

    let shared = Arc::clone(&world);
    let runtime = Runtime::new(
        shared,
        profile,
        parity,
        policy,
        pushes,
        Arc::clone(&shutdown),
    );

    let thread = thread::Builder::new()
        .name(String::from("atlas-sim"))
        .spawn(move || runtime.run(&inbox))
        .context("failed to spawn the simulation thread")?;

    Ok(Handle::new(world, host, outbox, thread, shutdown))
}

/// The host's end of the boundary: the shared World, frames, pause changes,
/// commands and activations in, readiness and tick-end pushes out.
///
/// Dropping it raises the shutdown flag and then waits for the sim thread,
/// which abandons any queued work promptly instead of draining the inbox.
/// The Shutdown message that follows the flag only wakes a thread parked in
/// `recv`, and may be dropped once the thread has already stopped.
pub struct Handle {
    world: Arc<RwLock<World>>,
    host: mpsc::Sender<Message>,
    pushes: mpsc::Receiver<Push>,
    thread: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
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
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            world,
            host,
            pushes,
            thread: Some(thread),
            shutdown,
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
        self.shutdown.store(true, Ordering::Release);

        let _ = self.host.send(Message::Shutdown);

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
    policy: DrainPolicy,
    materials: PhysicalMaterialTable,
    queue: UpdateQueue,
    tracked: TrackedCoords,
    commands: Vec<Command>,
    rule_edits: Vec<VoxelEdit>,
    rule_overlay: Overlay,
    emitter: Option<Emitter>,
    tick_count: u64,
    scheduler: Scheduler,
    player: PlayerState,
    controller: Controller,
    movement: Vec2,
    pushes: mpsc::Sender<Push>,
    shutdown: Arc<AtomicBool>,
    active: bool,
    paused: bool,
    snap_pending: bool,
}

impl Runtime {
    fn new(
        world: Arc<RwLock<World>>,
        profile: PlayerProfile,
        parity: ParityPolicy,
        policy: DrainPolicy,
        pushes: mpsc::Sender<Push>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        let scheduler = Scheduler::new(profile.tick_period());

        Self {
            world,
            profile,
            parity,
            policy,
            materials: PhysicalMaterialTable::default(),
            queue: UpdateQueue::default(),
            tracked: TrackedCoords::default(),
            commands: Vec::new(),
            rule_edits: Vec::new(),
            rule_overlay: Overlay::default(),
            emitter: None,
            tick_count: 0,
            scheduler,
            player: floor_pose(),
            controller: Controller::new(),
            movement: Vec2::ZERO,
            pushes,
            shutdown,
            active: false,
            paused: false,
            snap_pending: false,
        }
    }

    /// Whether the host has dropped its handle and no queued work is worth
    /// starting. Checked before each message, between a frame's ticks, and
    /// inside an activation, so a drop joins without draining the inbox.
    fn shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Takes one message, and a frame takes the run of frames queued behind it
    /// as one update. A message that is not a frame ends the run and is handled
    /// next, in the position it held against the frames.
    fn run(mut self, inbox: &mpsc::Receiver<Message>) {
        let mut deferred = None;

        loop {
            let message = match deferred.take() {
                Some(message) => message,
                None => match inbox.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
            };

            if self.shutting_down() {
                break;
            }

            match message {
                Message::Frame { elapsed, sample } => {
                    let (elapsed, sample) =
                        self.coalesce_frames(inbox, elapsed, sample, &mut deferred);

                    self.on_frame(elapsed, sample);
                }
                Message::Paused(paused) => self.on_paused(paused),
                Message::Command(command) => self.on_command(command),
                Message::Activation(activation) => self.on_activation(*activation),
                Message::Shutdown => break,
            }
        }
    }

    /// Merges the run of frame messages already queued behind the first one
    /// into a single update: the elapsed times sum, the newest movement wins,
    /// and the newest jump edge of the run survives, so a press released
    /// inside the run still reaches the controller. The first message that is
    /// not a frame ends the run and is left in `deferred` for the next turn,
    /// so only consecutive frames ever merge.
    fn coalesce_frames(
        &self,
        inbox: &mpsc::Receiver<Message>,
        mut elapsed: Duration,
        mut sample: InputSample,
        deferred: &mut Option<Message>,
    ) -> (Duration, InputSample) {
        while !self.shutting_down() {
            match inbox.try_recv() {
                Ok(Message::Frame {
                    elapsed: next_elapsed,
                    sample: next_sample,
                }) => {
                    elapsed = elapsed.saturating_add(next_elapsed);
                    sample.movement = next_sample.movement;
                    sample.jump_edge = sample.jump_edge.max(next_sample.jump_edge);
                }
                Ok(message) => {
                    *deferred = Some(message);

                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }

        (elapsed, sample)
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

        let mut snap = mem::take(&mut self.snap_pending);

        for index in 0..ticks {
            if self.shutting_down() {
                return;
            }

            let mut report = UpdateReport {
                batch: None,
                discarded: if index == 0 { discarded } else { 0 },
                processed: 0,
                queued: self.queue.len(),
                emission: Emission::default(),
                tick_time: Duration::ZERO,
                commit_time: Duration::ZERO,
            };

            self.tick(&mut report);

            let push = Push::Tick(TickEnd {
                player: self.player,
                remainder: self.scheduler.remainder(),
                snap,
                report,
            });

            let _ = self.pushes.send(push);

            snap = false;
        }
    }

    /// One tick: the player moves and the voxel rules drain the queue under
    /// the read lock, then the pending edits and the emitter's grains commit
    /// under the write lock.
    fn tick(&mut self, report: &mut UpdateReport) {
        let (evaluated, processed) = self.evaluate();
        let (committed, batch, emission) = self.commit();

        report.tick_time = evaluated;
        report.commit_time = committed;
        report.processed = processed;
        report.queued = self.queue.len();
        report.batch = batch;
        report.emission = emission;

        self.tick_count = self.tick_count.saturating_add(1);
    }

    fn on_command(&mut self, command: Command) {
        self.commands.push(command);
    }

    fn on_activation(&mut self, activation: Activation) {
        if self.shutting_down() {
            return;
        }

        let Activation {
            world,
            snapshots,
            tracked,
            materials,
            granular_cells,
            emitter,
        } = activation;

        let outgoing = mem::take(&mut self.tracked);
        let shared = Arc::clone(&self.world);

        self.materials = materials;

        let (planned, player) = {
            let mut guard = shared.write().unwrap_or_else(PoisonError::into_inner);

            *guard = world;

            self.queue
                .seed(&guard, &self.materials, granular_cells.as_deref());

            let planned = plan_load(snapshots, &outgoing);
            let player = pose(&guard, self.profile, &self.materials, &self.queue);

            drop(guard);

            (planned, player)
        };

        if self.shutting_down() {
            return;
        }

        self.tracked = planned.tracked;
        self.tracked.extend(tracked);
        self.player = player;
        self.emitter = emitter;
        self.tick_count = 0;
        self.scheduler.reset();
        self.controller.reset();
        self.active = true;
        self.snap_pending = true;

        let _ = self.pushes.send(Push::ActivationBatch(planned.snapshots));
        let _ = self.pushes.send(Push::Ready { player });
    }

    /// Runs one tick's movement on the player and one drain of the rule
    /// queue on the same read lock, so the rules read one immutable view of
    /// the World for the whole tick and hand back a pending edit list. A tick
    /// drains every cell the queue holds, so the rules advance the whole World
    /// as one step and a scene that costs more than a frame period makes the
    /// World's rate fall rather than advancing it partially. Returns the
    /// window's duration and the queued cells the drain evaluated. The window
    /// starts before the lock is taken, so it carries any wait for a host
    /// reader and stamps the jump deadline.
    fn evaluate(&mut self) -> (Duration, usize) {
        let started = Instant::now();
        let shared = Arc::clone(&self.world);

        let guard = shared.read().unwrap_or_else(PoisonError::into_inner);
        let mut queue = mem::take(&mut self.queue);

        {
            let field = Field::new(&guard, &self.materials, &queue);

            self.controller.advance(
                &field,
                &mut self.player,
                self.profile,
                self.movement,
                started,
            );
        }

        let drained = rules::drain(
            &guard,
            &self.materials,
            &mut queue,
            self.parity,
            self.policy,
        );

        drop(guard);

        self.queue = queue;
        self.rule_edits = drained.edits;
        self.rule_overlay = drained.overlay;

        (started.elapsed(), drained.processed)
    }

    /// Applies the rule edits as one batch and then every queued command as
    /// its own batch, in arrival order, all under one write lock, so a later
    /// command diffs against the World the earlier ones left. The emitter then
    /// plants its grains against the World those left.
    ///
    /// The rule batch writes whole every chunk the drain moved enough cells in,
    /// so the snapshot list it hands back is the per-cell path's, chunk for
    /// chunk. The batches of a tick merge into one snapshot list, the last
    /// snapshot per Micro-chunk winning. A failed rule batch reseeds the queue
    /// from the World it left behind; a failed command is logged and dropped.
    /// The measured window starts before the lock is taken, so it carries any
    /// wait for a host reader.
    fn commit(&mut self) -> (Duration, Option<Vec<MicroChunkSnapshot>>, Emission) {
        if self.rule_edits.is_empty() && self.commands.is_empty() && self.emitter.is_none() {
            return (Duration::ZERO, None, Emission::default());
        }

        let started = Instant::now();
        let shared = Arc::clone(&self.world);
        let mut merged = SnapshotFold::default();
        let emission;

        {
            let mut guard = shared.write().unwrap_or_else(PoisonError::into_inner);
            let rules = mem::take(&mut self.rule_edits);
            let overlay = mem::take(&mut self.rule_overlay);

            if !rules.is_empty() {
                let outcome = commit::commit(
                    &mut guard,
                    &rules,
                    &overlay,
                    REWRITE_THRESHOLD,
                    &self.tracked,
                );

                if outcome.is_err() {
                    self.queue.seed(&guard, &self.materials, None);
                }

                debug_assert!(
                    outcome.is_ok(),
                    "a rule edit left the lattice between validation and commit: {outcome:?}"
                );

                match outcome {
                    Ok(batch) => {
                        self.tracked = batch.tracked;
                        merged.merge(batch.snapshots);
                    }
                    Err(error) => error!("atlas_rt: dropped a failed rule batch: {error}"),
                }
            }

            for command in mem::take(&mut self.commands) {
                self.apply_command(&mut guard, command, &mut merged);
            }

            emission = self.emit(&mut guard, &mut merged);

            drop(guard);
        }

        let elapsed = started.elapsed();
        let pushed = (!merged.is_empty()).then(|| merged.into_vec());

        (elapsed, pushed, emission)
    }

    /// Plants the tick's grains from the emitter, after the rule edits and the
    /// commands, so a hole the pile has reached is skipped rather than cleared.
    /// Each planted cell is woken the way a host edit is, so the following
    /// tick's drain serves it.
    fn emit(&mut self, guard: &mut World, merged: &mut SnapshotFold) -> Emission {
        let Some(emitter) = self.emitter.as_ref() else {
            return Emission::default();
        };

        let planted = rain::emitted(self.tick_count, emitter);
        let sand = Material::Sand.index();
        let mut emission = Emission::default();
        let mut edits: Vec<VoxelEdit> = Vec::with_capacity(planted.len());

        for position in planted {
            if guard.get_voxel(&position).is_some() {
                emission.skipped = emission.skipped.saturating_add(1);

                continue;
            }

            edits.push(VoxelEdit {
                position,
                change: VoxelChange::Set(sand),
            });
        }

        if edits.is_empty() {
            return emission;
        }

        match edit_world(guard, &edits, &self.tracked) {
            Ok(batch) => {
                self.tracked = batch.tracked;
                merged.merge(batch.snapshots);

                for edit in &edits {
                    self.wake(edit.position);
                }

                emission.emitted = edits.len();
            }
            Err(error) => error!("atlas_rt: dropped a failed rain batch: {error}"),
        }

        emission
    }

    /// Diffs one command against the World it will land in and applies what
    /// the diff asks for, folding the snapshots into `merged`. A command
    /// that fails validation, or that changes nothing, leaves the World,
    /// the tracked set, and the queue alone.
    fn apply_command(&mut self, guard: &mut World, command: Command, merged: &mut SnapshotFold) {
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
                merged.merge(batch.snapshots);

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
                self.queue.wake(woken);
            }
        }
    }
}

/// Folds a tick's batches into one snapshot list, keyed by Micro-chunk so a
/// chunk edited more than once reaches the renderer once, in its final state.
#[derive(Default)]
pub(super) struct SnapshotFold {
    merged: Vec<MicroChunkSnapshot>,
    slots: FxHashMap<IVec3, usize>,
}

impl SnapshotFold {
    /// Folds one batch in, the last snapshot per Micro-chunk winning. A chunk
    /// already folded keeps its place, so the output order is the order the
    /// chunks were first touched, the order the linear scan it replaced kept.
    pub(super) fn merge(&mut self, snapshots: Vec<MicroChunkSnapshot>) {
        for snapshot in snapshots {
            match self.slots.entry(snapshot.global_coords) {
                Entry::Occupied(slot) => {
                    let previous = self.merged.get_mut(*slot.get());

                    debug_assert!(previous.is_some(), "a fold slot always names a snapshot");

                    if let Some(previous) = previous {
                        *previous = snapshot;
                    }
                }
                Entry::Vacant(slot) => {
                    slot.insert(self.merged.len());
                    self.merged.push(snapshot);
                }
            }
        }
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.merged.is_empty()
    }

    pub(super) fn into_vec(self) -> Vec<MicroChunkSnapshot> {
        self.merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::micro::MicroChunk;

    /// A local xorshift so the fold oracle test can collide coordinates across
    /// batches without reaching into the World test support.
    fn below(state: &mut u64, bound: u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;

        state.checked_rem(bound).unwrap_or_default()
    }

    fn snapshot(x: i32, z: i32, material: u8) -> MicroChunkSnapshot {
        let mut mask = [0u8; 64];

        if let Some(first) = mask.first_mut() {
            *first = 1;
        }

        MicroChunkSnapshot {
            global_coords: IVec3::new(x, 0, z),
            chunk: MicroChunk::new(mask, vec![material]).expect("one material, one cell"),
        }
    }

    /// The linear scan the map replaced: first-touch order, last write wins.
    fn merge_linear(merged: &mut Vec<MicroChunkSnapshot>, snapshots: Vec<MicroChunkSnapshot>) {
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

    #[test]
    fn a_chunk_touched_twice_keeps_its_last_snapshot() {
        let mut fold = SnapshotFold::default();

        fold.merge(vec![snapshot(0, 0, 1), snapshot(8, 0, 2)]);
        fold.merge(vec![snapshot(0, 0, 3)]);

        let merged = fold.into_vec();

        assert_eq!(merged.len(), 2, "the chunk reaches the renderer once");
        assert_eq!(
            merged.first().map(|snapshot| snapshot.global_coords),
            Some(IVec3::new(0, 0, 0))
        );
        assert_eq!(
            merged
                .first()
                .map(|snapshot| snapshot.chunk.materials().to_vec()),
            Some(vec![3]),
            "the later edit wins"
        );
        assert_eq!(
            merged.get(1).map(|snapshot| snapshot.global_coords),
            Some(IVec3::new(8, 0, 0))
        );
    }

    #[test]
    fn the_fold_matches_the_linear_scan_across_batches() {
        for case in 0..64u64 {
            let mut fold = SnapshotFold::default();
            let mut oracle: Vec<MicroChunkSnapshot> = Vec::new();
            let mut state = 0xB0_1D ^ case;

            for _ in 0..below(&mut state, 6) {
                let batch: Vec<MicroChunkSnapshot> = (0..below(&mut state, 8))
                    .map(|_| {
                        let x = i32::try_from(below(&mut state, 3))
                            .unwrap_or_default()
                            .saturating_mul(8);
                        let z = i32::try_from(below(&mut state, 3))
                            .unwrap_or_default()
                            .saturating_mul(8);
                        let material = u8::try_from(below(&mut state, 256)).unwrap_or_default();

                        snapshot(x, z, material)
                    })
                    .collect();

                fold.merge(batch.clone());
                merge_linear(&mut oracle, batch);
            }

            assert_eq!(fold.into_vec(), oracle, "case {case}");
        }
    }
}
