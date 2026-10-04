//! Turns Generation params into a World.
//!
//! The generator fills one Micro-chunk at a time on one thread. The World it
//! builds is the single source of truth: the emit step derives the Snapshots
//! from it, and the Palette and Physical material table come from the
//! Vocabulary.

use glam::IVec3;

use super::{
    World,
    grid::{LATTICE_EXTENT, LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH},
    load::progress::{Progress, Stage},
    material::PhysicalMaterialTable,
    vocabulary::{Material, Vocabulary},
};

use super::diff::edit::{MICRO_BYTES, cell_offset};

/// What a Generation is asked for: the integer that fixes the World and how far
/// its ground extends, x by z, from the lattice's negative corner. The fill
/// spans the full lattice depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationParams {
    pub seed: u64,
    /// The ground area's extent, x by z, from the lattice's negative corner. The
    /// fill spans the full lattice depth, so a y extent does not bound it. A
    /// footprint with any negative extent is refused.
    pub footprint: IVec3,
}

impl GenerationParams {
    #[must_use]
    pub const fn new(seed: u64, footprint: IVec3) -> Self {
        Self { seed, footprint }
    }

    /// The full Lattice: one Generation parameter, the benchmark run.
    #[must_use]
    pub const fn full_lattice(seed: u64) -> Self {
        Self {
            seed,
            footprint: IVec3::splat(LATTICE_EXTENT.cast_signed()),
        }
    }
}

impl Default for GenerationParams {
    fn default() -> Self {
        Self::full_lattice(0)
    }
}

/// A finished Generation: the World, its Palette, and its Physical material
/// table. Its Snapshots are emitted from the World by the pipeline.
pub struct GeneratedWorld {
    pub world: World,
    pub palette: [glam::Vec4; 256],
    pub materials: PhysicalMaterialTable,
}

/// Builds a World from `params`, filling Micro-chunk by Micro-chunk and
/// reporting the Generate stage as it goes.
///
/// # Errors
///
/// Returns a reason when the footprint has a negative extent.
pub fn generate(progress: &Progress, params: GenerationParams) -> Result<GeneratedWorld, String> {
    if params.footprint.cmplt(IVec3::ZERO).any() {
        return Err(format!(
            "the footprint {} has a negative extent",
            params.footprint
        ));
    }

    let mut world = World::empty();

    fill(&mut world, progress, params);

    progress.end_stage(Stage::Generate);

    let vocabulary = Vocabulary::new();

    Ok(GeneratedWorld {
        world,
        palette: vocabulary.palette(),
        materials: vocabulary.materials(),
    })
}

/// The terrain-only fill: every column in the footprint is solid from Bedrock to
/// ground level, in one material. Deterministic and independent of any other
/// column; the surface shape and layering arrive with ticket 05.
fn fill(world: &mut World, progress: &Progress, params: GenerationParams) {
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let lower = IVec3::splat(half.saturating_neg());
    let upper = lower
        .saturating_add(params.footprint)
        .min(IVec3::splat(half));

    if upper.x <= lower.x || upper.z <= lower.z {
        return;
    }

    let total = fillable_chunks(lower, upper, params.seed);
    let mut filled = 0usize;
    let mut origin = lower;

    while origin.z < upper.z {
        origin.x = lower.x;

        while origin.x < upper.x {
            filled = filled.saturating_add(write_column_of_micro_chunks(
                world,
                origin,
                params.seed,
                upper,
            ));
            progress.count_generated(total, filled);

            origin.x = origin.x.saturating_add(MICRO_CHUNK_LENGTH.cast_signed());
        }

        origin.z = origin.z.saturating_add(MICRO_CHUNK_LENGTH.cast_signed());
    }
}

/// How many Micro-chunks the fill writes: one per chunk layer from Bedrock to
/// ground level, over the footprint's columns.
fn fillable_chunks(lower: IVec3, upper: IVec3, seed: u64) -> usize {
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let edge_span = edge.saturating_sub(1);
    let columns_x = u32::try_from(
        upper
            .x
            .saturating_sub(lower.x)
            .saturating_add(edge_span)
            .div_euclid(edge),
    )
    .unwrap_or(0);
    let columns_z = u32::try_from(
        upper
            .z
            .saturating_sub(lower.z)
            .saturating_add(edge_span)
            .div_euclid(edge),
    )
    .unwrap_or(0);
    let mut layers = 0u32;
    let mut y = half.saturating_neg();

    while y < half {
        if fill_column(seed, y).is_some() {
            layers = layers.saturating_add(1);
        }

        y = y.saturating_add(edge);
    }

    let columns = usize::try_from(columns_x.saturating_mul(columns_z)).unwrap_or(0);
    let layers = usize::try_from(layers).unwrap_or(0);

    columns.saturating_mul(layers)
}

