//! Timings and regression budgets for the World's load and edit paths.
//!
//! `load_pipeline_timings`, release, Windows, 2026-10-02:
//!
//! | stage          | church 14.3M vox | bistro 98.6M vox |
//! | -------------- | ---------------- | ---------------- |
//! | parse           | 96.7 ms          | 1.349 s          |
//! | world_new       | 143.6 ms         | 894.6 ms         |
//! | emit_snapshots  | 578.5 ms         | 4.295 s          |
//! | pack            | 6.4 ms           | 69.9 ms          |
//!
//! `load_stage_weights` over castle, sponza, nuke, and bistro, same store:
//! mean shares of read 2.1%, parse 10.8%, build 21.3%, emit 65.8%. That gives
//! the cumulative endpoints `world::load::progress` uses: 21_000, 129_000,
//! 342_000.
//!
//! The budgets below are these figures with headroom for run-to-run variance.
//! Emission dominates, and its per-voxel record reserve, `total / 256` per
//! bucket and 8 bytes per voxel overall, is the largest remaining CPU cost.
//!
//! `edit_path_timings`, release, AMD Ryzen 7 9800X3D, 2026-10-02. The compile
//! is measured alone over the touched chunks, with the mutation, validation,
//! budget, and assemble `edit_world` also does left out of both columns, and the
//! minimum of eight passes taken. The probe reads all 512 cells of a touched
//! Micro-chunk through `world.get_voxel`; the entry copy reads the Region store's
//! 64-byte mask and compacted materials in one pass. The entry copy is installed,
//! so `edit_world` uses it. Ticket 15 took the rank scan off `get_voxel`, which
//! halved the probe from about 10.5 to about 5.5 µs; the entry copy is unchanged
//! at about 100 ns:
//!
//! | placement | edits     | chunks | probe ns/chunk | entry ns/chunk |
//! | --------- | --------- | ------ | -------------- | -------------- |
//! | clustered |     1,000 |   447 | 5,479 | 100 |
//! | scattered |     1,000 |   894 | 5,487 | 104 |
//! | clustered |    10,000 |   512 | 5,484 |  99 |
//! | scattered |    10,000 | 3,717 | 5,492 | 105 |
//! | clustered |   100,000 |   512 | 5,462 | 100 |
//! | scattered |   100,000 | 4,096 | 5,488 | 111 |
//! | clustered | 1,000,000 |   512 | 5,461 | 100 |
//! | scattered | 1,000,000 | 4,096 | 5,485 | 110 |
//!
//! The test also prints the whole `edit_world` call against a 60 Hz frame
//! (16.667 ms). A million-edit batch is the first to exceed it; every smaller
//! batch fits, and even the 100,000-edit batches stay under a fifth of the
//! frame:
//!
//! | placement | 1,000  | 10,000  | 100,000  | 1,000,000 |
//! | --------- | ------ | ------- | -------- | --------- |
//! | clustered | 135 µs | 303 µs  | 1.911 ms | 18.049 ms |
//! | scattered | 263 µs | 1.210 ms | 2.975 ms | 20.326 ms |
//!
//! The batch is pre-applied to the world before `edit_world` is timed, so every
//! edit here overwrites an already-occupied cell in a dense Region. First writes
//! and growth between size classes are `free_list_edit_timings`; clears, a
//! populated tracked set, the budget projection, and the renderer's CPU pack are
//! `edit_workload_timings`. The older 11 to 17 µs figures included mutation and
//! assembly.
//!
//! `edit_workload_timings`, release, same host. `edit_world` alone over 100,000
//! and 1,000,000 edits on the dense fixture, by path, with the renderer's CPU
//! pack beside it. `tracked` is the same batch with the touched chunks in the
//! tracked set. `budget` forces the projection `edit_world` runs once a cell
//! threshold is set; in production `cell_budget()` is `usize::MAX`, so the
//! projection is skipped and this row is the cost the day a number lands:
//!
//! | workload  | 100k ns/edit | 1M ns/edit | 1M edit_world |
//! | --------- | ------------ | ---------- | ------------- |
//! | overwrite |         34.0 |       21.0 |      21.038 ms |
//! | mixed     |         58.2 |       40.8 |      40.847 ms |
//! | tracked   |         32.9 |       21.2 |      21.233 ms |
//! | budget    |         85.1 |       78.1 |      78.071 ms |
//!
//! A clear roughly doubles the per-edit cost, a populated tracked set is within
//! noise, and the budget projection is the large one: it hashes every edit into
//! a pending overlay and reads the world once per distinct cell, about 57 ms of
//! the 1M budget row. At 60 Hz that is about 200,000 projected edits against
//! about 800,000 unprojected overwrites. The renderer's CPU pack is about 100 ns
//! per touched Micro-chunk, 0.4 ms over the 4096 chunks, a small share of the
//! edit cost on these dense workloads. The upload, BLAS, and TLAS are
//! `render::region::bench`'s `gpu_rebuild_timings`: a one-Region update is about
//! 1.3 ms of pack and 0.6 ms of apply on church's widest Region.
//!
//! `rank_read_path_timings`, release, 2026-10-02, same host. `rank` scanned the
//! mask from byte 0 on every random read and on every voxel `iter_voxels`
//! yielded. Ticket 15 removed it from the walk with a running counter and made
//! the random-access form read the 8-byte word holding the cell. Dense fixture,
//! eight 64^3 blocks, one per Region, minimum of eight passes. The before column
//! is the pre-change measurement on the same host and fixture; the test times
//! the current path only:
//!
//! | path                 | before rank scan | after |
//! | -------------------- | ---------------- | ----- |
//! | get_voxel ns/get     |            20.40 | 10.26 |
//! | iter_voxels ns/voxel |            18.39 |  3.04 |
//! | rank ns/call         |            16.23 |  4.65 |
//!
//! On the assets, the store scan and the emitter separate. `scan` is a bare
//! `iter_voxels` fold; `emit - scan` approximates the emitter's per-voxel record
//! construction, bucketing, and final sort, so it is the emitter's own cost and
//! not the store read:
//!
//! | asset  | scan ns/vox | emit ns/vox | emit - scan ns/vox |
//! | ------ | ----------- | ----------- | ------------------ |
//! | church |   24.5 → 8.6 | 41.5 → 25.8 |   16.9 → 17.2      |
//! | bistro |   22.6 → 8.1 | 41.8 → 28.2 |   19.1 → 20.1      |
//!
//! Removing `rank` from the walk took bistro's emit from 4.119 s to 2.781 s, a
//! 32% cut, and the emitter's own share is now the larger term, as the storage
//! spec predicted. A stored per-entry rank prefix was rejected: the running
//! counter costs nothing on iteration, the word popcount needs no storage, and a
//! 16-byte prefix would push the full 576-byte entry past the free list's class
//! ceiling and add about 2.8% to a full Region's blob.

