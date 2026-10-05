//! Turns Generation params into a World.
//!
//! The generator fills one Micro-chunk at a time on one thread. The World it
//! builds is the single source of truth: the emit step derives the Snapshots
//! from it, and the Palette and Physical material table come from the
//! Vocabulary.
//!
//! The ground is a quantized height field: one surface level per column, a pure
//! function of the Seed and the column, so no column depends on another. The
//! noise behind it is fixed-point gradient noise keyed by the Seed, the Terrain
//! tag, the octave index and the corner, so no floating point enters.

use glam::IVec3;

use super::{
    World,
    grid::{LATTICE_EXTENT, LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH},
    load::progress::{Progress, Stage},
    material::{PhysicalMaterialTable, Rule},
    vocabulary::{Feature, Material, Vocabulary},
};

use super::diff::edit::{MICRO_BYTES, cell_offset};

/// The surface range's lower edge. A column's surface never dips below this, so
/// every filled column has solid ground under it.
const SURFACE_FLOOR: i32 = -32;

/// The surface range's upper edge.
const SURFACE_CEILING: i32 = 32;

/// The level the spawn and camera conventions call ground. The surface straddles
/// it, so generated ground sits at ground level on average.
const GROUND_LEVEL: i32 = 0;

/// The lowest level the fill writes.
const BEDROCK: i32 = -64;

/// How deep the surface material reaches below a column's surface, in levels.
const SURFACE_DEPTH: i32 = 1;

/// How deep the soil layer reaches below the surface material, in levels.
const SOIL_DEPTH: i32 = 4;

/// How many octaves the height noise sums.
const OCTAVES: u32 = 4;

/// The first octave's lattice spacing in voxels. Each later octave halves it, so
/// the octaves run 128, 64, 32 and 16 voxels.
const BASE_SPACING: i32 = 128;

/// The bit count of the fixed-point fraction. Every noise value is an integer
/// count of `1 << FADE_BITS` per unit, and every shift of one is exact.
const FADE_BITS: u32 = 16;

/// One, in Q16.
const ONE: i64 = 1 << FADE_BITS;

/// `1/sqrt(2)` in Q16, rounded. It brings the diagonal gradients to the length
/// of the axis ones, which an unscaled `{-1, 0, 1}` set does not.
const DIAGONAL: i64 = 46_341;

/// The levels one unit of gradient noise is worth. This is the scale that puts
/// the surface's standard deviation near ten levels while the clamped share
/// stays far below one percent; the ignored
/// `the_surface_distribution_chooses_the_height_scale` measures it.
const HEIGHT_SCALE: i64 = 25;

/// The eight unit directions a lattice corner's gradient is drawn from, in Q16.
const GRADIENTS: [[i64; 2]; 8] = [
    [ONE, 0],
    [-ONE, 0],
    [0, ONE],
    [0, -ONE],
    [DIAGONAL, DIAGONAL],
    [-DIAGONAL, DIAGONAL],
    [DIAGONAL, -DIAGONAL],
    [-DIAGONAL, -DIAGONAL],
];

/// The bits of a corner hash that index [`GRADIENTS`]: one less than its length,
/// which is a power of two.
const GRADIENT_MASK: u64 = GRADIENTS.len() as u64 - 1;

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
/// table, plus the Falling granular cells it wrote.
pub struct GeneratedWorld {
    pub world: World,
    pub palette: [glam::Vec4; 256],
    pub materials: PhysicalMaterialTable,
    pub granular_cells: Vec<IVec3>,
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
    let mut granular_cells = Vec::new();

    fill(&mut world, progress, params, &mut granular_cells);

    progress.end_stage(Stage::Generate);

    let vocabulary = Vocabulary::new();

    Ok(GeneratedWorld {
        world,
        palette: vocabulary.palette(),
        materials: vocabulary.materials(),
        granular_cells,
    })
}

