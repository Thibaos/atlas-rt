//! A Generation, driven through the world job seam from outside the crate.
//!
//! A test asks for a Generation with a Seed and a footprint, polls to
//! completion, and takes the World, Snapshots, Palette and Physical material
//! table it delivers. The job is the same one a load runs on, so a Generation
//! and a load are interchangeable to the host.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use atlas_rt::sim::{self, Activation, ParityPolicy, PlayerProfile};
use atlas_rt::world::{
    World,
    diff::batch::TrackedCoords,
    diff::snapshot::{MicroChunkSnapshot, emit_snapshots},
    generation::GenerationParams,
    grid::LATTICE_HALF_EXTENT,
    load::job::{Finished, LoadedWorld, Refusal, Status, WorldUpdateJob},
    material::{PhysicalMaterial, Rule},
    vocabulary::{Material, Vocabulary},
};
use glam::{IVec3, Vec4};

mod common;

/// A small footprint, so the run stays within the poll deadline.
const FOOTPRINT: IVec3 = IVec3::splat(64);
const SEED: u64 = 0x5EED_1234;

/// The activation test's own Seed. Its spawn block, the six by six columns under
/// the collider, has to reach above ground level, where the surface cell is
/// Grass and blocks the player. Below ground level the surface cell is Sand,
/// which the granular queue leaves non-blocking at activation, so the readiness
/// pose would sit one level under the surface until the first tick. About half
/// of all Seeds put that block below ground level.
const WALKABLE_SEED: u64 = 0x5EED_1238;

/// Runs one Generation through the real job and hands back the work it produces
/// with the status it settles at.
fn generate(params: GenerationParams) -> (LoadedWorld, Status) {
    let mut job = WorldUpdateJob::new();

    job.generate(params, 0)
        .unwrap_or_else(|refusal| panic!("the generation was refused: {refusal:?}"));

    let deadline = Instant::now() + Duration::from_secs(15);

    let finished = loop {
        if let Some(finished) = job.poll() {
            break finished;
        }

        assert!(
            Instant::now() < deadline,
            "the generation did not settle within fifteen seconds"
        );

        std::thread::sleep(Duration::from_millis(1));
    };

    assert_eq!(finished, Finished::Loaded, "the generation succeeds");

    let loaded = job
        .take_loaded()
        .unwrap_or_else(|| panic!("the generation must hand over its work"));
    job.arrive();

    (loaded, job.status())
}

const fn small() -> GenerationParams {
    GenerationParams::new(SEED, FOOTPRINT)
}

/// The largest difference in levels two adjacent columns may carry. Over a
/// 64-edge footprint the worst adjacent pair is 2 levels against a mean of 0.351,
/// and a sweep of 24 seeds at footprints 16 and 64 reached no pair above 2. The
/// per-column white noise this replaced ran at a mean of 21.7 and opened with a
/// 16-level pair at the footprint's corner.
const COHERENT_STEP_BOUND: u32 = 2;

/// The footprint the two-Seed and coherence tests run at, in place of the pinned
/// fingerprint's small one.
const SHAPE_FOOTPRINT: i32 = 64;

/// A Seed other than [`SEED`]. Against the pre-01 height function, seeds 0 and
/// 0xDEAD_BEEF built byte-identical Worlds at footprint 16.
const DIFFERENT_SEED: u64 = 0xDEAD_BEEF;

/// The footprint the pinned fingerprint is taken at. It is small because the
/// fingerprint has to be a fixed constant, not because the promise is smaller
/// there.
const PINNED_FOOTPRINT: i32 = 16;

/// The FNV-1a fingerprint of the Snapshots one Seed emits at one footprint,
/// checked in beside the test that reads it. It is the guard for "the same Seed
/// survives a rebuild": the Snapshots are the delivery contract the renderer
/// consumes, and their emitted order is defined, so this constant fixes the
/// World a Seed builds. Rewrite it only when the terrain function changes on
/// purpose, never to make a red test green.
const PINNED_SNAPSHOT_FINGERPRINT: u64 = 0xfa15_592e_2a94_3fbd;

