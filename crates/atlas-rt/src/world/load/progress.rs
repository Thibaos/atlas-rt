use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// A load's progress, written by the loader thread and read by the host, in
/// units of one millionth.
const SCALE: u32 = 1_000_000;

/// How often the emit stage reports, in voxels. A shorter step than this is a
/// progress write no frame can observe.
pub(crate) const VOXEL_STEP: usize = 65_536;

/// Cumulative stage endpoints in millionths for the load path, based on mean
/// timings across the example project's four worlds (castle, sponza, nuke,
/// bistro), re-measured on the Region store: read 2.1%, parse +10.8%, build
/// +21.3% of the total. Emit covers all work after build and now dominates at
/// about 66%. Measured with
/// `cargo test --release -p atlas-rt --lib load_stage_weights -- --ignored
/// --nocapture`. Revisit when the loader changes.
const LOAD_STAGES: [(u32, u8, &str); 3] = [
    (21_000, 1, "read"),
    (129_000, 2, "parse"),
    (342_000, 3, "build"),
];

/// Cumulative stage endpoints in millionths for the generation path: generate
/// 82.1%, build about 0.1%, emit 17.8% of the total, measured over generated
/// terrain at 256- and 512-edge footprints (generate 16.6 ms, emit 3.4 ms at
/// 4.26M voxels; generate 59.7 ms, emit 13.9 ms at 17.0M). Measured with
/// `cargo test --release -p atlas-rt --lib generation_stage_weights -- --ignored
/// --nocapture`. Revisit when the generator changes.
const GENERATE_STAGES: [(u32, u8, &str); 2] = [(821_000, 5, "generate"), (822_000, 6, "build")];

/// Emit's progress limit. Full progress requires a frame that includes the batch.
const EMIT_END: u32 = 999_000;

const NO_STAGE: u8 = 0;
const EMIT: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Read,
    Parse,
    Build,
    Emit,
    Generate,
}

/// Which pipeline a job runs: a load's Read, Parse, Build, Emit or a
/// Generation's Generate, Build, Emit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Path {
    #[default]
    Load,
    Generate,
}

impl Path {
    const fn stages(self) -> &'static [(u32, u8, &'static str)] {
        match self {
            Self::Load => &LOAD_STAGES,
            Self::Generate => &GENERATE_STAGES,
        }
    }
}

const fn stage_table(path: Path, stage: Stage) -> (u32, u8, &'static str) {
    match stage {
        Stage::Emit => (EMIT_END, EMIT, "emit"),
        Stage::Build => {
            let stages = path.stages();

            match stages.last() {
                Some(&last) => last,
                None => (EMIT_END, EMIT, "emit"),
            }
        }
        Stage::Read if matches!(path, Path::Load) => LOAD_STAGES[0],
        Stage::Parse if matches!(path, Path::Load) => LOAD_STAGES[1],
        Stage::Generate | Stage::Read | Stage::Parse => GENERATE_STAGES[0],
    }
}

impl Stage {
    /// Whether the stage reports as it runs, so it is published more than once:
    /// Emit through the voxel walk, Generate through the fill.
    const fn repeats(self) -> bool {
        matches!(self, Self::Emit | Self::Generate)
    }

    const fn share(self, path: Path) -> u32 {
        stage_table(path, self).0
    }

    const fn code(self, path: Path) -> u8 {
        stage_table(path, self).1
    }

    const fn name(self, path: Path) -> &'static str {
        stage_table(path, self).2
    }
}

/// Load or Generation progress from 0 to 1.
///
/// Stages report once each on the loader thread, in order, except emit, which
/// reports throughout voxel traversal. `path` fixes the stage table the reports
/// are measured against.
#[derive(Debug, Default)]
pub struct Progress {
    path: Path,
    millionths: AtomicU32,
    stage: AtomicU8,
}

impl Progress {
    /// A load's progress: Read, Parse, Build, Emit.
    #[must_use]
    pub const fn new() -> Self {
        Self::load_path()
    }

    /// A load's progress: Read, Parse, Build, Emit.
    #[must_use]
    pub const fn load_path() -> Self {
        Self {
            path: Path::Load,
            millionths: AtomicU32::new(0),
            stage: AtomicU8::new(NO_STAGE),
        }
    }

    /// A Generation's progress: Generate, Build, Emit.
    #[must_use]
    pub const fn generate_path() -> Self {
        Self {
            path: Path::Generate,
            millionths: AtomicU32::new(0),
            stage: AtomicU8::new(NO_STAGE),
        }
    }

    /// Reports `done` filled Micro-chunks out of `total`, advancing through the
    /// Generate span. The caller reports once per filled Micro-chunk.
    pub(crate) fn count_generated(&self, total: usize, done: usize) {
        let end = Stage::Generate.share(self.path);
        let filled = (done.min(total) as u64)
            .saturating_mul(u64::from(end))
            .div_ceil(total.max(1) as u64);

        self.publish(Stage::Generate, u32::try_from(filled).unwrap_or(end));
    }

    /// Records completion of all stages before `stage` and sets progress to
    /// that stage's cumulative endpoint.
    pub(crate) fn end_stage(&self, stage: Stage) {
        self.publish(stage, stage.share(self.path));
    }

    /// Opens the one stage that reports as it runs.
    pub(crate) fn start_emit(&self) {
        self.publish(Stage::Emit, Stage::Build.share(self.path));
    }

    /// Ends emit at its progress limit, below 1.
    pub(crate) fn end_emit(&self) {
        self.publish(Stage::Emit, EMIT_END);
    }

