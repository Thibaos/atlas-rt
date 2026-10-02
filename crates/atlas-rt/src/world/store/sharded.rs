use std::collections::HashMap;

use glam::IVec3;
use rustc_hash::FxBuildHasher;

use crate::world::{grid, store::VoxelStore};

pub const SHARD_COUNT: usize = 64;
const SHARD_ROUTE_SHIFT: u32 = 64 - SHARD_COUNT.trailing_zeros();

const LATTICE_BIAS: i32 = grid::LATTICE_HALF_EXTENT.cast_signed();
const FOLD_FIELD_BITS: u32 = grid::LATTICE_HALF_EXTENT.trailing_zeros() + 1;
const FOLD_FIELD_MASK: u64 = (1u64 << FOLD_FIELD_BITS) - 1;

pub type VoxelMap = HashMap<u64, u8, FxBuildHasher>;

/// One hash map per shard, routed by the top bits of the fold key.
#[derive(Debug)]
pub struct ShardedMap {
    shards: [VoxelMap; SHARD_COUNT],
}

impl Default for ShardedMap {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| HashMap::default()),
        }
    }
}

impl ShardedMap {
    fn shard(&self, position: IVec3) -> &VoxelMap {
        let index = shard_index(fold(position));

        self.shards
            .get(index)
            .unwrap_or_else(|| panic!("shard {index} out of the {SHARD_COUNT} shards"))
    }

    fn shard_mut(&mut self, position: IVec3) -> &mut VoxelMap {
        let index = shard_index(fold(position));

        self.shards
            .get_mut(index)
            .unwrap_or_else(|| panic!("shard {index} out of the {SHARD_COUNT} shards"))
    }
}

// Bijective 36-bit fold: three biased 12-bit axis fields, x high.
#[must_use]
pub fn fold(position: IVec3) -> u64 {
    let biased = position.wrapping_add(IVec3::splat(LATTICE_BIAS)).as_uvec3();

    (u64::from(biased.x) << (2 * FOLD_FIELD_BITS))
        | (u64::from(biased.y) << FOLD_FIELD_BITS)
        | u64::from(biased.z)
}

#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub fn unfold(key: u64) -> IVec3 {
    let axis = |field: u64| (field as i32).wrapping_sub(LATTICE_BIAS);

    IVec3::new(
        axis((key >> (2 * FOLD_FIELD_BITS)) & FOLD_FIELD_MASK),
        axis((key >> FOLD_FIELD_BITS) & FOLD_FIELD_MASK),
        axis(key & FOLD_FIELD_MASK),
    )
}

#[must_use]
pub const fn shard_index(key: u64) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> SHARD_ROUTE_SHIFT) as usize
}

impl VoxelStore for ShardedMap {
    fn set(&mut self, position: IVec3, material: u8) -> bool {
        self.shard_mut(position)
            .insert(fold(position), material)
            .is_some()
    }

    fn get(&self, position: IVec3) -> Option<u8> {
        self.shard(position).get(&fold(position)).copied()
    }

    fn clear(&mut self, position: IVec3) {
        self.shard_mut(position).remove(&fold(position));
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_> {
        Box::new(
            self.shards
                .iter()
                .flat_map(|map| map.iter().map(|(key, voxel)| (unfold(*key), *voxel))),
        )
    }

    fn count(&self) -> usize {
        self.shards.iter().map(HashMap::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_round_trips_lattice_extremes() {
        for position in [
            IVec3::new(-2048, -2048, -2048),
            IVec3::new(2047, 2047, 2047),
            IVec3::new(-2048, 2047, 0),
            IVec3::new(0, -2048, 2047),
            IVec3::new(123, -456, 789),
            IVec3::ZERO,
        ] {
            assert!(grid::in_lattice(position));
            assert_eq!(unfold(fold(position)), position);
        }

        assert_ne!(
            fold(IVec3::new(-2048, -2048, -2048)),
            fold(IVec3::new(-2048, -2048, -2047))
        );
    }
}
