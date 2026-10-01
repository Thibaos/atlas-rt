use std::fmt;

use glam::IVec3;

use crate::world::{grid, store::VoxelStore};

const MICRO_CHUNK: usize = grid::MICRO_CHUNK_LENGTH as usize;
const MICRO_CHUNK_SIDE: usize = (grid::REGION_LENGTH / grid::MICRO_CHUNK_LENGTH) as usize;
const MICRO_CHUNK_SIDE_AREA: usize = MICRO_CHUNK_SIDE * MICRO_CHUNK_SIDE;
const MICRO_CHUNK_AREA: usize = MICRO_CHUNK * MICRO_CHUNK;
const MICRO_CHUNK_CELLS: usize = MICRO_CHUNK * MICRO_CHUNK * MICRO_CHUNK;
const MICRO_CHUNKS_PER_REGION: usize = MICRO_CHUNK_SIDE * MICRO_CHUNK_SIDE * MICRO_CHUNK_SIDE;
const MASK_BYTES: usize = MICRO_CHUNK_CELLS / 8;
const OFFSET_SENTINEL: u32 = u32::MAX;

/// A live Region: one blob offset per Micro-chunk, plus the occupied
/// Micro-chunks concatenated. Each entry is a 64-byte Occupancy mask followed
/// by the material indices of its occupied cells in ascending cell order.
struct Region {
    index: Box<[u32]>,
    blob: Vec<u8>,
    count: usize,
}

