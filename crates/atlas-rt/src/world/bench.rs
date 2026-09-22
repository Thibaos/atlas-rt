#[cfg(test)]
mod load_bench {
    use std::time::{Duration, Instant};

    use crate::{
        render::region::pack::pack_regions,
        world::{
            World,
            load::progress::{Progress, Stage},
            update::snapshot::{emit_snapshots, emit_snapshots_reporting},
        },
    };

    const DEFAULT_ASSETS: &[&str] = &["assets/church.vox", "assets/bistro.vox"];

    const EXAMPLE_WORLDS: &[&str] = &[
        "../atlas-rt-godot/examples/project/worlds/castle.vox",
        "../atlas-rt-godot/examples/project/worlds/sponza.vox",
        "../atlas-rt-godot/examples/project/worlds/nuke.vox",
        "../atlas-rt-godot/examples/project/worlds/bistro.vox",
    ];

    struct StageModel {
        floor: Duration,
        per_voxel: Duration,
        per_micro_chunk: Duration,
        per_byte: Duration,
    }

    const WORLD_NEW: StageModel = StageModel {
        floor: ms(10),
        per_voxel: ns(70),
        per_micro_chunk: ns(200),
        per_byte: ns(0),
    };

    const EMIT_SNAPSHOTS: StageModel = StageModel {
        floor: ms(10),
        per_voxel: ns(75),
        per_micro_chunk: ns(50),
        per_byte: ns(0),
    };

    const PACK: StageModel = StageModel {
        floor: ms(10),
        per_voxel: ns(0),
        per_micro_chunk: ns(500),
        per_byte: ns(0),
    };

    const PARSE: StageModel = StageModel {
        floor: ms(50),
        per_voxel: ns(0),
        per_micro_chunk: ns(0),
        per_byte: ns(4),
    };

    const fn ns(count: u64) -> Duration {
        Duration::from_nanos(count)
    }

