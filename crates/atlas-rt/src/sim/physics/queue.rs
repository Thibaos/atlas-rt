use std::mem;

use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::world::World;
use crate::world::grid::MICRO_CHUNK_LENGTH;
use crate::world::material::{PhysicalMaterialTable, Rule};
use crate::world::micro::{MICRO_BYTES, MICRO_EDGE};

const EDGE: usize = MICRO_EDGE;
const EDGE_I32: i32 = MICRO_CHUNK_LENGTH.cast_signed();

/// A queued Micro-chunk's cells as a 64-byte mask: byte `y * 8 + x` holds the z
/// bits of that (y, x), the transpose of the store's byte `y + 8z`.
#[derive(Clone, Copy, Debug)]
struct ChunkMask {
    bytes: [u8; MICRO_BYTES],
}

impl Default for ChunkMask {
    fn default() -> Self {
        Self {
            bytes: [0u8; MICRO_BYTES],
        }
    }
}

impl ChunkMask {
    const fn byte(x: usize, y: usize) -> usize {
        y.strict_mul(EDGE).strict_add(x)
    }

    fn holds(&self, bit: MaskBit) -> bool {
        self.bytes
            .get(bit.byte)
            .is_some_and(|byte| byte & bit.bit != 0)
    }

    /// Sets the bit and answers whether it was clear.
    fn insert(&mut self, bit: MaskBit) -> bool {
        let Some(byte) = self.bytes.get_mut(bit.byte) else {
            return false;
        };

        let was_clear = *byte & bit.bit == 0;

        *byte |= bit.bit;

        was_clear
    }

    /// The mask one row down: rows 0 to 6 from rows 1 to 7, and row 7 from
    /// `above`'s row 0 when the chunk above translates with this one.
    fn translated(&self, above: Option<[u8; EDGE]>) -> Self {
        let mut bytes = [0u8; MICRO_BYTES];

        for (index, byte) in bytes.iter_mut().take(MICRO_BYTES - EDGE).enumerate() {
            if let Some(source) = self.bytes.get(index.strict_add(EDGE)) {
                *byte = *source;
            }
        }

        if let Some(above) = above
            && let Some(top) = bytes.get_mut(MICRO_BYTES - EDGE..)
        {
            top.copy_from_slice(&transposed_row(above));
        }

        Self { bytes }
    }
}

/// The same mask in this layout: byte `y * 8 + x` holding the z bits of that
/// (y, x), where `mask` keeps byte `y + 8z` holding the x bits of that (y, z).
fn transpose(mask: &[u8; MICRO_BYTES]) -> [u8; MICRO_BYTES] {
    let mut out = [0u8; MICRO_BYTES];

    for y in 0..EDGE {
        let mut column = 0u64;

        for z in 0..EDGE {
            let byte = mask.get(z.strict_mul(EDGE).strict_add(y)).copied();
            let byte = u64::from(byte.unwrap_or(0));

            column |= byte << z.strict_mul(EDGE);
        }

        if let Some(slice) = out.get_mut(y.strict_mul(EDGE)..) {
            let transposed = transpose8(column).to_le_bytes();

            if let Some(slice) = slice.get_mut(..EDGE) {
                slice.copy_from_slice(&transposed);
            }
        }
    }

    out
}

/// A chunk row 0 in the store's layout as this layout's bytes for that row.
const fn transposed_row(row: [u8; EDGE]) -> [u8; EDGE] {
    transpose8(u64::from_le_bytes(row)).to_le_bytes()
}

/// The 8 by 8 bit transpose: bit `row * 8 + bit` of the answer is bit
/// `bit * 8 + row` of the input.
const fn transpose8(mut bits: u64) -> u64 {
    let swapped = (bits ^ (bits >> 7)) & 0x00AA_00AA_00AA_00AA;

    bits ^= swapped ^ (swapped << 7);

    let swapped = (bits ^ (bits >> 14)) & 0x0000_CCCC_0000_CCCC;

    bits ^= swapped ^ (swapped << 14);

    let swapped = (bits ^ (bits >> 28)) & 0x0000_0000_F0F0_F0F0;

    bits ^= swapped ^ (swapped << 28);

    bits
}

/// A cell's bit in its chunk's mask: the byte that holds its z row and the bit
/// that is its own z.
#[derive(Clone, Copy)]
struct MaskBit {
    byte: usize,
    bit: u8,
}

