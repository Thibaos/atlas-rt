use std::fmt;
use std::sync::{Mutex, PoisonError};

use glam::IVec3;

use crate::world::{
    diff::edit::{EditError, MICRO_BYTES, validate_entry},
    grid,
    store::{ChunkEntry, MicroChunkEntry, VoxelStore},
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

/// Frees the block an entry holds and clears its index slot.
fn release_entry(region: &mut Region, ordinal: usize, entry: u32) {
    if let Some(offset) = entry_offset(entry) {
        free_block(region, entry_class(entry), offset);
    }

    if let Some(cell) = region.index.get_mut(ordinal) {
        *cell = EMPTY;
    }
}

/// The World's content as a flat table of Region slots, one per region id.
pub struct RegionStore {
    regions: Vec<Option<Region>>,
    count: usize,
    bounds: Mutex<BoundsCache>,
}

/// The cached [`VoxelStore::bounds`]. It starts exact for the empty store and
/// an addition grows it in place, so a write-only load or Generation stays
/// cheap. A removal marks it stale and the next query rescans;
/// [`RegionStore::recount`] does the same.
#[derive(Debug)]
struct BoundsCache {
    bounds: Option<(IVec3, IVec3)>,
    valid: bool,
}

impl Default for BoundsCache {
    /// An empty store's bounds are known, so the cache starts valid and only a
    /// removal makes it recompute.
    fn default() -> Self {
        Self {
            bounds: None,
            valid: true,
        }
    }
}

impl Default for RegionStore {
    fn default() -> Self {
        Self {
            regions: (0..grid::REGION_COUNT).map(|_| None).collect(),
            count: 0,
            bounds: Mutex::new(BoundsCache::default()),
        }
    }
}

impl fmt::Debug for RegionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let regions = self.regions.iter().filter(|slot| slot.is_some()).count();

        f.debug_struct("RegionStore")
            .field("regions", &regions)
            .field("voxels", &self.count)
            .finish_non_exhaustive()
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

        self.invalidate_bounds();
    }

    /// Grows the cached bounds to cover a cell range. Only an addition calls
    /// this, so the cache stays exact while it is valid; while it is stale it
    /// stays a superset and the next query still recomputes.
    fn grow_bounds(&mut self, min: IVec3, max: IVec3) {
        let cache = self
            .bounds
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);

        cache.bounds = Some(match cache.bounds {
            Some((low, high)) => (low.min(min), high.max(max)),
            None => (min, max),
        });
    }

    /// Marks the cached bounds stale, for an operation that can remove a cell
    /// at the cache's edge.
    fn invalidate_bounds(&mut self) {
        self.bounds
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .valid = false;
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
        .saturating_add(
            micro_chunk.saturating_mul(IVec3::splat(i32::try_from(MICRO_CHUNK).unwrap_or(0))),
        )
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

            let entry = region.index.get(self.ordinal).copied().unwrap_or(EMPTY);

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

struct Entries<'a> {
    store: &'a RegionStore,
    slot: usize,
    ordinal: usize,
}

impl<'a> Entries<'a> {
    const fn new(store: &'a RegionStore) -> Self {
        Self {
            store,
            slot: 0,
            ordinal: 0,
        }
    }

    const fn advance_slot(&mut self) {
        self.slot = self.slot.saturating_add(1);
        self.ordinal = 0;
    }
}

impl Iterator for Entries<'_> {
    type Item = MicroChunkEntry;

    fn next(&mut self) -> Option<MicroChunkEntry> {
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

            let ordinal = self.ordinal;
            self.ordinal = self.ordinal.saturating_add(1);

            let entry = region.index.get(ordinal).copied().unwrap_or(EMPTY);
            let Some(offset) = entry_offset(entry) else {
                continue;
            };

            let Some(mask) = region.blob.get(offset..offset.strict_add(MASK_BYTES)) else {
                continue;
            };

            let mut owned = [0u8; MICRO_BYTES];

            owned.copy_from_slice(mask);

            let populated = entry_popcount(region, offset);
            let base = offset.strict_add(MASK_BYTES);

            let Some(materials) = region.blob.get(base..base.strict_add(populated)) else {
                continue;
            };

            return Some(MicroChunkEntry {
                origin: cell_position(self.slot, ordinal, 0),
                mask: owned,
                materials: materials.to_vec(),
            });
        }
    }
}