    const fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
    }

    fn scale(duration: Duration, count: u64) -> Duration {
        let factor = u32::try_from(count).unwrap_or(u32::MAX);

        duration.saturating_mul(factor)
    }

    impl StageModel {
        fn budget(&self, voxels: u64, micro_chunks: u64, bytes: u64) -> Duration {
            self.floor
                .saturating_add(scale(self.per_voxel, voxels))
                .saturating_add(scale(self.per_micro_chunk, micro_chunks))
                .saturating_add(scale(self.per_byte, bytes))
        }
    }

    fn budget_check(stage: &str, elapsed: Duration, budget: Duration) -> Result<(), String> {
        if elapsed <= budget {
            Ok(())
        } else {
            Err(format!(
                "{stage} took {elapsed:.3?}, budget {budget:.3?} (regression in load pipeline)"
            ))
        }
    }

    #[test]
    #[ignore = "bench: cargo test --release load_pipeline_timings -- --ignored --nocapture (church + bistro by default, ATLAS_BENCH_VOX pins one asset)"]
    fn load_pipeline_timings() {
        match std::env::var("ATLAS_BENCH_VOX") {
            Ok(path) => run_asset(&path),
            Err(_) => {
                for asset in DEFAULT_ASSETS {
                    run_asset(asset);
                }
            }
        }
    }

    /// The loader's stages as the job itself runs them, including the report the
    /// emit stage makes. The numbers here are what the progress weights in
    /// `world::progress` are derived from.
    #[test]
    #[ignore = "bench: cargo test --release load_stage_weights -- --ignored --nocapture (ATLAS_BENCH_VOX pins one asset, otherwise the example project's worlds)"]
    fn load_stage_weights() {
        let assets: Vec<String> = match std::env::var("ATLAS_BENCH_VOX") {
            Ok(path) => vec![path],
            Err(_) => EXAMPLE_WORLDS
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        };

        for asset in &assets {
            run_stage_weights(asset);
        }
    }

    fn run_stage_weights(path: &str) {
        let start = Instant::now();
        let bytes = std::fs::read(path).unwrap();
        let read = start.elapsed();

        let start = Instant::now();
        let data = dot_vox::load_bytes(&bytes).unwrap();
        let parse = start.elapsed();

        let start = Instant::now();
        let (world, clipped) = World::new_clipped(&data);
        let build = start.elapsed();

        let progress = Progress::new();
        progress.end_stage(Stage::Read);
        progress.end_stage(Stage::Parse);
        progress.end_stage(Stage::Build);

        let start = Instant::now();
        let snapshots = emit_snapshots_reporting(&world, Some(&progress)).unwrap();
        let emit = start.elapsed();

        let total = read
            .saturating_add(parse)
            .saturating_add(build)
            .saturating_add(emit);
        let share = |stage: Duration| {
            let millionths = stage.as_nanos().saturating_mul(1_000_000) / total.as_nanos().max(1);

            u32::try_from(millionths).unwrap_or(0)
        };

        println!("asset           {path}");
        println!("voxels          {}", world.voxel_count());
        println!("clipped         {clipped}");
        println!("micro chunks    {}", snapshots.len());
        println!("file bytes      {}", bytes.len());
        println!("read            {read:10.3?}  {}", share(read));
        println!("parse           {parse:10.3?}  {}", share(parse));
        println!("build           {build:10.3?}  {}", share(build));
        println!("emit            {emit:10.3?}  {}", share(emit));
        println!("total           {total:10.3?}");
    }

    fn run_asset(path: &str) {
        let bytes = std::fs::metadata(path).map_or(0, |meta| meta.len());

        let start = Instant::now();
        let data = dot_vox::load(path).unwrap();
        let parse = start.elapsed();

        let start = Instant::now();
        let (world, clipped) = World::new_clipped(&data);
        let world_new = start.elapsed();
        let reserved = world.reserved_capacity();

        let start = Instant::now();
        let snapshots = emit_snapshots(&world).unwrap();
        let emit = start.elapsed();

        let start = Instant::now();
        let packed = pack_regions(&snapshots).unwrap();
        let pack = start.elapsed();

        let total = parse
            .saturating_add(world_new)
            .saturating_add(emit)
            .saturating_add(pack);

        let voxels = world.voxel_count() as u64;
        let micro_chunks = snapshots.len() as u64;

        println!("path            {path}");
        println!("voxels          {}", world.voxel_count());
        println!("clipped         {clipped}");
        println!("micro chunks    {}", snapshots.len());
        println!("regions         {}", packed.len());
        println!("parse           {parse:10.3?}");
        println!("world_new       {world_new:10.3?}");
        println!("reserved        {reserved}");
        println!("emit_snapshots  {emit:10.3?}");
        println!("pack            {pack:10.3?}");
        println!("total           {total:10.3?}");

        let stage_budgets = [
            WORLD_NEW.budget(voxels, micro_chunks, 0),
            EMIT_SNAPSHOTS.budget(voxels, micro_chunks, 0),
            PACK.budget(voxels, micro_chunks, 0),
        ];
        let total_budget = PARSE
            .budget(0, 0, bytes)
            .saturating_add(stage_budgets[0])
            .saturating_add(stage_budgets[1])
            .saturating_add(stage_budgets[2]);

        let mut failures = String::new();

        for (stage, elapsed, budget) in [
            ("world_new", world_new, stage_budgets[0]),
            ("emit_snapshots", emit, stage_budgets[1]),
            ("pack", pack, stage_budgets[2]),
            ("total", total, total_budget),
        ] {
            if let Err(message) = budget_check(stage, elapsed, budget) {
                failures.push_str(&message);
                failures.push('\n');
            }
        }

        assert!(failures.is_empty(), "{failures}");
    }

    #[test]
    fn budgets_scale_with_asset_size() {
        let small = WORLD_NEW.budget(1_000, 10, 1);
        let large = WORLD_NEW.budget(1_000_000, 10_000, 1);

        assert!(large > small);
        assert_eq!(WORLD_NEW.budget(0, 0, 0), WORLD_NEW.floor);
        assert!(WORLD_NEW.floor > Duration::ZERO);
    }

    #[test]
    fn inflated_stage_trips_the_budget() {
        let budget = EMIT_SNAPSHOTS.budget(10_000, 100, 0);

        assert!(budget_check("emit_snapshots", budget, budget).is_ok());

        let inflated = budget.saturating_add(ms(1));
        assert!(budget_check("emit_snapshots", inflated, budget).is_err());
    }
}

#[cfg(test)]
mod edit_bench {
    use std::time::Instant;

    use glam::IVec3;
    use rustc_hash::FxHashSet;

    use crate::world::{
        World,
        grid::{REGION_LENGTH, region_index_of},
        placement::Rng,
        update::{
            batch::TrackedCoords,
            edit::{VoxelChange, VoxelEdit, edit_world},
        },
    };

    const BATCH_SIZES: [usize; 3] = [1_000, 10_000, 100_000];
    const DENSE_EDGE: i32 = 64;
    const SEED: u64 = 0x00ED_1704;
    const WORLD_MATERIAL: u8 = 1;
    const EDIT_MATERIAL: u8 = 2;

    const REGION_CENTERS: [IVec3; 8] = [
        IVec3::new(0, 0, 0),
        IVec3::new(1, 0, 0),
        IVec3::new(-1, 0, 0),
        IVec3::new(0, 1, 0),
        IVec3::new(0, -1, 0),
        IVec3::new(0, 0, 1),
        IVec3::new(0, 0, -1),
        IVec3::new(1, 1, 0),
    ];

