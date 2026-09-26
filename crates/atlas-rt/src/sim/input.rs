use std::time::Instant;

use glam::{Vec2, Vec3};

/// One frame's input, sampled by the host before it sends the frame. Movement
/// is a world-space XZ key sum; a jump rising transition carries the sample's
/// monotonic timestamp.
#[derive(Clone, Copy, Debug, Default)]
pub struct InputSample {
    pub movement: Vec2,
    pub jump_edge: Option<Instant>,
}

/// The player pose a tick end reports: the feet position and the Grounded
/// state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerState {
    pub feet: Vec3,
    pub grounded: bool,
}
