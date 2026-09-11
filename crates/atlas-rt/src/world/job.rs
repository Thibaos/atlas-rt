use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread::{JoinHandle, spawn},
};

use glam::Vec3;

use crate::{
    render::display_gate::DisplayGate,
    world::{
        World,
        format::{get_palette, open_bytes},
        snapshot::{MicroChunkSnapshot, emit_snapshots},
    },
};

const STATUS_EMPTY: u8 = 0;
const STATUS_LOADING: u8 = 1;
const STATUS_READY: u8 = 2;
const STATUS_FAILED: u8 = 3;

/// Whether the host has a world to show and whether it is still working on one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Empty,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug)]
pub struct LoadUpgrade {
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub palette: [Vec3; 256],
}

/// The work a load or a clear finished with, ready for the main thread.
#[derive(Debug)]
pub enum Upgrade {
    Load,
    Cleared,
    Failed,
}

/// A world's bytes, read on the thread that runs the load pipeline.
pub trait WorldSource: Send {
    fn name(&self) -> String;

    /// # Errors
    ///
    /// Returns a reason the world could not be read.
    fn read(&self) -> Result<Vec<u8>, String>;
}

impl WorldSource for Vec<u8> {
    fn name(&self) -> String {
        String::from("in-memory world")
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        Ok(self.clone())
    }
}

/// How far the renderer has to have got for a frame to carry a submitted batch.
///
/// A frame whose generation reaches this one was built after the renderer took
/// the batch, so the world it drew is the world the host asked for.
#[derive(Clone, Copy, Debug)]
pub struct Residency {
    generation: u64,
}

impl Residency {
    #[must_use]
    pub const fn new(generation: u64) -> Self {
        Self { generation }
    }

    #[must_use]
    pub const fn is_resident(&self, frame_generation: u64) -> bool {
        frame_generation >= self.generation
    }
}

enum JobState {
    Idle,
    Loading { upgrade: Option<Box<LoadUpgrade>> },
}

enum Finished {
    Load(Box<LoadUpgrade>),
    Cleared,
}

struct Job {
    state: JobState,
    display: DisplayGate,
    residency: Option<Residency>,
}

/// Whether a request was taken on.
#[derive(Debug)]
pub enum Refusal {
    Busy,
    Failed(String),
}

/// The load and clear jobs, one at a time, with the world they leave behind.
pub struct WorldJob {
    status: AtomicU8,
    error: Mutex<Option<String>>,
    running: Option<JoinHandle<()>>,
    finished: Option<mpsc::Receiver<Result<Finished, String>>>,
    job: Mutex<Job>,
}