/// The two column offsets that make an adjacent pair of columns on the xz plane.
const ADJACENT_COLUMNS: [(i32, i32); 2] = [(1, 0), (0, 1)];

/// Runs one Generation for one Seed and footprint through the real job, asserting
/// it succeeds.
fn generate_seeded(seed: u64, footprint: i32) -> LoadedWorld {
    let params = GenerationParams::new(seed, IVec3::splat(footprint));
    let (loaded, status) = generate(params);

    assert_eq!(
        status,
        Status::Ready,
        "the Generation for Seed {seed:#x} at footprint {footprint} must succeed"
    );

    loaded
}

/// The level of the highest filled cell at one column: the surface the fill
/// wrote, read back from the World.
fn surface_at(world: &World, x: i32, z: i32) -> i32 {
    (-64..=32)
        .rev()
        .find(|level| world.contains(&IVec3::new(x, *level, z)))
        .unwrap_or_else(|| panic!("({x}, {z}) has no floor"))
}

/// An FNV-1a fingerprint of a Generation's Snapshots, in the order they were
/// emitted: the octets of every Snapshot's coordinates, then every Snapshot's
/// occupancy mask and material list.
///
/// The Snapshots carry a defined order, which is why the fingerprint is taken
/// over them rather than over the World's voxel walk: the store's iteration
/// order is not part of any contract.
fn snapshot_fingerprint(snapshots: &[MicroChunkSnapshot]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;

    let octets = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.global_coords.to_array())
        .flat_map(i32::to_le_bytes)
        .chain(snapshots.iter().flat_map(|snapshot| {
            snapshot
                .mask
                .iter()
                .copied()
                .chain(snapshot.materials.iter().copied())
        }));

    for octet in octets {
        hash ^= u64::from(octet);
        hash = hash.wrapping_mul(PRIME);
    }

    hash
}

#[test]
fn a_generation_delivers_a_world_snapshots_palette_and_material_table() {
    let (loaded, status) = generate(small());

    assert_eq!(status, Status::Ready);
    assert!(loaded.world.voxel_count() > 0, "the terrain fills");

    let occupied: usize = loaded
        .snapshots
        .iter()
        .map(MicroChunkSnapshot::occupied_count)
        .sum();

    assert_eq!(
        loaded.world.voxel_count(),
        occupied,
        "every voxel reaches the renderer"
    );
    assert_eq!(
        emit_snapshots(&loaded.world).unwrap(),
        loaded.snapshots,
        "the snapshots are emission of the world they arrive with"
    );
    assert!(
        loaded.snapshots.iter().all(|snapshot| {
            let half = LATTICE_HALF_EXTENT.cast_signed();
            let extent = IVec3::splat(half);
            snapshot.global_coords.cmplt(extent).all()
                && snapshot
                    .global_coords
                    .cmpge(extent.saturating_mul(IVec3::splat(-1)))
                    .all()
        }),
        "every snapshot sits inside the lattice"
    );

    let vocabulary = Vocabulary::new();

    assert_eq!(
        loaded.materials,
        vocabulary.materials(),
        "the Physical material table comes from the Vocabulary"
    );
    assert_eq!(
        loaded.palette,
        vocabulary.palette(),
        "the Palette comes from the Vocabulary"
    );
    assert!(
        loaded.granular_cells.is_some(),
        "a Generation hands its Falling granular cells on"
    );
}