/// The exact occupied bounds of one Micro-chunk entry, or `None` for a zero
/// mask. Scans the mask's 64 bytes rather than its set bits, so it is bounded
/// by the entry size, not by the voxels it holds.
fn entry_cell_bounds(origin: IVec3, mask: &[u8; MICRO_BYTES]) -> Option<(IVec3, IVec3)> {
    let mut min = IVec3::splat(i32::MAX);
    let mut max = IVec3::splat(i32::MIN);
    let mut occupied = false;

    for (index, byte) in mask.iter().copied().enumerate() {
        if byte == 0 {
            continue;
        }

        occupied = true;

        let row = i32::try_from(index).unwrap_or(0);
        let edge = i32::try_from(MICRO_CHUNK).unwrap_or(1);
        let y = row.strict_rem(edge);
        let z = row.strict_div(edge);
        let low = i32::try_from(byte.trailing_zeros()).unwrap_or(0);
        let high = i32::try_from(7u32.saturating_sub(byte.leading_zeros())).unwrap_or(0);
        let cell_min = IVec3::new(low, y, z);
        let cell_max = IVec3::new(high, y, z);

        min = min.min(cell_min);
        max = max.max(cell_max);
    }

    occupied.then(|| (origin.saturating_add(min), origin.saturating_add(max)))
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
            self.grow_bounds(position, position);
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

        if region
            .blob
            .get(offset.strict_add(byte))
            .copied()
            .unwrap_or(0)
            & bit
            == 0
        {
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

        if region
            .blob
            .get(offset.strict_add(byte))
            .copied()
            .unwrap_or(0)
            & bit
            == 0
        {
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
            release_entry(region, ordinal, entry);
        }

        if region.count == 0 {
            *slot_region = None;
        }

        self.invalidate_bounds();
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_> {
        Box::new(Voxels::new(self))
    }

    fn count(&self) -> usize {
        self.count
    }

    fn bounds(&self) -> Option<(IVec3, IVec3)> {
        let mut cache = self.bounds.lock().unwrap_or_else(PoisonError::into_inner);

        if !cache.valid {
            cache.bounds = self.iter().fold(None, |bounds, (position, _)| {
                Some(match bounds {
                    Some((min, max)) => (min.min(position), max.max(position)),
                    None => (position, position),
                })
            });
            cache.valid = true;
        }

        cache.bounds
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

    fn entries(&self) -> Box<dyn Iterator<Item = MicroChunkEntry> + '_> {
        Box::new(Entries::new(self))
    }

    fn write_entry(
        &mut self,
        origin: IVec3,
        mask: &[u8; MICRO_BYTES],
        materials: &[u8],
    ) -> Result<(), EditError> {
        validate_entry(origin, mask, materials)?;

        let slot = region_slot(origin);
        let ordinal = micro_chunk_ordinal(origin);
        let occupied = materials.len();

        let Some(slot_region) = self.regions.get_mut(slot) else {
            return Ok(());
        };

        if occupied == 0 {
            let Some(region) = slot_region.as_mut() else {
                return Ok(());
            };

            let entry = region.index.get(ordinal).copied().unwrap_or(EMPTY);
            let Some(offset) = entry_offset(entry) else {
                return Ok(());
            };

            let popcount = entry_popcount(region, offset);

            release_entry(region, ordinal, entry);

            region.count = region.count.saturating_sub(popcount);
            self.count = self.count.saturating_sub(popcount);

            if region.count == 0 {
                *slot_region = None;
            }

            self.invalidate_bounds();

            return Ok(());
        }

        let region = slot_region.get_or_insert_with(Region::new);
        let entry = region.index.get(ordinal).copied().unwrap_or(EMPTY);
        let old_offset = entry_offset(entry);
        let replacing = old_offset.is_some();
        let written_bounds = entry_cell_bounds(origin, mask);
        let old_popcount = old_offset.map_or(0, |offset| entry_popcount(region, offset));
        let class = class_of_size(MASK_BYTES.strict_add(occupied));
        let reuse = old_offset.filter(|_| entry_class(entry) == class);

        let offset = if let Some(offset) = reuse {
            offset
        } else {
            let replacement = alloc_block(region, class);

            if let Some(old) = old_offset {
                free_block(region, entry_class(entry), old);
            }

            if let Some(cell) = region.index.get_mut(ordinal) {
                *cell = encode(replacement, class);
            }

            replacement
        };

        region.count = region
            .count
            .saturating_sub(old_popcount)
            .saturating_add(occupied);
        self.count = self
            .count
            .saturating_sub(old_popcount)
            .saturating_add(occupied);

        if let Some(dst) = region.blob.get_mut(offset..offset.strict_add(MASK_BYTES)) {
            dst.copy_from_slice(mask);
        }

        let base = offset.strict_add(MASK_BYTES);

        if let Some(dst) = region.blob.get_mut(base..base.strict_add(occupied)) {
            dst.copy_from_slice(materials);
        }

        if replacing {
            self.invalidate_bounds();
        } else if let Some((min, max)) = written_bounds {
            self.grow_bounds(min, max);
        }

        Ok(())
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

    use crate::world::{
        diff::edit::mask_occupied,
        test_support::{Rng, u8_below},
    };

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

                bits =
                    bits.saturating_add(mask.iter().map(|value| value.count_ones() as usize).sum());
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

    /// Writes, entry writes, and clears against an independent hash map, with
    /// the cached bounds checked after every operation, so an addition that
    /// grows them and a clear or a replacing entry write that shrinks them are
    /// both exercised.
    #[test]
    fn bounds_stay_exact_through_writes_entry_writes_and_clears() {
        let seed = 0x00B0_0D15;
        let mut rng = Rng::new(seed);
        let mut store = RegionStore::default();
        let mut reference: HashMap<IVec3, u8> = HashMap::new();

        for case in 0..600u32 {
            match rng.below(4) {
                0 => {
                    let position = clustered_position(&mut rng);

                    store.clear(position);
                    reference.remove(&position);
                }
                1 => {
                    let origin = chunk_origin(&mut rng, 6);
                    let mask = random_mask(&mut rng, 4);
                    let materials = entry_materials(&mask, &mut rng);

                    store
                        .write_entry(origin, &mask, &materials)
                        .unwrap_or_else(|error| panic!("case {case}: {error}"));

                    for index in 0..MICRO_CHUNK_CELLS {
                        let position = origin.saturating_add(cell_in_chunk(index));

                        if mask_occupied(&mask, index) {
                            reference.insert(position, 0);
                        } else {
                            reference.remove(&position);
                        }
                    }
                }
                _ => {
                    let position = clustered_position(&mut rng);
                    let material = u8_below(&mut rng, 256);

                    store.set(position, material);
                    reference.insert(position, material);
                }
            }

            assert_eq!(
                store.bounds(),
                reference_bounds(&reference),
                "seed {seed:#x} case {case}"
            );
        }
    }

    fn bounds_valid(store: &RegionStore) -> bool {
        store
            .bounds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .valid
    }

    #[test]
    fn a_clear_invalidates_the_cached_bounds_and_a_query_recomputes_them() {
        let mut store = RegionStore::default();
        let far = IVec3::new(100, -20, 40);
        let near = IVec3::new(0, 0, 0);

        store.set(far, 1);
        store.set(near, 1);

        assert_eq!(
            store.bounds(),
            Some((IVec3::new(0, -20, 0), IVec3::new(100, 0, 40))),
            "the bounds span both cells"
        );
        assert!(bounds_valid(&store), "a write leaves the cache exact");

        store.clear(far);

        assert!(!bounds_valid(&store), "a clear invalidates the cache");
        assert_eq!(
            store.bounds(),
            Some((near, near)),
            "the query recomputes the shrunken bounds"
        );
        assert!(
            bounds_valid(&store),
            "the query leaves the cache exact again"
        );
    }

    #[test]
    fn a_write_only_store_keeps_its_bounds_valid_without_a_scan() {
        let mut store = RegionStore::default();
        let mut mask = [0u8; MICRO_BYTES];

        mask[0] = 0b0000_0011;

        store
            .write_entry(IVec3::new(0, 0, 0), &mask, &[1, 2])
            .unwrap_or_else(|error| panic!("{error}"));

        assert!(
            bounds_valid(&store),
            "a generation that only writes never needs a scan"
        );
        assert_eq!(
            store.bounds(),
            Some((IVec3::ZERO, IVec3::new(1, 0, 0))),
            "the bounds come straight from the entry"
        );
    }

    /// A store that implements only the primitives, so `write_entry` comes from
    /// the trait default and its per-cell loop is the oracle.
    #[derive(Debug)]
    struct PerCellStore(RegionStore);

    impl VoxelStore for PerCellStore {
        fn set(&mut self, position: IVec3, material: u8) -> bool {
            self.0.set(position, material)
        }

        fn get(&self, position: IVec3) -> Option<u8> {
            self.0.get(position)
        }

        fn clear(&mut self, position: IVec3) {
            self.0.clear(position);
        }

        fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_> {
            self.0.iter()
        }

        fn count(&self) -> usize {
            self.0.count()
        }

        fn chunk_entry(&self, origin: IVec3) -> Option<ChunkEntry<'_>> {
            self.0.chunk_entry(origin)
        }

        fn storage_size(&self) -> StorageSize {
            self.0.storage_size()
        }
    }

    /// A store that yields its voxels in reverse, so the default enumeration's
    /// bucketing is exercised with out-of-order cells.
    #[derive(Debug)]
    struct ReversedStore(RegionStore);

    impl VoxelStore for ReversedStore {
        fn set(&mut self, position: IVec3, material: u8) -> bool {
            self.0.set(position, material)
        }

        fn get(&self, position: IVec3) -> Option<u8> {
            self.0.get(position)
        }

        fn clear(&mut self, position: IVec3) {
            self.0.clear(position);
        }

        fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_> {
            Box::new(self.0.iter().collect::<Vec<_>>().into_iter().rev())
        }

        fn count(&self) -> usize {
            self.0.count()
        }

        fn chunk_entry(&self, origin: IVec3) -> Option<ChunkEntry<'_>> {
            self.0.chunk_entry(origin)
        }

        fn storage_size(&self) -> StorageSize {
            self.0.storage_size()
        }
    }

    fn chunk_origin(rng: &mut Rng, span: u64) -> IVec3 {
        let mut axis = || {
            i32::try_from(rng.below(span))
                .unwrap_or(0)
                .wrapping_sub(i32::try_from(span / 2).unwrap_or(0))
                .wrapping_mul(MICRO_CHUNK as i32)
        };

        IVec3::new(axis(), axis(), axis())
    }

    fn random_mask(rng: &mut Rng, density: u64) -> [u8; MICRO_BYTES] {
        let mut mask = [0u8; MICRO_BYTES];

        for byte in &mut mask {
            *byte = u8_below(rng, density);
        }

        mask
    }

    /// One random material per set mask bit, in ascending cell order.
    fn entry_materials(mask: &[u8; MICRO_BYTES], rng: &mut Rng) -> Vec<u8> {
        let mut materials = Vec::new();

        for byte in mask {
            let mut bits = *byte;

            while bits != 0 {
                materials.push(u8_below(rng, 256));
                bits &= bits.strict_sub(1);
            }
        }

        materials
    }

    fn occupied_cells(mask: &[u8; MICRO_BYTES]) -> Vec<usize> {
        (0..MICRO_CHUNK_CELLS)
            .filter(|index| mask_occupied(mask, *index))
            .collect()
    }

    /// Asserts every set mask bit reads back its material and every clear bit
    /// is absent, through both `get` and the entry read.
    fn assert_entry_content(
        store: &dyn VoxelStore,
        origin: IVec3,
        mask: &[u8; MICRO_BYTES],
        materials: &[u8],
        context: &str,
    ) {
        let mut rank = 0usize;

        for index in 0..MICRO_CHUNK_CELLS {
            let expected = if mask_occupied(mask, index) {
                let material = materials.get(rank).copied();
                rank = rank.saturating_add(1);
                material
            } else {
                None
            };

            assert_eq!(
                store.get(origin.saturating_add(cell_in_chunk(index))),
                expected,
                "{context}: cell {index}"
            );
        }

        let entry = store
            .chunk_entry(origin)
            .unwrap_or_else(|| panic!("{context}: the entry is missing"));

        assert_eq!(entry.mask, mask.as_slice(), "{context}: mask");
        assert_eq!(entry.materials, materials, "{context}: materials");
    }

    fn assert_write_rejected(
        store: &mut dyn VoxelStore,
        origin: IVec3,
        mask: &[u8; MICRO_BYTES],
        materials: &[u8],
        message: &str,
    ) {
        let error = store
            .write_entry(origin, mask, materials)
            .expect_err("the write must be rejected");

        assert_eq!(error.to_string(), message);
        assert_eq!(store.count(), 0, "a rejected write changes nothing");
    }

    #[test]
    fn entry_write_round_trips_every_mask_bit() {
        let mut store = RegionStore::default();
        let origin = IVec3::new(-16, 8, 24);
        let mut mask = [0u8; MICRO_BYTES];

        mask[0] = 0b1000_0001;
        mask[8] = 0b0000_0100;
        mask[63] = 0b1000_0000;

        let materials = vec![10u8, 20, 30, 40];

        store.write_entry(origin, &mask, &materials).unwrap();

        assert_entry_content(&store, origin, &mask, &materials, "round trip");
        assert_eq!(store.count(), occupied_cells(&mask).len());
    }

    #[test]
    fn entry_write_replaces_the_previous_content() {
        let mut store = RegionStore::default();
        let origin = IVec3::new(8, 8, 8);
        let mut first = [0u8; MICRO_BYTES];

        first[0] = 0b0000_0011;
        store.write_entry(origin, &first, &[7, 8]).unwrap();

        let mut second = [0u8; MICRO_BYTES];

        second[0] = 0b0000_0010;
        store.write_entry(origin, &second, &[9]).unwrap();

        assert_eq!(store.get(origin), None, "the dropped cell is cleared");
        assert_eq!(
            store.get(origin.saturating_add(cell_in_chunk(1))),
            Some(9),
            "the kept cell holds the new material"
        );
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn entry_write_matches_the_per_cell_default_on_randomized_entries() {
        let seed = 0x00E2_7A11;
        let mut rng = Rng::new(seed);
        let mut region = RegionStore::default();
        let mut wrapper = PerCellStore(RegionStore::default());

        for case in 0..256u32 {
            let origin = chunk_origin(&mut rng, 24);
            let mask = random_mask(&mut rng, 8);
            let materials = entry_materials(&mask, &mut rng);

            let context = format!("seed {seed:#x} case {case} at {origin}");

            region
                .write_entry(origin, &mask, &materials)
                .unwrap_or_else(|error| panic!("{context}: the override refused: {error}"));
            wrapper
                .write_entry(origin, &mask, &materials)
                .unwrap_or_else(|error| panic!("{context}: the default refused: {error}"));

            assert_entry_content(
                &region,
                origin,
                &mask,
                &materials,
                &format!("{context}: override"),
            );
            assert_entry_content(
                &wrapper,
                origin,
                &mask,
                &materials,
                &format!("{context}: default"),
            );
        }

        assert_eq!(
            content(&region),
            content(&wrapper),
            "seed {seed:#x}: the override and the default disagree"
        );
        assert_eq!(region.count(), wrapper.count(), "seed {seed:#x}: count");
        assert_eq!(region.bounds(), wrapper.bounds(), "seed {seed:#x}: bounds");
    }

    #[test]
    fn entry_write_rejects_an_origin_off_the_micro_chunk_grid() {
        let mut region = RegionStore::default();
        let mut wrapper = PerCellStore(RegionStore::default());
        let message = "voxel [1, 0, 0] is not a Micro-chunk origin";

        assert_write_rejected(
            &mut region,
            IVec3::new(1, 0, 0),
            &[0u8; MICRO_BYTES],
            &[],
            message,
        );
        assert_write_rejected(
            &mut wrapper,
            IVec3::new(1, 0, 0),
            &[0u8; MICRO_BYTES],
            &[],
            message,
        );
    }

    #[test]
    fn entry_write_rejects_an_origin_outside_the_lattice() {
        let mut region = RegionStore::default();
        let mut wrapper = PerCellStore(RegionStore::default());
        let message = "voxel [2048, 0, 0] is outside the lattice";

        assert_write_rejected(
            &mut region,
            IVec3::new(2048, 0, 0),
            &[0u8; MICRO_BYTES],
            &[],
            message,
        );
        assert_write_rejected(
            &mut wrapper,
            IVec3::new(2048, 0, 0),
            &[0u8; MICRO_BYTES],
            &[],
            message,
        );
    }

    #[test]
    fn entry_write_rejects_a_material_count_mismatch() {
        let mut region = RegionStore::default();
        let mut wrapper = PerCellStore(RegionStore::default());
        let mut mask = [0u8; MICRO_BYTES];

        mask[0] = 0b0000_0011;

        let message = "the mask marks 2 cells but carries 1 materials";

        assert_write_rejected(&mut region, IVec3::ZERO, &mask, &[1], message);
        assert_write_rejected(&mut wrapper, IVec3::ZERO, &mask, &[1], message);
    }

    #[test]
    fn entry_write_of_a_zero_mask_empties_the_micro_chunk() {
        let mut store = RegionStore::default();
        let origin = IVec3::new(24, -8, 0);
        let mut mask = [0u8; MICRO_BYTES];

        mask[0] = 0b0000_0011;
        store.write_entry(origin, &mask, &[5, 6]).unwrap();

        assert_eq!(store.count(), 2);

        store.write_entry(origin, &[0u8; MICRO_BYTES], &[]).unwrap();

        assert_eq!(store.count(), 0);
        assert_eq!(store.get(origin), None);
        assert!(store.chunk_entry(origin).is_none());
        assert!(
            store
                .regions
                .get(region_slot(origin))
                .is_some_and(Option::is_none),
            "the emptied region is released"
        );
    }

    #[test]
    fn entry_enumeration_matches_the_per_cell_default() {
        let seed = 0x00DE_FACE;
        let mut rng = Rng::new(seed);
        let mut region = RegionStore::default();
        let mut wrapper = PerCellStore(RegionStore::default());

        for case in 0..256u32 {
            let origin = chunk_origin(&mut rng, 24);
            let mask = random_mask(&mut rng, 8);
            let materials = entry_materials(&mask, &mut rng);

            let context = format!("seed {seed:#x} case {case} at {origin}");

            region
                .write_entry(origin, &mask, &materials)
                .unwrap_or_else(|error| panic!("{context}: the override refused: {error}"));
            wrapper
                .write_entry(origin, &mask, &materials)
                .unwrap_or_else(|error| panic!("{context}: the default refused: {error}"));
        }

        let entries: Vec<MicroChunkEntry> = region.entries().collect();
        let default_entries: Vec<MicroChunkEntry> = wrapper.entries().collect();

        assert_eq!(entries, default_entries, "seed {seed:#x}");

        let keys: Vec<(usize, usize)> = entries
            .iter()
            .map(|entry| (region_slot(entry.origin), micro_chunk_ordinal(entry.origin)))
            .collect();

        assert!(
            keys.windows(2).all(|pair| pair[0] < pair[1]),
            "the enumeration is strictly increasing in Region then ordinal"
        );
    }

    #[test]
    fn the_default_enumeration_buckets_out_of_order_voxels() {
        let seed = 0x00BA_5EBA;
        let mut rng = Rng::new(seed);
        let mut region = RegionStore::default();
        let mut reversed = ReversedStore(RegionStore::default());

        for case in 0..128u32 {
            let origin = chunk_origin(&mut rng, 24);
            let mask = random_mask(&mut rng, 8);
            let materials = entry_materials(&mask, &mut rng);

            let context = format!("seed {seed:#x} case {case} at {origin}");

            region
                .write_entry(origin, &mask, &materials)
                .unwrap_or_else(|error| panic!("{context}: the override refused: {error}"));
            reversed
                .write_entry(origin, &mask, &materials)
                .unwrap_or_else(|error| panic!("{context}: the default refused: {error}"));
        }

        let mut entries: Vec<MicroChunkEntry> = region.entries().collect();
        let mut reversed_entries: Vec<MicroChunkEntry> = reversed.entries().collect();

        entries.sort_unstable_by_key(|entry| entry.origin.to_array());
        reversed_entries.sort_unstable_by_key(|entry| entry.origin.to_array());

        assert_eq!(entries, reversed_entries, "seed {seed:#x}");
    }

    #[test]
    fn entry_write_reclaims_the_block_it_replaces() {
        let mut store = RegionStore::default();
        let origin = IVec3::new(0, 0, 0);
        let full = [0xFFu8; MICRO_BYTES];
        let full_materials: Vec<u8> = (0..MICRO_CHUNK_CELLS).map(|index| index as u8).collect();

        store.write_entry(origin, &full, &full_materials).unwrap();

        let after_full = store.storage_size().blob;

        let mut single = [0u8; MICRO_BYTES];

        single[0] = 1;
        store.write_entry(origin, &single, &[1]).unwrap();

        let after_single = store.storage_size().blob;

        assert!(
            after_single > after_full,
            "the smaller entry appends a new block"
        );

        store.write_entry(origin, &full, &full_materials).unwrap();

        assert_eq!(
            store.storage_size().blob,
            after_single,
            "the freed full block is reclaimed instead of appended"
        );
        assert_eq!(store.count(), MICRO_CHUNK_CELLS);
    }
}
