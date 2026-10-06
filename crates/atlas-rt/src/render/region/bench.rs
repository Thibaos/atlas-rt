//! GPU rebuild timings for the Region store.
//!
//! `RenderContext::new_headless` creates a device with no surface, so the
//! renderer's rebuild runs in a bench. This is the path the Godot example cannot
//! take `--headless`; atlas-rt's own device does not have that restriction.
//!
//! `gpu_rebuild_timings`, release, 2026-10-02, RTX 3070, headless. church.vox:
//! 14.3M voxels, 222,791 Micro-chunks, 112 Regions. `pack` is the input worker's
//! whole-region repack (`wait_until_idle`); `apply` is the store's decide, plan,
//! taskgraph compile, and GPU execute.
//!
//! | work               | pack   | apply | plan                        |
//! | ------------------ | ------ | ----- | --------------------------- |
//! | load, 112 Regions  | 104 ms | 39 ms | 112 uploads, 112 BLAS, TLAS |
//! | update, 1 Region   | 1.3 ms | 0.6 ms| 1 upload, 1 BLAS            |
//!
//! The update row is the widest Region, 4,908 Micro-chunks, over five passes of
//! a material-only edit and five of a shape edit. The two are within noise
//! because the apply rebuilds the BLAS in place either way; `apply` ranged from
//! 0.5 to 2.0 ms across passes, so it is quoted as its median. A single-Region
//! rebuild is CPU-pack-bound, about 270 ns per Micro-chunk serial against the
//! parallel `pack_regions` figure of about 100 ns in `edit_workload_timings`.

mod rebuild_bench {
    use std::time::Instant;

    use glam::IVec3;
    use rustc_hash::FxHashMap;

    use crate::{
        render::{
            context::RenderContext,
            region::{
                queue::RendererInput,
                rebuild::RebuildLogEntry,
                residency::{ApplyReport, RegionStore},
            },
        },
        world::{
            World,
            diff::snapshot::{MicroChunkSnapshot, emit_snapshots},
            grid::region_index_of,
        },
    };

    const PATH: &str = "assets/church.vox";
    const REPEATS: usize = 5;

    fn by_region(snapshots: &[MicroChunkSnapshot]) -> FxHashMap<IVec3, Vec<MicroChunkSnapshot>> {
        let mut regions: FxHashMap<IVec3, Vec<MicroChunkSnapshot>> = FxHashMap::default();

        for snapshot in snapshots {
            regions
                .entry(region_index_of(snapshot.global_coords))
                .or_default()
                .push(snapshot.clone());
        }

        regions
    }

    /// The same content with every material advanced by one. The geometry is
    /// unchanged, so the pool buffer is re-uploaded and the BLAS is reused.
    fn paint(mut snapshots: Vec<MicroChunkSnapshot>) -> Vec<MicroChunkSnapshot> {
        for snapshot in &mut snapshots {
            for material in &mut snapshot.materials {
                *material = material.wrapping_add(1);
            }
        }

        snapshots
    }

    /// Cell 0 toggled in every Micro-chunk, so the hull moves and the BLAS has
    /// to rebuild. A chunk that holds only cell 0 is left alone, so the region
    /// never empties.
    fn reshape(mut snapshots: Vec<MicroChunkSnapshot>) -> Vec<MicroChunkSnapshot> {
        for snapshot in &mut snapshots {
            if snapshot.mask[0] & 1 != 0 {
                if snapshot.occupied_count() <= 1 {
                    continue;
                }

                snapshot.mask[0] &= !1;
                snapshot.materials.remove(0);
            } else {
                snapshot.mask[0] |= 1;
                snapshot.materials.insert(0, 7);
            }
        }

        snapshots
    }

    /// The plan the apply built: uploads, BLAS builds, and whether a TLAS
    /// rebuild is in it.
    fn plan_counts(report: &ApplyReport) -> (usize, usize, bool) {
        let uploads = report
            .rebuild_log
            .iter()
            .filter(|entry| matches!(entry, RebuildLogEntry::Upload { .. }))
            .count();
        let blas = report
            .rebuild_log
            .iter()
            .filter(|entry| matches!(entry, RebuildLogEntry::BuildBlas { .. }))
            .count();
        let tlas = report
            .rebuild_log
            .iter()
            .any(|entry| matches!(entry, RebuildLogEntry::BuildTlas { .. }));

        (uploads, blas, tlas)
    }

    #[test]
    #[ignore = "gpu: cargo test --release gpu_rebuild_timings -- --ignored --nocapture"]
    fn gpu_rebuild_timings() {
        let gpu = match RenderContext::new_headless() {
            Ok(gpu) => gpu,
            Err(error) => {
                println!("no headless device ({error}), skipping");

                return;
            }
        };

        println!(
            "device          {}",
            gpu.device.physical_device().properties().device_name
        );

        let data = dot_vox::load(PATH).unwrap();
        let world = World::new(&data);
        let snapshots = emit_snapshots(&world).unwrap();
        let mut regions = by_region(&snapshots);
        let mut region_list: Vec<IVec3> = regions.keys().copied().collect();

        region_list.sort_unstable_by_key(IVec3::to_array);

        println!("voxels          {}", world.voxel_count());
        println!("snapshots       {}", snapshots.len());
        println!("regions         {}", region_list.len());

        let input = RendererInput::new().unwrap();
        let mut store = RegionStore::new_empty(&gpu).unwrap();

        input.submit_batch(snapshots).unwrap();

        let wait = Instant::now();
        input.wait_until_idle().unwrap();
        let pack = wait.elapsed();

        let start = Instant::now();
        let report = store.apply(&gpu, &input).unwrap();
        let apply = start.elapsed();

        let (uploads, blas, tlas) = plan_counts(&report);

        println!(
            "load            pack {pack:>10.3?} apply {apply:>10.3?}  entered {} uploads {uploads} blas {blas} tlas {tlas}",
            report.became_resident.len()
        );

        let widest = region_list
            .iter()
            .copied()
            .max_by_key(|region| regions[region].len())
            .unwrap_or(IVec3::ZERO);
        let chunks = regions.get(&widest).map_or(0, Vec::len);

        println!();
        println!(
            "{:<7} {:<6} {:>10} {:>10} {:>10} {:>7} {:>7} {:>7} {:>5}",
            "rebuild", "pass", "pack", "apply", "total", "dirty", "uploads", "blas", "tlas"
        );

        let transforms: [(&str, fn(Vec<MicroChunkSnapshot>) -> Vec<MicroChunkSnapshot>); 2] =
            [("paint", paint), ("shape", reshape)];

        for (kind, transform) in transforms {
            for pass in 0..REPEATS {
                let batch = transform(regions.remove(&widest).unwrap_or_default());

                input.submit_batch(batch.clone()).unwrap();

                let wait = Instant::now();
                input.wait_until_idle().unwrap();
                let pack = wait.elapsed();

                let start = Instant::now();
                let report = store.apply(&gpu, &input).unwrap();
                let apply = start.elapsed();

                regions.insert(widest, batch);

                let (uploads, blas, tlas) = plan_counts(&report);

                println!(
                    "{kind:<7} {pass:<6} {pack:>10.3?} {apply:>10.3?} {:>10.3?} {:>7} {uploads:>7} {blas:>7} {tlas:>5}",
                    pack + apply,
                    report.dirty.len()
                );
            }
        }

        println!("widest region   {widest} with {chunks} chunks");
    }
}
