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

/// The most queued cells one Simulation tick drains.
///
/// A rail, not the mechanism: a tick drains the whole queue, so this binds no
/// designed scene. It bounds a pathological one, a World filled with granular
/// cells, at about 414 ns a drained cell and 106 ns a committed move, so
/// roughly one second per tick and a rate below 1 Hz. A count and not a
/// duration, so a tick's outcome does not depend on the machine. Recorded in
/// ADR 0018.
///
/// `pub` rather than `pub(crate)`: the tripwire sweep in `app/sim_host/bench.rs`
/// lives in the binary crate, which is a separate crate from this library, and
/// it derives its rule-work budget from this value rather than repeating the
/// literal.
pub const MAX_CELLS_PER_TICK: usize = 2_000_000;

pub use input::{InputSample, PlayerState};
pub use physics::rules::ParityPolicy;
pub use profile::{PlayerProfile, ProfileError};
pub use runtime::{Activation, Command, Handle, Push, TickEnd, UpdateReport, spawn};
