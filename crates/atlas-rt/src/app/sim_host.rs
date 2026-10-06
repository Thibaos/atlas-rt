//! The host's end of the simulation boundary: the World handover once the
//! renderer exists, one frame of time and input per frame, and the pushes
//! drained into the renderer in the order they arrive.

#[cfg(test)]
mod bench;

use std::mem;
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use glam::IVec3;
use tracing::{error, warn};

use atlas_rt::host::ViewInterpolation;
use atlas_rt::sim::{
    self, Activation, Command, Handle, InputSample, ParityPolicy, PlayerProfile, PlayerState, Push,
};
use atlas_rt::world::World;
use atlas_rt::world::diff::batch::TrackedCoords;
use atlas_rt::world::diff::snapshot::MicroChunkSnapshot;
use atlas_rt::world::material::PhysicalMaterialTable;

/// The readiness wait the first frame makes, and how often it re-asks.
const READY_WAIT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(50);

/// What the host holds until the renderer exists to receive it.
struct Handover {
    snapshots: Vec<MicroChunkSnapshot>,
    tracked: TrackedCoords,
    materials: PhysicalMaterialTable,
    granular_cells: Option<Vec<IVec3>>,
}

/// Nothing on this side waits for the renderer.
pub struct SimHost {
    handle: Handle,
    handover: Option<Handover>,
    view: ViewInterpolation,
    ready: bool,
    waited: bool,
    queued: usize,
}

impl SimHost {
    /// Spawns the simulation thread against `world`, holding the load's
    /// snapshots, tracked set, and material table until the renderer exists to
    /// receive them.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system refuses the thread.
    pub fn spawn(
        world: Arc<RwLock<World>>,
        profile: PlayerProfile,
        snapshots: Vec<MicroChunkSnapshot>,
        tracked: TrackedCoords,
        granular_cells: Option<Vec<IVec3>>,
        materials: &PhysicalMaterialTable,
    ) -> Result<Self> {
        let period = profile.tick_period();
        let handle = sim::spawn(world, profile, ParityPolicy::default())?;

        Ok(Self {
            handle,
            handover: Some(Handover {
                snapshots,
                tracked,
                materials: *materials,
                granular_cells,
            }),
            view: ViewInterpolation::new(period),
            ready: false,
            waited: false,
            queued: 0,
        })
    }

    /// Hands the loaded World over, taking it out of the shared lock so the
    /// activation is the only swap the World ever sees. Call it once the
    /// renderer holds the palette and the initial residency.
    pub fn start(&mut self) {
        let Some(Handover {
            snapshots,
            tracked,
            materials,
            granular_cells,
        }) = self.handover.take()
        else {
            return;
        };

        let world = mem::take(
            &mut *self
                .handle
                .world()
                .write()
                .unwrap_or_else(PoisonError::into_inner),
        );

        self.handle.activate(Activation {
            world,
            snapshots,
            tracked,
            materials,
            granular_cells,
        });
    }

    /// The simulation advances only while frames arrive, so this frame is the
    /// only clock it has.
    fn frame(&self, elapsed: Duration, sample: InputSample) {
        self.handle.frame(elapsed, sample);
    }

    /// Queues a Voxel edit for the next commit, sent between frames and never
    /// inside one.
    pub fn command(&self, command: Command) {
        self.handle.command(command);
    }

    /// One frame's worth of the boundary: send elapsed and input, then take
    /// the pushes. The first frame after the handover waits, bounded, for
    /// readiness; every later frame drains without waiting, and a frame
    /// before the handover sends nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when readiness never arrives or the simulation
    /// thread has stopped.
    pub fn drive(
        &mut self,
        elapsed: Duration,
        sample: InputSample,
        forward: &mut impl FnMut(Vec<MicroChunkSnapshot>) -> Result<()>,
    ) -> Result<()> {
        if !self.started() {
            return Ok(());
        }

        self.frame(elapsed, sample);

        if self.waited {
            return self.drain(forward);
        }

        self.wait_ready(forward)
    }

