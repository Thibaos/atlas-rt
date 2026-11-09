//! The Bedrock floor the fixed scenes share: one cell at the bottom layer of
//! the Lattice under every column.

use glam::IVec3;

use super::{
    World,
    grid::{LATTICE_EXTENT, LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH, chunks_across},
    load::progress::Progress,
    micro::{MICRO_AREA, MICRO_BYTES, MICRO_EDGE, MicroChunkRef},
    vocabulary::Material,
};

/// The floor's level, in cells: the Lattice's bottom layer.
pub const HEIGHT: i32 = -LATTICE_HALF_EXTENT.cast_signed();

/// The first cell above the floor.
pub const SURFACE: i32 = HEIGHT + 1;

/// Writes the floor, reporting each Micro-chunk against `total`, and returns
/// how many it wrote.
pub fn write(world: &mut World, progress: &Progress, total: usize) -> usize {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let mut written = 0usize;
    let mut z = half.saturating_neg();

    while z < half {
        let mut x = half.saturating_neg();

        while x < half {
            write_micro_chunk(world, IVec3::new(x, HEIGHT, z));

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
/// [`HEIGHT`] is a Micro-chunk boundary. Cell order `x + 8y + 64z` puts that
/// layer in the low byte of every eighth mask byte.
fn write_micro_chunk(world: &mut World, origin: IVec3) {
    let mut mask = [0u8; MICRO_BYTES];

    for z in 0..MICRO_EDGE {
        if let Some(layer) = mask.get_mut(z.saturating_mul(MICRO_EDGE)) {
            *layer = u8::MAX;
        }
    }

    let materials = [Material::Bedrock.index(); MICRO_AREA];

    if let Err(error) = world.write_entry(origin, MicroChunkRef::new(&mask, &materials)) {
        panic!("the floor wrote an invalid entry at {origin}: {error}");
    }
}

/// How many Micro-chunks the floor spans: one per Lattice column.
pub fn chunks() -> usize {
    let across = chunks_across(LATTICE_EXTENT.cast_signed());

    across.saturating_mul(across)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_chunk_is_one_bedrock_layer_under_every_column() {
        assert_eq!(HEIGHT, -2048);
        assert_eq!(SURFACE, -2047);
        assert_eq!(chunks(), 512 * 512);

        for origin in [
            IVec3::new(-2048, HEIGHT, -2048),
            IVec3::new(0, HEIGHT, 0),
            IVec3::new(2040, HEIGHT, 2040),
        ] {
            let mut world = World::empty();

            write_micro_chunk(&mut world, origin);

            for z in 0..MICRO_EDGE {
                for x in 0..MICRO_EDGE {
                    let floor_cell = origin + IVec3::new(x as i32, 0, z as i32);

                    assert_eq!(
                        world.get_voxel(&floor_cell),
                        Some(Material::Bedrock.index())
                    );
                    assert_eq!(world.get_voxel(&(floor_cell + IVec3::Y)), None);
                }
            }

            assert_eq!(world.voxel_count(), MICRO_AREA);
        }
    }
}