impl MaskBit {
    fn of(cell: IVec3) -> Self {
        let x = local(cell.x);
        let y = local(cell.y);
        let z = local(cell.z);

        Self {
            byte: ChunkMask::byte(x, y),
            bit: 1u8 << z,
        }
    }
}

/// A Micro-chunk's coordinates, the key its mask is stored under, ordered the
/// way one tick walks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ChunkId {
    y: i32,
    x: i32,
    z: i32,
}

impl ChunkId {
    const fn of(cell: IVec3) -> Self {
        Self {
            y: cell.y.div_euclid(EDGE_I32),
            x: cell.x.div_euclid(EDGE_I32),
            z: cell.z.div_euclid(EDGE_I32),
        }
    }

    const fn origin(self) -> IVec3 {
        IVec3::new(
            self.x.saturating_mul(EDGE_I32),
            self.y.saturating_mul(EDGE_I32),
            self.z.saturating_mul(EDGE_I32),
        )
    }
}

fn local(coordinate: i32) -> usize {
    usize::try_from(coordinate.rem_euclid(EDGE_I32)).unwrap_or(0)
}

/// One half of the queue: a mask per chunk it holds and the number of cells
/// those masks set.
#[derive(Clone, Debug, Default)]
struct ChunkMasks {
    chunks: FxHashMap<ChunkId, ChunkMask>,
    cells: usize,
}

impl ChunkMasks {
    const fn len(&self) -> usize {
        self.cells
    }

    const fn is_empty(&self) -> bool {
        self.cells == 0
    }

    fn contains(&self, cell: IVec3) -> bool {
        self.chunks
            .get(&ChunkId::of(cell))
            .is_some_and(|mask| mask.holds(MaskBit::of(cell)))
    }

    /// Queues the cell, allocating its chunk's mask on the first one.
    fn insert(&mut self, cell: IVec3) {
        let mask = self.chunks.entry(ChunkId::of(cell)).or_default();

        if mask.insert(MaskBit::of(cell)) {
            self.cells = self.cells.saturating_add(1);
        }
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.cells = 0;
    }

    /// Sets every bit `bytes` sets in the chunk's mask, counting only the ones
    /// that were clear.
    fn merge(&mut self, id: ChunkId, bytes: &[u8; MICRO_BYTES]) {
        let target = self.chunks.entry(id).or_default();
        let mut added = 0usize;

        for (index, byte) in bytes.iter().copied().enumerate() {
            let Some(slot) = target.bytes.get_mut(index) else {
                continue;
            };

            let fresh = !*slot & byte;

            *slot |= byte;
            added = added.saturating_add(fresh.count_ones() as usize);
        }

        self.cells = self.cells.saturating_add(added);
    }

    /// The chunks this half holds, in drain order: ascending chunk y, then x,
    /// then z.
    fn origins(&self) -> Vec<IVec3> {
        let mut keys: Vec<ChunkId> = self.chunks.keys().copied().collect();

        keys.sort_unstable();

        keys.into_iter().map(ChunkId::origin).collect()
    }

    fn walk(&self) -> CellWalk<'_> {
        CellWalk::new(self, None)
    }

    fn walk_except(&self, skip: &FxHashSet<IVec3>) -> CellWalk<'_> {
        CellWalk::new(self, Some(skip))
    }
}

/// The cells a half holds, in drain order: ascending y, then x, then z, and
/// none of the chunks a run pass translates for itself.
struct CellWalk<'a> {
    chunks: &'a FxHashMap<ChunkId, ChunkMask>,
    keys: Vec<ChunkId>,
    skipped: Vec<bool>,
    remaining: usize,
    level: usize,
    level_end: usize,
    row: usize,
    row_end: usize,
    chunk: usize,
    y: usize,
    x: usize,
    bits: u8,
}

impl<'a> CellWalk<'a> {
    fn new(masks: &'a ChunkMasks, skip: Option<&FxHashSet<IVec3>>) -> Self {
        let mut keys: Vec<ChunkId> = masks.chunks.keys().copied().collect();

        keys.sort_unstable();

        let skipped: Vec<bool> = keys
            .iter()
            .map(|key| skip.is_some_and(|skip| skip.contains(&key.origin())))
            .collect();
        let level_end = level_extent(&keys, 0);
        let row_end = row_extent(&keys, 0);

        let mut walk = Self {
            chunks: &masks.chunks,
            keys,
            skipped,
            remaining: masks.cells,
            level: 0,
            level_end,
            row: 0,
            row_end,
            chunk: 0,
            y: 0,
            x: 0,
            bits: 0,
        };

        walk.bits = walk.byte();

        walk
    }

