use anyhow::Context;
use std::collections::HashMap;

use glam::{IVec3, UVec3};
use rustc_hash::FxBuildHasher;

use crate::world::{
    World,
    grid::{MICRO_CHUNK_LENGTH, grid_origin},
};

const MICRO_EDGE: i32 = MICRO_CHUNK_LENGTH.cast_signed();

const BUCKET_COUNT: usize = 256;
const X_ORDINALS_PER_BUCKET: u16 = 2;

const CHUNK_FIELD_BITS: u32 = 9;
const SLOT_FIELD_BITS: u32 = 9;
const MATERIAL_FIELD_BITS: u32 = 8;
const CHUNK_ID_SHIFT: u32 = SLOT_FIELD_BITS + MATERIAL_FIELD_BITS;
const CHUNK_ID_MASK: u64 = (1u64 << (3 * CHUNK_FIELD_BITS)) - 1;
const AXIS_MASK: u32 = (1u32 << CHUNK_FIELD_BITS) - 1;
const SLOT_MASK: u64 = (1u64 << SLOT_FIELD_BITS) - 1;
const MATERIAL_MASK: u64 = (1u64 << MATERIAL_FIELD_BITS) - 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroChunkSnapshot {
    pub global_coords: IVec3,
    pub mask: [u8; 64],
    pub materials: Vec<u8>,
}

impl MicroChunkSnapshot {
    #[allow(clippy::as_conversions)]
    #[must_use]
    pub fn occupied_count(&self) -> usize {
        self.mask
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum()
    }
}

#[derive(Clone, Copy)]
struct SlotGroup {
    materials: [u8; 8],
    occupied: u8,
}

struct ChunkBuf {
    groups: [SlotGroup; 64],
}

impl ChunkBuf {
    const fn new() -> Self {
        Self {
            groups: [SlotGroup {
                materials: [0; 8],
                occupied: 0,
            }; 64],
        }
    }

    fn record(&mut self, idx: u16, material: u8) -> anyhow::Result<()> {
        let group = self
            .groups
            .get_mut(usize::from(idx >> 3))
            .with_context(|| format!("mask byte for cell {idx} out of range"))?;
        group.occupied |= 1 << (idx & 7);

        let slot = group
            .materials
            .get_mut(usize::from(idx & 7))
            .with_context(|| format!("material slot for cell {idx} out of range"))?;
        *slot = material;

        Ok(())
    }

    fn into_snapshot(self, global_coords: IVec3) -> anyhow::Result<MicroChunkSnapshot> {
        let mut mask = [0u8; 64];

        for (byte, group) in mask.iter_mut().zip(self.groups.iter()) {
            *byte = group.occupied;
        }

        let occupied: u32 = mask.iter().map(|byte| byte.count_ones()).sum();
        let mut materials = Vec::with_capacity(usize::try_from(occupied)?);

        for group in &self.groups {
            let mut bits = group.occupied;

            while bits != 0 {
                let bit = usize::try_from(bits.trailing_zeros())?;
                let material = *group
                    .materials
                    .get(bit)
                    .with_context(|| format!("material slot for bit {bit} out of range"))?;
                materials.push(material);
                bits &= bits.strict_sub(1);
            }
        }

        let snapshot = MicroChunkSnapshot {
            global_coords,
            mask,
            materials,
        };

        debug_assert_eq!(snapshot.materials.len(), snapshot.occupied_count());

        Ok(snapshot)
    }
}

fn chunk_axis_ordinal(origin_axis: i32) -> anyhow::Result<u16> {
    let biased = (origin_axis >> 3)
        .checked_add(256)
        .context("voxel outside the micro chunk ordinal range")?;

    u16::try_from(biased).context("chunk ordinal out of range")
}

fn origin_axis(biased: u32) -> anyhow::Result<i32> {
    Ok(i32::try_from(biased)
        .context("chunk ordinal out of range")?
        .checked_sub(256)
        .context("chunk ordinal out of lattice range")?
        .strict_mul(MICRO_EDGE))
}

