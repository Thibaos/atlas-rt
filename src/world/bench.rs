#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod load_bench {
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };

    use glam::IVec3;

    use crate::{
        render::region::pack::{RegionData, pack_region},
        world::{
            World, grid::region_index_of, snapshot::MicroChunkSnapshot, snapshot::emit_snapshots,
        },
    };

    const DEFAULT_ASSETS: &[&str] = &["assets/church.vox", "assets/bistro.vox"];

    struct StageModel {
        floor: Duration,
        per_voxel: Duration,
        per_micro_chunk: Duration,
        per_byte: Duration,
    }

    const WORLD_NEW: StageModel = StageModel {
        floor: ms(10),
        per_voxel: ns(70),
        per_micro_chunk: ns(100),
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

    fn run_asset(path: &str) {
        let bytes = std::fs::metadata(path).map_or(0, |meta| meta.len());

        let start = Instant::now();
        let data = dot_vox::load(path).unwrap();
        let parse = start.elapsed();

        let start = Instant::now();
        let (world, clipped) = World::new_clipped(&data);
        let world_new = start.elapsed();

        let start = Instant::now();
        let snapshots = emit_snapshots(&world).unwrap();
        let emit = start.elapsed();

        let start = Instant::now();
        let mut by_region: HashMap<IVec3, Vec<&MicroChunkSnapshot>> = HashMap::new();
        for snapshot in &snapshots {
            by_region
                .entry(region_index_of(snapshot.global_coords))
                .or_default()
                .push(snapshot);
        }

        // The collected regions are the stage's measured work, not a needless intermediate.
        #[allow(clippy::needless_collect)]
        let packed: Vec<RegionData> = by_region
            .into_iter()
            .map(|(region_index, region_snapshots)| {
                pack_region(region_index, &region_snapshots).unwrap()
            })
            .collect();
        let pack = start.elapsed();

        let total = parse
            .saturating_add(world_new)
            .saturating_add(emit)
            .saturating_add(pack);

        let voxels = u64::try_from(world.voxel_count()).unwrap_or(0);
        let micro_chunks = u64::try_from(snapshots.len()).unwrap_or(0);

        println!("path            {path}");
        println!("voxels          {}", world.voxel_count());
        println!("clipped         {clipped}");
        println!("micro chunks    {}", snapshots.len());
        println!("regions         {}", packed.len());
        println!("parse           {parse:10.3?}");
        println!("world_new       {world_new:10.3?}");
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