    /// Waits, bounded, for the activation's readiness reply, forwarding the
    /// activation batch on the way. The latch makes it the one wait the frame
    /// path ever makes: a failed wait degrades to draining.
    ///
    /// # Errors
    ///
    /// Returns an error when readiness does not arrive in time or the
    /// simulation thread has stopped.
    fn wait_ready(
        &mut self,
        forward: &mut impl FnMut(Vec<MicroChunkSnapshot>) -> Result<()>,
    ) -> Result<()> {
        self.waited = true;

        let deadline = Instant::now()
            .checked_add(READY_WAIT)
            .unwrap_or_else(Instant::now);

        while !self.ready {
            match self.handle.recv_timeout(READY_POLL) {
                Ok(push) => self.apply(push, forward),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        bail!("the simulation never reported readiness");
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("the simulation thread stopped");
                }
            }
        }

        Ok(())
    }

    /// Takes every push waiting, forwarding report batches in tick order. A
    /// frame that ran no ticks sends no push, which is the common case, and a
    /// tick that emitted no snapshots contributes no batch.
    ///
    /// # Errors
    ///
    /// Returns an error when the simulation thread has stopped.
    fn drain(
        &mut self,
        forward: &mut impl FnMut(Vec<MicroChunkSnapshot>) -> Result<()>,
    ) -> Result<()> {
        loop {
            match self.handle.try_recv() {
                Ok(push) => self.apply(push, forward),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => bail!("the simulation thread stopped"),
            }
        }
    }

    /// Whether the activation has been sent, so readiness is worth waiting for.
    #[must_use]
    const fn started(&self) -> bool {
        self.handover.is_none()
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.ready
    }

    /// The grains the queue held after the last tick the sim reported, which
    /// is stale by at most one tick period and zero before the first tick.
    #[must_use]
    pub const fn queued(&self) -> usize {
        self.queued
    }

    /// The pose one frame draws from the pushes applied so far: feet
    /// interpolated between the previous and current tick as of `now`,
    /// discrete state from the current tick, valid from readiness on.
    #[must_use]
    pub fn frame_state(&self, now: Instant) -> PlayerState {
        self.view.state(now)
    }

    fn apply(
        &mut self,
        push: Push,
        forward: &mut impl FnMut(Vec<MicroChunkSnapshot>) -> Result<()>,
    ) {
        match push {
            Push::ActivationBatch(batch) => forward_batch(forward, batch),
            Push::Ready { player } => {
                self.view.snap(Instant::now(), player);
                self.ready = true;
            }
            Push::Tick(tick) => {
                for batch in tick.report.batches {
                    forward_batch(forward, batch);
                }

                if tick.report.discarded > 0 {
                    warn!(
                        "atlas_rt: dropped {} ticks past the catch-up cap",
                        tick.report.discarded
                    );
                }

                self.queued = tick.report.queued;

                self.view
                    .advance(Instant::now(), tick.player, tick.remainder);
            }
        }
    }
}

