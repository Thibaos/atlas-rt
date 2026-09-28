//! The simulation on its own thread: frames, pause changes, commands, and
//! activations in, readiness and tick-end pushes out, against a World behind a
//! shared lock.

mod input;
mod physics;
mod profile;
mod runtime;
mod scheduler;

pub use input::{InputSample, PlayerState};
pub use physics::rules::ParityPolicy;
pub use profile::{PlayerProfile, ProfileError};
pub use runtime::{Activation, Command, Handle, Push, TickEnd, UpdateReport, spawn};
