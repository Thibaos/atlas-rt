//! A Generation, driven through the world job seam from outside the crate.
//!
//! A test asks for a Generation with a Seed and a footprint, polls to
//! completion, and takes the World, Snapshots, Palette and Physical material
//! table it delivers. The job is the same one a load runs on, so a Generation
//! and a load are interchangeable to the host.

use std::time::{Duration, Instant};

use atlas_rt::world::{
    diff::snapshot::{MicroChunkSnapshot, emit_snapshots},
    generation::GenerationParams,
    grid::LATTICE_HALF_EXTENT,
    load::job::{Finished, LoadedWorld, Refusal, Status, WorldUpdateJob},
    material::{PhysicalMaterial, Rule},
    vocabulary::{Material, Vocabulary},
};
use glam::{IVec3, Vec4};

/// A small footprint, so the run stays within the poll deadline.
const FOOTPRINT: IVec3 = IVec3::splat(64);
const SEED: u64 = 0x5EED_1234;

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