#[test]
fn the_generated_ground_is_walkable() {
    let (loaded, _) = generate(small());

    // Every column in the footprint is solid from Bedrock to its own surface,
    // so spawn placement finds a floor wherever it lands. The surface is read
    // from the world: the highest filled level at the column.
    for x in -2048..-2048 + 64 {
        for z in -2048..-2048 + 64 {
            let surface = (-64..=32)
                .rev()
                .find(|level| loaded.world.contains(&IVec3::new(x, *level, z)))
                .unwrap_or_else(|| panic!("({x}, {z}) has no floor"));

            assert!(
                (-32..=32).contains(&surface),
                "the surface at ({x}, {z}) is {surface}, outside the range"
            );
            assert!(
                loaded.world.contains(&IVec3::new(x, -64, z)),
                "Bedrock floors the column at ({x}, {z})"
            );
            assert!(
                !loaded.world.contains(&IVec3::new(x, surface + 1, z)),
                "nothing sits above the surface at ({x}, {z})"
            );
        }
    }
}

#[test]
fn a_generated_world_activates_and_the_player_stands_on_its_surface() {
    let (loaded, _) = generate(GenerationParams::new(WALKABLE_SEED, FOOTPRINT));

    let tracked: TrackedCoords = loaded
        .snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();

    let world = Arc::new(RwLock::new(World::default()));
    let handle = sim::spawn(
        Arc::clone(&world),
        PlayerProfile::default(),
        ParityPolicy::default(),
    )
    .unwrap_or_else(|error| panic!("the simulation must spawn: {error}"));

    handle.activate(Activation {
        world: loaded.world,
        snapshots: loaded.snapshots,
        tracked,
        materials: loaded.materials,
        granular_cells: loaded.granular_cells,
    });

    let player = common::wait_ready(&handle);

    assert!(player.grounded, "the player stands on generated ground");

    let profile = PlayerProfile::default();
    let guard = world
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let start_x = (player.feet.x - profile.width * 0.5 + 1.0e-3).floor() as i32;
    let end_x = (player.feet.x + profile.width * 0.5 - 1.0e-3).floor() as i32;
    let start_z = (player.feet.z - profile.depth * 0.5 + 1.0e-3).floor() as i32;
    let end_z = (player.feet.z + profile.depth * 0.5 - 1.0e-3).floor() as i32;

    let mut support = i32::MIN;

    for x in start_x..=end_x {
        for z in start_z..=end_z {
            let surface = (-64..=32)
                .rev()
                .find(|level| guard.contains(&IVec3::new(x, *level, z)))
                .unwrap_or_else(|| panic!("({x}, {z}) has no floor"));

            support = support.max(surface);
        }
    }

    drop(guard);

    assert!(
        (player.feet.y - (support as f32 + 1.0)).abs() < 1.0e-3,
        "the feet rest on the highest surface under the collider: feet {} against surface {support}",
        player.feet.y
    );
}

#[test]
fn two_generations_from_one_seed_are_equal() {
    let (first, _) = generate(small());
    let (second, _) = generate(small());

    assert_eq!(
        first.world.iter_voxels().collect::<Vec<_>>(),
        second.world.iter_voxels().collect::<Vec<_>>(),
        "one Seed fixes the World voxel for voxel"
    );
    assert_eq!(
        first.snapshots, second.snapshots,
        "and Snapshot for Snapshot"
    );
}

/// Guards the second half of the Seed promise: two Seeds build two Worlds. The
/// test beside this one asks one Seed for the same World twice, which a height
/// function that ignores the Seed passes trivially, so it takes two distinct
/// Seeds and an inequality to catch one. Seeds 0 and [`DIFFERENT_SEED`] are the
/// pair the defect was measured with.
#[test]
fn two_seeds_differ() {
    let first = generate_seeded(0, PINNED_FOOTPRINT);
    let second = generate_seeded(DIFFERENT_SEED, PINNED_FOOTPRINT);
    let mut first_voxels: Vec<(IVec3, u8)> = first.world.iter_voxels().collect();
    let mut second_voxels: Vec<(IVec3, u8)> = second.world.iter_voxels().collect();

    // Sorted, because the store's iteration order is not a contract: an
    // unsorted comparison could report two equal Worlds as different.
    first_voxels.sort_unstable_by_key(|(position, material)| (position.to_array(), *material));
    second_voxels.sort_unstable_by_key(|(position, material)| (position.to_array(), *material));

    assert_ne!(
        first_voxels, second_voxels,
        "two Seeds must not build the same World voxel for voxel"
    );
    assert_ne!(
        first.snapshots, second.snapshots,
        "two Seeds must not build the same World Snapshot for Snapshot"
    );
}

