pub mod region;
#[cfg(feature = "map-oracle")]
pub mod sharded;

use std::fmt::Debug;

use glam::IVec3;

use crate::world::{BoundsPolicy, InsertResult, grid};

pub use region::RegionStore;
#[cfg(feature = "map-oracle")]
pub use sharded::ShardedMap;

/// The World's voxel storage.
///
/// Positions are in-lattice. `insert` is the one operation that resolves the
/// `BoundsPolicy`, because it is the one that reports `Clipped`; the World
/// checks the lattice for every other operation.
pub trait VoxelStore: Debug + Send + Sync {
    fn set(&mut self, position: IVec3, material: u8) -> bool;

    #[must_use]
    fn get(&self, position: IVec3) -> Option<u8>;

    #[must_use]
    fn contains(&self, position: IVec3) -> bool {
        self.get(position).is_some()
    }

    fn clear(&mut self, position: IVec3);

    fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_>;

    #[must_use]
    fn count(&self) -> usize;

    #[must_use]
    fn bounds(&self) -> Option<(IVec3, IVec3)> {
        let mut bounds: Option<(IVec3, IVec3)> = None;

        for (position, _) in self.iter() {
            bounds = Some(match bounds {
                Some((min, max)) => (min.min(position), max.max(position)),
                None => (position, position),
            });
        }

        bounds
    }

    #[cfg(test)]
    fn reserved_capacity(&self) -> usize;

    /// The World's `insert`: the `BoundsPolicy` is resolved and the material
    /// narrowed before the write. A clipped position never reaches storage.
    ///
    /// # Panics
    ///
    /// Panics on a `Panic`-policy position outside the lattice, and on a
    /// material index that does not fit a byte.
    fn insert(&mut self, position: IVec3, voxel: u32, policy: BoundsPolicy) -> InsertResult {
        if !grid::in_lattice(position) {
            match policy {
                BoundsPolicy::Panic => grid::assert_in_lattice(position),
                BoundsPolicy::Clip => return InsertResult::Clipped,
            }
        }

        let material = u8::try_from(voxel)
            .unwrap_or_else(|_| panic!("material index {voxel} does not fit a byte"));

        if self.set(position, material) {
            InsertResult::Existing
        } else {
            InsertResult::Ok
        }
    }
}