/// # Errors
///
/// Returns an error if unsigned grid operation, material index, chunk bucketing, or snapshot creation failed.
pub fn emit_snapshots(world: &World) -> anyhow::Result<Vec<MicroChunkSnapshot>> {
    let mut buckets: [Vec<u64>; BUCKET_COUNT] = std::array::from_fn(|_| Vec::new());

    let total = world.voxel_count();
    for bucket in &mut buckets {
        bucket.reserve(total / BUCKET_COUNT);
    }

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

        let material = u8::try_from(*voxel)?;

        let chunk_x = chunk_axis_ordinal(origin.x)?;
        let chunk_y = chunk_axis_ordinal(origin.y)?;
        let chunk_z = chunk_axis_ordinal(origin.z)?;

        let idx = u16::try_from(
            local
                .x
                .strict_add(local.y.strict_mul(8))
                .strict_add(local.z.strict_mul(64)),
        )?;

        let chunk_id = (u64::from(chunk_x) << (2 * CHUNK_FIELD_BITS))
            | (u64::from(chunk_y) << CHUNK_FIELD_BITS)
            | u64::from(chunk_z);
        let record = (chunk_id << CHUNK_ID_SHIFT)
            | (u64::from(idx) << MATERIAL_FIELD_BITS)
            | u64::from(material);

        buckets
            .get_mut(usize::from(chunk_x / X_ORDINALS_PER_BUCKET))
            .context("x slab bucket out of range")?
            .push(record);
    }

    let mut snapshots: Vec<MicroChunkSnapshot> = Vec::new();

    for records in buckets {
        if records.is_empty() {
            continue;
        }

        let mut chunks: HashMap<u32, ChunkBuf, FxBuildHasher> = HashMap::default();

        for record in records {
            let chunk_id = u32::try_from((record >> CHUNK_ID_SHIFT) & CHUNK_ID_MASK)
                .context("chunk id bits out of range")?;
            let idx = u16::try_from((record >> MATERIAL_FIELD_BITS) & SLOT_MASK)
                .context("slot idx out of range")?;
            let material =
                u8::try_from(record & MATERIAL_MASK).context("material bits out of range")?;

            chunks
                .entry(chunk_id)
                .or_insert_with(ChunkBuf::new)
                .record(idx, material)?;
        }

        for (chunk_id, chunk) in chunks {
            let origin = IVec3::new(
                origin_axis(chunk_id >> (2 * CHUNK_FIELD_BITS))?,
                origin_axis((chunk_id >> CHUNK_FIELD_BITS) & AXIS_MASK)?,
                origin_axis(chunk_id & AXIS_MASK)?,
            );

            snapshots.push(chunk.into_snapshot(origin)?);
        }
    }

    snapshots.sort_unstable_by_key(|s| s.global_coords.to_array());

    Ok(snapshots)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::world::testing::Rng;

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

            per_microchunk
                .entry(origin)
                .or_default()
                .push((idx, u8::try_from(*voxel)?));
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

    fn u32_below(rng: &mut Rng, bound: u64) -> u32 {
        u32::try_from(rng.below(bound)).unwrap_or(u32::MAX)
    }

    fn random_world(rng: &mut Rng) -> World {
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

                        world.insert_voxel_at(center + IVec3::new(dx, dy, dz), u32_below(rng, 256));
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

            world.insert_voxel_at(position, u32_below(rng, 256));
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
                    world.insert_voxel_at(IVec3::new(x, y, z), 3);
                }
            }
        }

        world.insert_voxel_at(IVec3::new(-1, -1, -1), 5);

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
        world.insert_voxel_at(IVec3::new(0, 0, 0), 1); // idx 0
        world.insert_voxel_at(IVec3::new(7, 0, 0), 2); // idx 7
        world.insert_voxel_at(IVec3::new(0, 1, 0), 3); // idx 8
        world.insert_voxel_at(IVec3::new(0, 0, 1), 4); // idx 64

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
    #[ignore = "asset: cargo test --release church_matches_two_pass_oracle -- --ignored --nocapture"]
    fn church_matches_two_pass_oracle() {
        let data = dot_vox::load("assets/church.vox").unwrap();
        let (world, _) = World::new_clipped(&data);

        assert_eq!(
            emit_snapshots(&world).unwrap(),
            emit_snapshots_two_pass(&world).unwrap()
        );
    }

    #[test]
    #[ignore = "asset: cargo test --release bistro_matches_two_pass_oracle -- --ignored --nocapture"]
    fn bistro_matches_two_pass_oracle() {
        let data = dot_vox::load("assets/bistro.vox").unwrap();
        let (world, _) = World::new_clipped(&data);

        assert_eq!(
            emit_snapshots(&world).unwrap(),
            emit_snapshots_two_pass(&world).unwrap()
        );
    }
}