/// The terrain fill: every column in the footprint is solid from Bedrock to
/// its own surface level, with materials chosen by depth below that surface.
/// Pure and column-independent: a column's surface is the Seed's gradient noise
/// at that column, so no floating point enters and no column depends on another.
fn fill(
    world: &mut World,
    progress: &Progress,
    params: GenerationParams,
    granular_cells: &mut Vec<IVec3>,
) {
    let half = LATTICE_HALF_EXTENT.cast_signed();
    let lower = IVec3::splat(half.saturating_neg());
    let upper = lower
        .saturating_add(params.footprint)
        .min(IVec3::splat(half));

    if upper.x <= lower.x || upper.z <= lower.z {
        return;
    }

    let total = fillable_chunks(lower, upper);
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
                granular_cells,
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
    seed: u64,
    upper: IVec3,
    granular_cells: &mut Vec<IVec3>,
) -> usize {
    let mut written = 0usize;

    origin.y = BEDROCK;

    let surfaces = column_surfaces(origin, seed);

    while origin.y <= SURFACE_CEILING {
        if !chunk_floor_over_surface(&surfaces, origin.y) {
            write_micro_chunk(world, origin, upper, &surfaces, granular_cells);

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
    granular_cells: &mut Vec<IVec3>,
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

            if material.physical().rule == Rule::FallingGranular {
                granular_cells.push(position);
            }
        }
    }

    if let Err(error) = world.write_entry(origin, &mask, &materials) {
        panic!("the generator wrote an invalid entry at {origin}: {error}");
    }
}

