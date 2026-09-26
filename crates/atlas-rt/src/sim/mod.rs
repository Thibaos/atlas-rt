//! The simulation on its own thread: frames, commands, and activations in,
//! readiness and tick-end pushes out, against a World behind a shared lock.

mod input;
mod profile;
mod runtime;
mod scheduler;
mod spawn;

pub use input::{InputSample, PlayerState};
pub use profile::PlayerProfile;
pub use runtime::{Activation, Command, Handle, Push, TickEnd, UpdateReport, spawn};