    fn dense_origins() -> Vec<IVec3> {
        let edge = REGION_LENGTH.cast_signed();
        let pad = (edge - DENSE_EDGE) / 2;

        REGION_CENTERS
            .iter()
            .map(|center| {
                center
                    .saturating_mul(IVec3::splat(edge))
                    .saturating_add(IVec3::splat(pad))
            })
            .collect()
    }

    fn dense_world(origins: &[IVec3]) -> World {
        let cells = usize::try_from(DENSE_EDGE)
            .unwrap_or(8)
            .pow(3)
            .saturating_mul(origins.len());

        let mut world = World::default();
        world.reserve(cells);

        for origin in origins {
            for x in 0..DENSE_EDGE {
                for y in 0..DENSE_EDGE {
                    for z in 0..DENSE_EDGE {
                        world.set_voxel(origin.saturating_add(IVec3::new(x, y, z)), WORLD_MATERIAL);
                    }
                }
            }
        }

        world
    }

    fn random_axis(rng: &mut Rng) -> i32 {
        i32::try_from(rng.below(DENSE_EDGE as u64)).unwrap_or(0)
    }

    fn random_cell(origin: IVec3, rng: &mut Rng) -> IVec3 {
        origin + IVec3::new(random_axis(rng), random_axis(rng), random_axis(rng))
    }

    fn set_edit(position: IVec3) -> VoxelEdit {
        VoxelEdit {
            position,
            change: VoxelChange::Set(EDIT_MATERIAL),
        }
    }

    fn clustered_edits(rng: &mut Rng, origin: IVec3, count: usize) -> Vec<VoxelEdit> {
        (0..count)
            .map(|_| set_edit(random_cell(origin, rng)))
            .collect()
    }

    fn scattered_edits(rng: &mut Rng, origins: &[IVec3], count: usize) -> Vec<VoxelEdit> {
        let mut cycle = origins.iter().cycle();

        (0..count)
            .map(|_| {
                let origin = cycle.next().copied().unwrap_or(IVec3::ZERO);

                set_edit(random_cell(origin, rng))
            })
            .collect()
    }

    fn regions_touched(edits: &[VoxelEdit]) -> usize {
        let mut seen: FxHashSet<IVec3> = FxHashSet::default();

        for edit in edits {
            seen.insert(region_index_of(edit.position));
        }

        seen.len()
    }

    fn per_unit(nanos: u128, count: usize) -> f64 {
        nanos as f64 / count.max(1) as f64
    }

    #[test]
    #[ignore = "bench: cargo test --release edit_path_timings -- --ignored --nocapture"]
    fn edit_path_timings() {
        let origins = dense_origins();
        let mut world = dense_world(&origins);
        let mut rng = Rng::new(SEED);

        println!("regions         {}", origins.len());
        println!("dense edge      {DENSE_EDGE}");
        println!("voxels          {}", world.voxel_count());
        println!();
        println!(
            "{:<9} {:>7} {:>7} {:>10} {:>9} {:>11} {:>9} {:>9}",
            "placement",
            "edits",
            "chunks",
            "mutation",
            "ns/edit",
            "mut+compile",
            "ns/edit",
            "ns/chunk"
        );

        let first = origins.first().copied().unwrap_or(IVec3::ZERO);

        for size in BATCH_SIZES {
            let batches = [
                ("clustered", 1, clustered_edits(&mut rng, first, size)),
                (
                    "scattered",
                    origins.len(),
                    scattered_edits(&mut rng, &origins, size),
                ),
            ];

            for (placement, expected_regions, edits) in batches {
                let start = Instant::now();

                for edit in &edits {
                    match edit.change {
                        VoxelChange::Set(material) => world.set_voxel(edit.position, material),
                        VoxelChange::Clear => world.clear_voxel(edit.position),
                    }
                }

                let mutate = start.elapsed();

                let start = Instant::now();
                let batch = edit_world(&mut world, &edits, &TrackedCoords::default()).unwrap();
                let full = start.elapsed();

                let chunks = batch.snapshots.len();
                let touched_regions = regions_touched(&edits);

                assert_eq!(
                    touched_regions, expected_regions,
                    "{placement} at {size} edits must span {expected_regions} regions"
                );
                assert!(chunks > 0, "{placement} at {size} edits must touch a chunk");

                let mutate_per = per_unit(mutate.as_nanos(), size);
                let full_per = per_unit(full.as_nanos(), size);
                let chunk_per = per_unit(full.as_nanos(), chunks);

                println!(
                    "{placement:9} {size:>7} {chunks:>7} {mutate:>10.3?} {mutate_per:>9.1} {full:>11.3?} {full_per:>9.1} {chunk_per:>9.1}"
                );
            }
        }
    }
}
