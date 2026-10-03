use std::fmt;

use glam::IVec3;

use crate::world::{
    grid,
    store::{ChunkEntry, VoxelStore},
};

#[cfg(test)]
use crate::world::store::StorageSize;

const MICRO_CHUNK: usize = grid::MICRO_CHUNK_LENGTH as usize;
const MICRO_CHUNK_SIDE: usize = (grid::REGION_LENGTH / grid::MICRO_CHUNK_LENGTH) as usize;
const MICRO_CHUNK_SIDE_AREA: usize = MICRO_CHUNK_SIDE * MICRO_CHUNK_SIDE;
const MICRO_CHUNK_AREA: usize = MICRO_CHUNK * MICRO_CHUNK;
const MICRO_CHUNK_CELLS: usize = MICRO_CHUNK * MICRO_CHUNK * MICRO_CHUNK;
const MICRO_CHUNKS_PER_REGION: usize = MICRO_CHUNK_SIDE * MICRO_CHUNK_SIDE * MICRO_CHUNK_SIDE;
const MASK_BYTES: usize = MICRO_CHUNK_CELLS / 8;

/// Blocks are 8-byte aligned and sized to an 8-byte class, so each index entry
/// carries both its offset and its class rather than needing a second table.
/// The low 25 bits hold `offset / 8` (a 256 MiB Region blob) and the top 7 hold
/// `class + 1`, leaving zero as the empty sentinel.
const BLOCK_ALIGN: usize = 8;
const CLASS_MIN: usize = 72;
const CLASS_STEP: usize = 8;
const CLASS_MAX: usize = 576;
const CLASS_COUNT: usize = (CLASS_MAX - CLASS_MIN) / CLASS_STEP + 1;
const OFFSET_BITS: u32 = 25;
const OFFSET_MASK: u32 = (1u32 << OFFSET_BITS) - 1;
const CLASS_SHIFT: u32 = OFFSET_BITS;
const NO_BLOCK: u32 = u32::MAX;

const EMPTY: u32 = 0;

/// The smallest class that holds `size` bytes, from a one-material Micro-chunk
/// up to a full 576-byte entry.
const fn class_of_size(size: usize) -> usize {
    size.saturating_sub(CLASS_MIN).div_ceil(CLASS_STEP)
}

const fn class_size(class: usize) -> usize {
    CLASS_MIN.strict_add(class.strict_mul(CLASS_STEP))
}

fn encode(offset: usize, class: usize) -> u32 {
    let scaled = u32::try_from(offset.strict_div(BLOCK_ALIGN))
        .ok()
        .filter(|value| *value <= OFFSET_MASK)
        .unwrap_or_else(|| panic!("Region blob offset {offset} exceeds the index range"));

    let field = u32::try_from(class.strict_add(1)).unwrap_or(u32::MAX);

    field.wrapping_shl(CLASS_SHIFT) | scaled
}

fn entry_offset(entry: u32) -> Option<usize> {
    if entry == EMPTY {
        return None;
    }

    Some(
        usize::try_from(entry & OFFSET_MASK)
            .unwrap_or(0)
            .strict_mul(BLOCK_ALIGN),
    )
}

fn entry_class(entry: u32) -> usize {
    let field = entry >> CLASS_SHIFT;

    usize::try_from(field.saturating_sub(1)).unwrap_or(0)
}

/// A live Region: one packed offset and class per Micro-chunk, plus the occupied
/// Micro-chunks concatenated. Each entry is a 64-byte Occupancy mask followed
/// by the material indices of its occupied cells in ascending cell order.
///
/// The blob is never compacted. An emptied entry goes onto its size class's
/// free list at its existing offset, and a later first write pops an exact-fit
/// block from that list before appending.
pub(in crate::world) struct Region {
    index: Box<[u32]>,
    blob: Vec<u8>,
    free: [u32; CLASS_COUNT],
    count: usize,
}

impl Region {
    pub(in crate::world) fn new() -> Self {
        Self {
            index: vec![EMPTY; MICRO_CHUNKS_PER_REGION].into_boxed_slice(),
            blob: Vec::new(),
            free: [NO_BLOCK; CLASS_COUNT],
            count: 0,
        }
    }

    /// Writes one cell. An occupied cell overwrites its material in place, a
    /// clear one inserts at its rank. Returns whether the cell was occupied.
    pub(in crate::world) fn set(&mut self, position: IVec3, material: u8) -> bool {
        let ordinal = micro_chunk_ordinal(position);
        let entry = self.index.get(ordinal).copied().unwrap_or(EMPTY);

        if entry == EMPTY {
            write_new_entry(self, ordinal, position, material);
            self.count = self.count.saturating_add(1);

            return false;
        }

        let offset = entry_offset(entry).unwrap_or(0);
        let class = entry_class(entry);
        let (byte, bit) = mask_bit(position);
        let occupied = self.blob.get(offset.strict_add(byte)).copied().unwrap_or(0) & bit != 0;

        if occupied {
            let cell = cell_index(position);
            let at = offset
                .strict_add(MASK_BYTES)
                .strict_add(rank(entry_mask(self, offset), cell));

            if let Some(slot) = self.blob.get_mut(at) {
                *slot = material;
            }

            return true;
        }

        let cell = cell_index(position);
        let used = MASK_BYTES.strict_add(entry_popcount(self, offset));
        let required = used.strict_add(1);

        let offset = if required > class_size(class) {
            let next_class = class_of_size(required);
            let new_offset = alloc_block(self, next_class);

            self.blob
                .copy_within(offset..offset.strict_add(used), new_offset);
            free_block(self, class, offset);

            if let Some(slot) = self.index.get_mut(ordinal) {
                *slot = encode(new_offset, next_class);
            }

            new_offset
        } else {
            offset
        };

        let at = offset
            .strict_add(MASK_BYTES)
            .strict_add(rank(entry_mask(self, offset), cell));
        let content_end = offset.strict_add(used);

        self.blob.copy_within(at..content_end, at.strict_add(1));

        if let Some(slot) = self.blob.get_mut(offset.strict_add(byte)) {
            *slot |= bit;
        }

        if let Some(slot) = self.blob.get_mut(at) {
            *slot = material;
        }

        self.count = self.count.saturating_add(1);

        false
    }
}