impl WorldJob {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            status: AtomicU8::new(STATUS_EMPTY),
            error: Mutex::new(None),
            running: None,
            finished: None,
            job: Mutex::new(Job {
                state: JobState::Idle,
                display: DisplayGate::new(),
                residency: None,
            }),
        }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        match self.status.load(Ordering::Acquire) {
            STATUS_LOADING => Status::Loading,
            STATUS_READY => Status::Ready,
            STATUS_FAILED => Status::Failed,
            _ => Status::Empty,
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }

    /// Declares a world resident. The pipeline is up without one before the
    /// first load lands.
    pub fn enter_ready(&mut self) {
        self.status.store(STATUS_READY, Ordering::Release);
    }

    /// # Errors
    ///
    /// Returns `Refusal::Busy` while another job is in flight.
    pub fn load(&mut self, source: Box<dyn WorldSource>, version: u64) -> Result<(), Refusal> {
        self.begin(version)?;

        let (sender, receiver) = mpsc::channel();

        let thread = spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| run_load(&*source)))
                .unwrap_or_else(|_| Err(String::from("the loader panicked")));

            let _ = sender.send(result);
        });

        self.running = Some(thread);
        self.finished = Some(receiver);

        Ok(())
    }

    /// # Errors
    ///
    /// Returns `Refusal::Busy` while another job is in flight.
    pub fn clear(&mut self, version: u64) -> Result<(), Refusal> {
        self.begin(version)?;

        let (sender, receiver) = mpsc::channel();

        let thread = spawn(move || {
            let _ = sender.send(Ok(Finished::Cleared));
        });

        self.running = Some(thread);
        self.finished = Some(receiver);

        Ok(())
    }

    fn begin(&mut self, version: u64) -> Result<(), Refusal> {
        if self.running.is_some() {
            return Err(Refusal::Busy);
        }

        self.join();

        let mut job = lock(&self.job);

        job.display.arm(version);
        job.residency = None;
        drop(job);

        *lock(&self.error) = None;
        self.status.store(STATUS_LOADING, Ordering::Release);

        Ok(())
    }

    /// Takes the background result once it is there. A load's snapshots are held
    /// until the frame they are resident in asks for them.
    pub fn poll(&mut self) -> Option<Upgrade> {
        if self.status() != Status::Loading {
            return None;
        }

        let result = {
            let upgrades = self.finished.as_ref()?;

            let result = match upgrades.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(String::from("the loading thread died without a result"))
                }
            };

            self.finished = None;
            self.join();

            result
        };

        match result {
            Ok(Finished::Load(upgrade)) => {
                lock(&self.job).state = JobState::Loading {
                    upgrade: Some(upgrade),
                };

                Some(Upgrade::Load)
            }
            Ok(Finished::Cleared) => {
                lock(&self.job).state = JobState::Idle;
                self.status.store(STATUS_EMPTY, Ordering::Release);

                Some(Upgrade::Cleared)
            }
            Err(reason) => {
                lock(&self.job).state = JobState::Idle;
                *lock(&self.error) = Some(reason);
                self.status.store(STATUS_FAILED, Ordering::Release);

                Some(Upgrade::Failed)
            }
        }
    }

    /// Whether a frame of `generation` carries the batch the host submitted. The
    /// renderer only reaches this generation after it has taken that batch, so a
    /// frame at or past it draws the world the host asked for.
    #[must_use]
    pub fn resident(&self, generation: u64) -> bool {
        lock(&self.job)
            .residency
            .as_ref()
            .is_some_and(|residency| residency.is_resident(generation))
    }

    /// Whether a frame of `version` is the one the armed display gate opens on.
    /// Reports a frame once, so a load completes once.
    #[must_use]
    pub fn admitted(&self, version: u64) -> bool {
        lock(&self.job).display.admitted(version)
    }

    /// The pending load's snapshots and palette, once the frame they are
    /// resident in has been admitted.
    pub fn take_upgrade(&self) -> Option<LoadUpgrade> {
        let mut job = lock(&self.job);

        let JobState::Loading { upgrade } = &mut job.state else {
            return None;
        };

        let upgrade = upgrade.take()?;

        job.state = JobState::Idle;
        drop(job);

        Some(*upgrade)
    }

    /// Records how far the renderer has to have got for the job to be complete.
    pub fn record(&self, residency: Residency) {
        lock(&self.job).residency = Some(residency);
    }

    /// Refuses the frames the world on screen was built from.
    pub fn suppress(&self, version: u64) {
        lock(&self.job).display.suppress(version);
    }

    /// Gives up on the job in flight, for a failure the background thread could
    /// not see.
    pub fn fail(&mut self, reason: String) {
        self.finished = None;
        self.join();

        lock(&self.job).state = JobState::Idle;
        *lock(&self.error) = Some(reason);
        self.status.store(STATUS_FAILED, Ordering::Release);
    }

    /// Waits out the job in flight, leaving its result for `poll`. Only for
    /// teardown and for tests.
    pub fn settle(&mut self) {
        self.join();
    }

    fn join(&mut self) {
        if let Some(thread) = self.running.take()
            && thread.join().is_err()
        {
            *lock(&self.error) = Some(String::from("the loading thread panicked"));
            self.status.store(STATUS_FAILED, Ordering::Release);
        }
    }
}

impl Default for WorldJob {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WorldJob {
    fn drop(&mut self) {
        self.join();
    }
}

/// The load pipeline off the main thread: read, parse, world build, snapshot
/// emission. Nothing here touches the renderer.
fn run_load(source: &dyn WorldSource) -> Result<Finished, String> {
    let name = source.name();
    let bytes = source
        .read()
        .map_err(|reason| format!("could not open {name}: {reason}"))?;

    let voxel_data =
        open_bytes(&bytes).map_err(|error| format!("could not parse {name}: {error:#}"))?;

    let (world, clipped) = World::new_clipped(&voxel_data);

    if clipped > 0 {
        eprintln!("atlas_rt: clipped {clipped} voxels outside the lattice");
    }

    let snapshots =
        emit_snapshots(&world).map_err(|error| format!("could not emit {name}: {error:#}"))?;

    Ok(Finished::Load(Box::new(LoadUpgrade {
        snapshots,
        palette: get_palette(&voxel_data),
    })))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::as_conversions
)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Drives the frame loop until the background thread has something to hand
    /// over. The host's own loop is the only other thing that calls `poll`.
    fn poll_until(job: &mut WorldJob) -> Upgrade {
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            if let Some(upgrade) = job.poll() {
                return upgrade;
            }

            assert!(
                Instant::now() < deadline,
                "the job did not settle within five seconds"
            );

            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// One voxel at the origin, in the engine's `.vox` dialect.
    fn one_voxel_world() -> Vec<u8> {
        fn chunk(id: [u8; 4], content: &[u8], children: &[u8]) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&id);
            bytes.extend_from_slice(&(content.len() as i32).to_le_bytes());
            bytes.extend_from_slice(&(children.len() as i32).to_le_bytes());
            bytes.extend_from_slice(content);
            bytes.extend_from_slice(children);

            bytes
        }

