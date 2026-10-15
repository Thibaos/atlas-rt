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

/// The most queued cells one Simulation tick drains: a rail, not the mechanism.
/// A tick drains the whole queue, so this binds no designed scene. It bounds a
/// pathological one, a World filled with granular cells, at about 414 ns a
/// drained cell and 106 ns a committed move, so roughly one second per tick and
/// a rate below 1 Hz. A count and not a duration, so a tick's outcome does not
/// depend on the machine. Recorded in ADR 0018.
const MAX_CELLS_PER_TICK: usize = 2_000_000;

pub use input::{InputSample, PlayerState};
pub use physics::rules::ParityPolicy;
pub use profile::{PlayerProfile, ProfileError};
pub use runtime::{Activation, Command, Handle, Push, TickEnd, UpdateReport, spawn};
