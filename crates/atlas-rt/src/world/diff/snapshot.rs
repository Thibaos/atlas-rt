use glam::IVec3;

use crate::world::{
    World,
    load::progress::{Progress, VOXEL_STEP},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroChunkSnapshot {
    pub global_coords: IVec3,
    pub mask: [u8; 64],
    pub materials: Vec<u8>,
}

impl MicroChunkSnapshot {
    /// A Snapshot that empties its Micro-chunk.
    #[must_use]
    pub const fn cleared(global_coords: IVec3) -> Self {
        Self {
            global_coords,
            mask: [0u8; 64],
            materials: Vec::new(),
        }
    }

    #[must_use]
    pub fn occupied_count(&self) -> usize {
        self.mask
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum()
    }
}

/// # Errors
///
/// Infallible today; the fallible shape is the load pipeline's.
pub fn emit_snapshots(world: &World) -> anyhow::Result<Vec<MicroChunkSnapshot>> {
    emit_snapshots_reporting(world, None)
}

/// Emits the world's snapshots from its live Micro-chunk entries, reporting how
/// far the read has got.
///
/// # Errors
///
/// See [`emit_snapshots`]; the entry read cannot refuse.
pub fn emit_snapshots_reporting(
    world: &World,
    progress: Option<&Progress>,
) -> anyhow::Result<Vec<MicroChunkSnapshot>> {
    if let Some(progress) = progress {
        progress.start_emit();
    }

    let total = world.voxel_count();
    let mut seen = 0usize;
    let mut next_step = VOXEL_STEP;
    let mut snapshots: Vec<MicroChunkSnapshot> = Vec::new();

    for entry in world.entries() {
        if let Some(progress) = progress {
            seen = seen.saturating_add(entry.materials.len());

            while seen >= next_step {
                progress.count_voxel(total, next_step);
                next_step = next_step.saturating_add(VOXEL_STEP);
            }
        }

        snapshots.push(MicroChunkSnapshot {
            global_coords: entry.origin,
            mask: entry.mask,
            materials: entry.materials,
        });
    }

    snapshots.sort_unstable_by_key(|snapshot| snapshot.global_coords.to_array());

    if let Some(progress) = progress {
        progress.end_emit();
    }

    Ok(snapshots)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use anyhow::Context;
    use glam::UVec3;
    use rustc_hash::FxBuildHasher;

    use crate::world::{
        grid::{MICRO_CHUNK_LENGTH, grid_origin},
        test_support::{Rng, u8_below},
    };

    use super::*;

    fn emit_snapshots_two_pass(world: &World) -> anyhow::Result<Vec<MicroChunkSnapshot>> {
        let mut per_microchunk: HashMap<IVec3, Vec<(u32, u8)>, FxBuildHasher> = HashMap::default();

        for (global, voxel) in world.iter_voxels() {
            let origin = grid_origin(global, MICRO_CHUNK_LENGTH);
            let local = global
                .checked_sub(origin)
                .context("voxel below its micro chunk origin")?;

            debug_assert!(local.cmpge(IVec3::ZERO).all());
            debug_assert!(
                local
                    .cmplt(UVec3::splat(MICRO_CHUNK_LENGTH).as_ivec3())
                    .all()
            );

            let idx = u32::try_from(
                local
                    .x
                    .strict_add(local.y.strict_mul(8))
                    .strict_add(local.z.strict_mul(64)),
            )?;

            per_microchunk.entry(origin).or_default().push((idx, voxel));
        }

        let mut snapshots: Vec<MicroChunkSnapshot> = per_microchunk
            .into_iter()
            .map(
                |(global_coords, mut cells)| -> anyhow::Result<MicroChunkSnapshot> {
                    cells.sort_unstable_by_key(|&(idx, _)| idx);

                    let mut mask = [0u8; 64];
                    let mut materials = Vec::with_capacity(cells.len());

                    for (idx, material) in cells {
                        let slot = mask
                            .get_mut(usize::try_from(idx / 8)?)
                            .with_context(|| format!("mask byte for cell {idx} out of range"))?;
                        *slot |= 1 << (idx % 8);
                        materials.push(material);
                    }

                    let snapshot = MicroChunkSnapshot {
                        global_coords,
                        mask,
                        materials,
                    };

                    debug_assert_eq!(snapshot.materials.len(), snapshot.occupied_count());

                    Ok(snapshot)
                },
            )
            .collect::<anyhow::Result<_>>()?;

        snapshots.sort_unstable_by_key(|s| s.global_coords.to_array());

        Ok(snapshots)
    }

    fn i32_below(rng: &mut Rng, bound: u64) -> i32 {
        i32::try_from(rng.below(bound)).unwrap_or(i32::MAX)
    }

    pub(crate) fn random_world(rng: &mut Rng) -> World {
        let mut world = World::default();

        for _ in 0..rng.below(8).saturating_add(1) {
            let center = IVec3::new(
                i32_below(rng, 96).saturating_sub(48),
                i32_below(rng, 96).saturating_sub(48),
                i32_below(rng, 96).saturating_sub(48),
            );

            let extent = IVec3::splat(i32_below(rng, 12).saturating_add(1));

            for dx in 0..extent.x {
                for dy in 0..extent.y {
                    for dz in 0..extent.z {
                        if rng.below(4) == 0 {
                            continue;
                        }

                        world.set_voxel(center + IVec3::new(dx, dy, dz), u8_below(rng, 256));
                    }
                }
            }
        }

        for _ in 0..rng.below(24).saturating_add(1) {
            let position = IVec3::new(
                i32_below(rng, 128).saturating_sub(64),
                i32_below(rng, 128).saturating_sub(64),
                i32_below(rng, 128).saturating_sub(64),
            );

            world.set_voxel(position, u8_below(rng, 256));
        }

        world
    }

    #[test]
    fn mask_bit_convention() {
        let mut mask = [0u8; 64];
        for idx in [0u32, 1, 7, 8, 63, 64, 511] {
            mask[(idx / 8) as usize] |= 1 << (idx % 8);
        }
        for idx in 0..512u32 {
            let byte = mask[(idx / 8) as usize];
            assert_eq!(
                (byte >> (idx % 8)) & 1 != 0,
                matches!(idx, 0 | 1 | 7 | 8 | 63 | 64 | 511)
            );
        }
    }

    #[test]
    fn emitter_covers_all_voxels() {
        let mut world = World::default();
        for x in 0..10 {
            for y in 0..3 {
                for z in 0..3 {
                    world.set_voxel(IVec3::new(x, y, z), 3);
                }
            }
        }

        world.set_voxel(IVec3::new(-1, -1, -1), 5);

        let snapshots = emit_snapshots(&world).unwrap();
        let total: usize = snapshots.iter().map(|s| s.occupied_count()).sum();
        assert_eq!(total, world.voxel_count());

        assert!(snapshots.iter().any(|s| {
            s.global_coords == IVec3::new(-8, -8, -8) && s.mask[63] & 0b1000_0000 != 0
        }));
    }

    #[test]
    fn materials_in_bit_order() {
        let mut world = World::default();
        world.set_voxel(IVec3::new(0, 0, 0), 1); // idx 0
        world.set_voxel(IVec3::new(7, 0, 0), 2); // idx 7
        world.set_voxel(IVec3::new(0, 1, 0), 3); // idx 8
        world.set_voxel(IVec3::new(0, 0, 1), 4); // idx 64

        let snapshots = emit_snapshots(&world).unwrap();
        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.materials, vec![1, 2, 3, 4]);
        assert_eq!(snapshot.occupied_count(), 4);
    }

    #[test]
    fn single_pass_matches_two_pass_oracle_on_random_worlds() {
        let mut rng = Rng::new(0x00C0_FFEE);

        for case in 0..64u32 {
            let world = random_world(&mut rng);
            assert_eq!(
                emit_snapshots(&world).unwrap(),
                emit_snapshots_two_pass(&world).unwrap(),
                "case {case}"
            );
        }
    }

    #[test]
    fn entry_emission_matches_the_voxel_walk_across_regions() {
        let mut world = World::default();
        let region_length = 256;

        for region in -1..=1 {
            for chunk in 0..3 {
                let origin = IVec3::new(region * region_length + chunk * 8, 0, 0);

                for cell in 0..512usize {
                    let x = (cell % 8) as i32;
                    let y = ((cell / 8) % 8) as i32;
                    let z = (cell / 64) as i32;

                    world.set_voxel(origin + IVec3::new(x, y, z), (cell % 200) as u8);
                }
            }
        }

        let snapshots = emit_snapshots(&world).unwrap();

        assert_eq!(snapshots.len(), 9, "three Regions hold three chunks each");
        assert_eq!(snapshots, emit_snapshots_two_pass(&world).unwrap());
    }

    #[test]
    #[ignore = "asset: cargo test --release church_matches_two_pass_oracle -- --ignored --nocapture"]
    fn church_matches_two_pass_oracle() {
        let data = dot_vox::load("assets/church.vox").unwrap();
        let world = World::new(&data);

        assert_eq!(
            emit_snapshots(&world).unwrap(),
            emit_snapshots_two_pass(&world).unwrap()
        );
    }

    #[test]
    #[ignore = "asset: cargo test --release bistro_matches_two_pass_oracle -- --ignored --nocapture"]
    fn bistro_matches_two_pass_oracle() {
        let data = dot_vox::load("assets/bistro.vox").unwrap();
        let world = World::new(&data);

        assert_eq!(
            emit_snapshots(&world).unwrap(),
            emit_snapshots_two_pass(&world).unwrap()
        );
    }
}