/// Guards the Height field's coherence: two adjacent columns carry levels that
/// differ by at most [`COHERENT_STEP_BOUND`]. Every adjacent pair on the
/// footprint is checked, so one independent column is caught.
#[test]
fn the_surface_is_coherent() {
    let loaded = generate_seeded(SEED, SHAPE_FOOTPRINT);
    let edge = SHAPE_FOOTPRINT;
    let start = LATTICE_HALF_EXTENT.cast_signed().saturating_neg();
    let mut worst = 0u32;
    let mut pairs = 0i32;

    for x in 0..edge {
        for z in 0..edge {
            let here = surface_at(&loaded.world, start + x, start + z);

            for (dx, dz) in ADJACENT_COLUMNS {
                if x + dx >= edge || z + dz >= edge {
                    continue;
                }

                let step = here.abs_diff(surface_at(&loaded.world, start + x + dx, start + z + dz));

                assert!(
                    step <= COHERENT_STEP_BOUND,
                    "the columns ({}, {}) and ({}, {}) differ by {step} levels, past the bound of {COHERENT_STEP_BOUND}",
                    start + x,
                    start + z,
                    start + x + dx,
                    start + z + dz
                );

                worst = worst.max(step);
                pairs = pairs.saturating_add(1);
            }
        }
    }

    let adjacent_pairs = 2 * edge * (edge - 1);

    assert_eq!(
        usize::try_from(pairs).unwrap_or(0),
        usize::try_from(adjacent_pairs).unwrap_or(0),
        "every adjacent pair is checked"
    );
    assert!(
        worst > 0,
        "a surface with no step at all is not a measured surface"
    );
}

/// Guards the pinning promise: the same Seed survives a rebuild. The Snapshots
/// one Seed emits at one footprint are reduced to an FNV-1a fingerprint and held
/// against [`PINNED_SNAPSHOT_FINGERPRINT`], so a refactor of the noise cannot
/// move every World without this test failing.
#[test]
fn a_seed_pins_a_world() {
    let loaded = generate_seeded(SEED, PINNED_FOOTPRINT);

    assert!(
        !loaded.snapshots.is_empty(),
        "a pinned World has Snapshots to fingerprint"
    );
    assert_eq!(
        snapshot_fingerprint(&loaded.snapshots),
        PINNED_SNAPSHOT_FINGERPRINT,
        "the Snapshots for Seed {SEED:#x} at footprint {PINNED_FOOTPRINT} moved"
    );
}

#[test]
fn the_footprint_defaults_to_the_full_lattice() {
    let params = GenerationParams::full_lattice(SEED);
    let half = LATTICE_HALF_EXTENT.cast_signed();

    assert_eq!(params.footprint, IVec3::splat(2 * half));
}

#[test]
fn a_generation_is_refused_while_another_job_is_in_flight() {
    let mut job = WorldUpdateJob::new();

    job.generate(small(), 0)
        .unwrap_or_else(|refusal| panic!("the first generation was refused: {refusal:?}"));

    assert!(matches!(job.generate(small(), 0), Err(Refusal::Busy)));
    assert_eq!(
        job.status(),
        Status::Loading,
        "a refused request changes nothing"
    );
}