fn read_head(blob: &[u8], offset: usize) -> u32 {
    let Some(bytes) = blob.get(offset..offset.strict_add(4)) else {
        return NO_BLOCK;
    };

    let mut array = [0u8; 4];
    array.copy_from_slice(bytes);

    u32::from_le_bytes(array)
}

fn write_head(blob: &mut [u8], offset: usize, value: u32) {
    if let Some(slot) = blob.get_mut(offset..offset.strict_add(4)) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
}

fn pop_block(region: &mut Region, class: usize) -> Option<usize> {
    let head = region.free.get(class).copied().unwrap_or(NO_BLOCK);

    if head == NO_BLOCK {
        return None;
    }

    let offset = usize::try_from(head).unwrap_or(0);
    let next = read_head(&region.blob, offset);

    if let Some(slot) = region.free.get_mut(class) {
        *slot = next;
    }

    Some(offset)
}

fn alloc_block(region: &mut Region, class: usize) -> usize {
    if let Some(offset) = pop_block(region, class) {
        return offset;
    }

    let offset = region.blob.len();

    region.blob.resize(offset.strict_add(class_size(class)), 0);

    offset
}

fn free_block(region: &mut Region, class: usize, offset: usize) {
    let head = region.free.get(class).copied().unwrap_or(NO_BLOCK);

    write_head(&mut region.blob, offset, head);

    if let Some(slot) = region.free.get_mut(class) {
        *slot = u32::try_from(offset).unwrap_or(NO_BLOCK);
    }
}

/// The World's content as a flat table of Region slots, one per region id.
pub struct RegionStore {
    regions: Vec<Option<Region>>,
    count: usize,
}

impl Default for RegionStore {
    fn default() -> Self {
        Self {
            regions: (0..grid::REGION_COUNT).map(|_| None).collect(),
            count: 0,
        }
    }
}

impl fmt::Debug for RegionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let regions = self.regions.iter().filter(|slot| slot.is_some()).count();

        f.debug_struct("RegionStore")
            .field("regions", &regions)
            .field("voxels", &self.count)
            .finish()
    }
}

impl RegionStore {
    /// The flat Region table, one slot per region id. Used by the loader to
    /// give every Region exactly one owning thread, each with `&mut` to its own
    /// slot.
    pub(in crate::world) fn slots_mut(&mut self) -> &mut [Option<Region>] {
        &mut self.regions
    }

    /// Recomputes the cell counter from the live Regions after a build that
    /// wrote through [`slots_mut`].
    pub(in crate::world) fn recount(&mut self) {
        self.count = self
            .regions
            .iter()
            .flatten()
            .map(|region| region.count)
            .sum();
    }
}

/// The Region slot a position falls in, which is its region id.
pub(in crate::world) fn region_slot(position: IVec3) -> usize {
    grid::region_id(grid::region_index_of(position)) as usize
}

fn region_local(position: IVec3) -> IVec3 {
    position.saturating_sub(grid::grid_origin(position, grid::REGION_LENGTH))
}

fn axis_local(value: i32) -> usize {
    usize::try_from(value).unwrap_or(0)
}

fn micro_chunk_ordinal(position: IVec3) -> usize {
    let local = region_local(position);
    let x = axis_local(local.x).strict_div(MICRO_CHUNK);
    let y = axis_local(local.y).strict_div(MICRO_CHUNK);
    let z = axis_local(local.z).strict_div(MICRO_CHUNK);

    z.strict_mul(MICRO_CHUNK_SIDE)
        .strict_add(y)
        .strict_mul(MICRO_CHUNK_SIDE)
        .strict_add(x)
}

fn cell_index(position: IVec3) -> usize {
    let local = region_local(position);
    let x = axis_local(local.x).strict_rem(MICRO_CHUNK);
    let y = axis_local(local.y).strict_rem(MICRO_CHUNK);
    let z = axis_local(local.z).strict_rem(MICRO_CHUNK);

    x.strict_add(y.strict_mul(MICRO_CHUNK))
        .strict_add(z.strict_mul(MICRO_CHUNK_AREA))
}

fn mask_bit(position: IVec3) -> (usize, u8) {
    let cell = cell_index(position);
    let bit = u32::try_from(cell.strict_rem(8)).unwrap_or(0);

    (cell.strict_div(8), 1u8.wrapping_shl(bit))
}

fn mask_word(mask: &[u8], start: usize) -> u64 {
    let Some(bytes) = mask.get(start..start.strict_add(8)) else {
        return 0;
    };

    let mut array = [0u8; 8];
    array.copy_from_slice(bytes);

    u64::from_le_bytes(array)
}

