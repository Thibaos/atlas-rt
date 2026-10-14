//! One fixed ball of Sand, written instead of a height field.
//!
//! A solid sphere of radius [`RADIUS`], centred on the Lattice centre in x and z
//! with its y at [`CENTRE_HEIGHT`]: about 1.1 million grains in about 2,700
//! Micro-chunks. Every cell of it is inside the Lattice, so the fill writes no
//! clipped cell and refuses no grain.
//!
//! The ball hangs in an empty World with no floor under it, so its lowest grains
//! fall 2,176 cells to the Lattice floor at y = -2048 and its highest 2,304. The
//! first grain reaches the floor after about 72 seconds, and the whole ball
//! drains in about 0.6 million ticks, about 5.5 hours at the default 30 Hz. The
//! grains that land settle at the floor.

use glam::IVec3;

use super::{
    World,
    generation::GeneratedWorld,
    grid::{MICRO_CHUNK_LENGTH, grid_origin},
    load::progress::{Progress, Stage},
    micro::{self, MICRO_BYTES, MICRO_CELLS, MicroChunkRef},
    vocabulary::{Material, Vocabulary},
};

/// The ball's radius, in cells.
pub const RADIUS: i32 = 64;

/// The ball's centre level, in cells.
pub const CENTRE_HEIGHT: i32 = 192;

/// Builds the ball, filling Micro-chunk by Micro-chunk and reporting the
/// Generate stage as it goes.
#[must_use]
pub fn build(progress: &Progress) -> GeneratedWorld {
    let mut world = World::empty();
    let mut granular_cells = Vec::new();

    fill(&mut world, progress, &mut granular_cells);

    progress.end_stage(Stage::Generate);

    let vocabulary = Vocabulary::new();

    GeneratedWorld {
        world,
        palette: vocabulary.palette(),
        materials: vocabulary.materials(),
        granular_cells,
    }
}

/// The ball's centre: the Lattice centre in x and z, at [`CENTRE_HEIGHT`].
const fn centre() -> IVec3 {
    IVec3::new(0, CENTRE_HEIGHT, 0)
}

/// The ball's fill: every cell within [`RADIUS`] of the centre is Sand. The
/// chunks are walked in the Generation's x then z order, one column of y chunks
/// at a time, and a chunk the sphere misses is skipped, so the World holds no
/// empty entry.
fn fill(world: &mut World, progress: &Progress, granular_cells: &mut Vec<IVec3>) {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let lower = chunk_origin(centre().saturating_sub(IVec3::splat(RADIUS)));
    let upper = chunk_origin(centre().saturating_add(IVec3::splat(RADIUS)))
        .saturating_add(IVec3::splat(edge));
    let total = fillable_chunks(lower, upper);
    let mut filled = 0usize;
    let mut origin = lower;

    while origin.z < upper.z {
        origin.x = lower.x;

        while origin.x < upper.x {
            filled = filled.saturating_add(write_column_of_micro_chunks(
                world,
                origin,
                lower.y,
                upper.y,
                granular_cells,
            ));
            progress.count_generated(total, filled);

            origin.x = origin.x.saturating_add(edge);
        }

        origin.z = origin.z.saturating_add(edge);
    }
}

/// How many Micro-chunks the fill walks: one per chunk of the ball's bounding
/// box. The ball misses the corners of the box, so the count is an upper bound
/// and the reported progress is never below the truth.
fn fillable_chunks(lower: IVec3, upper: IVec3) -> usize {
    chunks_across(upper.x.saturating_sub(lower.x))
        .saturating_mul(chunks_across(upper.z.saturating_sub(lower.z)))
        .saturating_mul(chunks_across(upper.y.saturating_sub(lower.y)))
}

/// The Micro-chunk origin at or below `position`.
fn chunk_origin(position: IVec3) -> IVec3 {
    grid_origin(position, MICRO_CHUNK_LENGTH)
}

/// How many Micro-chunk origins the fill steps through over `span` cells.
fn chunks_across(span: i32) -> usize {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let edge_span = edge.saturating_sub(1);

    usize::try_from(span.saturating_add(edge_span).div_euclid(edge)).unwrap_or(0)
}

/// Fills the column of Micro-chunks at `origin`'s x and z, from `floor` up to
/// `ceiling`. Returns how many of them hold a cell, so the caller can report
/// progress.
fn write_column_of_micro_chunks(
    world: &mut World,
    origin: IVec3,
    floor: i32,
    ceiling: i32,
    granular_cells: &mut Vec<IVec3>,
) -> usize {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let mut written = 0usize;
    let mut y = floor;

    while y < ceiling {
        if write_micro_chunk(world, IVec3::new(origin.x, y, origin.z), granular_cells) {
            written = written.saturating_add(1);
        }

        y = y.saturating_add(edge);
    }

    written
}

/// Fills one Micro-chunk. Its cells inside the ball are Sand, and their
/// positions join the granular cell list in the chunk's ascending cell order.
/// Returns whether the chunk holds a cell, so an empty one is never written.
fn write_micro_chunk(world: &mut World, origin: IVec3, granular_cells: &mut Vec<IVec3>) -> bool {
    let mut mask = [0u8; MICRO_BYTES];
    let mut materials: Vec<u8> = Vec::with_capacity(MICRO_CELLS);

    for index in 0..MICRO_CELLS {
        let position = origin.saturating_add(micro::cell_offset(index));

        if !inside(position) {
            continue;
        }

        micro::set_cell(&mut mask, index, true);

        materials.push(Material::Sand.index());
        granular_cells.push(position);
    }

    if materials.is_empty() {
        return false;
    }

    if let Err(error) = world.write_entry(origin, MicroChunkRef::new(&mask, &materials)) {
        panic!("the sand ball wrote an invalid entry at {origin}: {error}");
    }

    true
}

/// Whether `position` is inside the ball.
fn inside(position: IVec3) -> bool {
    position.saturating_sub(centre()).length_squared() <= RADIUS.saturating_mul(RADIUS)
}
