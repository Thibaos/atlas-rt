use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, MutexGuard,
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
        progress::{Progress, Stage},
        snapshot::{MicroChunkSnapshot, emit_snapshots_reporting},
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
    Clearing,
    Submitted,
    Done,
}

/// What a job asks the view to hold when its batch has been carried: the world
/// it loaded, or no world at all.
#[derive(Clone, Copy)]
enum Outcome {
    Load,
    Clear,
}

enum RunResult {
    Loaded(Box<LoadedWorld>),
    Cleared,
}

struct Job {
    state: JobState,
    outcome: Outcome,
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
    progress: Arc<Progress>,
    running: Option<JoinHandle<()>>,
    finished: Option<mpsc::Receiver<Result<RunResult, String>>>,
    job: Mutex<Job>,
}

impl WorldJob {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: AtomicU8::new(STATUS_EMPTY),
            error: Mutex::new(None),
            progress: Arc::new(Progress::new()),
            running: None,
            finished: None,
            job: Mutex::new(Job {
                state: JobState::Idle,
                outcome: Outcome::Load,
                display: DisplayGate::new(),
                residency: None,
            }),
        }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        Status::from_code(self.status.load(Ordering::Acquire))
    }

    /// How far the job in flight has got, 0 to 1. A job that is not in flight
    /// reads 1, so a host that polls until the status settles holds the maximum.
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.status() == Status::Loading {
            self.progress.load()
        } else {
            1.0
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }

    /// Declares the view to hold no world, for a clear that had nothing left to
    /// take away: no batch is submitted, so the job is done on this call and
    /// stops holding.
    pub fn no_world(&mut self) {
        lock(&self.job).state = JobState::Done;
        self.status.store(STATUS_EMPTY, Ordering::Release);
    }

    /// # Errors
    ///
    /// Returns `Refusal::Busy` while another job is in flight.
    ///
    /// `version` is the renderer's content version before this job's batch, so
    /// the frame that carries the batch is the first one past it.
    pub fn load(&mut self, source: Box<dyn WorldSource>, version: u64) -> Result<(), Refusal> {
        self.begin(version, Outcome::Load)?;

        let (sender, receiver) = mpsc::channel();
        let progress = Arc::clone(&self.progress);

        let thread = spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| run_pipeline(&progress, &*source)))
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
    ///
    /// `version` is the renderer's content version before this job's batch.
    pub fn clear(&mut self, version: u64) -> Result<(), Refusal> {
        self.begin(version, Outcome::Clear)?;

        let (sender, receiver) = mpsc::channel();

        let thread = spawn(move || {
            let _ = sender.send(Ok(RunResult::Cleared));
        });

        self.running = Some(thread);
        self.finished = Some(receiver);

        Ok(())
    }

    fn begin(&mut self, version: u64, outcome: Outcome) -> Result<(), Refusal> {
        if self.holding() {
            return Err(Refusal::Busy);
        }

        if self.running.is_some() {
            return Err(Refusal::Busy);
        }

        self.join();

        let mut job = lock(&self.job);

        job.outcome = outcome;
        job.display.arm(version);
        job.residency = None;
        drop(job);

        self.progress = Arc::new(Progress::new());
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
                lock(&self.job).state = JobState::Clearing;

                Some(Finished::Cleared)
            }
            Err(reason) => {
                lock(&self.job).state = JobState::Idle;
                self.progress.finish();
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

    /// Whether a job that finished its background work is waiting for the frame
    /// that carries it. The job is past its work but not done, so it is still in
    /// flight. A job whose carrying frame was admitted is `Done`, so it stops
    /// holding in the same turn its status settles.
    #[must_use]
    pub fn holding(&self) -> bool {
        matches!(
            lock(&self.job).state,
            JobState::Loading { loaded: Some(_) } | JobState::Clearing | JobState::Submitted
        )
    }

    /// Records that the frame carrying the submitted batch was admitted, which
    /// is when the view holds what the job asked for. A load leaves a world
    /// resident and a clear leaves none, and the counter reaches the top either
    /// way, so the host's bar ends when the view does.
    pub fn arrive(&self) {
        let status = {
            let mut job = lock(&self.job);

            job.state = JobState::Done;

            match job.outcome {
                Outcome::Load => STATUS_READY,
                Outcome::Clear => STATUS_EMPTY,
            }
        };

        self.progress.finish();
        self.status.store(status, Ordering::Release);
    }

    /// The submitted batch is the renderer's now, so the job is no longer
    /// holding work. It is still in flight, and still refuses, until the frame
    /// that carries the batch is admitted.
    pub fn taken(&self) {
        lock(&self.job).state = JobState::Submitted;
    }

    /// The pending load's work, handed over once.
    pub fn take_loaded(&self) -> Option<LoadedWorld> {
        let loaded = {
            let mut job = lock(&self.job);

            let JobState::Loading { loaded } = &mut job.state else {
                return None;
            };

            let loaded = *loaded.take()?;

            job.state = JobState::Idle;

            loaded
        };

        Some(loaded)
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
        self.progress.finish();
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

    /// Stands in for the view handing the planned batch to the renderer.
    #[cfg(test)]
    fn submitted(&self) {
        self.taken();
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

/// The load pipeline, from a world's bytes to the snapshots the renderer takes.
/// Its only output is plain data, so it runs on a thread with no renderer
/// access.
fn run_pipeline(progress: &Progress, source: &dyn WorldSource) -> Result<RunResult, String> {
    let name = source.name();
    let bytes = source
        .read()
        .map_err(|reason| format!("could not open {name}: {reason}"))?;

    progress.end_stage(Stage::Read);

    let voxel_data =
        open_bytes(&bytes).map_err(|error| format!("could not parse {name}: {error:#}"))?;

    progress.end_stage(Stage::Parse);

    let (world, clipped) = World::new_clipped(&voxel_data);

    progress.end_stage(Stage::Build);

    if clipped > 0 {
        eprintln!("atlas_rt: clipped {clipped} voxels outside the lattice");
    }

    let snapshots = emit_snapshots_reporting(&world, Some(progress))
        .map_err(|error| format!("could not emit {name}: {error:#}"))?;

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
        job.arrive();

        assert!(job.load(Box::new(source(one_voxel_world())), 0).is_ok());
        assert_eq!(job.status(), Status::Loading);
    }

    #[test]
    fn a_second_request_while_one_is_in_flight_is_refused() {
        let mut job = WorldJob::new();
        job.arrive();

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
        job.arrive();

        job.clear(0).unwrap();

        assert!(matches!(
            job.load(Box::new(source(one_voxel_world())), 0),
            Err(Refusal::Busy)
        ));
    }

    #[test]
    fn a_finished_load_hands_its_work_over_once() {
        let mut job = WorldJob::new();
        job.arrive();

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
        job.arrive();

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
        job.arrive();

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
    fn a_clear_reaches_the_empty_state_on_the_frame_that_carries_it() {
        let mut job = WorldJob::new();
        job.arrive();

        job.clear(0).unwrap();

        assert_eq!(poll_until(&mut job), Finished::Cleared);
        assert_eq!(
            job.status(),
            Status::Loading,
            "computed is not carried: the renderer still holds the old world"
        );
        assert!(job.holding(), "a submitted clear is still in flight");

        job.arrive();

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
        job.arrive();
        job.load(Box::new(source(one_voxel_world())), 7).unwrap();
        job.settle();
        job.poll();

        assert!(!job.admitted(7), "the outgoing content stays out");
        assert!(job.admitted(8));
        assert!(!job.admitted(9), "a later frame is not a second completion");
    }

    #[test]
    fn a_load_whose_batch_was_submitted_is_still_in_flight() {
        let mut job = WorldJob::new();
        job.arrive();
        job.load(Box::new(source(one_voxel_world())), 0).unwrap();
        job.settle();

        assert_eq!(job.poll(), Some(Finished::Loaded));

        job.take_loaded();
        job.submitted();

        assert_eq!(
            job.status(),
            Status::Loading,
            "the carrying frame has not been admitted"
        );
        assert!(
            matches!(
                job.load(Box::new(source(one_voxel_world())), 0),
                Err(Refusal::Busy)
            ),
            "a job between submitting its batch and having it carried is in flight"
        );
        assert_eq!(job.status(), Status::Loading, "the refusal changed nothing");
    }

    #[test]
    fn a_job_reports_how_far_its_load_has_got() {
        let mut job = WorldJob::new();
        job.arrive();
        job.load(Box::new(source(one_voxel_world())), 0).unwrap();

        assert!(
            job.progress().abs() < f64::EPSILON,
            "a fresh job has got nowhere"
        );

        job.settle();

        assert_eq!(job.poll(), Some(Finished::Loaded));

        assert!(
            job.progress() < 1.0,
            "the background work being done is not the world being resident"
        );

        job.arrive();

        assert!(
            (job.progress() - 1.0).abs() < f64::EPSILON,
            "the job is done once the frame that carries it is admitted"
        );
        assert!(
            !job.holding(),
            "a settled job is not holding, so the next request is not refused"
        );
    }

    #[test]
    fn a_settled_load_whose_work_is_untaken_is_still_in_flight() {
        let mut job = WorldJob::new();
        job.arrive();
        job.load(Box::new(source(one_voxel_world())), 0).unwrap();
        job.settle();

        assert_eq!(job.poll(), Some(Finished::Loaded));

        assert!(matches!(
            job.load(Box::new(source(one_voxel_world())), 0),
            Err(Refusal::Busy)
        ));
        assert_eq!(job.status(), Status::Loading);
    }

    #[test]
    fn a_clear_reaches_full_progress_when_it_is_carried() {
        let mut job = WorldJob::new();
        job.arrive();
        job.clear(0).unwrap();

        assert_eq!(poll_until(&mut job), Finished::Cleared);
        assert!(job.progress() < 1.0, "the clear is not carried yet");

        job.arrive();

        assert_eq!(job.status(), Status::Empty);
        assert!((job.progress() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_failed_load_still_reaches_full_progress() {
        let mut job = WorldJob::new();
        job.arrive();
        job.load(Box::new(source(vec![0xde, 0xad])), 0).unwrap();

        assert_eq!(poll_until(&mut job), Finished::Failed);
        assert_eq!(job.status(), Status::Failed);
        assert!(
            (job.progress() - 1.0).abs() < f64::EPSILON,
            "the loading bar is done either way"
        );
    }
}
