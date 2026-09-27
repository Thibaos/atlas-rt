use std::time::Instant;

use glam::{Quat, Vec2, Vec3};

/// One frame's input, sampled by the host before it sends the frame. Movement
/// is a world-space XZ key sum; a jump rising transition carries the sample's
/// monotonic timestamp.
#[derive(Clone, Copy, Debug, Default)]
pub struct InputSample {
    pub movement: Vec2,
    pub jump_edge: Option<Instant>,
}

impl InputSample {
    /// Rotates a yaw and key state into world space with the x negation the
    /// drawn frame's mirror applies, so strafe right reads positive against
    /// the view in either host.
    #[must_use]
    pub fn from_local(yaw: f32, strafe: f32, forward: f32, jump_edge: Option<Instant>) -> Self {
        let rotated = Quat::from_rotation_y(yaw).mul_vec3(Vec3::new(-strafe, 0.0, -forward));

        Self {
            movement: Vec2::new(rotated.x, rotated.z),
            jump_edge,
        }
    }
}

/// The player pose a tick end reports: the feet position and the Grounded
/// state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerState {
    pub feet: Vec3,
    pub grounded: bool,
}
