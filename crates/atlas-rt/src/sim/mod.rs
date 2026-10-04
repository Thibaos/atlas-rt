//! The simulation on its own thread: frames, pause changes, commands, and
//! activations in, readiness and tick-end pushes out, against a World behind a
//! shared lock.

mod input;
mod physics;
mod profile;
mod runtime;
mod scheduler;

#[cfg(test)]
mod bench;

/// The most queued cells one Simulation tick drains, the other fixed bound on
/// an update's work beside the scheduler's catch-up cap. Sized to the tick
/// tripwire and recorded in ADR 0018.
const MAX_CELLS_PER_TICK: usize = 4096;

pub use input::{InputSample, PlayerState};
pub use physics::rules::ParityPolicy;
pub use profile::{PlayerProfile, ProfileError};
pub use runtime::{Activation, Command, Handle, Push, TickEnd, UpdateReport, spawn};