/// The 8x8 column surfaces of a chunk, in the `lx + 8*lz` slot order
/// [`column_slot`] reads, so a chunk runs the height function once per column
/// instead of once per cell.
fn column_surfaces(origin: IVec3, seed: u64) -> [i32; 64] {
    let edge = MICRO_CHUNK_LENGTH.cast_signed();
    let mut surfaces = [SURFACE_FLOOR; 64];

    for lz in 0..edge {
        for lx in 0..edge {
            let slot = column_slot(IVec3::new(lx, 0, lz));

            if let Some(entry) = surfaces.get_mut(slot) {
                *entry = surface_level(
                    seed,
                    origin.x.saturating_add(lx),
                    origin.z.saturating_add(lz),
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
/// `SURFACE_CEILING`. A pure function of the Seed and the column: four octaves
/// of gradient noise, each one half as tall and twice as fine as the one before,
/// summed and scaled to levels.
///
/// An octave's spacing is a power of two, so `div_euclid` is a plain arithmetic
/// shift and `rem_euclid` is a mask, and the offset inside a cell stays positive.
/// That matters for negative x and z, which fall in the cell below zero rather
/// than wrapping into the one above it.
fn surface_level(seed: u64, x: i32, z: i32) -> i32 {
    let tag = Feature::Terrain.tag();
    let mut value = 0i64;

    for octave in 0..OCTAVES {
        let spacing = BASE_SPACING >> octave;
        let offset_shift = FADE_BITS.saturating_sub(spacing.trailing_zeros());
        let cell_x = x.div_euclid(spacing);
        let cell_z = z.div_euclid(spacing);
        let fx = i64::from(x.rem_euclid(spacing)) << offset_shift;
        let fz = i64::from(z.rem_euclid(spacing)) << offset_shift;

        let corners = [
            (cell_x, cell_z, fx, fz),
            (cell_x.saturating_add(1), cell_z, fx.saturating_sub(ONE), fz),
            (cell_x, cell_z.saturating_add(1), fx, fz.saturating_sub(ONE)),
            (
                cell_x.saturating_add(1),
                cell_z.saturating_add(1),
                fx.saturating_sub(ONE),
                fz.saturating_sub(ONE),
            ),
        ];
        let [n00, n10, n01, n11] = corners.map(|(cx, cz, ox, oz)| {
            let [gx, gz] = gradient(seed, tag, octave, cx, cz);

            dot(gx, gz, ox, oz)
        });

        let u = fade(fx);
        let v = fade(fz);
        let octave_value = lerp(lerp(n00, n10, u), lerp(n01, n11, u), v);

        value = value.wrapping_add(octave_value >> octave);
    }

    let level = i32::try_from(value.wrapping_mul(HEIGHT_SCALE) >> FADE_BITS).unwrap_or(0);

    level.clamp(SURFACE_FLOOR, SURFACE_CEILING)
}

/// The gradient of one lattice corner in one octave, drawn from [`GRADIENTS`].
///
/// The corner's coordinates are hashed explicitly rather than reached by an
/// offset from a neighbouring corner, so every column that reads the corner
/// agrees on its gradient. Folding the octave index in decorrelates the octaves,
/// which is why there is no per-octave offset table.
fn gradient(seed: u64, tag: u64, octave: u32, cell_x: i32, cell_z: i32) -> [i64; 2] {
    let mut hash = seed;

    for value in [
        tag,
        u64::from(octave),
        u64::from(cell_x.cast_unsigned()),
        u64::from(cell_z.cast_unsigned()),
    ] {
        hash = splitmix64(hash ^ value.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }

    GRADIENTS
        .get(usize::try_from(hash & GRADIENT_MASK).unwrap_or(0))
        .copied()
        .unwrap_or([0, 0])
}

/// The Q16 dot product of a gradient with a corner-relative offset.
const fn dot(gx: i64, gz: i64, fx: i64, fz: i64) -> i64 {
    gx.wrapping_mul(fx).wrapping_add(gz.wrapping_mul(fz)) >> FADE_BITS
}

/// `6t^5 - 15t^4 + 10t^3` in Q16: the curve that flattens the noise at a cell
/// edge, so the noise's slope is continuous across it.
const fn fade(t: i64) -> i64 {
    let t3 = t.wrapping_mul(t) >> FADE_BITS;
    let t4 = t3.wrapping_mul(t) >> FADE_BITS;
    let t5 = t4.wrapping_mul(t) >> FADE_BITS;

    (6i64.wrapping_mul(t5))
        .wrapping_sub(15i64.wrapping_mul(t4))
        .wrapping_add(10i64.wrapping_mul(t3))
}

/// `a` moved toward `b` by `w` in Q16, rounding half up.
const fn lerp(a: i64, b: i64, w: i64) -> i64 {
    a.wrapping_add(
        b.wrapping_sub(a)
            .wrapping_mul(w)
            .wrapping_add(1i64 << (FADE_BITS - 1))
            >> FADE_BITS,
    )
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

    const SEED: u64 = 0x5EED;

    fn generated(footprint: IVec3) -> GeneratedWorld {
        generate(
            &Progress::generate_path(),
            GenerationParams::new(SEED, footprint),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn the_granular_cells_are_the_worlds_falling_granular_cells() {
        let generated = generated(IVec3::splat(64));

        let mut expected: Vec<IVec3> = generated
            .world
            .iter_voxels()
            .filter(|(_, voxel)| generated.materials.get(*voxel).rule == Rule::FallingGranular)
            .map(|(position, _)| position)
            .collect();

        expected.sort_unstable_by_key(|cell| cell.to_array());

        let mut actual = generated.granular_cells;
        actual.sort_unstable_by_key(|cell| cell.to_array());

        assert!(
            !expected.is_empty(),
            "the surface dips below ground level somewhere"
        );
        assert_eq!(actual, expected, "the list is exactly the granular cells");
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
        let column = IVec3::new(-2048, 0, -2048);
        let surface = surface_level(SEED, column.x, column.z);

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
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();

        for x in start..start + 64 {
            for z in start..start + 64 {
                let surface = surface_level(SEED, x, z);

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
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();
        let column = IVec3::new(start, 0, start);
        let surface = surface_level(SEED, column.x, column.z);
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
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();
        let surface = surface_level(SEED, start + 7, start + 7);

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

    /// The fill advances through the Generate span without reaching its end:
    /// `fillable_chunks` counts every layer from Bedrock to the ceiling, and the
    /// fill skips the layers above a chunk column's own surface, so it reports
    /// 0.634 at this footprint where white noise reported about 0.795. Only the
    /// stage's own end reaches the endpoint.
    #[test]
    fn progress_advances_during_the_generate_stage() {
        let progress = Progress::generate_path();
        let mut world = World::empty();
        let mut granular_cells = Vec::new();

        fill(
            &mut world,
            &progress,
            GenerationParams::new(0x5EED, IVec3::splat(64)),
            &mut granular_cells,
        );

        let filled = progress.load();

        assert!(
            filled >= 0.62,
            "the fill advances through the Generate span, reaching {filled}"
        );
        assert!(world.voxel_count() > 0, "the fill wrote a World");

        progress.end_stage(Stage::Generate);

        assert!(
            (progress.load() - 0.829).abs() < 1e-6,
            "the Generate stage ends at its measured share"
        );
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

    /// The measurement behind [`HEIGHT_SCALE`]: the surface's distribution over a
    /// 512-edge footprint, printing the range, the standard deviation, the share
    /// of columns clamped at each end, the mean adjacent step and the share of
    /// columns whose surface is below ground level. It prints rather than
    /// asserts, because it chooses a constant once.
    #[test]
    #[ignore = "measure: cargo test -p atlas-rt --release --lib the_surface_distribution -- --ignored --nocapture"]
    fn the_surface_distribution_chooses_the_height_scale() {
        let edge = 512i32;
        let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();
        let mut min = i32::MAX;
        let mut max = i32::MIN;
        let mut sum = 0i64;
        let mut squares = 0i64;
        let mut at_floor = 0usize;
        let mut at_ceiling = 0usize;
        let mut below_ground = 0usize;
        let mut steps = 0i64;
        let mut step_pairs = 0usize;
        let mut previous: Vec<i32> = Vec::new();

        for lz in 0..edge {
            let row: Vec<i32> = (0..edge)
                .map(|lx| surface_level(SEED, start.saturating_add(lx), start.saturating_add(lz)))
                .collect();

            for surface in &row {
                min = min.min(*surface);
                max = max.max(*surface);
                sum = sum.saturating_add(i64::from(*surface));
                squares =
                    squares.saturating_add(i64::from(*surface).wrapping_mul(i64::from(*surface)));
                at_floor = at_floor.saturating_add(usize::from(*surface == SURFACE_FLOOR));
                at_ceiling = at_ceiling.saturating_add(usize::from(*surface == SURFACE_CEILING));
                below_ground = below_ground.saturating_add(usize::from(*surface < GROUND_LEVEL));
            }

            for (left, right) in row.iter().zip(row.iter().skip(1)) {
                steps = steps.saturating_add(i64::from(left.abs_diff(*right)));
                step_pairs = step_pairs.saturating_add(1);
            }

            for (here, above) in row.iter().zip(previous.iter()) {
                steps = steps.saturating_add(i64::from(here.abs_diff(*above)));
                step_pairs = step_pairs.saturating_add(1);
            }

            previous = row;
        }

        let columns = f64::from(edge) * f64::from(edge);
        let mean = sum as f64 / columns;
        let deviation = mean.mul_add(-mean, squares as f64 / columns).max(0.0).sqrt();
        let share = |count: usize| 100.0 * count as f64 / columns;

        println!("height scale    {HEIGHT_SCALE}");
        println!("columns         {}", edge.saturating_mul(edge));
        println!("min             {min}");
        println!("max             {max}");
        println!("mean            {mean:.3}");
        println!("std dev         {deviation:.3}");
        println!("at floor        {at_floor} ({:.3}%)", share(at_floor));
        println!("at ceiling      {at_ceiling} ({:.3}%)", share(at_ceiling));
        println!(
            "mean step       {:.3}",
            steps as f64 / step_pairs.max(1) as f64
        );
        println!(
            "below ground    {below_ground} ({:.1}%)",
            share(below_ground)
        );
    }
}
