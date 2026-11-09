pub mod region;

use glam::IVec3;

use crate::world::micro::MicroChunk;

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

/// A live Micro-chunk as enumeration yields it: the origin and the payload of
/// the occupied cells in ascending cell order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroChunkEntry {
    pub origin: IVec3,
    pub chunk: MicroChunk,
}