        let size = chunk(*b"SIZE", &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[]);
        let voxel = chunk(*b"XYZI", &[1, 0, 0, 0, 0, 0, 0, 7], &[]);
        let palette = chunk(*b"RGBA", &[0u8; 1024], &[]);
        let main = chunk(*b"MAIN", &[], &[size, voxel, palette].concat());

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(&main);

        bytes
    }

    fn source(bytes: Vec<u8>) -> impl WorldSource {
        bytes
    }

    #[test]
    fn a_job_starts_with_no_world_and_nothing_in_flight() {
        let job = WorldJob::new();

        assert_eq!(job.status(), Status::Empty);
        assert!(job.error().is_none());
    }

    #[test]
    fn an_accepted_load_reports_loading_before_it_finishes() {
        let mut job = WorldJob::new();
        job.enter_ready();

        assert!(job.load(Box::new(source(one_voxel_world())), 0).is_ok());
        assert_eq!(job.status(), Status::Loading);
    }

    #[test]
    fn a_second_request_while_one_is_in_flight_is_refused() {
        let mut job = WorldJob::new();
        job.enter_ready();

        job.load(Box::new(source(one_voxel_world())), 0).unwrap();

        assert!(matches!(
            job.load(Box::new(source(one_voxel_world())), 0),
            Err(Refusal::Busy)
        ));
        assert!(matches!(job.clear(0), Err(Refusal::Busy)));
        assert_eq!(
            job.status(),
            Status::Loading,
            "a refused request changes nothing"
        );
    }

    #[test]
    fn a_clear_while_a_load_is_in_flight_is_refused() {
        let mut job = WorldJob::new();
        job.enter_ready();

        job.clear(0).unwrap();

        assert!(matches!(
            job.load(Box::new(source(Vec::new())), 0),
            Err(Refusal::Busy)
        ));
    }

    #[test]
    fn a_finished_load_hands_its_snapshots_over_once() {
        let mut job = WorldJob::new();
        job.enter_ready();

        job.load(Box::new(source(one_voxel_world())), 4).unwrap();
        job.settle();

        assert!(
            matches!(job.poll(), Some(Upgrade::Load)),
            "the background work is done"
        );
        assert_eq!(
            job.status(),
            Status::Loading,
            "still loading until a frame carries it"
        );

        let Some(upgrade) = job.take_upgrade() else {
            panic!("the finished load must yield its snapshots");
        };

        assert!(
            !upgrade.snapshots.is_empty(),
            "the one voxel world emits one micro chunk"
        );
        assert!(
            job.take_upgrade().is_none(),
            "the snapshots are handed over once"
        );
    }

    #[test]
    fn a_malformed_world_fails_without_killing_the_thread() {
        let mut job = WorldJob::new();
        job.enter_ready();

        job.load(Box::new(source(vec![0xde, 0xad, 0xbe, 0xef])), 0)
            .unwrap();

        assert!(matches!(poll_until(&mut job), Upgrade::Failed));
        assert_eq!(job.status(), Status::Failed);
        assert!(job.error().is_some(), "the failure carries a reason");
        assert!(job.take_upgrade().is_none(), "no world came out of it");

        job.load(Box::new(source(one_voxel_world())), 0).unwrap();

        assert!(
            matches!(poll_until(&mut job), Upgrade::Load),
            "the next job runs on a live thread"
        );
    }

    #[test]
    fn a_clear_reaches_the_empty_state() {
        let mut job = WorldJob::new();
        job.enter_ready();

        job.clear(0).unwrap();

        assert!(matches!(poll_until(&mut job), Upgrade::Cleared));
        assert_eq!(job.status(), Status::Empty);
    }

    #[test]
    fn only_the_generation_the_batch_landed_in_is_resident() {
        let job = WorldJob::new();

        job.record(Residency::new(9));

        assert!(!job.resident(8), "the frame predates the batch");
        assert!(job.resident(9));
        assert!(job.resident(10));
    }

    #[test]
    fn only_the_first_admissible_frame_reports_a_completion() {
        let mut job = WorldJob::new();
        job.enter_ready();
        job.load(Box::new(source(one_voxel_world())), 7).unwrap();
        job.settle();
        job.poll();

        assert!(!job.admitted(7), "the outgoing content stays out");
        assert!(job.admitted(8));
        assert!(!job.admitted(9), "a later frame is not a second completion");
    }
}
