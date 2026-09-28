use std::time::{Duration, Instant};

use atlas_rt::sim::PlayerState;
use glam::Vec3;

/// The host's sub-tick view: feet interpolate between the previous and the
/// current push's pose with alpha = (the sim's sub-tick remainder plus the
/// time since that push was applied) divided by the tick period, clamped to
/// zero through one. Discrete state always comes from the current push.
pub(super) struct ViewInterpolation {
    period: Duration,
    previous: PlayerState,
    current: PlayerState,
    remainder: Duration,
    applied_at: Instant,
}

impl ViewInterpolation {
    pub(super) fn new(period: Duration) -> Self {
        let idle = PlayerState {
            feet: Vec3::ZERO,
            grounded: false,
        };

        Self {
            period,
            previous: idle,
            current: idle,
            remainder: Duration::ZERO,
            applied_at: Instant::now(),
        }
    }

    /// Readiness or a snapped tick end: previous and current both hold
    /// `player`, so the feet hold that pose instead of lerping from a pose
    /// the sim never reported.
    pub(super) const fn snap(&mut self, now: Instant, player: PlayerState) {
        self.previous = player;
        self.current = player;
        self.remainder = Duration::ZERO;
        self.applied_at = now;
    }

    /// An interpolating tick end: the feet move from the pose it replaces to
    /// the one it carries, starting at the remainder the sim reports.
    ///
    /// Every tick end advances, including the one the sim flags as a snap:
    /// `Push::Ready` already snapped the activation's unbounded gap, so the
    /// first tick has a pose to interpolate from and glides like the rest.
    pub(super) const fn advance(&mut self, now: Instant, player: PlayerState, remainder: Duration) {
        self.previous = self.current;
        self.current = player;
        self.remainder = remainder;
        self.applied_at = now;
    }

    /// The frame's pose: feet lerped between the two pushes as of `now`,
    /// discrete state from the current push. Equal endpoints skip the lerp,
    /// so a snapped pose comes out exactly as the push reported it.
    #[must_use]
    pub(super) fn state(&self, now: Instant) -> PlayerState {
        if self.previous.feet == self.current.feet {
            return self.current;
        }

        PlayerState {
            feet: self.previous.feet.lerp(self.current.feet, self.alpha(now)),
            grounded: self.current.grounded,
        }
    }

    fn alpha(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.applied_at);
        let advanced = self.remainder.saturating_add(elapsed);

        (advanced.as_secs_f32() / self.period.as_secs_f32()).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use atlas_rt::sim::{PlayerProfile, PlayerState};
    use glam::Vec3;

    use super::*;

    const EPSILON: f32 = 0.00001;

    fn period() -> Duration {
        PlayerProfile::default().tick_period()
    }

    fn pose(feet_x: f32) -> PlayerState {
        PlayerState {
            feet: Vec3::new(feet_x, 0.0, 0.0),
            grounded: false,
        }
    }

    #[test]
    fn a_snap_holds_the_pose_it_carries() {
        let at = Instant::now();
        let mut view = ViewInterpolation::new(period());

        view.snap(at, pose(0.0));
        view.snap(at, pose(9.0));

        assert_eq!(view.state(at), pose(9.0));
        assert_eq!(
            view.state(at + period()).feet,
            Vec3::new(9.0, 0.0, 0.0),
            "the snapped pose holds for as long as it is current"
        );
    }

    #[test]
    fn the_feet_lerp_between_two_known_tick_poses() {
        let at = Instant::now();
        let mut view = ViewInterpolation::new(period());

        view.snap(at, pose(2.0));
        view.advance(at, pose(6.0), period().div_f64(2.0));

        let half = view.state(at).feet;

        assert!(
            (half.x - 4.0).abs() < EPSILON,
            "half a tick interpolates to the midpoint: {half:?}"
        );

        let three_quarters = view.state(at + period().div_f64(4.0)).feet;

        assert!(
            (three_quarters.x - 5.0).abs() < EPSILON,
            "the frame's elapsed time adds to the remainder: {three_quarters:?}"
        );
    }

    #[test]
    fn the_alpha_clamps_at_both_ends() {
        let at = Instant::now();
        let mut view = ViewInterpolation::new(period());

        view.snap(at, pose(2.0));

        view.advance(at + period(), pose(6.0), Duration::ZERO);

        let low = view.state(at).feet;

        assert!(
            (low.x - 2.0).abs() < EPSILON,
            "alpha clamps at zero, so a push stamped ahead of the frame leaves the feet on the previous pose: {low:?}"
        );

        view.advance(at, pose(10.0), Duration::ZERO);

        let high = view.state(at + period() + period()).feet;

        assert!(
            (high.x - 10.0).abs() < EPSILON,
            "alpha clamps at one, so a frame past its push holds the current pose: {high:?}"
        );
    }

    #[test]
    fn discrete_state_comes_from_the_current_push() {
        let at = Instant::now();
        let mut view = ViewInterpolation::new(period());

        view.snap(at, pose(2.0));
        view.advance(
            at,
            PlayerState {
                feet: Vec3::new(6.0, 0.0, 0.0),
                grounded: true,
            },
            Duration::ZERO,
        );

        assert!(
            view.state(at).grounded,
            "grounded is the current tick's flag, never blended"
        );
    }
}
