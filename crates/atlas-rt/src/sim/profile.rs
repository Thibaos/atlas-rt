use std::time::Duration;

/// The controller's immutable dimensions and motion values in voxel units. The
/// sim copies one profile and it survives every Activation, so a different
/// profile means a new sim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerProfile {
    pub width: f32,
    pub depth: f32,
    pub body_height: f32,
    pub eye_offset: f32,
    pub move_speed: f32,
    pub gravity: f32,
    pub jump_velocity: f32,
    pub step_height: u32,
    pub jump_buffer: Duration,
    pub tick_rate: u32,
}

impl Default for PlayerProfile {
    fn default() -> Self {
        Self {
            width: 0.6,
            depth: 0.6,
            body_height: 1.8,
            eye_offset: 1.62,
            move_speed: 4.0,
            gravity: 24.0,
            jump_velocity: 8.0,
            step_height: 2,
            jump_buffer: Duration::from_millis(150),
            tick_rate: 30,
        }
    }
}

impl PlayerProfile {
    /// One fixed tick at `tick_rate`, truncated to whole nanoseconds.
    ///
    /// # Panics
    ///
    /// Panics when `tick_rate` is zero, which the validated constructor in
    /// ticket 03 rejects.
    #[must_use]
    pub fn tick_period(&self) -> Duration {
        assert!(self.tick_rate > 0, "a tick rate of zero has no period");

        Duration::from_nanos(
            1_000_000_000u64
                .checked_div(u64::from(self.tick_rate))
                .unwrap_or(0),
        )
    }
}
