//! The Micro-chunk payload's width.

use crate::world::grid::MICRO_CHUNK_LENGTH;

pub const MICRO_EDGE: usize = MICRO_CHUNK_LENGTH as usize;
pub const MICRO_AREA: usize = MICRO_EDGE * MICRO_EDGE;
pub const MICRO_CELLS: usize = MICRO_EDGE * MICRO_AREA;
pub const MICRO_BYTES: usize = MICRO_CELLS / MICRO_EDGE;