impl Region {
    fn new() -> Self {
        Self {
            index: vec![OFFSET_SENTINEL; MICRO_CHUNKS_PER_REGION].into_boxed_slice(),
            blob: Vec::new(),
            count: 0,
        }
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

/// The Region slot a position falls in, which is its region id.
pub(in crate::world) fn region_slot(position: IVec3) -> usize {
    grid::region_id(grid::region_index_of(position)) as usize
}

/// The sort key that makes a serial build append: Micro-chunk ordinal first,
/// then ascending cell index inside the Micro-chunk.
pub(in crate::world) fn micro_chunk_key(position: IVec3) -> (usize, usize) {
    (micro_chunk_ordinal(position), cell_index(position))
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

/// The number of set mask bits before `cell`, which indexes the material
/// indices.
fn rank(mask: &[u8], cell: usize) -> usize {
    let byte = cell.strict_div(8);
    let bit = u32::try_from(cell.strict_rem(8)).unwrap_or(0);

    let below: usize = mask
        .iter()
        .take(byte)
        .map(|value| value.count_ones() as usize)
        .sum();
    let own = mask.get(byte).copied().unwrap_or(0) & 1u8.wrapping_shl(bit).wrapping_sub(1);

    below.saturating_add(own.count_ones() as usize)
}

/// Moves every offset at or past `from` by `delta`.
fn shift_offsets(index: &mut [u32], from: usize, delta: isize) {
    for entry in index {
        if *entry == OFFSET_SENTINEL {
            continue;
        }

        let offset = usize::try_from(*entry).unwrap_or(usize::MAX);

        if offset >= from {
            let moved = offset.wrapping_add_signed(delta);

            *entry = u32::try_from(moved).unwrap_or(OFFSET_SENTINEL);
        }
    }
}

fn entry_mask(region: &Region, offset: usize) -> &[u8] {
    region
        .blob
        .get(offset..offset.strict_add(MASK_BYTES))
        .unwrap_or(&[])
}

fn append_entry(region: &mut Region, ordinal: usize, position: IVec3, material: u8) {
    let (byte, bit) = mask_bit(position);
    let mut mask = [0u8; MASK_BYTES];

    if let Some(slot) = mask.get_mut(byte) {
        *slot = bit;
    }

    let offset = region.blob.len();

    if let Some(slot) = region.index.get_mut(ordinal) {
        *slot = u32::try_from(offset).unwrap_or(OFFSET_SENTINEL);
    }

    region.blob.extend_from_slice(&mask);
    region.blob.push(material);
}

/// Inserts a material index at `at`, shifting only when the entry is not the
/// blob's tail. A build that writes Micro-chunks in ordinal order always
/// appends.
fn insert_material(region: &mut Region, at: usize, material: u8) {
    if at == region.blob.len() {
        region.blob.push(material);
    } else {
        region.blob.insert(at, material);
        shift_offsets(&mut region.index, at, 1);
    }
}

fn remove_material(region: &mut Region, at: usize) {
    if at.strict_add(1) == region.blob.len() {
        region.blob.pop();
    } else {
        region.blob.remove(at);
        shift_offsets(&mut region.index, at, -1);
    }
}

/// Drops an emptied Micro-chunk's entry and shifts the blob tail left.
fn remove_entry(region: &mut Region, ordinal: usize, offset: usize) {
    if let Some(slot) = region.index.get_mut(ordinal) {
        *slot = OFFSET_SENTINEL;
    }

    region.blob.drain(offset..offset.strict_add(MASK_BYTES));
    shift_offsets(
        &mut region.index,
        offset,
        0isize.wrapping_sub(isize::try_from(MASK_BYTES).unwrap_or(0)),
    );
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
}

impl<'a> Voxels<'a> {
    const fn new(store: &'a RegionStore) -> Self {
        Self {
            store,
            slot: 0,
            ordinal: 0,
            cell: 0,
        }
    }

    const fn advance_slot(&mut self) {
        self.slot = self.slot.saturating_add(1);
        self.ordinal = 0;
        self.cell = 0;
    }

    const fn advance_micro_chunk(&mut self) {
        self.ordinal = self.ordinal.saturating_add(1);
        self.cell = 0;
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

            let offset = region
                .index
                .get(self.ordinal)
                .copied()
                .unwrap_or(OFFSET_SENTINEL);

            if offset == OFFSET_SENTINEL {
                self.advance_micro_chunk();
                continue;
            }

            let offset = usize::try_from(offset).unwrap_or(usize::MAX);

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
            self.cell = self.cell.strict_add(1);

            let material = region
                .blob
                .get(offset.strict_add(MASK_BYTES).strict_add(rank(mask, cell)))
                .copied()?;

            return Some((cell_position(self.slot, self.ordinal, cell), material));
        }
    }
}

impl VoxelStore for RegionStore {
    fn set(&mut self, position: IVec3, material: u8) -> bool {
        let slot = region_slot(position);
        let ordinal = micro_chunk_ordinal(position);

        let Some(slot_region) = self.regions.get_mut(slot) else {
            return false;
        };

        let region = slot_region.get_or_insert_with(Region::new);
        let offset = region
            .index
            .get(ordinal)
            .copied()
            .unwrap_or(OFFSET_SENTINEL);

        if offset == OFFSET_SENTINEL {
            append_entry(region, ordinal, position, material);
            region.count = region.count.saturating_add(1);
            self.count = self.count.saturating_add(1);

            return false;
        }

        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let cell = cell_index(position);
        let (byte, bit) = mask_bit(position);
        let occupied = region.blob.get(offset.strict_add(byte)).copied().unwrap_or(0) & bit != 0;

        if occupied {
            let material_at = offset
                .strict_add(MASK_BYTES)
                .strict_add(rank(entry_mask(region, offset), cell));

            if let Some(slot) = region.blob.get_mut(material_at) {
                *slot = material;
            }

            return true;
        }

        if let Some(slot) = region.blob.get_mut(offset.strict_add(byte)) {
            *slot |= bit;
        }

        let at = offset
            .strict_add(MASK_BYTES)
            .strict_add(rank(entry_mask(region, offset), cell));
        insert_material(region, at, material);

        region.count = region.count.saturating_add(1);
        self.count = self.count.saturating_add(1);

        false
    }

    fn get(&self, position: IVec3) -> Option<u8> {
        let slot = region_slot(position);
        let region = self.regions.get(slot)?.as_ref()?;
        let offset = region
            .index
            .get(micro_chunk_ordinal(position))
            .copied()
            .unwrap_or(OFFSET_SENTINEL);

        if offset == OFFSET_SENTINEL {
            return None;
        }

        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
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

        let offset = region
            .index
            .get(ordinal)
            .copied()
            .unwrap_or(OFFSET_SENTINEL);

        if offset == OFFSET_SENTINEL {
            return;
        }

        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let cell = cell_index(position);
        let (byte, bit) = mask_bit(position);

        if region.blob.get(offset.strict_add(byte)).copied().unwrap_or(0) & bit == 0 {
            return;
        }

        let at = offset
            .strict_add(MASK_BYTES)
            .strict_add(rank(entry_mask(region, offset), cell));
        remove_material(region, at);

        if let Some(slot) = region.blob.get_mut(offset.strict_add(byte)) {
            *slot &= !bit;
        }

        region.count = region.count.saturating_sub(1);
        self.count = self.count.saturating_sub(1);
        let emptied = region.count == 0;

        let empty = entry_mask(region, offset).iter().all(|value| *value == 0);

        if empty {
            remove_entry(region, ordinal, offset);
        }

        if emptied {
            *slot_region = None;
        }
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (IVec3, u8)> + '_> {
        Box::new(Voxels::new(self))
    }

    fn count(&self) -> usize {
        self.count
    }

    #[cfg(test)]
    fn reserved_capacity(&self) -> usize {
        let table = self
            .regions
            .len()
            .saturating_mul(std::mem::size_of::<Option<Region>>());
        let index = self
            .regions
            .iter()
            .flatten()
            .count()
            .saturating_mul(MICRO_CHUNKS_PER_REGION)
            .saturating_mul(std::mem::size_of::<u32>());
        let blob: usize = self
            .regions
            .iter()
            .flatten()
            .map(|region| region.blob.capacity())
            .sum();

        table.saturating_add(index).saturating_add(blob)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::world::{
        BoundsPolicy, World,
        store::ShardedMap,
        test_support::{Rng, u8_below},
    };

    use super::*;

    fn content(store: &dyn VoxelStore) -> HashMap<IVec3, u8> {
        store.iter().collect()
    }

    fn random_position(rng: &mut Rng) -> IVec3 {
        let mut axis = || i32::try_from(rng.below(512)).unwrap_or(0).wrapping_sub(256);

        IVec3::new(axis(), axis(), axis())
    }

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
            OFFSET_SENTINEL,
            "the first micro-chunk has an entry"
        );

        store.clear(first);

        let region = store.regions.get(slot).and_then(Option::as_ref);
        let Some(region) = region else {
            panic!("the region still holds the second micro-chunk");
        };

        assert_eq!(
            region.index[micro_chunk_ordinal(first)],
            OFFSET_SENTINEL,
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
                if *entry == OFFSET_SENTINEL {
                    continue;
                }

                let offset = usize::try_from(*entry).unwrap_or(usize::MAX);
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

    fn assert_worlds_agree(region: &World, map: &World) {
        assert_eq!(region.voxel_count(), map.voxel_count());
        assert_eq!(region.voxel_bounds(), map.voxel_bounds());

        let region_content: HashMap<IVec3, u8> = region.iter_voxels().collect();
        let map_content: HashMap<IVec3, u8> = map.iter_voxels().collect();

        assert_eq!(region_content, map_content);

        for (position, material) in &region_content {
            assert_eq!(region.get_voxel(position), Some(*material));
            assert_eq!(map.get_voxel(position), Some(*material));
            assert!(region.contains(position));
            assert!(map.contains(position));
        }
    }

    fn asset_answers_match_the_map(path: &str) {
        let data = dot_vox::load(path).unwrap();
        let (region, region_clipped) = World::new_clipped(&data);

        let mut map = World::from_store(Box::new(ShardedMap::default()));
        let map_clipped = crate::world::load::build::load_into(&mut map, &data, BoundsPolicy::Clip);

        assert_eq!(region_clipped, map_clipped, "clipped counts diverge");
        assert_worlds_agree(&region, &map);
    }

    #[test]
    #[ignore = "asset: cargo test --release church_answers_match_the_map -- --ignored --nocapture"]
    fn church_answers_match_the_map() {
        asset_answers_match_the_map("assets/church.vox");
    }

    #[test]
    #[ignore = "asset: cargo test --release bistro_answers_match_the_map -- --ignored --nocapture"]
    fn bistro_answers_match_the_map() {
        asset_answers_match_the_map("assets/bistro.vox");
    }
}
