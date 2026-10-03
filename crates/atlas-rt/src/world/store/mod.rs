pub mod region;

use std::fmt::Debug;

use glam::IVec3;

use crate::world::{BoundsPolicy, InsertResult, grid};

pub use region::RegionStore;

/// A store's storage size: the flat Region table, every live Region's
/// Micro-chunk index, and every live Region's blob at its high-water mark.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StorageSize {
    pub(crate) table: usize,
    pub(crate) index: usize,
    pub(crate) blob: usize,
}

#[cfg(test)]
impl StorageSize {
    pub(crate) const fn total(self) -> usize {
        self.table
            .saturating_add(self.index)
            .saturating_add(self.blob)
    }
}

/// A Micro-chunk as a store holds it: the Occupancy mask and the materials of
/// the occupied cells in ascending cell order, borrowed from the store.
pub struct ChunkEntry<'a> {
    pub mask: &'a [u8],
    pub materials: &'a [u8],
}

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

    /// The Micro-chunk entry `origin` names, or `None` for a Micro-chunk with no
    /// entry.
    #[must_use]
    fn chunk_entry(&self, origin: IVec3) -> Option<ChunkEntry<'_>>;

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

    /// The Region store's three size terms.
    #[cfg(test)]
    fn storage_size(&self) -> StorageSize;

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
