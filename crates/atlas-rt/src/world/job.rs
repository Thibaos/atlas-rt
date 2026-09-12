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
///
/// The codes are the host-facing contract: empty and idle, a job in flight, a
/// world resident, or the failure of the last job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Empty = STATUS_EMPTY,
    Loading = STATUS_LOADING,
    Ready = STATUS_READY,
    Failed = STATUS_FAILED,
}

impl Status {
    const fn from_code(code: u8) -> Self {
        match code {
            STATUS_LOADING => Self::Loading,
            STATUS_READY => Self::Ready,
            STATUS_FAILED => Self::Failed,
            _ => Self::Empty,
        }
    }

    /// The code the host reads. Empty and idle, a job in flight, a world
    /// resident, or the last job's failure.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Empty => STATUS_EMPTY,
            Self::Loading => STATUS_LOADING,
            Self::Ready => STATUS_READY,
            Self::Failed => STATUS_FAILED,
        }
    }

    /// The status spelled out, for a host that reads it by name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// A finished load's snapshots and palette, ready for the main thread.
#[derive(Debug)]
pub struct LoadedWorld {
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub palette: [Vec3; 256],
}

/// What a job left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finished {
    Loaded,
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

/// How far the renderer has to have got for a frame to carry a submitted batch.
#[derive(Clone, Copy, Debug)]
pub struct Residency {
    generation: u64,
}

impl Residency {
    #[must_use]
    pub const fn new(generation: u64) -> Self {
        Self { generation }
    }

    /// A frame at or past this generation was built after the renderer took the
    /// batch, so the world it drew is the world the host asked for.
    #[must_use]
    pub const fn reached(&self, frame_generation: u64) -> bool {
        frame_generation >= self.generation
    }
}

enum JobState {
    Idle,
    Loading { loaded: Option<Box<LoadedWorld>> },
}

