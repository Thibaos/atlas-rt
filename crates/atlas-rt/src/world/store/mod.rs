pub mod region;

use std::fmt::Debug;

use glam::IVec3;
use rustc_hash::FxHashMap;

use crate::world::{
    BoundsPolicy, InsertResult,
    diff::edit::{
        EditError, MICRO_BYTES, MICRO_CELLS, MICRO_EDGE, cell_offset, mask_index, mask_occupied,
        validate_entry,
    },
    grid,
};

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

/// A live Micro-chunk as enumeration yields it: the origin, the Occupancy mask
/// and the materials of the occupied cells in ascending cell order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroChunkEntry {
    pub origin: IVec3,
    pub mask: [u8; MICRO_BYTES],
    pub materials: Vec<u8>,
}

/// A Micro-chunk under construction during the default enumeration's voxel
/// walk: the occupancy and the material at each cell. Cell order recovers the
/// entry's compacted materials.
struct Bucket {
    mask: [u8; MICRO_BYTES],
    materials: [u8; MICRO_CELLS],
}

impl Bucket {
    const fn new() -> Self {
        Self {
            mask: [0u8; MICRO_BYTES],
            materials: [0u8; MICRO_CELLS],
        }
    }

    fn record(&mut self, index: usize, material: u8) {
        if let Some(byte) = self.mask.get_mut(index / MICRO_EDGE) {
            *byte |= 1u8 << (index % MICRO_EDGE);
        }

        if let Some(slot) = self.materials.get_mut(index) {
            *slot = material;
        }
    }

    fn into_entry(self, origin: IVec3) -> MicroChunkEntry {
        let mut materials = Vec::new();

        for (index, material) in self.materials.iter().enumerate() {
            if mask_occupied(&self.mask, index) {
                materials.push(*material);
            }
        }

        MicroChunkEntry {
            origin,
            mask: self.mask,
            materials,
        }
    }
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

    /// Enumerates the live Micro-chunk entries in Region then Micro-chunk
    /// ordinal order.
    ///
    /// The default walks [`Self::iter`] and buckets by Micro-chunk origin, so a
    /// store that only implements the primitives enumerates correctly and stays
    /// comparable to the store that overrides it.
    fn entries(&self) -> Box<dyn Iterator<Item = MicroChunkEntry> + '_> {
        let mut buckets: FxHashMap<IVec3, Bucket> = FxHashMap::default();
        let mut order: Vec<IVec3> = Vec::new();

        for (position, material) in self.iter() {
            let origin = grid::grid_origin(position, grid::MICRO_CHUNK_LENGTH);
            let index = mask_index(position.saturating_sub(origin));

            let bucket = buckets.entry(origin).or_insert_with(|| {
                order.push(origin);
                Bucket::new()
            });

            bucket.record(index, material);
        }

        let entries: Vec<MicroChunkEntry> = order
            .into_iter()
            .filter_map(|origin| buckets.remove(&origin).map(|bucket| bucket.into_entry(origin)))
            .collect();

        Box::new(entries.into_iter())
    }

    /// Writes a Micro-chunk's whole entry: its 64-byte Occupancy mask and the
    /// materials of its occupied cells in ascending cell order. A zero mask
    /// empties the Micro-chunk.
    ///
    /// The default clears every cell of the Micro-chunk and sets the occupied
    /// ones through [`Self::set`], so a store that does not override stays
    /// correct.
    ///
    /// # Errors
    ///
    /// Rejects an origin outside the lattice or off the Micro-chunk grid, and
    /// materials whose count does not match the mask's popcount.
    fn write_entry(
        &mut self,
        origin: IVec3,
        mask: &[u8; MICRO_BYTES],
        materials: &[u8],
    ) -> Result<(), EditError> {
        validate_entry(origin, mask, materials)?;

        let mut next = 0usize;

        for index in 0..MICRO_CELLS {
            let position = origin.saturating_add(cell_offset(index));

            if mask_occupied(mask, index) {
                if let Some(material) = materials.get(next).copied() {
                    self.set(position, material);
                }

                next = next.saturating_add(1);
            } else {
                self.clear(position);
            }
        }

        Ok(())
    }

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