#[test]
fn a_clear_and_a_load_follow_a_generation() {
    let mut job = WorldUpdateJob::new();

    job.generate(small(), 0).unwrap_or_else(|r| panic!("{r:?}"));

    let deadline = Instant::now() + Duration::from_secs(5);

    while job.poll() != Some(Finished::Loaded) {
        assert!(Instant::now() < deadline, "the generation did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }

    let generated = job
        .take_loaded()
        .unwrap_or_else(|| panic!("the generation yields"));

    job.arrive();
    assert_eq!(job.status(), Status::Ready);

    job.clear(0).unwrap_or_else(|r| panic!("{r:?}"));

    let deadline = Instant::now() + Duration::from_secs(5);

    while job.poll() != Some(Finished::Cleared) {
        assert!(Instant::now() < deadline, "the clear did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }

    job.arrive();
    assert_eq!(
        job.status(),
        Status::Empty,
        "a Generation clears like a load"
    );

    assert!(generated.world.voxel_count() > 0);
}

#[test]
fn a_generation_ignores_the_cell_budget() {
    // The budget is a test-only per-thread value; the job runs its pipeline on a
    // background thread. The unit test `a_generation_reads_no_cell_budget` in
    // `world::generation` pins the refusal with the budget visible. Here the
    // budget stays unbounded, so a large generation still succeeds.
    let (loaded, status) = generate(small());

    assert_eq!(status, Status::Ready);
    assert!(loaded.world.voxel_count() > 0);
}

#[test]
fn a_failed_generation_reports_a_reason() {
    let mut job = WorldUpdateJob::new();

    job.generate(GenerationParams::new(SEED, IVec3::new(0, -1, 0)), 0)
        .unwrap_or_else(|r| panic!("{r:?}"));

    let deadline = Instant::now() + Duration::from_secs(5);

    let finished = loop {
        if let Some(finished) = job.poll() {
            break finished;
        }

        assert!(Instant::now() < deadline, "the generation did not settle");
        std::thread::sleep(Duration::from_millis(1));
    };

    assert_eq!(finished, Finished::Failed);
    assert_eq!(job.status(), Status::Failed);
    assert!(
        job.error()
            .is_some_and(|error| error.contains("negative extent")),
        "the failure names the reason"
    );
}

#[test]
fn the_generated_world_paints_every_material_index_it_uses() {
    let (loaded, _) = generate(small());
    let palette = loaded.palette;

    let mut used = std::collections::BTreeSet::new();

    for (_, material) in loaded.world.iter_voxels() {
        used.insert(material);

        let color = palette
            .get(usize::from(material))
            .unwrap_or_else(|| panic!("material {material} is inside the Palette"));

        assert!(
            *color != Vec4::ZERO,
            "material {material} must paint, not stay transparent"
        );
    }

    assert!(
        used.len() > 1,
        "the terrain uses more than one material, so layering reads"
    );
    assert!(
        used.contains(&Material::Bedrock.index()),
        "Bedrock paints the fill's floor"
    );
    assert!(
        used.iter().any(|material| *material != Material::Bedrock.index()),
        "the surface and subsurface differ from Bedrock"
    );
}

#[test]
fn the_generated_material_table_settles_falling_granular() {
    // The Vocabulary's table is what a generated World simulates with. This
    // pins the one non-default rule the table carries.
    let (loaded, _) = generate(GenerationParams::new(SEED, IVec3::splat(8)));

    assert_eq!(
        loaded.materials.get(Material::Sand.index()),
        PhysicalMaterial {
            rule: Rule::FallingGranular,
            solid: true,
        }
    );
}

#[test]
#[ignore = "bench: cargo test --release -p atlas-rt --test generation the_full_lattice -- --ignored --nocapture"]
fn the_full_lattice_generation_runs_in_seconds_not_minutes() {
    let start = Instant::now();
    let (loaded, status) = generate(GenerationParams::full_lattice(SEED));
    let elapsed = start.elapsed();

    assert_eq!(status, Status::Ready);
    assert!(loaded.world.voxel_count() > 0);
    assert!(
        elapsed < Duration::from_secs(30),
        "a full-Lattice Generation took {elapsed:.3?}, not seconds"
    );

    println!(
        "full lattice: {} voxels, {} snapshots, {elapsed:.3?}",
        loaded.world.voxel_count(),
        loaded.snapshots.len()
    );
}
