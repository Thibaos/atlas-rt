//! The simulation on its own thread: frames, pause changes, commands, and
//! activations in, readiness and tick-end pushes out, against a World behind a
//! shared lock.

mod input;
mod physics;
mod profile;
mod rain;
mod runtime;
mod scheduler;

#[cfg(test)]
mod bench;

pub use input::{InputSample, PlayerState};
pub use physics::rules::{DrainPolicy, ParityPolicy};
pub use profile::{PlayerProfile, ProfileError};
pub use runtime::{Activation, Command, Emission, Handle, Push, TickEnd, UpdateReport, spawn};
