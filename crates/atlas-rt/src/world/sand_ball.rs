//! One fixed ball of Sand above a one-cell Bedrock floor, written instead of a
//! height field.
//!
//! A solid sphere of radius [`RADIUS`], centred on the Lattice centre in x and z
//! with its y at [`CENTRE_HEIGHT`]: about 1.1 million grains in about 2,700
//! Micro-chunks. Under it, the Lattice's bottom layer is a floor of one Bedrock
//! cell at [`FLOOR_HEIGHT`] under each of the 4,096 by 4,096 columns, in 262,144
//! Micro-chunks. Every cell of both is inside the Lattice, so the fill writes no
//! clipped cell and refuses no grain.
//!
//! The ball's lowest layer hangs [`FLOOR_CLEARANCE`] cells above the floor's
//! surface, so its lowest grains fall 128 cells to land at y = -2047 and its
//! highest 256. The body descends one cell a tick and its span stays at 128, so
//! the first grains reach the floor around tick 128. A tick on the full ball
//! costs about 578 ms, so that fall is roughly 75 seconds of wall time and the
//! pile that follows settles more slowly still. The grains that land settle in a
//! pile on the floor, and the pile shortens the later falls.

use glam::IVec3;

use super::{
    World,
    generation::GeneratedWorld,
    grid::{LATTICE_EXTENT, LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH, chunks_across, grid_origin},
    load::progress::{Progress, Stage},
    micro::{self, MICRO_AREA, MICRO_BYTES, MICRO_CELLS, MICRO_EDGE, MicroChunkRef},
    vocabulary::{Material, Vocabulary},
};

/// The ball's radius, in cells.
pub const RADIUS: i32 = 64;

/// The floor's level, in cells: the Lattice's bottom layer.
pub const FLOOR_HEIGHT: i32 = -LATTICE_HALF_EXTENT.cast_signed();

/// The ball's lowest layer, in cells above the floor's surface.
pub const FLOOR_CLEARANCE: i32 = 128;

/// The ball's centre level, in cells: one layer for the floor, then
/// [`FLOOR_CLEARANCE`], then the ball's own [`RADIUS`].
pub const CENTRE_HEIGHT: i32 = FLOOR_HEIGHT + 1 + FLOOR_CLEARANCE + RADIUS;

/// Builds the floor and the ball, filling Micro-chunk by Micro-chunk and
/// reporting the Generate stage as it goes.
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

/// Fills the floor, then the ball: every cell within [`RADIUS`] of the centre is
/// Sand. The floor is written first, one Micro-chunk per Lattice column. The
/// ball's chunks are then walked in the Generation's x then z order, one column
/// of y chunks at a time, and a chunk the sphere misses is skipped, so the World
/// holds no empty entry. Progress is counted over both passes, so the Generate
/// stage reports the floor before the ball.
fn fill(world: &mut World, progress: &Progress, granular_cells: &mut Vec<IVec3>) {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let lower = chunk_origin(centre().saturating_sub(IVec3::splat(RADIUS)));
    let upper = chunk_origin(centre().saturating_add(IVec3::splat(RADIUS)))
        .saturating_add(IVec3::splat(edge));
    let total = floor_chunks().saturating_add(fillable_chunks(lower, upper));
    let mut filled = write_floor(world, progress, total);
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

/// Writes the floor: one Bedrock cell at [`FLOOR_HEIGHT`] under every column of
/// the Lattice. Returns how many Micro-chunks it wrote, so the caller can carry
/// the count into the ball's progress.
fn write_floor(world: &mut World, progress: &Progress, total: usize) -> usize {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let mut written = 0usize;
    let mut z = half.saturating_neg();

    while z < half {
        let mut x = half.saturating_neg();

        while x < half {
            write_floor_micro_chunk(world, IVec3::new(x, FLOOR_HEIGHT, z));

            written = written.saturating_add(1);
            progress.count_generated(total, written);

            x = x.saturating_add(edge);
        }

        z = z.saturating_add(edge);
    }

    written
}

/// Writes one floor Micro-chunk: its whole bottom layer is Bedrock, one cell
/// under each of the chunk's 8 by 8 columns, which is the floor because
/// [`FLOOR_HEIGHT`] is a Micro-chunk boundary. Cell order `x + 8y + 64z` puts
/// that layer in the low byte of every eighth mask byte.
fn write_floor_micro_chunk(world: &mut World, origin: IVec3) {
    let mut mask = [0u8; MICRO_BYTES];

    for z in 0..MICRO_EDGE {
        if let Some(layer) = mask.get_mut(z.saturating_mul(MICRO_EDGE)) {
            *layer = u8::MAX;
        }
    }

    let materials = [Material::Bedrock.index(); MICRO_AREA];

    if let Err(error) = world.write_entry(origin, MicroChunkRef::new(&mask, &materials)) {
        panic!("the sand ball floor wrote an invalid entry at {origin}: {error}");
    }
}

/// How many Micro-chunks the floor spans: one per Lattice column.
fn floor_chunks() -> usize {
    let across = chunks_across(LATTICE_EXTENT.cast_signed());

    across.saturating_mul(across)
}

/// How many Micro-chunks the ball's fill walks: one per chunk of its bounding
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