/// The number of set mask bits before `cell`, which indexes the material
/// indices. The 8-byte word holding the cell is read once and masked to the
/// bits below it, so the scan is popcounts of whole words, not of every byte.
pub(in crate::world) fn rank(mask: &[u8], cell: usize) -> usize {
    let byte = cell.strict_div(8);
    let word_index = byte.strict_div(8);

    let mut below = 0usize;

    for index in 0..word_index {
        below = below.saturating_add(mask_word(mask, index.strict_mul(8)).count_ones() as usize);
    }

    let bit = u32::try_from(cell.strict_rem(8)).unwrap_or(0);
    let keep = u32::try_from(byte.strict_rem(8))
        .unwrap_or(0)
        .strict_mul(8)
        .strict_add(bit);
    let target = mask_word(mask, word_index.strict_mul(8));
    let partial = if keep == 0 {
        0
    } else {
        target & ((1u64 << keep).wrapping_sub(1))
    };

    below.saturating_add(partial.count_ones() as usize)
}

fn entry_mask(region: &Region, offset: usize) -> &[u8] {
    region
        .blob
        .get(offset..offset.strict_add(MASK_BYTES))
        .unwrap_or(&[])
}

fn entry_popcount(region: &Region, offset: usize) -> usize {
    entry_mask(region, offset)
        .iter()
        .map(|value| value.count_ones() as usize)
        .sum()
}

/// Claims a block for a Micro-chunk's first write, from the 72-byte class's
/// free list if it holds one, else by appending.
fn write_new_entry(region: &mut Region, ordinal: usize, position: IVec3, material: u8) {
    let class = class_of_size(MASK_BYTES.strict_add(1));
    let offset = alloc_block(region, class);

    if let Some(mask) = region.blob.get_mut(offset..offset.strict_add(MASK_BYTES)) {
        mask.fill(0);
    }

    let (byte, bit) = mask_bit(position);

    if let Some(slot) = region.blob.get_mut(offset.strict_add(byte)) {
        *slot = bit;
    }

    if let Some(slot) = region.blob.get_mut(offset.strict_add(MASK_BYTES)) {
        *slot = material;
    }

    if let Some(slot) = region.index.get_mut(ordinal) {
        *slot = encode(offset, class);
    }
}

fn cell_position(slot: usize, ordinal: usize, cell: usize) -> IVec3 {
    let region_index = grid::region_index_from_id(u32::try_from(slot).unwrap_or(0));
    let micro_chunk = IVec3::new(
        i32::try_from(ordinal.strict_rem(MICRO_CHUNK_SIDE)).unwrap_or(0),
        i32::try_from(
            ordinal
                .strict_div(MICRO_CHUNK_SIDE)
                .strict_rem(MICRO_CHUNK_SIDE),
        )
        .unwrap_or(0),
        i32::try_from(ordinal.strict_div(MICRO_CHUNK_SIDE_AREA)).unwrap_or(0),
    );
    let local = IVec3::new(
        i32::try_from(cell.strict_rem(MICRO_CHUNK)).unwrap_or(0),
        i32::try_from(cell.strict_div(MICRO_CHUNK).strict_rem(MICRO_CHUNK)).unwrap_or(0),
        i32::try_from(cell.strict_div(MICRO_CHUNK_AREA)).unwrap_or(0),
    );

    region_index
        .saturating_mul(IVec3::splat(
            i32::try_from(grid::REGION_LENGTH).unwrap_or(0),
        ))
        .saturating_add(micro_chunk.saturating_mul(IVec3::splat(
            i32::try_from(MICRO_CHUNK).unwrap_or(0),
        )))
        .saturating_add(local)
}

struct Voxels<'a> {
    store: &'a RegionStore,
    slot: usize,
    ordinal: usize,
    cell: usize,
    rank: usize,
}

impl<'a> Voxels<'a> {
    const fn new(store: &'a RegionStore) -> Self {
        Self {
            store,
            slot: 0,
            ordinal: 0,
            cell: 0,
            rank: 0,
        }
    }

    const fn advance_slot(&mut self) {
        self.slot = self.slot.saturating_add(1);
        self.ordinal = 0;
        self.cell = 0;
        self.rank = 0;
    }

    const fn advance_micro_chunk(&mut self) {
        self.ordinal = self.ordinal.saturating_add(1);
        self.cell = 0;
        self.rank = 0;
    }
}

impl Iterator for Voxels<'_> {
    type Item = (IVec3, u8);

    fn next(&mut self) -> Option<(IVec3, u8)> {
        loop {
            let slot = self.store.regions.get(self.slot)?;
            let Some(region) = slot.as_ref() else {
                self.advance_slot();
                continue;
            };

            if self.ordinal >= MICRO_CHUNKS_PER_REGION {
                self.advance_slot();
                continue;
            }

            let entry = region
                .index
                .get(self.ordinal)
                .copied()
                .unwrap_or(EMPTY);

            let Some(offset) = entry_offset(entry) else {
                self.advance_micro_chunk();
                continue;
            };

            let Some(mask) = region.blob.get(offset..offset.strict_add(MASK_BYTES)) else {
                self.advance_micro_chunk();
                continue;
            };

            while self.cell < MICRO_CHUNK_CELLS {
                let byte = mask.get(self.cell.strict_div(8)).copied().unwrap_or(0);
                let bit = u32::try_from(self.cell.strict_rem(8)).unwrap_or(0);

                if byte & 1u8.wrapping_shl(bit) != 0 {
                    break;
                }

                self.cell = self.cell.strict_add(1);
            }

            if self.cell >= MICRO_CHUNK_CELLS {
                self.advance_micro_chunk();
                continue;
            }

            let cell = self.cell;
            let rank = self.rank;
            self.cell = self.cell.strict_add(1);
            self.rank = self.rank.strict_add(1);

            let material = region
                .blob
                .get(offset.strict_add(MASK_BYTES).strict_add(rank))
                .copied()?;

            return Some((cell_position(self.slot, self.ordinal, cell), material));
        }
    }
}

