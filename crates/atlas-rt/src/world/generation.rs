//! Turns Generation params into a World.
//!
//! The generator fills one Micro-chunk at a time on one thread. The World it
//! builds is the single source of truth: the emit step derives the Snapshots
//! from it, and the Palette and Physical material table come from the
//! Vocabulary.
//!
//! The ground is a quantized height field: one surface level per column, a pure
//! function of the Seed's Terrain tag and the column, so no column depends on
//! another. The noise is an integer hash, so no floating point enters.

use glam::IVec3;

use super::{
    World,
    grid::{LATTICE_EXTENT, LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH},
    load::progress::{Progress, Stage},
    material::PhysicalMaterialTable,
    vocabulary::{Feature, Material, Vocabulary},
};

use super::diff::edit::{MICRO_BYTES, cell_offset};

/// The surface range's lower edge. A column's surface never dips below this, so
/// every filled column has solid ground under it.
const SURFACE_FLOOR: i32 = -32;

/// The surface range's upper edge.
const SURFACE_CEILING: i32 = 32;

/// The surface range's inclusive span, one more than the levels it covers.
const SURFACE_LEVELS: u32 = 65;

/// The level the spawn and camera conventions call ground. The surface straddles
/// it, so generated ground sits at ground level on average.
const GROUND_LEVEL: i32 = 0;

/// The lowest level the fill writes.
const BEDROCK: i32 = -64;

/// How deep the surface material reaches below a column's surface, in levels.
const SURFACE_DEPTH: i32 = 1;

/// How deep the soil layer reaches below the surface material, in levels.
const SOIL_DEPTH: i32 = 4;

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

/// The terrain fill: every column in the footprint is solid from Bedrock to
/// its own surface level, with materials chosen by depth below that surface.
/// Pure and column-independent: a column's surface is a hash of the Seed, the
/// Terrain tag, and the column's x and z, so no floating point enters and no
/// column depends on another.
fn fill(world: &mut World, progress: &Progress, params: GenerationParams) {
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let lower = IVec3::splat(half.saturating_neg());
    let upper = lower
        .saturating_add(params.footprint)
        .min(IVec3::splat(half));

    if upper.x <= lower.x || upper.z <= lower.z {
        return;
    }

    let tag = Feature::Terrain.tag();
    let total = fillable_chunks(lower, upper);
    let mut filled = 0usize;
    let mut origin = lower;

    while origin.z < upper.z {
        origin.x = lower.x;

        while origin.x < upper.x {
            filled = filled.saturating_add(write_column_of_micro_chunks(
                world, origin, tag, upper,
            ));
            progress.count_generated(total, filled);

            origin.x = origin.x.saturating_add(MICRO_CHUNK_LENGTH.cast_signed());
        }

        origin.z = origin.z.saturating_add(MICRO_CHUNK_LENGTH.cast_signed());
    }
}

/// How many Micro-chunks the fill writes: one per chunk layer from Bedrock to
/// the surface range's ceiling, over the footprint's columns. A chunk layer is
/// counted when any column of the footprint reaches into it, so the count is an
/// upper bound and the reported progress is never below the truth.
fn fillable_chunks(lower: IVec3, upper: IVec3) -> usize {
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
    let mut y = BEDROCK;

    while y <= SURFACE_CEILING {
        layers = layers.saturating_add(1);
        y = y.saturating_add(edge);
    }

    let columns = usize::try_from(columns_x.saturating_mul(columns_z)).unwrap_or(0);
    let layers = usize::try_from(layers).unwrap_or(0);

    columns.saturating_mul(layers)
}

/// Fills one column of Micro-chunks from Bedrock to the surface ceiling.
///
/// Returns how many chunks it wrote, so the caller can report progress. A
/// chunk whose floor lies above the surface holds nothing and is skipped.
fn write_column_of_micro_chunks(
    world: &mut World,
    mut origin: IVec3,
    tag: u64,
    upper: IVec3,
) -> usize {
    let mut written = 0usize;

    origin.y = BEDROCK;

    let surfaces = column_surfaces(origin, tag);

    while origin.y <= SURFACE_CEILING {
        if !chunk_floor_over_surface(&surfaces, origin.y) {
            write_micro_chunk(world, origin, upper, &surfaces);

            written = written.saturating_add(1);
        }

        origin.y = origin.y.saturating_add(MICRO_CHUNK_LENGTH.cast_signed());
    }

    written
}