    /// Reports `done` emitted voxels out of `total`. The caller reports every
    /// `VOXEL_STEP` voxels. This method also checks the interval and ignores
    /// calls between steps.
    pub(crate) fn count_voxel(&self, total: usize, done: usize) {
        if !done.is_multiple_of(VOXEL_STEP) {
            return;
        }

        let span = u64::from(EMIT_END.saturating_sub(Stage::Build.share(self.path)));
        let walked = (done.min(total) as u64)
            .saturating_mul(span)
            .div_ceil(total.max(1) as u64);
        let millionths = Stage::Build
            .share(self.path)
            .saturating_add(u32::try_from(walked).unwrap_or(EMIT_END));

        self.publish(Stage::Emit, millionths);
    }

    /// Ends the job. Only this method sets progress to 1. Runs on the main
    /// thread when the frame containing the batch is admitted.
    pub(crate) fn finish(&self) {
        self.stage.store(EMIT, Ordering::Relaxed);
        self.millionths.store(SCALE, Ordering::Release);
    }

    /// Job progress from 0 to 1.
    #[must_use]
    pub fn load(&self) -> f64 {
        f64::from(self.millionths.load(Ordering::Acquire)) / f64::from(SCALE)
    }

    fn publish(&self, stage: Stage, millionths: u32) {
        let previous = self.stage.load(Ordering::Relaxed);

        assert!(
            previous != EMIT || stage == Stage::Emit,
            "no stage follows emit"
        );

        assert!(
            previous != stage.code(self.path) || stage.repeats(),
            "the {} stage ran twice",
            stage.name(self.path)
        );

        assert!(
            millionths >= self.millionths.load(Ordering::Relaxed),
            "the {} stage reported less progress than the stage before it",
            stage.name(self.path)
        );

        self.stage.store(stage.code(self.path), Ordering::Relaxed);
        self.millionths.store(millionths, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_load_has_got_nowhere() {
        assert!(Progress::new().load().abs() < f64::EPSILON);
    }

    #[test]
    fn a_stage_boundary_advances_the_counter_to_its_share() {
        let progress = Progress::new();

        progress.end_stage(Stage::Read);
        progress.end_stage(Stage::Parse);
        progress.end_stage(Stage::Build);

        assert!(
            (progress.load() - f64::from(Stage::Build.share(Path::Load)) / f64::from(SCALE)).abs()
                < 1e-6
        );
    }

    #[test]
    fn emit_progress_is_monotonic_and_stops_short_of_the_top() {
        const TOTAL: usize = 1_000_000;

        let progress = Progress::new();

        progress.end_stage(Stage::Read);
        progress.end_stage(Stage::Parse);
        progress.end_stage(Stage::Build);
        progress.start_emit();

        let mut previous = progress.load();

        for done in (VOXEL_STEP..=TOTAL).step_by(VOXEL_STEP) {
            progress.count_voxel(TOTAL, done);

            let current = progress.load();

            assert!(current >= previous, "progress went backwards at {done}");

            previous = current;
        }

        progress.end_emit();

        assert!(
            progress.load() < 1.0,
            "the walk does not reach the top; the frame that carries it does"
        );

        progress.finish();

        assert!((progress.load() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_end_of_the_job_reaches_the_top() {
        let progress = Progress::new();
        progress.start_emit();

        progress.finish();

        assert!((progress.load() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_emit_stage_reports_between_its_ends() {
        let progress = Progress::new();
        progress.end_stage(Stage::Read);
        progress.end_stage(Stage::Parse);
        progress.end_stage(Stage::Build);
        progress.start_emit();

        let start = progress.load();

        progress.count_voxel(VOXEL_STEP * 2, VOXEL_STEP);

        assert!(progress.load() > start);
        assert!(progress.load() <= f64::from(EMIT_END) / f64::from(SCALE));
    }

    #[test]
    fn the_walk_reports_only_on_its_step() {
        let progress = Progress::new();
        progress.start_emit();

        progress.count_voxel(VOXEL_STEP * 2, 1);

        assert!(
            (progress.load() - f64::from(Stage::Build.share(Path::Load)) / f64::from(SCALE)).abs()
                < 1e-6
        );
    }

    #[test]
    fn a_stage_cannot_run_twice() {
        let progress = Progress::new();
        progress.end_stage(Stage::Build);

        let repeated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            progress.end_stage(Stage::Build);
        }));

        assert!(repeated.is_err());
    }

    #[test]
    fn no_stage_follows_emit() {
        let progress = Progress::new();
        progress.start_emit();

        let late = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            progress.end_stage(Stage::Build);
        }));

        assert!(late.is_err());
    }

    #[test]
    fn the_generation_path_advances_through_generate_build_and_emit() {
        let progress = Progress::generate_path();

        progress.end_stage(Stage::Generate);

        assert!(
            (progress.load() - 0.821).abs() < 1e-6,
            "generate ends at its measured share"
        );

        progress.end_stage(Stage::Build);

        assert!((progress.load() - 0.822).abs() < 1e-6);

        progress.start_emit();
        progress.count_voxel(VOXEL_STEP * 2, VOXEL_STEP);

        assert!(progress.load() > 0.822);
        assert!(progress.load() <= f64::from(EMIT_END) / f64::from(SCALE));

        progress.end_emit();
        progress.finish();

        assert!((progress.load() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_generation_fill_reports_monotonically_through_its_span() {
        let progress = Progress::generate_path();
        let mut previous = progress.load();

        for done in [1usize, 2, 3, 4, 5] {
            progress.count_generated(5, done);

            let current = progress.load();

            assert!(current >= previous, "generate progress went backwards");
            assert!(
                current <= 0.821 + 1e-6,
                "the fill stays below the generate endpoint"
            );

            previous = current;
        }

        progress.end_stage(Stage::Generate);

        assert!((progress.load() - 0.821).abs() < 1e-6);
    }
}