impl VoxelStore for RegionStore {
    fn set(&mut self, position: IVec3, material: u8) -> bool {
        let slot = region_slot(position);

        let Some(slot_region) = self.regions.get_mut(slot) else {
            return false;
        };

        let region = slot_region.get_or_insert_with(Region::new);
        let existing = region.set(position, material);

        if !existing {
            self.count = self.count.saturating_add(1);
        }

        existing
    }

    fn get(&self, position: IVec3) -> Option<u8> {
        let slot = region_slot(position);
        let region = self.regions.get(slot)?.as_ref()?;
        let entry = region
            .index
            .get(micro_chunk_ordinal(position))
            .copied()
            .unwrap_or(EMPTY);
        let offset = entry_offset(entry)?;
        let (byte, bit) = mask_bit(position);

        if region.blob.get(offset.strict_add(byte)).copied().unwrap_or(0) & bit == 0 {
            return None;
        }

        let cell = cell_index(position);
        let mask = entry_mask(region, offset);

        region
            .blob
            .get(offset.strict_add(MASK_BYTES).strict_add(rank(mask, cell)))
            .copied()
    }

    fn clear(&mut self, position: IVec3) {
        let slot = region_slot(position);
        let ordinal = micro_chunk_ordinal(position);

        let Some(slot_region) = self.regions.get_mut(slot) else {
            return;
        };
        let Some(region) = slot_region.as_mut() else {
            return;
        };

        let entry = region.index.get(ordinal).copied().unwrap_or(EMPTY);
        let Some(offset) = entry_offset(entry) else {
            return;
        };

        let (byte, bit) = mask_bit(position);

        if region.blob.get(offset.strict_add(byte)).copied().unwrap_or(0) & bit == 0 {
            return;
        }

        let cell = cell_index(position);
        let popcount = entry_popcount(region, offset);
        let position_rank = rank(entry_mask(region, offset), cell);
        let base = offset.strict_add(MASK_BYTES);
        let content_end = base.strict_add(popcount);

        region.blob.copy_within(
            base.strict_add(position_rank).strict_add(1)..content_end,
            base.strict_add(position_rank),
        );

        if let Some(slot) = region.blob.get_mut(offset.strict_add(byte)) {
            *slot &= !bit;
        }

        region.count = region.count.saturating_sub(1);
        self.count = self.count.saturating_sub(1);

        if popcount <= 1 {
            free_block(region, entry_class(entry), offset);

            if let Some(slot) = region.index.get_mut(ordinal) {
                *slot = EMPTY;
            }
        }

        if region.count == 0 {
            *slot_region = None;
        }
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_> {
        Box::new(Voxels::new(self))
    }

    fn count(&self) -> usize {
        self.count
    }

    fn chunk_entry(&self, origin: IVec3) -> Option<ChunkEntry<'_>> {
        let region = self.regions.get(region_slot(origin))?.as_ref()?;
        let entry = region
            .index
            .get(micro_chunk_ordinal(origin))
            .copied()
            .unwrap_or(EMPTY);
        let offset = entry_offset(entry)?;

        let mask = region.blob.get(offset..offset.strict_add(MASK_BYTES))?;
        let populated = entry_popcount(region, offset);
        let base = offset.strict_add(MASK_BYTES);
        let materials = region.blob.get(base..base.strict_add(populated))?;

        Some(ChunkEntry { mask, materials })
    }