/// Fills one column of Micro-chunks from the lattice's floor to its ceiling.
///
/// The fill is solid from Bedrock up to the surface, so a chunk whose floor is
/// above the surface holds nothing and is skipped whole. Returns how many
/// chunks it wrote, so the caller can report progress.
fn write_column_of_micro_chunks(
    world: &mut World,
    mut origin: IVec3,
    seed: u64,
    upper: IVec3,
) -> usize {
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let mut written = 0usize;

    origin.y = half.saturating_neg();

    while origin.y < half {
        if fill_column(seed, origin.y).is_some() {
            write_micro_chunk(world, origin, seed, upper);

            written = written.saturating_add(1);
        }

        origin.y = origin.y.saturating_add(MICRO_CHUNK_LENGTH.cast_signed());
    }

    written
}

/// Fills one Micro-chunk. Every column here is solid up to ground level, one
/// material, so the mask is contiguous from the chunk's floor and the materials
/// follow cell order.
fn write_micro_chunk(world: &mut World, origin: IVec3, seed: u64, upper: IVec3) {
    let mut mask = [0u8; MICRO_BYTES];
    let mut materials: Vec<u8> = Vec::new();

    for index in 0..512usize {
        let offset = cell_offset(index);
        let position = origin.saturating_add(offset);

        if position.x >= upper.x || position.z >= upper.z {
            continue;
        }

        if let Some(material) = fill_column(seed, position.y) {
            if let Some(byte) = mask.get_mut(index / 8) {
                *byte |= 1u8 << (index % 8);
            }

            materials.push(material.index());
        }
    }

    if let Err(error) = world.write_entry(origin, &mask, &materials) {
        panic!("the generator wrote an invalid entry at {origin}: {error}");
    }
}

/// The material at `level` for a column whose ground is at zero. `None` above
/// ground, so the fill stops there. Terrain-only: one material until ticket 05
/// gives each column its own height and depths their own layers.
const fn fill_column(_seed: u64, level: i32) -> Option<Material> {
    match level {
        -64..=0 => Some(Material::Stone),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::load::progress::Progress;

    fn generated(footprint: IVec3) -> GeneratedWorld {
        generate(
            &Progress::generate_path(),
            GenerationParams::new(0x5EED, footprint),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn the_same_seed_and_footprint_are_deterministic() {
        let a = generated(IVec3::splat(16));
        let b = generated(IVec3::splat(16));

        assert_eq!(
            a.world.iter_voxels().collect::<Vec<_>>(),
            b.world.iter_voxels().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_fill_is_solid_from_bedrock_to_ground_level() {
        let world = generated(IVec3::splat(8)).world;

        for level in -64..=0 {
            assert_eq!(
                world.get_voxel(&IVec3::new(-2048, level, -2048)),
                Some(Material::Stone.index()),
                "level {level} is filled"
            );
        }

        assert!(world.get_voxel(&IVec3::new(-2048, 1, -2048)).is_none());
        assert!(world.get_voxel(&IVec3::new(-2048, -65, -2048)).is_none());
    }

    #[test]
    fn nothing_is_generated_outside_the_footprint() {
        let world = generated(IVec3::splat(8)).world;

        assert!(world.get_voxel(&IVec3::new(-2048 + 8, 0, -2048)).is_none());
        assert!(world.get_voxel(&IVec3::new(-2048, 0, -2048 + 8)).is_none());
        assert!(
            world
                .get_voxel(&IVec3::new(-2048 + 7, 0, -2048 + 7))
                .is_some()
        );
    }

    #[test]
    fn a_negative_footprint_is_refused() {
        let refused = generate(
            &Progress::generate_path(),
            GenerationParams::new(0x5EED, IVec3::new(0, -1, 0)),
        );

        assert!(refused.is_err());
    }

    #[test]
    fn progress_advances_during_the_generate_stage() {
        let progress = Progress::generate_path();
        let _ = generate(&progress, GenerationParams::new(0x5EED, IVec3::splat(64)))
            .unwrap_or_else(|error| panic!("{error}"));

        // The generate stage ends at its share, and progress never went
        // backwards while the fill ran.
        assert!(progress.load() >= 0.795 - 1e-6);
    }

    #[test]
    fn a_generation_reads_no_cell_budget() {
        let _budget = crate::world::budget::set_cell_budget(0);

        let generated = generated(IVec3::splat(8));

        assert!(
            generated.world.voxel_count() > 0,
            "a Generation past the cell budget still succeeds"
        );
    }
}