/// Queues one batch and drops it on failure: the World runs ahead of a
/// renderer that could not take it, and the frame keeps going.
fn forward_batch(
    forward: &mut impl FnMut(Vec<MicroChunkSnapshot>) -> Result<()>,
    batch: Vec<MicroChunkSnapshot>,
) {
    if let Err(error) = forward(batch) {
        error!("atlas_rt: dropped a failed batch: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use glam::{IVec3, Vec3};

    use atlas_rt::sim::TickEnd;
    use atlas_rt::world::diff::edit::{VoxelChange, VoxelEdit, edit_world};

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(2);
    const SETTLE: Duration = Duration::from_millis(150);

    fn spawn_with(
        world: World,
        snapshots: Vec<MicroChunkSnapshot>,
        tracked: TrackedCoords,
    ) -> (SimHost, Arc<RwLock<World>>) {
        let world = Arc::new(RwLock::new(world));

        let host = SimHost::spawn(
            Arc::clone(&world),
            PlayerProfile::default(),
            snapshots,
            tracked,
            None,
            &PhysicalMaterialTable::default(),
        )
        .unwrap_or_else(|error| panic!("the host must spawn: {error}"));

        (host, world)
    }

    fn period() -> Duration {
        PlayerProfile::default().tick_period()
    }

    fn ready_host() -> (SimHost, Arc<RwLock<World>>) {
        let (mut host, world) = spawn_with(World::default(), Vec::new(), TrackedCoords::default());

        host.start();
        host.drive(
            Duration::from_millis(1),
            InputSample::default(),
            &mut |_batch| Ok(()),
        )
        .unwrap_or_else(|error| panic!("the sim must report readiness: {error}"));

        (host, world)
    }

    fn take_tick(host: &SimHost) -> TickEnd {
        match host.handle.recv_timeout(TIMEOUT) {
            Ok(Push::Tick(tick)) => tick,
            Ok(push) => panic!("expected a tick, got {push:?}"),
            Err(error) => panic!("the sim must push: {error}"),
        }
    }

    #[derive(Default)]
    struct Collector {
        batches: Vec<Vec<MicroChunkSnapshot>>,
    }

    impl Collector {
        fn capture(&mut self) -> impl FnMut(Vec<MicroChunkSnapshot>) -> Result<()> + '_ {
            move |batch| {
                self.batches.push(batch);

                Ok(())
            }
        }
    }

    #[test]
    fn readiness_arrives_after_the_activation_batch() {
        let mut world = World::default();
        let loaded = edit_world(
            &mut world,
            &[VoxelEdit {
                position: IVec3::new(0, 0, 0),
                change: VoxelChange::Set(1),
            }],
            &TrackedCoords::default(),
        )
        .unwrap_or_else(|error| panic!("the load edit must apply: {error}"));

        let load_size = loaded.snapshots.len();
        let (mut host, _world) = spawn_with(world, loaded.snapshots, loaded.tracked);

        host.start();

        let mut collector = Collector::default();

        host.drive(
            Duration::from_millis(1),
            InputSample::default(),
            &mut collector.capture(),
        )
        .unwrap_or_else(|error| panic!("the sim must report readiness: {error}"));

        assert!(host.ready());
        assert_eq!(
            collector.batches.len(),
            1,
            "the activation batch reaches the renderer before readiness"
        );
        assert_eq!(
            collector.batches.first().map(Vec::len),
            Some(load_size),
            "the loader's snapshots ride the activation batch"
        );
    }

    #[test]
    fn a_frame_shorter_than_a_tick_pushes_nothing() {
        let (host, _world) = ready_host();

        host.frame(period().div_f64(2.0), InputSample::default());

        thread::sleep(SETTLE);

        match host.handle.try_recv() {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => panic!("the simulation thread stopped"),
            Ok(push) => panic!("expected no push from a sub-tick frame, got {push:?}"),
        }
    }

    #[test]
    fn a_tick_without_edits_advances_the_pose_and_pushes_no_batch() {
        let (mut host, _world) = ready_host();
        let before = host.frame_state(Instant::now());

        host.frame(period(), InputSample::from_local(0.0, 0.0, 1.0, None));

        let tick = take_tick(&host);
        let feet = tick.player.feet;

        assert!(tick.report.batches.is_empty(), "no edits means no batch");
        assert_ne!(feet, before.feet, "the forward sample moved the feet");

        let mut collector = Collector::default();

        host.apply(Push::Tick(tick), &mut collector.capture());

        assert!(collector.batches.is_empty());
        assert_eq!(
            host.frame_state(Instant::now() + period()).feet,
            feet,
            "the frame reaches the tick's own pose once alpha clamps at one"
        );
    }

    #[test]
    fn the_first_tick_interpolates_from_the_readiness_pose() {
        let (mut host, _world) = ready_host();

        let held = host.frame_state(Instant::now()).feet;

        host.frame(period(), InputSample::from_local(0.0, 0.0, 1.0, None));

        let tick = take_tick(&host);
        let pose = tick.player.feet;

        assert!(
            tick.snap,
            "the sim flags the first update after activation, and readiness has already snapped it"
        );
        assert_ne!(held, pose, "the forward sample moved the feet");

        let stamped = Instant::now();

        host.apply(Push::Tick(tick), &mut |_batch| Ok(()));

        let first = host.frame_state(stamped).feet;

        assert_ne!(
            first, pose,
            "the first tick starts from the readiness pose instead of snapping to its own"
        );
        assert_ne!(
            first, held,
            "the sub-tick remainder starts the interpolation within the same frame"
        );

        assert_eq!(
            host.frame_state(stamped + period()).feet,
            pose,
            "alpha reaches one a tick after the push"
        );
    }

    #[test]
    fn a_later_tick_push_interpolates_between_the_two_tick_poses() {
        let (mut host, _world) = ready_host();

        let step = InputSample::from_local(0.0, 0.0, 1.0, None);

        host.frame(period(), step);
        let first = take_tick(&host);
        let from = first.player.feet;
        host.apply(Push::Tick(first), &mut |_batch| Ok(()));

        host.frame(period(), step);
        let second = take_tick(&host);
        let to = second.player.feet;

        assert!(!second.snap, "only the first push snaps");
        assert_ne!(from, to, "the forward sample moved the feet");

        let stamped = Instant::now();
        host.apply(Push::Tick(second), &mut |_batch| Ok(()));

        let between = host.frame_state(stamped).feet;

        const EPSILON: f32 = 0.00001;
        let low = from.min(to) - Vec3::splat(EPSILON);
        let high = from.max(to) + Vec3::splat(EPSILON);

        assert!(
            between.cmpge(low).all() && between.cmple(high).all(),
            "the feet stay between the two tick poses: {between:?}"
        );
        assert_ne!(
            between, to,
            "a push past the first one interpolates instead of snapping"
        );
    }

    #[test]
    fn the_push_after_activation_renders_one_pose_for_a_whole_tick() {
        let (host, _world) = ready_host();

        let early = host.frame_state(Instant::now()).feet;
        let late = host.frame_state(Instant::now() + period()).feet;

        assert_ne!(
            early,
            Vec3::ZERO,
            "the readiness reply replaced the host's idle pose"
        );
        assert_eq!(
            early, late,
            "activation is the only unbounded gap, so it holds one pose"
        );
    }

    #[test]
    fn a_command_commits_at_the_first_tick_that_runs() {
        let (mut host, world) = ready_host();
        let position = IVec3::new(0, 400, 500);

        host.command(Command::Cell(VoxelEdit {
            position,
            change: VoxelChange::Set(1),
        }));

        host.frame(period(), InputSample::default());

        let tick = take_tick(&host);

        assert_eq!(
            tick.report.batches.len(),
            1,
            "the command's batch rides its tick"
        );

        let mut collector = Collector::default();

        host.apply(Push::Tick(tick), &mut collector.capture());

        assert_eq!(collector.batches.len(), 1, "the batch is forwarded once");

        let held = world
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get_voxel(&position);

        assert_eq!(held, Some(1), "the command landed in the World");
    }

    #[test]
    fn the_host_reads_the_queue_depth_the_tick_reported() {
        let (mut host, _world) = ready_host();

        assert_eq!(host.queued(), 0, "nothing is queued before a tick reports");

        host.command(Command::Cell(VoxelEdit {
            position: IVec3::new(0, 400, 500),
            change: VoxelChange::Set(1),
        }));

        host.frame(period(), InputSample::default());

        let tick = take_tick(&host);
        let woken = tick.report.queued;

        assert_eq!(
            woken, 4,
            "the command wakes the edited cell and the three above it"
        );

        host.apply(Push::Tick(tick), &mut |_batch| Ok(()));

        assert_eq!(
            host.queued(),
            woken,
            "the depth the tick reported reaches the log's reader"
        );

        host.frame(period(), InputSample::default());

        let tick = take_tick(&host);

        assert_eq!(tick.report.queued, 0, "the drained wakes leave nothing");

        host.apply(Push::Tick(tick), &mut |_batch| Ok(()));

        assert_eq!(host.queued(), 0, "an empty queue reaches the log's reader");
    }
}
