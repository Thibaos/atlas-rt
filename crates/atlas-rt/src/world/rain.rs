//! One fixed plate of Bedrock with a grid of holes above a one-cell Bedrock
//! floor, and the emitter that rains Sand through those holes while the
//! simulation runs.
//!
//! The plate is a disc of radius [`RADIUS`] at [`PLATE_HEIGHT`], one cell thick:
//! every cell inside the disc is Bedrock except the holes, whose x and z offsets
//! are both multiples of [`HOLE_STRIDE`]. That leaves 3,209 holes in 202,652
//! plate cells over about 3,200 Micro-chunks, and the disc is 513 cells across.
//! Under it, the Lattice's bottom layer is a floor of one Bedrock cell at
//! [`FLOOR_HEIGHT`] under each of the 4,096 by 4,096 columns, in 262,144
//! Micro-chunks.
//!
//! A grain leaves its hole at -1024 and lands on the floor's surface at -2047,
//! 1,023 cells and 1,023 ticks later. The fill seeds nothing: the World holds no
//! Sand until the simulation plants some, so it hands the emitter the hole
//! positions and the schedule its interval and its per-tick count.

use glam::IVec3;

use super::{
    World,
    generation::GeneratedWorld,
    grid::{LATTICE_EXTENT, LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH, chunks_across, grid_origin},
    load::progress::{Progress, Stage},
    micro::{self, MICRO_AREA, MICRO_BYTES, MICRO_CELLS, MICRO_EDGE, MicroChunkRef},
    vocabulary::{Material, Vocabulary},
};

/// The plate's radius, in cells.
pub const RADIUS: i32 = 256;

/// The floor's level, in cells: the Lattice's bottom layer.
pub const FLOOR_HEIGHT: i32 = -LATTICE_HALF_EXTENT.cast_signed();

/// The plate's level, in cells above the floor's own level.
pub const PLATE_CLEARANCE: i32 = 1024;

/// The plate's level, in cells: a Micro-chunk boundary.
pub const PLATE_HEIGHT: i32 = FLOOR_HEIGHT + PLATE_CLEARANCE;

/// The spacing of the plate's holes in x and z, in cells.
pub const HOLE_STRIDE: i32 = 8;

/// The ticks between two emissions the same hole may serve.
pub const EMIT_INTERVAL: u64 = 2;

/// The grains the schedule plants per tick.
pub const GRAINS_PER_TICK: usize = 128;

/// The holes a rain World plants grains into, with the schedule's own counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emitter {
    pub cells: Vec<IVec3>,
    pub interval: u64,
    pub per_tick: usize,
}

/// Builds the floor and the plate, filling Micro-chunk by Micro-chunk and
/// reporting the Generate stage as it goes, and returns the emitter its holes
/// make.
#[must_use]
pub fn build(progress: &Progress) -> (GeneratedWorld, Emitter) {
    let mut world = World::empty();
    let mut holes = Vec::new();

    fill(&mut world, progress, &mut holes);

    progress.end_stage(Stage::Generate);

    let vocabulary = Vocabulary::new();

    let built = GeneratedWorld {
        world,
        palette: vocabulary.palette(),
        materials: vocabulary.materials(),
        granular_cells: Vec::new(),
    };
    let emitter = Emitter {
        cells: holes,
        interval: EMIT_INTERVAL,
        per_tick: GRAINS_PER_TICK,
    };

    (built, emitter)
}

/// Fills the floor, then the plate. Progress is counted over both passes, so
/// the Generate stage reports the floor before the plate.
fn fill(world: &mut World, progress: &Progress, holes: &mut Vec<IVec3>) {
    let total = floor_chunks().saturating_add(plate_chunks());
    let mut filled = write_floor(world, progress, total);

    write_plate(world, progress, total, &mut filled, holes);
}

/// Writes the floor: one Bedrock cell at [`FLOOR_HEIGHT`] under every column of
/// the Lattice. Returns how many Micro-chunks it wrote, so the caller can carry
/// the count into the plate's progress.
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
        panic!("the rain floor wrote an invalid entry at {origin}: {error}");
    }
}

/// Writes the plate over the chunks its disc spans, recording every hole it
/// leaves empty.
fn write_plate(
    world: &mut World,
    progress: &Progress,
    total: usize,
    filled: &mut usize,
    holes: &mut Vec<IVec3>,
) {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let (lower, upper) = plate_bounds();
    let mut origin = lower;

    while origin.z < upper.z {
        origin.x = lower.x;

        while origin.x < upper.x {
            if write_plate_micro_chunk(world, origin, holes) {
                *filled = filled.saturating_add(1);
            }

            progress.count_generated(total, *filled);

            origin.x = origin.x.saturating_add(edge);
        }

        origin.z = origin.z.saturating_add(edge);
    }
}

/// Writes one plate Micro-chunk: the cells of its bottom layer that are inside
/// the disc and are not holes are Bedrock, and its holes join the emitter's
/// cell list in the chunk's ascending cell order. Returns whether the chunk
/// holds a cell, so an empty one is never written.
fn write_plate_micro_chunk(world: &mut World, origin: IVec3, holes: &mut Vec<IVec3>) -> bool {
    let mut mask = [0u8; MICRO_BYTES];
    let mut materials: Vec<u8> = Vec::with_capacity(MICRO_AREA);

    for index in 0..MICRO_CELLS {
        let position = origin.saturating_add(micro::cell_offset(index));

        if position.y != PLATE_HEIGHT || !in_disc(position.x, position.z) {
            continue;
        }

        if is_hole(position.x, position.z) {
            holes.push(position);

            continue;
        }

        micro::set_cell(&mut mask, index, true);

        materials.push(Material::Bedrock.index());
    }

    if materials.is_empty() {
        return false;
    }

    if let Err(error) = world.write_entry(origin, MicroChunkRef::new(&mask, &materials)) {
        panic!("the rain plate wrote an invalid entry at {origin}: {error}");
    }

    true
}

/// The Micro-chunk origins the plate's disc spans, and one past the last.
fn plate_bounds() -> (IVec3, IVec3) {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let lower = chunk_origin(IVec3::new(
        RADIUS.saturating_neg(),
        PLATE_HEIGHT,
        RADIUS.saturating_neg(),
    ));
    let upper =
        chunk_origin(IVec3::new(RADIUS, PLATE_HEIGHT, RADIUS)).saturating_add(IVec3::splat(edge));

    (lower, upper)
}

/// The Micro-chunk origin at or below `position`.
fn chunk_origin(position: IVec3) -> IVec3 {
    grid_origin(position, MICRO_CHUNK_LENGTH)
}

/// How many Micro-chunks the floor spans: one per Lattice column.
fn floor_chunks() -> usize {
    let across = chunks_across(LATTICE_EXTENT.cast_signed());

    across.saturating_mul(across)
}

/// How many Micro-chunks the plate's disc spans.
fn plate_chunks() -> usize {
    let (lower, upper) = plate_bounds();
    let span = upper.saturating_sub(lower);

    chunks_across(span.x).saturating_mul(chunks_across(span.z))
}

/// Whether the column at `x` and `z` is inside the plate's disc.
const fn in_disc(x: i32, z: i32) -> bool {
    x.saturating_mul(x).saturating_add(z.saturating_mul(z)) <= RADIUS.saturating_mul(RADIUS)
}

/// Whether the column at `x` and `z` is a hole in the plate.
const fn is_hole(x: i32, z: i32) -> bool {
    x.rem_euclid(HOLE_STRIDE) == 0 && z.rem_euclid(HOLE_STRIDE) == 0
}