    /// The z bits at the walk's current (y, x) chunk-local pair.
    fn byte(&self) -> u8 {
        let Some(key) = self.keys.get(self.chunk) else {
            return 0;
        };

        if self.skipped.get(self.chunk).copied().unwrap_or(true) {
            return 0;
        }

        let Some(mask) = self.chunks.get(key) else {
            return 0;
        };

        mask.bytes
            .get(ChunkMask::byte(self.x, self.y))
            .copied()
            .unwrap_or(0)
    }

    fn cell(&self, z: usize) -> Option<IVec3> {
        let key = self.keys.get(self.chunk)?;

        Some(IVec3::new(
            key.x
                .saturating_mul(EDGE_I32)
                .saturating_add(offset(self.x)),
            key.y
                .saturating_mul(EDGE_I32)
                .saturating_add(offset(self.y)),
            key.z.saturating_mul(EDGE_I32).saturating_add(offset(z)),
        ))
    }
}

impl Iterator for CellWalk<'_> {
    type Item = IVec3;

    fn next(&mut self) -> Option<IVec3> {
        if self.remaining == 0 || self.level >= self.keys.len() {
            return None;
        }

        loop {
            if self.bits != 0 {
                let z = usize::try_from(self.bits.trailing_zeros()).unwrap_or(0);

                self.bits &= self.bits.wrapping_sub(1);
                self.remaining = self.remaining.saturating_sub(1);

                return self.cell(z);
            }

            self.chunk = self.chunk.saturating_add(1);

            if self.chunk < self.row_end {
                self.bits = self.byte();

                continue;
            }

            self.x = self.x.saturating_add(1);
            self.chunk = self.row;

            if self.x < EDGE {
                self.bits = self.byte();

                continue;
            }

            self.x = 0;
            self.row = self.row_end;
            self.row_end = row_extent(&self.keys, self.row);
            self.chunk = self.row;

            if self.row < self.level_end {
                self.bits = self.byte();

                continue;
            }

            self.y = self.y.saturating_add(1);
            self.row = self.level;
            self.row_end = row_extent(&self.keys, self.level);
            self.chunk = self.row;

            if self.y < EDGE {
                self.bits = self.byte();

                continue;
            }

            self.y = 0;
            self.level = self.level_end;
            self.level_end = level_extent(&self.keys, self.level);
            self.row = self.level;
            self.row_end = row_extent(&self.keys, self.level);
            self.chunk = self.row;
            self.bits = self.byte();

            if self.level >= self.keys.len() {
                return None;
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

fn offset(local: usize) -> i32 {
    i32::try_from(local).unwrap_or(0)
}

/// One past the last key sharing the first key's chunk y.
fn level_extent(keys: &[ChunkId], start: usize) -> usize {
    let Some(first) = keys.get(start) else {
        return start;
    };

    let run = keys
        .get(start..)
        .unwrap_or_default()
        .iter()
        .take_while(|key| key.y == first.y)
        .count();

    start.saturating_add(run)
}

/// One past the last key sharing the first key's chunk y and x.
fn row_extent(keys: &[ChunkId], start: usize) -> usize {
    let Some(first) = keys.get(start) else {
        return start;
    };

    let run = keys
        .get(start..)
        .unwrap_or_default()
        .iter()
        .take_while(|key| key.y == first.y && key.x == first.x)
        .count();

    start.saturating_add(run)
}

/// The voxel coordinates the voxel rules have not evaluated yet, held in the
/// order a tick drains them and never as World state. Settled means absent, so
/// a Falling granular cell blocks the player only while this holds it.
///
/// A tick serves every cell the queue holds. One half is the cells this tick
/// serves and the other is the cells its moves and wakes queue for the next, so
/// a grain moves at most once a tick and no cell is served twice in one.
#[derive(Clone, Debug, Default)]
pub(in crate::sim) struct UpdateQueue {
    ready: ChunkMasks,
    next: ChunkMasks,
}

impl UpdateQueue {
    /// Replaces the queue with the Falling granular cells of `world`, the seed
    /// every activation starts from: all of them ready for the first tick. A
    /// generator's `granular_cells` seeds in time proportional to the cells;
    /// `None` scans the World, which a `.vox` load and a failed rule batch need.
    pub(in crate::sim) fn seed(
        &mut self,
        world: &World,
        table: &PhysicalMaterialTable,
        granular_cells: Option<&[IVec3]>,
    ) {
        self.ready = ChunkMasks::default();
        self.next = ChunkMasks::default();

        if let Some(cells) = granular_cells {
            for cell in cells {
                self.ready.insert(*cell);
            }

            return;
        }

        for (position, voxel) in world.iter_voxels() {
            if table.get(voxel).rule == Rule::FallingGranular {
                self.ready.insert(position);
            }
        }
    }

    pub(in crate::sim) const fn len(&self) -> usize {
        self.ready.len().saturating_add(self.next.len())
    }

    /// Whether the queue holds the cell, which is what in flight and blocking
    /// ask: both halves are grains the rules have not settled.
    pub(in crate::sim) fn contains(&self, cell: IVec3) -> bool {
        self.ready.contains(cell) || self.next.contains(cell)
    }

    /// Every queued cell, this tick's in their order and then the next tick's,
    /// which only the tests read.
    #[cfg(test)]
    pub(in crate::sim) fn iter(&self) -> impl Iterator<Item = IVec3> {
        self.ready.walk().chain(self.next.walk())
    }

    /// The cells this tick serves, in drain order.
    pub(in crate::sim) fn unserved(&self) -> impl Iterator<Item = IVec3> + '_ {
        self.ready.walk()
    }

    /// The cells this tick serves that no run translates for itself, in drain
    /// order.
    pub(in crate::sim) fn unserved_except(
        &self,
        skip: &FxHashSet<IVec3>,
    ) -> impl Iterator<Item = IVec3> + '_ {
        self.ready.walk_except(skip)
    }

    /// The chunks this tick serves, in ascending chunk y.
    pub(in crate::sim) fn ready_chunks(&self) -> Vec<IVec3> {
        self.ready.origins()
    }

    /// Whether the queue holds every cell `mask` marks. A grain this tick does
    /// not serve would not move, so a chunk holding one is served cell by cell.
    pub(in crate::sim) fn holds_all(&self, origin: IVec3, mask: &[u8; MICRO_BYTES]) -> bool {
        self.ready
            .chunks
            .get(&ChunkId::of(origin))
            .is_some_and(|held| held.bytes == transpose(mask))
    }

    /// Queues a translated chunk's cells where it leaves them: its cells one row
    /// down, with `above`'s row 0 as the new top row, counting only the bits the
    /// queue did not hold already.
    pub(in crate::sim) fn translate(&mut self, origin: IVec3, above: Option<[u8; MICRO_EDGE]>) {
        let id = ChunkId::of(origin);

        let Some(mask) = self
            .ready
            .chunks
            .get(&id)
            .map(|held| held.translated(above))
        else {
            return;
        };

        self.next.merge(id, &mask.bytes);
    }

    /// Queues every cell `mask` marks for the next tick, counting only the bits
    /// the queue did not hold already. `mask` is in the store's layout, which is
    /// the one a shift of rows is written in.
    pub(in crate::sim) fn queue_mask(&mut self, origin: IVec3, mask: &[u8; MICRO_BYTES]) {
        self.next.merge(ChunkId::of(origin), &transpose(mask));
    }

    /// Makes the cells the last tick queued this tick's own. A tick ends once
    /// its ready half is served, so the swap waits only for the first tick
    /// after a seed, which already holds its cells.
    pub(in crate::sim) const fn begin_tick(&mut self) {
        if self.ready.is_empty() {
            mem::swap(&mut self.ready, &mut self.next);
        }
    }

    /// Drops the cells this tick served: a grain that moved is queued at its
    /// destination for the next tick, and one that settled leaves the queue.
    pub(in crate::sim) fn end_tick(&mut self) {
        self.ready.clear();
    }

    /// Queues a mover's destination for the next tick, since the grain was
    /// served this one.
    pub(in crate::sim) fn mark_moved(&mut self, cell: IVec3) {
        self.next.insert(cell);
    }

    /// Queues a cell whose support changed for the next tick, unless the queue
    /// holds it already. A cell served in this tick is gone from the ready half
    /// by the time the wakes arrive, so its wake queues it for the next.
    pub(in crate::sim) fn wake(&mut self, cell: IVec3) {
        if self.contains(cell) {
            return;
        }

        self.next.insert(cell);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::material::parse_override;

    fn table() -> PhysicalMaterialTable {
        parse_override("material 1 falling_granular solid=true")
            .unwrap_or_else(|rejections| panic!("{rejections:?}"))
    }

    fn world_with(cells: &[(IVec3, u8)]) -> World {
        let mut world = World::default();

        for (position, material) in cells {
            world.set_voxel(*position, *material);
        }

        world
    }

    fn mixed_world() -> World {
        world_with(&[
            (IVec3::new(1, 2, 3), 1),
            (IVec3::new(4, 5, 6), 0),
            (IVec3::new(-1, 0, 0), 1),
        ])
    }

    fn unserved(queue: &UpdateQueue) -> Vec<IVec3> {
        queue.unserved().collect()
    }

    /// The store's mask for the cells, in the layout byte `y + 8z`, bit `x`.
    fn store_mask(cells: &[IVec3]) -> [u8; MICRO_BYTES] {
        let mut mask = [0u8; MICRO_BYTES];

        for cell in cells {
            let index = crate::world::micro::mask_index(*cell);

            if let Some(byte) = mask.get_mut(index.strict_div(EDGE)) {
                *byte |= 1u8 << index.strict_rem(EDGE);
            }
        }

        mask
    }

    /// The same mask gathered a bit at a time, which is the oracle the word
    /// transpose has to match.
    fn gathered(mask: &[u8; MICRO_BYTES]) -> [u8; MICRO_BYTES] {
        let mut out = [0u8; MICRO_BYTES];

        for index in 0..MICRO_BYTES {
            let y = index.strict_rem(EDGE);
            let z = index.strict_div(EDGE);

            for x in 0..EDGE {
                let set = mask.get(index).is_some_and(|byte| byte & (1u8 << x) != 0);

                if !set {
                    continue;
                }

                if let Some(slot) = out.get_mut(y.strict_mul(EDGE).strict_add(x)) {
                    *slot |= 1u8 << z;
                }
            }
        }

        out
    }

    fn queue_mask(cells: &[IVec3]) -> ChunkMask {
        let mut mask = ChunkMask::default();

        for cell in cells {
            mask.insert(MaskBit::of(*cell));
        }

        mask
    }

    #[test]
    fn the_transposed_mask_matches_a_bit_at_a_time_gather() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;

        for case in 0..64 {
            let mut mask = [0u8; MICRO_BYTES];

            for byte in &mut mask {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = u8::try_from(state & 0xFF).unwrap_or(0);
            }

            assert_eq!(transpose(&mask), gathered(&mask), "case {case}");
        }
    }

    #[test]
    fn a_transposed_row_turns_each_z_byte_into_an_x_byte() {
        let mut row = [0u8; EDGE];

        if let Some(byte) = row.get_mut(0) {
            *byte = 0b0000_0010;
        }

        if let Some(byte) = row.get_mut(5) {
            *byte = 0b0000_0100;
        }

        let transposed = transposed_row(row);

        assert_eq!(
            transposed.first().copied(),
            Some(0),
            "column 0 is not marked"
        );
        assert_eq!(
            transposed.get(1).copied(),
            Some(0b0000_0001),
            "byte `x` holds the z bits of that column"
        );
        assert_eq!(transposed.get(2).copied(), Some(0b0010_0000));
    }

    #[test]
    fn a_translated_mask_moves_every_row_down_and_takes_its_top_from_above() {
        let mask = queue_mask(&[
            IVec3::new(0, 0, 0),
            IVec3::new(3, 1, 2),
            IVec3::new(7, 7, 7),
        ]);
        let mut above = [0u8; EDGE];

        if let Some(byte) = above.get_mut(0) {
            *byte = 0b0000_0010;
        }

        let shifted = mask.translated(Some(above));

        assert!(
            !shifted.holds(MaskBit::of(IVec3::new(0, 0, 0))),
            "row 0 of the source falls into the chunk below"
        );
        assert!(shifted.holds(MaskBit::of(IVec3::new(7, 6, 7))));
        assert!(
            shifted.holds(MaskBit::of(IVec3::new(1, 7, 0))),
            "the top row is the chunk above's row 0"
        );
        assert_eq!(
            shifted
                .bytes
                .iter()
                .map(|byte| byte.count_ones())
                .sum::<u32>(),
            3,
            "three cells in, three cells out"
        );

        let orphaned = mask.translated(None);

        assert!(
            !orphaned.holds(MaskBit::of(IVec3::new(1, 7, 0))),
            "a run's top row stays empty when no chunk above translates into it"
        );
    }

    #[test]
    fn a_chunk_the_queue_holds_every_cell_of_is_held_all() {
        let cells = [
            IVec3::new(0, 0, 0),
            IVec3::new(5, 3, 6),
            IVec3::new(7, 7, 7),
        ];
        let mask = store_mask(&cells);
        let mut queue = UpdateQueue::default();

        queue.seed(&World::default(), &table(), Some(&cells));

        assert!(queue.holds_all(IVec3::new(0, 0, 0), &mask));

        let partial = store_mask(&[IVec3::new(0, 0, 0), IVec3::new(5, 3, 6)]);

        assert!(
            !queue.holds_all(IVec3::new(0, 0, 0), &partial),
            "a mask holding fewer cells than the queue does is not held all"
        );

        let more = store_mask(&[
            IVec3::new(0, 0, 0),
            IVec3::new(5, 3, 6),
            IVec3::new(7, 7, 7),
            IVec3::new(1, 2, 3),
        ]);

        assert!(
            !queue.holds_all(IVec3::new(0, 0, 0), &more),
            "a mask holding a cell the queue does not is not held all"
        );
    }

    #[test]
    fn a_walk_skipping_a_chunk_leaves_none_of_its_cells() {
        let mut queue = UpdateQueue::default();
        let cells = [
            IVec3::new(0, 1, 0),
            IVec3::new(8, 1, 0),
            IVec3::new(0, 2, 0),
            IVec3::new(9, 0, 3),
        ];

        queue.seed(&World::default(), &table(), Some(&cells));
        queue.begin_tick();

        let skip: FxHashSet<IVec3> = [IVec3::new(8, 0, 0)].into_iter().collect();

        assert_eq!(
            queue.unserved_except(&skip).collect::<Vec<_>>(),
            vec![IVec3::new(0, 1, 0), IVec3::new(0, 2, 0)],
            "the skipped chunk's two cells are gone and the rest keep their order"
        );
        assert_eq!(unserved(&queue).len(), 4, "the walk itself is unchanged");
    }

    #[test]
    fn a_translated_chunk_queues_its_cells_where_it_leaves_them() {
        let mut queue = UpdateQueue::default();
        let cells = [
            IVec3::new(0, 0, 0),
            IVec3::new(1, 1, 0),
            IVec3::new(2, 2, 0),
        ];

        queue.seed(&World::default(), &table(), Some(&cells));
        queue.begin_tick();

        let mut above = [0u8; EDGE];

        if let Some(byte) = above.get_mut(0) {
            *byte = 0b0000_0001;
        }

        queue.translate(IVec3::new(0, 0, 0), Some(above));
        queue.end_tick();
        queue.begin_tick();

        assert_eq!(
            unserved(&queue),
            vec![
                IVec3::new(1, 0, 0),
                IVec3::new(2, 1, 0),
                IVec3::new(0, 7, 0),
            ],
            "every cell is one row down and the top row is the chunk above's"
        );
    }

    #[test]
    fn a_translation_the_queue_already_holds_some_of_does_not_count_twice() {
        let mut queue = UpdateQueue::default();
        let cells = [IVec3::new(0, 1, 0), IVec3::new(1, 2, 0)];

        queue.seed(&World::default(), &table(), Some(&cells));
        queue.begin_tick();
        queue.mark_moved(IVec3::new(0, 0, 0));
        queue.translate(IVec3::new(0, 0, 0), None);
        queue.end_tick();

        assert_eq!(
            queue.len(),
            2,
            "the destination the wake already queued is the one the translation moves into"
        );
    }

    #[test]
    fn a_scan_seeds_every_falling_granular_cell() {
        let mut queue = UpdateQueue::default();

        queue.seed(&mixed_world(), &table(), None);

        assert_eq!(queue.len(), 2);
        assert!(queue.contains(IVec3::new(1, 2, 3)));
        assert!(queue.contains(IVec3::new(-1, 0, 0)));
        assert!(
            !queue.contains(IVec3::new(4, 5, 6)),
            "a solid cell is not queued"
        );
    }

    #[test]
    fn a_provided_list_seeds_the_same_cells_as_the_scan() {
        let world = mixed_world();
        let table = table();
        let listed = [IVec3::new(1, 2, 3), IVec3::new(-1, 0, 0)];

        let mut scanned = UpdateQueue::default();
        scanned.seed(&world, &table, None);

        let mut seeded = UpdateQueue::default();
        seeded.seed(&world, &table, Some(&listed));

        assert_eq!(
            seeded.iter().collect::<Vec<_>>(),
            scanned.iter().collect::<Vec<_>>(),
            "both seeds queue the same cells in the same order"
        );
    }

    #[test]
    fn a_seed_holds_every_cell_for_the_first_tick() {
        let mut queue = UpdateQueue::default();

        queue.seed(&mixed_world(), &table(), None);

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(-1, 0, 0), IVec3::new(1, 2, 3)],
            "every seeded cell is served in the first tick, in drain order"
        );
        assert!(
            queue.next.is_empty(),
            "a seed queues nothing for a later tick"
        );

        queue.begin_tick();

        assert_eq!(
            unserved(&queue).len(),
            2,
            "the first tick serves the seed's own cells"
        );
    }

    #[test]
    fn the_unserved_half_iterates_in_drain_order() {
        let mut queue = UpdateQueue::default();
        let cells = [
            IVec3::new(1, 5, 0),
            IVec3::new(0, 4, 9),
            IVec3::new(0, 4, 1),
            IVec3::new(3, 4, 1),
            IVec3::new(0, 6, 0),
            IVec3::new(0, 4, 1),
            IVec3::new(-1, 4, -1),
            IVec3::new(-9, 4, 0),
            IVec3::new(8, 4, 0),
            IVec3::new(9, 4, 8),
            IVec3::new(1, 3, 0),
        ];

        queue.seed(&World::default(), &table(), Some(&cells));

        assert_eq!(
            unserved(&queue),
            vec![
                IVec3::new(1, 3, 0),
                IVec3::new(-9, 4, 0),
                IVec3::new(-1, 4, -1),
                IVec3::new(0, 4, 1),
                IVec3::new(0, 4, 9),
                IVec3::new(3, 4, 1),
                IVec3::new(8, 4, 0),
                IVec3::new(9, 4, 8),
                IVec3::new(1, 5, 0),
                IVec3::new(0, 6, 0),
            ],
            "y leads, then x, then z, across chunks and negative coordinates, and a repeated cell stays one entry"
        );
    }

    #[test]
    fn a_destination_waits_for_the_next_tick() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 5, 0));
        queue.begin_tick();
        queue.mark_moved(IVec3::new(0, 4, 0));

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 5, 0)],
            "this tick serves the cells it started with"
        );
        assert!(
            queue.contains(IVec3::new(0, 4, 0)),
            "the destination is queued for the next tick"
        );
        assert_eq!(queue.len(), 2, "both halves count toward the queue");

        queue.end_tick();
        queue.begin_tick();

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 4, 0)],
            "the next tick serves the destination"
        );
    }

    #[test]
    fn a_wake_of_a_queued_cell_leaves_it_in_place() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 5, 0));
        queue.wake(IVec3::new(0, 5, 0));

        assert_eq!(queue.len(), 1, "a woken cell is queued once");
    }

    #[test]
    fn a_wake_of_a_cell_the_queue_does_not_hold_waits_for_the_next_tick() {
        let mut queue = UpdateQueue::default();

        queue.begin_tick();
        queue.wake(IVec3::new(0, 3, 0));

        assert!(
            unserved(&queue).is_empty(),
            "the tick's cells are fixed once it begins"
        );
        assert!(
            queue.contains(IVec3::new(0, 3, 0)),
            "the woken cell is queued for the next tick"
        );
    }

    #[test]
    fn a_wake_after_the_tick_served_the_cell_queues_it_again() {
        let mut queue = UpdateQueue::default();
        let cell = IVec3::new(0, 5, 0);

        queue.wake(cell);
        queue.begin_tick();
        queue.end_tick();
        queue.wake(cell);

        assert!(unserved(&queue).is_empty(), "the cell was served this tick");
        assert!(
            queue.contains(cell),
            "a wake after the tick served the cell queues it for the next one"
        );

        queue.begin_tick();

        assert_eq!(
            unserved(&queue),
            vec![cell],
            "the next tick serves the woken cell again"
        );
    }

    #[test]
    fn a_settled_cell_leaves_the_queue() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 4, 0));
        queue.begin_tick();
        queue.end_tick();

        assert_eq!(queue.len(), 0, "a settled cell loses its place");
        assert!(!queue.contains(IVec3::new(0, 4, 0)));
    }
}
