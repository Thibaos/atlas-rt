use std::time::Duration;

/// The catch-up cap per update; the owed periods past it are discarded and
/// reported in `UpdateReport`.
const MAX_TICKS_PER_UPDATE: u32 = 5;

/// The frame-driven tick clock: frames accumulate elapsed time, whole periods
/// come due, and time only moves when a frame advances it.
#[derive(Debug)]
pub(super) struct Scheduler {
    period: Duration,
    accumulated: Duration,
}

impl Scheduler {
    /// # Panics
    ///
    /// Panics when `period` is zero, which `PlayerProfile::tick_period`
    /// already rejects.
    pub(super) const fn new(period: Duration) -> Self {
        assert!(!period.is_zero(), "a zero period has no tick");

        Self {
            period,
            accumulated: Duration::ZERO,
        }
    }

    pub(super) const fn advance(&mut self, elapsed: Duration) {
        self.accumulated = self.accumulated.saturating_add(elapsed);
    }

    pub(super) const fn reset(&mut self) {
        self.accumulated = Duration::ZERO;
    }

    /// The sub-period time the last due batch left behind.
    #[must_use]
    pub(super) const fn remainder(&self) -> Duration {
        self.accumulated
    }

    /// Whole periods owed now and the ones past the catch-up cap, leaving the
    /// sub-period remainder accumulated.
    #[must_use]
    pub(super) fn take_due(&mut self) -> (u32, u64) {
        let period = self.period.as_nanos();
        let owed = self.accumulated.as_nanos().checked_div(period).unwrap_or(0);
        let ticks = u32::try_from(owed)
            .unwrap_or(u32::MAX)
            .min(MAX_TICKS_PER_UPDATE);
        let discarded = u64::try_from(owed.saturating_sub(u128::from(ticks))).unwrap_or(u64::MAX);
        let remainder = self.accumulated.as_nanos().checked_rem(period).unwrap_or(0);

        self.accumulated = Duration::from_nanos(u64::try_from(remainder).unwrap_or(u64::MAX));

        (ticks, discarded)
    }
}