enum RunResult {
    Loaded(Box<LoadedWorld>),
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
    finished: Option<mpsc::Receiver<Result<RunResult, String>>>,
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
        Status::from_code(self.status.load(Ordering::Acquire))
    }

    #[must_use]
    pub fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }

    /// Declares a world resident. The pipeline is up without one before the
    /// first load lands.
    pub fn world_resident(&mut self) {
        self.status.store(STATUS_READY, Ordering::Release);
    }

    /// Declares the view to hold no world, for a clear that had nothing left to
    /// take away.
    pub fn no_world(&mut self) {
        self.status.store(STATUS_EMPTY, Ordering::Release);
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
            let _ = sender.send(Ok(RunResult::Cleared));
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

    /// Takes the background result once it is there. A load's work is held until
    /// the frame it is resident in asks for it.
    pub fn poll(&mut self) -> Option<Finished> {
        if self.status() != Status::Loading {
            return None;
        }

        let result = {
            let results = self.finished.as_ref()?;

            let result = match results.try_recv() {
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
            Ok(RunResult::Loaded(loaded)) => {
                lock(&self.job).state = JobState::Loading {
                    loaded: Some(loaded),
                };

                Some(Finished::Loaded)
            }
            Ok(RunResult::Cleared) => {
                lock(&self.job).state = JobState::Idle;
                self.status.store(STATUS_EMPTY, Ordering::Release);

                Some(Finished::Cleared)
            }
            Err(reason) => {
                lock(&self.job).state = JobState::Idle;
                *lock(&self.error) = Some(reason);
                self.status.store(STATUS_FAILED, Ordering::Release);

                Some(Finished::Failed)
            }
        }
    }

    #[must_use]
    pub fn resident(&self, generation: u64) -> bool {
        lock(&self.job)
            .residency
            .as_ref()
            .is_some_and(|residency| residency.reached(generation))
    }

    /// Whether a frame of `version` is the one the armed gate opens on. Reports
    /// a frame once, so a load completes once.
    #[must_use]
    pub fn admitted(&self, version: u64) -> bool {
        lock(&self.job).display.admitted(version)
    }

    /// The pending load's work, handed over once.
    pub fn take_loaded(&self) -> Option<LoadedWorld> {
        let mut job = lock(&self.job);

        let JobState::Loading { loaded } = &mut job.state else {
            return None;
        };

        let loaded = loaded.take()?;

        job.state = JobState::Idle;
        drop(job);

        Some(*loaded)
    }

    /// Records how far the renderer has to have got for the job to be complete.
    pub fn record(&self, residency: Residency) {
        lock(&self.job).residency = Some(residency);
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

    fn join(&mut self) {
        if let Some(thread) = self.running.take()
            && thread.join().is_err()
        {
            *lock(&self.error) = Some(String::from("the loading thread panicked"));
            self.status.store(STATUS_FAILED, Ordering::Release);
        }
    }

    #[cfg(test)]
    fn settle(&mut self) {
        self.join();
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

fn run_load(source: &dyn WorldSource) -> Result<RunResult, String> {
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

    Ok(RunResult::Loaded(Box::new(LoadedWorld {
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
    clippy::cast_precision_loss,
    clippy::as_conversions
)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// A world's bytes, standing in for a file on disk.
    struct Bytes(Vec<u8>);

    impl WorldSource for Bytes {
        fn name(&self) -> String {
            String::from("in-memory world")
        }

        fn read(&self) -> Result<Vec<u8>, String> {
            Ok(self.0.clone())
        }
    }

    fn source(bytes: Vec<u8>) -> Bytes {
        Bytes(bytes)
    }

    /// Runs the frame loop until the background thread has handed something
    /// over. The host's own loop is the only other thing that calls `poll`.
    fn poll_until(job: &mut WorldJob) -> Finished {
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            if let Some(finished) = job.poll() {
                return finished;
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
        paletted_world(&[0u8; 1024])
    }

    /// The same world with the given `RGBA` chunk body: 256 RGB entries, four
    /// bytes each.
    fn paletted_world(palette: &[u8]) -> Vec<u8> {
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
        let palette = chunk(*b"RGBA", palette, &[]);
        let main = chunk(*b"MAIN", &[], &[size, voxel, palette].concat());

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(&main);

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
        job.world_resident();

        assert!(job.load(Box::new(source(one_voxel_world())), 0).is_ok());
        assert_eq!(job.status(), Status::Loading);
    }

    #[test]
    fn a_second_request_while_one_is_in_flight_is_refused() {
        let mut job = WorldJob::new();
        job.world_resident();

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
    fn a_load_while_a_clear_is_in_flight_is_refused() {
        let mut job = WorldJob::new();
        job.world_resident();

        job.clear(0).unwrap();

        assert!(matches!(
            job.load(Box::new(source(one_voxel_world())), 0),
            Err(Refusal::Busy)
        ));
    }

    #[test]
    fn a_finished_load_hands_its_work_over_once() {
        let mut job = WorldJob::new();
        job.world_resident();

        job.load(Box::new(source(one_voxel_world())), 4).unwrap();
        job.settle();

        assert_eq!(
            job.poll(),
            Some(Finished::Loaded),
            "the background work is done"
        );
        assert_eq!(
            job.status(),
            Status::Loading,
            "still loading until a frame carries it"
        );

        let Some(loaded) = job.take_loaded() else {
            panic!("the finished load must yield its snapshots");
        };

        assert!(
            !loaded.snapshots.is_empty(),
            "the one voxel world emits one micro chunk"
        );
        assert!(job.take_loaded().is_none(), "the work is handed over once");
        assert_eq!(
            job.status(),
            Status::Loading,
            "handing the work over is not the world being resident"
        );
    }

    #[test]
    fn a_finished_load_hands_over_the_palette_that_colours_its_snapshots() {
        let mut job = WorldJob::new();
        job.world_resident();

        let mut rgba = [0u8; 1024];
        let (entries, _) = rgba.as_chunks_mut::<4>();

        for (entry, slot) in entries.iter_mut().enumerate() {
            slot.copy_from_slice(&[(entry & 0xFF) as u8, 1, 2, 3]);
        }

        job.load(Box::new(source(paletted_world(&rgba))), 0)
            .unwrap();
        job.settle();

        assert_eq!(job.poll(), Some(Finished::Loaded));

        let Some(loaded) = job.take_loaded() else {
            panic!("the finished load must yield its snapshots");
        };

        assert!(
            !loaded.snapshots.is_empty(),
            "the palette has to arrive with the content it colours"
        );

        for (entry, color) in loaded.palette.iter().enumerate().skip(1) {
            let expected = (entry & 0xFF) as f32 / 255.0;

            assert!(
                (color.x - expected).abs() < 1.0e-6 && color.y > 0.0,
                "entry {entry} does not carry its own colour"
            );
        }
    }

    #[test]
    fn a_malformed_world_fails_without_killing_the_thread() {
        let mut job = WorldJob::new();
        job.world_resident();

        job.load(Box::new(source(vec![0xde, 0xad, 0xbe, 0xef])), 0)
            .unwrap();

        assert_eq!(poll_until(&mut job), Finished::Failed);
        assert_eq!(job.status(), Status::Failed);
        assert!(job.error().is_some(), "the failure carries a reason");
        assert!(job.take_loaded().is_none(), "no world came out of it");

        job.load(Box::new(source(one_voxel_world())), 0).unwrap();

        assert_eq!(
            poll_until(&mut job),
            Finished::Loaded,
            "the next job runs on a live thread"
        );
    }

    #[test]
    fn a_clear_reaches_the_empty_state() {
        let mut job = WorldJob::new();
        job.world_resident();

        job.clear(0).unwrap();

        assert_eq!(poll_until(&mut job), Finished::Cleared);
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
        job.world_resident();
        job.load(Box::new(source(one_voxel_world())), 7).unwrap();
        job.settle();
        job.poll();

        assert!(!job.admitted(7), "the outgoing content stays out");
        assert!(job.admitted(8));
        assert!(!job.admitted(9), "a later frame is not a second completion");
    }
}