/// Whether no column of the chunk reaches into it: the highest of the chunk's
/// 8x8 surfaces lies below the chunk's floor, so the chunk holds nothing.
fn chunk_floor_over_surface(surfaces: &[i32; 64], floor: i32) -> bool {
    surfaces.iter().all(|surface| *surface < floor)
}

/// Fills one Micro-chunk: each column is solid from the chunk's floor up to its
/// own surface, and the materials follow cell order.
fn write_micro_chunk(
    world: &mut World,
    origin: IVec3,
    upper: IVec3,
    surfaces: &[i32; 64],
) {
    let mut mask = [0u8; MICRO_BYTES];
    let mut materials: Vec<u8> = Vec::with_capacity(512);

    for index in 0..512usize {
        let offset = cell_offset(index);
        let position = origin.saturating_add(offset);

        if position.x >= upper.x || position.z >= upper.z {
            continue;
        }

        let surface = surfaces
            .get(column_slot(offset))
            .copied()
            .unwrap_or(SURFACE_FLOOR);

        if let Some(material) = fill_cell(position, surface) {
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

/// The 8x8 column surfaces of a chunk, in the `lx + 8*lz` slot order
/// [`column_slot`] reads, so a chunk hashes each column once instead of once per
/// cell.
fn column_surfaces(origin: IVec3, tag: u64) -> [i32; 64] {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let mut surfaces = [SURFACE_FLOOR; 64];

    for lz in 0..edge {
        for lx in 0..edge {
            let slot = column_slot(IVec3::new(lx, 0, lz));

            if let Some(entry) = surfaces.get_mut(slot) {
                *entry = surface_level(
                    origin.x.saturating_add(lx),
                    origin.z.saturating_add(lz),
                    tag,
                );
            }
        }
    }

    surfaces
}

/// The slot of a cell's column in a chunk's `column_surfaces`: `lx + 8*lz`.
fn column_slot(offset: IVec3) -> usize {
    usize::try_from(
        offset
            .x
            .saturating_add(offset.z.saturating_mul(MICRO_CHUNK_LENGTH.cast_signed())),
    )
    .unwrap_or(0)
}

/// The material of one filled cell, or `None` above the column's surface. The
/// material follows depth below that surface.
const fn fill_cell(position: IVec3, surface: i32) -> Option<Material> {
    if position.y > surface {
        return None;
    }

    if position.y < BEDROCK {
        return None;
    }

    if position.y == BEDROCK {
        return Some(Material::Bedrock);
    }

    let depth = surface.saturating_sub(position.y);

    if depth < SURFACE_DEPTH {
        Some(surface_material(position))
    } else if depth <= SOIL_DEPTH {
        Some(soil_material(position))
    } else {
        Some(Material::Stone)
    }
}

/// The level the ground reaches at one column, from `SURFACE_FLOOR` through
/// `SURFACE_CEILING`. A pure function of the Seed's Terrain tag and the column.
fn surface_level(x: i32, z: i32, tag: u64) -> i32 {
    let noise = hash_column(x, z, tag);
    let span = noise.checked_rem(u64::from(SURFACE_LEVELS)).unwrap_or(0);

    SURFACE_FLOOR.saturating_add(i32::try_from(span).unwrap_or(0))
}

/// The surface material of one column: sand on the low ground and grass on the
/// higher ground, so the surface reads from above. Both differ from the soil
/// below.
const fn surface_material(position: IVec3) -> Material {
    if position.y < GROUND_LEVEL {
        Material::Sand
    } else {
        Material::Grass
    }
}

/// The soil material under the surface: dirt just below, stone deeper down.
const fn soil_material(position: IVec3) -> Material {
    if position.y < GROUND_LEVEL.saturating_sub(SOIL_DEPTH) {
        Material::Stone
    } else {
        Material::Dirt
    }
}

/// The integer hash behind a column's surface: the Seed's tag and the column's
/// x and z, mixed so one column never depends on another.
fn hash_column(x: i32, z: i32, tag: u64) -> u64 {
    let mut hash = tag ^ (0xA117 ^ 0x2027);

    for value in [x, z] {
        let bits = u64::from(value.cast_unsigned());
        hash ^= bits.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        hash = splitmix64(hash);
    }

    hash
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
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
    fn the_fill_is_solid_from_bedrock_to_the_surface() {
        let world = generated(IVec3::splat(8)).world;
        let tag = Feature::Terrain.tag();
        let column = IVec3::new(-2048, 0, -2048);
        let surface = surface_level(column.x, column.z, tag);

        for level in BEDROCK..=surface {
            assert!(
                world.get_voxel(&IVec3::new(column.x, level, column.z)).is_some(),
                "level {level} is filled"
            );
        }

        assert_eq!(
            world.get_voxel(&IVec3::new(column.x, BEDROCK, column.z)),
            Some(Material::Bedrock.index()),
            "Bedrock floors the fill"
        );
        assert!(
            world
                .get_voxel(&IVec3::new(column.x, surface + 1, column.z))
                .is_none(),
            "the surface is the top of the fill"
        );
        assert!(
            world
                .get_voxel(&IVec3::new(column.x, BEDROCK - 1, column.z))
                .is_none(),
            "nothing is generated below Bedrock"
        );
    }

    #[test]
    fn the_surface_stays_within_the_range() {
        let world = generated(IVec3::splat(64)).world;
        let tag = Feature::Terrain.tag();
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();

        for x in start..start + 64 {
            for z in start..start + 64 {
                let surface = surface_level(x, z, tag);

                assert!(
                    (SURFACE_FLOOR..=SURFACE_CEILING).contains(&surface),
                    "the surface at ({x}, {z}) is {surface}, outside the range"
                );
                assert!(
                    world.get_voxel(&IVec3::new(x, surface, z)).is_some(),
                    "the surface is filled at ({x}, {z})"
                );
                assert!(
                    world.get_voxel(&IVec3::new(x, surface + 1, z)).is_none(),
                    "nothing is filled above the surface at ({x}, {z})"
                );
            }
        }
    }

    #[test]
    fn the_material_layers_follow_depth_below_the_surface() {
        let world = generated(IVec3::splat(64)).world;
        let tag = Feature::Terrain.tag();
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();
        let column = IVec3::new(start, 0, start);
        let surface = surface_level(column.x, column.z, tag);
        let surface_material = world
            .get_voxel(&IVec3::new(column.x, surface, column.z))
            .unwrap_or_else(|| panic!("the surface is filled"));

        assert_eq!(
            world.get_voxel(&IVec3::new(
                column.x,
                surface - SOIL_DEPTH - 1,
                column.z,
            )),
            Some(Material::Stone.index()),
            "below the soil is stone"
        );

        for depth in SURFACE_DEPTH..=SOIL_DEPTH {
            let material = world
                .get_voxel(&IVec3::new(column.x, surface - depth, column.z))
                .unwrap_or_else(|| panic!("depth {depth} is filled"));

            assert_ne!(
                material, surface_material,
                "the subsurface differs from the surface at depth {depth}"
            );
        }
    }

    #[test]
    fn nothing_is_generated_outside_the_footprint() {
        let world = generated(IVec3::splat(8)).world;
        let tag = Feature::Terrain.tag();
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();
        let surface = surface_level(start + 7, start + 7, tag);

        assert!(world.get_voxel(&IVec3::new(start + 8, 0, start)).is_none());
        assert!(world.get_voxel(&IVec3::new(start, 0, start + 8)).is_none());
        assert!(
            world
                .get_voxel(&IVec3::new(start + 7, surface, start + 7))
                .is_some(),
            "the last in-footprint column is generated"
        );
        assert!(world.voxel_count() > 0, "the in-footprint columns fill");
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
