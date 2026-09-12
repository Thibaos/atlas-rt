use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// A load's progress, written by the loader thread and read by the host, in
/// units of one millionth.
const SCALE: u32 = 1_000_000;

/// How often the emit stage reports, in voxels. A shorter step than this is a
/// progress write no frame can observe.
pub(crate) const VOXEL_STEP: usize = 65_536;

/// The cumulative end of each stage except emit, in millionths, from the means
/// `cargo test --release -p atlas-rt --lib load_stage_weights -- --ignored
/// --nocapture` measured over the four worlds in the example project. Emit
/// walks the rest. Revisit them when the loader changes.
const STAGES: [(u32, u8, &str); 4] = [
    (62_000, 1, "read"),
    (226_000, 2, "parse"),
    (489_000, 3, "build"),
    (999_000, 4, "emit"),
];

/// Where the walk stops: the top belongs to the frame that carries the batch.
const EMIT_END: u32 = 999_000;

const NO_STAGE: u8 = 0;
const EMIT: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Read,
    Parse,
    Build,
    Emit,
}

// The index is the variant's own, so it is always inside the table.
#[allow(clippy::indexing_slicing)]
const fn stage_table(stage: Stage) -> (u32, u8, &'static str) {
    STAGES[stage.index()]
}

impl Stage {
    const fn index(self) -> usize {
        match self {
            Self::Read => 0,
            Self::Parse => 1,
            Self::Build => 2,
            Self::Emit => 3,
        }
    }

    const fn share(self) -> u32 {
        stage_table(self).0
    }

    const fn code(self) -> u8 {
        stage_table(self).1
    }

    const fn name(self) -> &'static str {
        stage_table(self).2
    }
}

/// How far the world being loaded has got, from 0 to 1. The stages run once
/// each on the loader thread, in order, except emit, which reports as it walks
/// the world's voxels.
#[derive(Debug, Default)]
pub struct Progress {
    millionths: AtomicU32,
    stage: AtomicU8,
}

impl Progress {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            millionths: AtomicU32::new(0),
            stage: AtomicU8::new(NO_STAGE),
        }
    }

    /// Records that every stage before `stage` is done, and that the counter now
    /// stands at that stage's share of the load.
    pub(crate) fn end_stage(&self, stage: Stage) {
        self.publish(stage, stage.share());
    }

    /// Opens the one stage that reports as it runs.
    pub(crate) fn start_emit(&self) {
        self.publish(Stage::Emit, Stage::Build.share());
    }

    /// Closes the emit stage's walk at the share it can reach, which is short of
    /// the top.
    pub(crate) fn end_emit(&self) {
        self.publish(Stage::Emit, EMIT_END);
    }

    /// Reports the emit stage's walk: `done` voxels of `total` are emitted. The
    /// caller asks at every `VOXEL_STEP` voxels and once at the end; the step is
    /// checked here too, so a report that no frame could see does nothing.
    #[allow(clippy::arithmetic_side_effects)]
    pub(crate) fn count_voxel(&self, total: usize, done: usize) {
        if done < total && !done.is_multiple_of(VOXEL_STEP) {
            return;
        }

        let span = u64::from(EMIT_END.saturating_sub(Stage::Build.share()));
        let walked = u64::try_from(done.min(total))
            .unwrap_or(u64::MAX)
            .saturating_mul(span)
            / u64::try_from(total.max(1)).unwrap_or(1);
        let millionths = Stage::Build
            .share()
            .saturating_add(u32::try_from(walked).unwrap_or(EMIT_END));

        self.publish(Stage::Emit, millionths);
    }

    /// The end of the job, and the only place the counter reaches the top. Runs
    /// on the main thread, when the frame that carries the batch is admitted.
    pub(crate) fn finish(&self) {
        self.stage.store(EMIT, Ordering::Relaxed);
        self.millionths.store(SCALE, Ordering::Release);
    }

    /// How far the load has got, 0 to 1.
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
            previous != stage.code() || stage == Stage::Emit,
            "the {} stage ran twice",
            stage.name()
        );

        assert!(
            millionths >= self.millionths.load(Ordering::Relaxed),
            "the {} stage reported less progress than the stage before it",
            stage.name()
        );

        self.stage.store(stage.code(), Ordering::Relaxed);
        self.millionths.store(millionths, Ordering::Release);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
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
            (progress.load() - f64::from(Stage::Build.share()) / f64::from(SCALE)).abs() < 1e-6
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
            (progress.load() - f64::from(Stage::Build.share()) / f64::from(SCALE)).abs() < 1e-6
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
}