mod load_bench {
    use std::time::{Duration, Instant};

    use crate::{
        render::region::pack::pack_regions,
        world::{
            World,
            diff::snapshot::{emit_snapshots, emit_snapshots_reporting},
            load::progress::{Progress, Stage},
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
        per_voxel: ns(15),
        per_micro_chunk: ns(50),
        per_byte: ns(0),
    };

    const EMIT_SNAPSHOTS: StageModel = StageModel {
        floor: ms(10),
        per_voxel: ns(60),
        per_micro_chunk: ns(200),
        per_byte: ns(0),
    };

    const PACK: StageModel = StageModel {
        floor: ms(2),
        per_voxel: ns(0),
        per_micro_chunk: ns(60),
        per_byte: ns(0),
    };

    const PARSE: StageModel = StageModel {
        floor: ms(25),
        per_voxel: ns(0),
        per_micro_chunk: ns(0),
        per_byte: ns(3),
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
        let size = world.storage_size();

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
        println!("storage table   {}", size.table);
        println!("storage index   {}", size.index);
        println!("storage blob    {}", size.blob);
        println!("emit_snapshots  {emit:10.3?}");
        println!("pack            {pack:10.3?}");
        println!("total           {total:10.3?}");

        let stages = [
            ("parse", parse),
            ("world_new", world_new),
            ("emit_snapshots", emit),
            ("pack", pack),
        ];
        let dominant = stages
            .iter()
            .copied()
            .max_by_key(|&(_, elapsed)| elapsed);

        if let Some((stage, elapsed)) = dominant {
            let share = elapsed.as_nanos().saturating_mul(100) / total.as_nanos().max(1);

            println!("dominant        {stage} {share}%");
        }

        println!("reserve         emission's per-voxel record is the largest remaining CPU cost");

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

mod edit_bench {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    use glam::IVec3;
    use rustc_hash::FxHashSet;

    use crate::{
        render::region::pack::pack_regions,
        world::{
            World,
            budget::set_cell_budget,
            diff::{
                batch::TrackedCoords,
                edit::{
                    VoxelChange, VoxelEdit, chunks_touched, compile_chunk, edit_world, probe_chunk,
                },
                snapshot::MicroChunkSnapshot,
            },
            grid::{MICRO_CHUNK_LENGTH, REGION_LENGTH, region_index_of},
            test_support::Rng,
        },
    };

    const BATCH_SIZES: [usize; 4] = [1_000, 10_000, 100_000, 1_000_000];
    const FRAME_BUDGET: Duration = Duration::from_micros(16_667);
    const DENSE_EDGE: i32 = 64;
    const SEED: u64 = 0x00ED_1704;
    const WORLD_MATERIAL: u8 = 1;
    const EDIT_MATERIAL: u8 = 2;
    const FREE_LIST_WRITES: usize = 10_000;

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
        let mut world = World::default();

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

    /// The Micro-chunk origin of the `index`th empty Micro-chunk in the dense
    /// Region, skipping the filled block at Micro-chunk coordinates 12..20.
    fn empty_micro_chunk_origin(index: usize) -> IVec3 {
        let side = (REGION_LENGTH / MICRO_CHUNK_LENGTH) as usize;
        let filled = (DENSE_EDGE as u32 / MICRO_CHUNK_LENGTH) as usize;
        let pad = (side - filled) / 2;
        let span = side - filled;
        let axis = |value: usize| {
            let local = value % span;

            if local < pad { local } else { local + filled }
        };
        let x = axis(index);
        let y = axis(index / span);
        let z = axis(index / (span * span));
        let edge = MICRO_CHUNK_LENGTH as i32;

        IVec3::new((x as i32) * edge, (y as i32) * edge, (z as i32) * edge)
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

    const COMPILE_REPEATS: usize = 8;

    /// The fastest of [`COMPILE_REPEATS`] compile passes over every touched
    /// Micro-chunk, with the total occupied count for a correctness check. The
    /// minimum absorbs a scheduler hiccup, and the allocation per chunk keeps
    /// the call from being optimised away.
    fn best_compile(
        world: &World,
        touched: &[IVec3],
        compile: fn(&World, IVec3) -> MicroChunkSnapshot,
    ) -> (Duration, usize) {
        let mut best = Duration::MAX;
        let mut occupied = 0usize;

        for _ in 0..COMPILE_REPEATS {
            let start = Instant::now();
            let mut total = 0usize;

            for origin in touched {
                total = total.saturating_add(compile(world, *origin).occupied_count());
            }

            let elapsed = start.elapsed();

            if elapsed < best {
                best = elapsed;
            }

            occupied = total;
        }

        (best, occupied)
    }

    /// Mutation and compile cost of a Voxel edit batch on the Region store,
    /// split by placement and batch size, with the entry-copy compile beside
    /// the installed probe compile.
    ///
    /// `mut+compile` is the whole `edit_world` call. The `probe` and `entry`
    /// columns are the compile alone over the same touched chunks: `probe_chunk`
    /// reads all 512 cells through `world.get_voxel`, and `compile_chunk` reads
    /// the Region store's 64-byte mask and compacted materials in one pass. The
    /// entry-copy compile is installed, so `mut+compile` uses it too.
    ///
    /// `frame` is `mut+compile` against a 60 Hz frame. The 1,000,000 rows are
    /// the point that exceeds it; every smaller batch fits. The batch is
    /// pre-applied before `edit_world` is timed, so the edits overwrite occupied
    /// cells on the dense fixture with an empty tracked set and no budget
    /// projection. A first write into an empty Micro-chunk, a clear, and a
    /// non-empty tracked set are all dearer and are not measured here.
    #[test]
    #[ignore = "bench: cargo test --release edit_path_timings -- --ignored --nocapture"]
    fn edit_path_timings() {
        let origins = dense_origins();
        let mut world = dense_world(&origins);
        let mut rng = Rng::new(SEED);

        println!("regions         {}", origins.len());
        println!("dense edge      {DENSE_EDGE}");
        println!("voxels          {}", world.voxel_count());
        println!("frame budget    {:.3?} (60 Hz)", FRAME_BUDGET);
        println!();
        println!(
            "{:<9} {:>7} {:>7} {:>10} {:>9} {:>11} {:>9} {:>9} {:>11} {:>9} {:>8}",
            "placement",
            "edits",
            "chunks",
            "mutation",
            "ns/edit",
            "mut+compile",
            "ns/edit",
            "ns/chunk",
            "probe",
            "entry",
            "frame %"
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
                let touched = chunks_touched(&edits);

                let probed: Vec<MicroChunkSnapshot> = touched
                    .iter()
                    .map(|origin| probe_chunk(&world, *origin))
                    .collect();
                let copied: Vec<MicroChunkSnapshot> = touched
                    .iter()
                    .map(|origin| compile_chunk(&world, *origin))
                    .collect();

                assert_eq!(
                    probed, copied,
                    "{placement} at {size} edits: the entry-copy compile diverged from the probe"
                );
                assert_eq!(
                    copied, batch.snapshots,
                    "{placement} at {size} edits: the compile diverged from edit_world"
                );

                let (probe, probe_voxels) = best_compile(&world, &touched, probe_chunk);
                let (copy, copy_voxels) = best_compile(&world, &touched, compile_chunk);

                assert_eq!(
                    probe_voxels, copy_voxels,
                    "{placement} at {size} edits: probe and entry compile disagree on occupancy"
                );
                assert_eq!(
                    touched_regions, expected_regions,
                    "{placement} at {size} edits must span {expected_regions} regions"
                );
                assert!(chunks > 0, "{placement} at {size} edits must touch a chunk");

                let mutate_per = per_unit(mutate.as_nanos(), size);
                let full_per = per_unit(full.as_nanos(), size);
                let chunk_per = per_unit(full.as_nanos(), chunks);
                let probe_per = per_unit(probe.as_nanos(), chunks);
                let copy_per = per_unit(copy.as_nanos(), chunks);
                let frame_per = full.as_secs_f64() / FRAME_BUDGET.as_secs_f64() * 100.0;

                println!(
                    "{placement:9} {size:>7} {chunks:>7} {mutate:>10.3?} {mutate_per:>9.1} {full:>11.3?} {full_per:>9.1} {chunk_per:>9.1} {probe_per:>11.0} {copy_per:>9.0} {frame_per:>7.0}%"
                );
            }
        }
    }

    const WORKLOAD_SIZES: [usize; 2] = [100_000, 1_000_000];
    const PACK_REPEATS: usize = 3;

    fn scattered_positions(rng: &mut Rng, origins: &[IVec3], count: usize) -> Vec<IVec3> {
        let mut cycle = origins.iter().cycle();

        (0..count)
            .map(|_| {
                let origin = cycle.next().copied().unwrap_or(IVec3::ZERO);

                random_cell(origin, rng)
            })
            .collect()
    }

    fn set_batch(positions: &[IVec3]) -> Vec<VoxelEdit> {
        positions.iter().copied().map(set_edit).collect()
    }

    fn mixed_batch(positions: &[IVec3]) -> Vec<VoxelEdit> {
        positions
            .iter()
            .enumerate()
            .map(|(index, position)| VoxelEdit {
                position: *position,
                change: if index.is_multiple_of(2) {
                    VoxelChange::Set(EDIT_MATERIAL)
                } else {
                    VoxelChange::Clear
                },
            })
            .collect()
    }

    /// The fastest of [`PACK_REPEATS`] renderer-side packs of the batch's
    /// snapshots. This is the CPU half of the rebuild; the buffer upload, BLAS,
    /// and TLAS need a Vulkan device.
    fn best_pack(snapshots: &[MicroChunkSnapshot]) -> Duration {
        let mut best = Duration::MAX;

        for _ in 0..PACK_REPEATS {
            let start = Instant::now();
            let packed = pack_regions(snapshots).unwrap();
            let elapsed = start.elapsed();

            black_box(packed.len());

            if elapsed < best {
                best = elapsed;
            }
        }

        best
    }

    fn run_workload(
        edits: &[VoxelEdit],
        tracked: &TrackedCoords,
        budget: bool,
    ) -> (usize, Duration, Duration) {
        let mut world = dense_world(&dense_origins());
        let _guard = budget.then(|| set_cell_budget(usize::MAX - 1));

        let start = Instant::now();
        let batch = edit_world(&mut world, edits, tracked).unwrap();
        let full = start.elapsed();

        let chunks = batch.snapshots.len();
        let pack = best_pack(&batch.snapshots);

        (chunks, full, pack)
    }

    /// The edit call across the workloads `edit_path_timings` does not cover:
    /// clears through `edit_world`, a populated tracked set, and the budget
    /// projection. First writes into empty Micro-chunks are not here because
    /// they are per touched chunk rather than per edit, and
    /// `free_list_edit_timings` already times them. The pack column is the
    /// renderer's CPU rebuild of the touched Regions beside the edit cost.
    #[test]
    #[ignore = "bench: cargo test --release edit_workload_timings -- --ignored --nocapture"]
    fn edit_workload_timings() {
        let origins = dense_origins();
        let mut rng = Rng::new(SEED);

        println!("frame budget    {:.3?} (60 Hz)", FRAME_BUDGET);
        println!();
        println!(
            "{:<10} {:>7} {:>7} {:>11} {:>9} {:>9} {:>11} {:>8}",
            "workload", "edits", "chunks", "edit_world", "ns/edit", "ns/chunk", "pack ns/chunk", "frame %"
        );

        for size in WORKLOAD_SIZES {
            let positions = scattered_positions(&mut rng, &origins, size);
            let sets = set_batch(&positions);
            let mixed = mixed_batch(&positions);
            let tracked: TrackedCoords = chunks_touched(&sets).into_iter().collect();

            for (workload, edits, tracked, budget) in [
                ("overwrite", &sets, &TrackedCoords::default(), false),
                ("mixed", &mixed, &TrackedCoords::default(), false),
                ("tracked", &sets, &tracked, false),
                ("budget", &sets, &TrackedCoords::default(), true),
            ] {
                let (chunks, full, pack) = run_workload(edits, tracked, budget);
                let full_per = per_unit(full.as_nanos(), size);
                let chunk_per = per_unit(full.as_nanos(), chunks);
                let pack_per = per_unit(pack.as_nanos(), chunks);
                let frame_per = full.as_secs_f64() / FRAME_BUDGET.as_secs_f64() * 100.0;

                println!(
                    "{workload:<10} {size:>7} {chunks:>7} {full:>11.3?} {full_per:>9.1} {chunk_per:>9.1} {pack_per:>11.0} {frame_per:>7.0}%"
                );
            }
        }
    }

    fn measure_first_writes(world: &mut World, label: &str) {
        let start = Instant::now();

        for index in 0..FREE_LIST_WRITES {
            world.set_voxel(empty_micro_chunk_origin(index), EDIT_MATERIAL);
        }

        let elapsed = start.elapsed();

        println!(
            "{label:9} {FREE_LIST_WRITES:>7} {elapsed:>10.3?} {:>9.1} ns/write",
            per_unit(elapsed.as_nanos(), FREE_LIST_WRITES)
        );
    }

    /// A first write claims a Micro-chunk-sized block, and an in-place clear and
    /// set stay inside the Micro-chunk instead of shifting the Region's blob.
    /// The same first writes land at the same cost whether the Region already
    /// holds a dense block or is empty.
    #[test]
    #[ignore = "bench: cargo test --release free_list_edit_timings -- --ignored --nocapture"]
    fn free_list_edit_timings() {
        let origins = dense_origins();
        let mut populated = dense_world(&origins[..1]);
        let mut empty = World::default();

        println!("populated voxels {}", populated.voxel_count());
        println!();
        println!("{:<9} {:>7} {:>10} {:>9}", "region", "writes", "elapsed", "ns/write");
        measure_first_writes(&mut populated, "populated");
        measure_first_writes(&mut empty, "empty");

        let origin = origins.first().copied().unwrap_or(IVec3::ZERO);
        let mut rng = Rng::new(SEED);
        let start = Instant::now();

        for _ in 0..FREE_LIST_WRITES {
            let position = random_cell(origin, &mut rng);

            populated.clear_voxel(position);
            populated.set_voxel(position, EDIT_MATERIAL);
        }

        let toggle = start.elapsed();

        println!(
            "toggle pairs  {FREE_LIST_WRITES:>7} {toggle:>10.3?} {:>9.1} ns/toggle",
            per_unit(toggle.as_nanos(), FREE_LIST_WRITES)
        );
    }
}

/// The `rank` scan's cost on each read path, and the emission stage split into
/// the store scan and the emitter's own record, bucket, and sort work.
///
/// The three paths are `get_voxel` over occupied cells, `iter_voxels` over the
/// world, and `emit_snapshots`, which walks `iter_voxels`. `rank` is timed
/// directly over the occupied cells of every live Micro-chunk to give its
/// per-call floor. `scan` is a bare `iter_voxels` fold on the asset, so
/// `emit - scan` is the emitter's per-voxel record construction, bucketing, and
/// final sort rather than the store read.
mod read_bench {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    use glam::IVec3;
    use rustc_hash::FxHashSet;

    use crate::world::{
        World,
        diff::snapshot::emit_snapshots,
        grid::{MICRO_CHUNK_LENGTH, REGION_LENGTH, grid_origin},
        store::region::rank,
    };

    const DENSE_EDGE: i32 = 64;
    const DENSE_CENTERS: [(i32, i32, i32); 8] = [
        (0, 0, 0),
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
        (1, 1, 0),
    ];
    const DENSE_REPEATS: usize = 8;
    const ASSET_REPEATS: usize = 3;
    const CELLS: usize = (MICRO_CHUNK_LENGTH * MICRO_CHUNK_LENGTH * MICRO_CHUNK_LENGTH) as usize;

    fn per_unit(nanos: u128, count: usize) -> f64 {
        nanos as f64 / count.max(1) as f64
    }

    fn best(count: usize, mut run: impl FnMut() -> u64) -> (Duration, u64) {
        let mut best = Duration::MAX;
        let mut result = 0;

        for _ in 0..count {
            let start = Instant::now();
            result = black_box(run());
            let elapsed = start.elapsed();

            if elapsed < best {
                best = elapsed;
            }
        }

        (best, result)
    }

    /// One full 64^3 block per Region, so every touched Micro-chunk is dense and
    /// the scan has its worst case.
    fn dense_world() -> World {
        let edge = REGION_LENGTH as i32;
        let pad = (edge - DENSE_EDGE) / 2;
        let mut world = World::default();

        for (x, y, z) in DENSE_CENTERS {
            let origin = IVec3::new(x, y, z).saturating_mul(IVec3::splat(edge)) + IVec3::splat(pad);

            for dx in 0..DENSE_EDGE {
                for dy in 0..DENSE_EDGE {
                    for dz in 0..DENSE_EDGE {
                        world.set_voxel(origin + IVec3::new(dx, dy, dz), 1);
                    }
                }
            }
        }

        world
    }

    fn chunk_origins(world: &World) -> Vec<IVec3> {
        let mut seen: FxHashSet<IVec3> = FxHashSet::default();
        let mut origins: Vec<IVec3> = Vec::new();

        for (position, _) in world.iter_voxels() {
            let origin = grid_origin(position, MICRO_CHUNK_LENGTH);

            if seen.insert(origin) {
                origins.push(origin);
            }
        }

        origins
    }

    #[test]
    #[ignore = "bench: cargo test --release rank_read_path_timings -- --ignored --nocapture"]
    fn rank_read_path_timings() {
        let world = dense_world();
        let positions: Vec<IVec3> = world.iter_voxels().map(|(position, _)| position).collect();
        let origins = chunk_origins(&world);
        let voxels = positions.len();

        println!("dense voxels    {voxels}");
        println!("dense chunks    {}", origins.len());

        let (get, material) = best(DENSE_REPEATS, || {
            positions
                .iter()
                .map(|position| u64::from(world.get_voxel(position).unwrap_or(0)))
                .sum()
        });
        println!(
            "get_voxel       {:>10.3?} {:>9.2} ns/get  {material}",
            get,
            per_unit(get.as_nanos(), voxels)
        );

        let (iter, count) = best(DENSE_REPEATS, || {
            world
                .iter_voxels()
                .map(|(_, material)| u64::from(material))
                .sum()
        });
        println!(
            "iter_voxels     {:>10.3?} {:>9.2} ns/voxel  {count}",
            iter,
            per_unit(iter.as_nanos(), voxels)
        );

        let mut rank_calls = 0usize;
        let (ranked, sum) = best(DENSE_REPEATS, || {
            let mut total = 0u64;
            let mut calls = 0usize;

            for origin in &origins {
                let Some(entry) = world.chunk_entry(*origin) else {
                    continue;
                };

                for cell in 0..CELLS {
                    let set = entry
                        .mask
                        .get(cell / 8)
                        .is_some_and(|byte| byte & (1u8 << (cell % 8)) != 0);

                    if set {
                        total = total.wrapping_add(rank(entry.mask, cell) as u64);
                        calls = calls.saturating_add(1);
                    }
                }
            }

            rank_calls = calls;

            total
        });
        println!(
            "rank            {:>10.3?} {:>9.2} ns/call  {sum}",
            ranked,
            per_unit(ranked.as_nanos(), rank_calls)
        );

        for path in ["assets/church.vox", "assets/bistro.vox"] {
            let Ok(data) = dot_vox::load(path) else {
                println!("{path} missing");
                continue;
            };

            let (world, _) = World::new_clipped(&data);
            let voxels = world.voxel_count();

            let (scan, scanned) = best(ASSET_REPEATS, || {
                world
                    .iter_voxels()
                    .map(|(_, material)| u64::from(material))
                    .sum()
            });
            let (emit, snapshots) = best(ASSET_REPEATS, || {
                emit_snapshots(&world).unwrap().len() as u64
            });

            let scan_per = per_unit(scan.as_nanos(), voxels);
            let emit_per = per_unit(emit.as_nanos(), voxels);
            let emitter = emit.saturating_sub(scan);
            let emitter_per = per_unit(emitter.as_nanos(), voxels);

            println!();
            println!("asset           {path}");
            println!("voxels          {voxels}  chunks {snapshots}  scanned {scanned}");
            println!("scan            {scan:>10.3?} {scan_per:>9.2} ns/voxel");
            println!("emit            {emit:>10.3?} {emit_per:>9.2} ns/voxel");
            println!("emit - scan     {emitter:>10.3?} {emitter_per:>9.2} ns/voxel");
        }
    }
}