    #[cfg(test)]
    fn storage_size(&self) -> StorageSize {
        let mut index = 0usize;
        let mut blob = 0usize;

        for region in self.regions.iter().flatten() {
            index = index.saturating_add(
                region
                    .index
                    .len()
                    .saturating_mul(std::mem::size_of::<u32>()),
            );
            blob = blob.saturating_add(region.blob.len());
        }

        StorageSize {
            table: self
                .regions
                .len()
                .saturating_mul(std::mem::size_of::<Option<Region>>()),
            index,
            blob,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::world::test_support::{Rng, u8_below};

    #[cfg(feature = "map-oracle")]
    use crate::world::{StoreKind, World};

    #[cfg(feature = "map-oracle")]
    use crate::world::diff::snapshot::emit_snapshots;

    #[cfg(feature = "map-oracle")]
    use crate::world::store::ShardedMap;

    use super::*;

    fn content(store: &dyn VoxelStore) -> HashMap<IVec3, u8> {
        store.iter().collect()
    }

    fn cell_in_chunk(index: usize) -> IVec3 {
        let x = index % MICRO_CHUNK;
        let y = (index / MICRO_CHUNK) % MICRO_CHUNK;
        let z = index / MICRO_CHUNK_AREA;

        IVec3::new(x as i32, y as i32, z as i32)
    }

    /// The most bytes one Region table slot may take; 4096 slots at this cap
    /// are a negligible share of the storage.
    const REGION_SLOT_MAX: usize = 512;

    /// A full 64^3 dense block: eight Micro-chunks per axis, well inside one
    /// Region.
    const DENSE_EDGE: i32 = 64;

    fn dense_chunk_origins() -> Vec<IVec3> {
        let mut origins = Vec::new();

        for z in (0..DENSE_EDGE).step_by(MICRO_CHUNK) {
            for y in (0..DENSE_EDGE).step_by(MICRO_CHUNK) {
                for x in (0..DENSE_EDGE).step_by(MICRO_CHUNK) {
                    origins.push(IVec3::new(x, y, z));
                }
            }
        }

        origins
    }

    /// A write-only dense fixture: one full 64^3 block per Region, laid out
    /// along x so every Region is distinct. Returns the store and its
    /// Micro-chunk count.
    fn dense_fixture(regions: usize) -> (RegionStore, usize) {
        let mut store = RegionStore::default();
        let local = dense_chunk_origins();
        let mut chunks = 0usize;

        for region in 0..regions {
            let offset = region * grid::REGION_LENGTH as usize;
            let base = IVec3::new(i32::try_from(offset).unwrap_or(0), 0, 0);

            for origin in &local {
                fill_micro_chunk(&mut store, base.saturating_add(*origin));
                chunks = chunks.saturating_add(1);
            }
        }

        (store, chunks)
    }

    fn fill_micro_chunk(store: &mut RegionStore, origin: IVec3) {
        for z in 0..MICRO_CHUNK as i32 {
            for y in 0..MICRO_CHUNK as i32 {
                for x in 0..MICRO_CHUNK as i32 {
                    store.set(origin.saturating_add(IVec3::new(x, y, z)), 1);
                }
            }
        }
    }

    /// A full Micro-chunk's 576-byte entry.
    fn full_entry() -> usize {
        class_size(class_of_size(MASK_BYTES + MICRO_CHUNK_CELLS))
    }

    /// A one-voxel Micro-chunk's entry: 65 bytes padded to 72.
    fn single_entry() -> usize {
        class_size(class_of_size(MASK_BYTES + 1))
    }

    /// The bytes a Micro-chunk's growth from empty to full leaves allocated:
    /// one block per class from 72 to 576 bytes.
    fn growth_ladder_bytes() -> usize {
        (0..CLASS_COUNT).map(class_size).sum()
    }

    #[test]
    fn full_region_layout_arithmetic() {
        let entry = full_entry();
        let blob = MICRO_CHUNKS_PER_REGION * entry;
        let index = MICRO_CHUNKS_PER_REGION * std::mem::size_of::<u32>();
        let voxels = MICRO_CHUNKS_PER_REGION * MICRO_CHUNK_CELLS;

        assert_eq!(MICRO_CHUNKS_PER_REGION, 32_768);
        assert_eq!(entry, 576, "a full Micro-chunk entry is 576 bytes");
        assert_eq!(blob, 18 * 1024 * 1024, "18 MiB of blob");
        assert_eq!(index, 128 * 1024, "128 KiB of index");
        assert_eq!(voxels, 16_777_216);

        let per_voxel = (blob + index) as f64 / voxels as f64;

        assert!(
            (per_voxel - 1.133).abs() < 0.001,
            "18.125 MiB over 16.78 Mi voxels is about 1.133 bytes each"
        );
    }

    #[test]
    fn dense_fixture_asserts_bytes_per_occupied_voxel() {
        let regions = 2usize;
        let (store, chunks) = dense_fixture(regions);
        let voxels = store.count();
        let size = store.storage_size();

        let entry = full_entry();
        let live_blob = chunks * entry;
        // The first full Micro-chunk in a Region allocates every size class;
        // each later Micro-chunk reuses them and appends only its own entry.
        let expected_blob = regions * growth_ladder_bytes() + (chunks - regions) * entry;
        let expected_index = regions * MICRO_CHUNKS_PER_REGION * std::mem::size_of::<u32>();
        let table_cap = grid::REGION_COUNT * REGION_SLOT_MAX;

        assert_eq!(voxels, chunks * MICRO_CHUNK_CELLS);
        assert_eq!(size.index, expected_index, "one index per live Region");
        assert_eq!(
            size.blob, expected_blob,
            "the write-only blob high-water mark"
        );
        assert!(
            size.table <= table_cap,
            "the Region table stays under its cap"
        );

        let layout_per_voxel = (live_blob + size.index) as f64 / voxels as f64;

        assert!(
            (layout_per_voxel - 1.625).abs() < 0.001,
            "the dense layout is about 1.625 bytes per voxel"
        );

        let high_water_per_voxel = (size.blob + size.index) as f64 / voxels as f64;

        assert!(
            (high_water_per_voxel - 1.702).abs() < 0.001,
            "the free list's growth ladder adds the rest of the high-water figure"
        );
    }

    #[test]
    fn sparse_fixture_asserts_the_padded_bound() {
        let mut store = RegionStore::default();
        let edge = grid::REGION_LENGTH as i32;
        let mut chunks = 0usize;

        for z in (0..edge).step_by(MICRO_CHUNK) {
            for y in (0..edge).step_by(MICRO_CHUNK) {
                for x in (0..edge).step_by(MICRO_CHUNK) {
                    store.set(IVec3::new(x, y, z), 1);
                    chunks = chunks.saturating_add(1);
                }
            }
        }

        let voxels = store.count();
        let size = store.storage_size();
        let padded = single_entry();
        let index = MICRO_CHUNKS_PER_REGION * std::mem::size_of::<u32>();

        assert_eq!(chunks, MICRO_CHUNKS_PER_REGION);
        assert_eq!(voxels, chunks);
        assert_eq!(padded, 72, "a one-voxel entry pads 65 bytes to 72");
        assert!(size.blob <= chunks * padded, "the padded blob bound");
        assert_eq!(size.index, index, "one index for the Region");

        let per_voxel = (size.blob + size.index) as f64 / voxels as f64;

        assert!(
            per_voxel <= 76.5,
            "one voxel per Micro-chunk stays under the looser padded bound"
        );
    }

    fn random_position(rng: &mut Rng) -> IVec3 {
        let mut axis = || i32::try_from(rng.below(512)).unwrap_or(0).wrapping_sub(256);

        IVec3::new(axis(), axis(), axis())
    }

    fn clustered_position(rng: &mut Rng) -> IVec3 {
        let chunk = IVec3::new(
            i32::try_from(rng.below(3)).unwrap_or(0).wrapping_sub(1),
            i32::try_from(rng.below(3)).unwrap_or(0).wrapping_sub(1),
            i32::try_from(rng.below(3)).unwrap_or(0).wrapping_sub(1),
        );
        let cell = IVec3::new(
            i32::try_from(rng.below(8)).unwrap_or(0),
            i32::try_from(rng.below(8)).unwrap_or(0),
            i32::try_from(rng.below(8)).unwrap_or(0),
        );

        chunk.saturating_mul(IVec3::splat(8)).saturating_add(cell)
    }

    #[cfg(feature = "map-oracle")]
    fn assert_agrees(region: &RegionStore, map: &ShardedMap) {
        assert_eq!(region.count(), map.count(), "voxel count");
        assert_eq!(region.bounds(), map.bounds(), "bounds");

        let region_content = content(region);
        let map_content = content(map);

        assert_eq!(region_content, map_content, "content");

        for (position, material) in &region_content {
            assert_eq!(region.get(*position), Some(*material));
            assert_eq!(map.get(*position), Some(*material));
            assert!(region.contains(*position));
            assert!(map.contains(*position));
        }
    }

    #[test]
    fn material_zero_is_occupied_while_its_mask_bit_is_set() {
        let mut store = RegionStore::default();
        let position = IVec3::new(-3, 5, 7);

        assert!(!store.set(position, 0));

        assert_eq!(store.get(position), Some(0), "material zero is a colour");
        assert!(store.contains(position));
        assert_eq!(store.count(), 1);

        store.clear(position);

        assert_eq!(store.get(position), None);
        assert!(!store.contains(position));
    }

    #[cfg(feature = "map-oracle")]
    #[test]
    fn randomized_writes_and_clears_match_the_map() {
        let mut rng = Rng::new(0x0303_0303);

        for case in 0..32u32 {
            let mut region = RegionStore::default();
            let mut map = ShardedMap::default();

            for _ in 0..512 {
                let position = random_position(&mut rng);

                if rng.below(4) == 0 {
                    region.clear(position);
                    map.clear(position);
                } else {
                    let material = u8_below(&mut rng, 256);

                    assert_eq!(
                        region.set(position, material),
                        map.set(position, material),
                        "case {case} set result at {position}"
                    );
                }
            }

            assert_agrees(&region, &map);
        }
    }

    #[cfg(feature = "map-oracle")]
    #[test]
    fn clustered_random_edits_exercise_growth_and_reuse() {
        let mut rng = Rng::new(0x0404_0404);

        for case in 0..16u32 {
            let mut region = RegionStore::default();
            let mut map = ShardedMap::default();

            for _ in 0..2_000 {
                let position = clustered_position(&mut rng);

                if rng.below(3) == 0 {
                    region.clear(position);
                    map.clear(position);
                } else {
                    let material = u8_below(&mut rng, 256);

                    assert_eq!(
                        region.set(position, material),
                        map.set(position, material),
                        "case {case} set result at {position}"
                    );
                }
            }

            assert_agrees(&region, &map);
        }
    }

    #[test]
    fn emptied_micro_chunk_reuses_its_block_without_growing() {
        let mut store = RegionStore::default();
        let anchor = IVec3::new(0, 0, 0);
        let recycled = IVec3::new(MICRO_CHUNK as i32, 0, 0);

        store.set(anchor, 1);
        store.set(recycled, 2);

        let before = store.storage_size().total();

        store.clear(recycled);

        assert_eq!(
            store.storage_size().total(),
            before,
            "clearing does not compact the blob"
        );

        store.set(recycled, 3);

        assert_eq!(
            store.storage_size().total(),
            before,
            "the freed block is reclaimed instead of appended"
        );
        assert_eq!(store.get(recycled), Some(3));
    }

    #[test]
    fn in_class_growth_does_not_grow_the_blob() {
        let mut store = RegionStore::default();
        let origin = IVec3::new(0, 0, 0);

        store.set(origin, 1);

        let before = store.storage_size().total();

        for index in 1..8 {
            store.set(origin.saturating_add(cell_in_chunk(index)), index as u8);
        }

        assert_eq!(
            store.storage_size().total(),
            before,
            "eight voxels fit one 72-byte block"
        );
        assert_eq!(store.get(origin.saturating_add(cell_in_chunk(7))), Some(7));
    }

    #[test]
    fn crossing_a_class_grows_by_a_block_and_releases_the_old_one() {
        let mut store = RegionStore::default();
        let origin = IVec3::new(0, 0, 0);

        for index in 0..8 {
            store.set(origin.saturating_add(cell_in_chunk(index)), 1);
        }

        let filled = store.storage_size().total();

        store.set(origin.saturating_add(cell_in_chunk(8)), 2);

        let crossed = store.storage_size().total();

        assert!(crossed > filled, "the ninth voxel moves to the next class");
        assert!(
            crossed.saturating_sub(filled) <= CLASS_MAX,
            "the move is bounded by one Micro-chunk block"
        );
        assert_eq!(store.get(origin), Some(1), "the copied mask survives");
        assert_eq!(store.get(origin.saturating_add(cell_in_chunk(8))), Some(2));

        store.set(IVec3::new(MICRO_CHUNK as i32, 0, 0), 9);

        assert_eq!(
            store.storage_size().total(),
            crossed,
            "the released block is reclaimed by the next first write"
        );
    }

    #[test]
    fn cleared_and_refilled_fixture_holds_the_high_water_mark() {
        let mut store = RegionStore::default();
        let anchor = IVec3::new(0, 0, 0);

        store.set(anchor, 1);

        let fillers: Vec<IVec3> = (0..64)
            .map(|index| IVec3::new((index % 8) * 8, (index / 8) * 8, 8))
            .collect();

        for filler in &fillers {
            store.set(*filler, 2);
        }

        let size = store.storage_size();
        let high_water = size.blob;
        let single = single_entry();

        assert_eq!(
            high_water,
            (fillers.len() + 1) * single,
            "65 one-voxel entries pad to 65 blocks"
        );

        for filler in &fillers {
            store.clear(*filler);
        }

        let cleared = store.storage_size();

        assert_eq!(
            cleared.blob, high_water,
            "clearing leaves the high-water mark in place"
        );
        assert_eq!(
            cleared.index, size.index,
            "the Region keeps its index while the anchor voxel holds it live"
        );

        for index in 1..MICRO_CHUNK_CELLS {
            store.set(anchor.saturating_add(cell_in_chunk(index)), 3);
        }

        let refilled = store.storage_size();

        assert!(
            refilled.blob > high_water,
            "refilling with larger entries raises the high-water mark"
        );
        assert_eq!(
            refilled.blob,
            high_water + growth_ladder_bytes() - single,
            "the refill appends every class above the reused one-voxel block"
        );
    }

    #[test]
    fn iteration_is_region_then_micro_chunk_then_cell_order() {
        let mut rng = Rng::new(0x00A0_0A0A);
        let mut store = RegionStore::default();

        for _ in 0..4_000 {
            store.set(random_position(&mut rng), u8_below(&mut rng, 256));
        }

        let mut expected: Vec<IVec3> = content(&store).into_keys().collect();

        expected.sort_by_key(|position| {
            (
                region_slot(*position),
                micro_chunk_ordinal(*position),
                cell_index(*position),
            )
        });

        let actual: Vec<IVec3> = store.iter().map(|(position, _)| position).collect();

        assert_eq!(actual, expected, "the walk is region, micro-chunk, cell");

        let keys: Vec<(usize, usize, usize)> = actual
            .iter()
            .map(|position| {
                (
                    region_slot(*position),
                    micro_chunk_ordinal(*position),
                    cell_index(*position),
                )
            })
            .collect();

        assert!(
            keys.windows(2).all(|pair| pair[0] < pair[1]),
            "the walk is strictly increasing"
        );
    }

    #[test]
    fn clearing_releases_the_micro_chunk_and_the_region() {
        let mut store = RegionStore::default();
        let first = IVec3::new(0, 0, 0);
        let second = IVec3::new(8, 0, 0);
        let slot = region_slot(first);

        assert_eq!(slot, region_slot(second), "the two cells share a region");

        store.set(first, 1);
        store.set(second, 2);

        let region = store.regions.get(slot).and_then(Option::as_ref);
        let Some(region) = region else {
            panic!("the region is live");
        };

        assert_ne!(
            region.index[micro_chunk_ordinal(first)],
            EMPTY,
            "the first micro-chunk has an entry"
        );

        store.clear(first);

        let region = store.regions.get(slot).and_then(Option::as_ref);
        let Some(region) = region else {
            panic!("the region still holds the second micro-chunk");
        };

        assert_eq!(
            region.index[micro_chunk_ordinal(first)],
            EMPTY,
            "the emptied micro-chunk is released"
        );
        assert_eq!(store.count(), 1);

        store.clear(second);

        assert!(
            store.regions.get(slot).is_some_and(Option::is_none),
            "the emptied region is released"
        );
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn materials_stay_in_ascending_cell_order_through_writes() {
        let mut store = RegionStore::default();

        store.set(IVec3::new(0, 0, 1), 4);
        store.set(IVec3::new(7, 0, 0), 2);
        store.set(IVec3::new(0, 0, 0), 1);
        store.set(IVec3::new(0, 1, 0), 3);
        store.set(IVec3::new(0, 0, 0), 9);

        let materials: Vec<u8> = store.iter().map(|(_, material)| material).collect();

        assert_eq!(
            materials,
            vec![9, 2, 3, 4],
            "overwrite replaces in place, insert stays at the rank"
        );
        assert_eq!(store.get(IVec3::new(0, 0, 0)), Some(9));
        assert_eq!(store.get(IVec3::new(7, 0, 0)), Some(2));
        assert_eq!(store.get(IVec3::new(0, 1, 0)), Some(3));
        assert_eq!(store.get(IVec3::new(0, 0, 1)), Some(4));
    }

    #[test]
    fn count_equals_the_set_mask_bits() {
        let mut rng = Rng::new(0x00CC_0C0C);
        let mut store = RegionStore::default();

        for _ in 0..2_000 {
            let position = random_position(&mut rng);

            if rng.below(3) == 0 {
                store.clear(position);
            } else {
                store.set(position, u8_below(&mut rng, 256));
            }
        }

        let mut bits = 0usize;

        for region in store.regions.iter().flatten() {
            for entry in region.index.iter() {
                let Some(offset) = entry_offset(*entry) else {
                    continue;
                };

                let mask = region
                    .blob
                    .get(offset..offset.strict_add(MASK_BYTES))
                    .unwrap_or(&[]);

                bits = bits
                    .saturating_add(mask.iter().map(|value| value.count_ones() as usize).sum());
            }
        }

        assert_eq!(store.count(), bits, "the counter tracks the mask bits");
        assert_eq!(store.count(), content(&store).len());
    }

    fn reference_bounds(reference: &HashMap<IVec3, u8>) -> Option<(IVec3, IVec3)> {
        reference.keys().copied().fold(None, |bounds, position| {
            Some(match bounds {
                Some((min, max)) => (min.min(position), max.max(position)),
                None => (position, position),
            })
        })
    }

    /// Asserts the store's count, bounds, iterated content, and per-position
    /// lookups all agree with a hash map that knows nothing of the layout.
    fn assert_agrees_with_reference(
        store: &RegionStore,
        reference: &HashMap<IVec3, u8>,
        context: &str,
    ) {
        assert_eq!(store.count(), reference.len(), "{context}: count");
        assert_eq!(
            store.bounds(),
            reference_bounds(reference),
            "{context}: bounds"
        );

        let iterated = content(store);

        assert_eq!(&iterated, reference, "{context}: iterated content");

        for (position, material) in reference {
            assert_eq!(
                store.get(*position),
                Some(*material),
                "{context}: lookup at {position}"
            );
            assert!(store.contains(*position), "{context}: contains {position}");
        }
    }

    /// Random writes and clears against an independent hash map, with `set`'s
    /// return checked against prior occupancy, scattered and clustered.
    #[test]
    fn randomized_writes_and_clears_match_an_independent_hash_map() {
        let seed = 0x0C01_7AAC;
        let mut rng = Rng::new(seed);

        for case in 0..32u32 {
            let clustered = case % 2 == 1;
            let mut store = RegionStore::default();
            let mut reference: HashMap<IVec3, u8> = HashMap::new();

            for _ in 0..1_500 {
                let position = if clustered {
                    clustered_position(&mut rng)
                } else {
                    random_position(&mut rng)
                };

                if rng.below(4) == 0 {
                    store.clear(position);
                    reference.remove(&position);
                } else {
                    let material = u8_below(&mut rng, 256);
                    let existed = reference.insert(position, material).is_some();

                    assert_eq!(
                        store.set(position, material),
                        existed,
                        "seed {seed:#x} case {case} (clustered: {clustered}): set return at {position}"
                    );
                }
            }

            let context = format!("seed {seed:#x} case {case} (clustered: {clustered})");

            assert_agrees_with_reference(&store, &reference, &context);
        }
    }

    #[cfg(feature = "map-oracle")]
    fn asset_answers_match_the_map(path: &str) {
        let data = dot_vox::load(path).unwrap();
        let (region, region_clipped) = World::new_clipped_with_store(&data, StoreKind::Region);
        let (map, map_clipped) = World::new_clipped_with_store(&data, StoreKind::Map);

        assert_eq!(region_clipped, map_clipped, "clipped counts diverge");
        crate::world::test_support::assert_worlds_agree(&region, &map, path);

        let region_snapshots = emit_snapshots(&region)
            .unwrap_or_else(|error| panic!("{path}: region emission: {error}"));
        let map_snapshots =
            emit_snapshots(&map).unwrap_or_else(|error| panic!("{path}: map emission: {error}"));

        assert_eq!(
            region_snapshots, map_snapshots,
            "{path}: emitted Snapshots diverge"
        );

        crate::world::test_support::report_region_density(&region, path);
    }

    #[cfg(feature = "map-oracle")]
    #[test]
    #[ignore = "asset: cargo test --release --features map-oracle church_answers_match_the_map -- --ignored --nocapture"]
    fn church_answers_match_the_map() {
        asset_answers_match_the_map("assets/church.vox");
    }

    #[cfg(feature = "map-oracle")]
    #[test]
    #[ignore = "asset: cargo test --release --features map-oracle bistro_answers_match_the_map -- --ignored --nocapture"]
    fn bistro_answers_match_the_map() {
        asset_answers_match_the_map("assets/bistro.vox");
    }
}
