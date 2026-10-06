//! The Micro-chunk payload's width and the rules that read and write it.

use std::{error::Error, fmt, fmt::Display};

use glam::IVec3;

use crate::world::grid::MICRO_CHUNK_LENGTH;

pub const MICRO_EDGE: usize = MICRO_CHUNK_LENGTH as usize;
pub const MICRO_AREA: usize = MICRO_EDGE * MICRO_EDGE;
pub const MICRO_CELLS: usize = MICRO_EDGE * MICRO_AREA;
pub const MICRO_BYTES: usize = MICRO_CELLS / MICRO_EDGE;

/// A Micro-chunk's payload where it is owned rather than borrowed: the
/// Occupancy mask and the materials of the occupied cells in ascending cell
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroChunk {
    mask: [u8; MICRO_BYTES],
    materials: Vec<u8>,
}

/// Why a mask and a material list do not make a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicroChunkError {
    MaterialCount { occupied: usize, given: usize },
}

impl MicroChunk {
    /// # Errors
    ///
    /// Rejects a material count that disagrees with the mask's occupancy.
    pub fn new(mask: [u8; MICRO_BYTES], materials: Vec<u8>) -> Result<Self, MicroChunkError> {
        check_materials(&mask, &materials)?;

        Ok(Self { mask, materials })
    }

    /// The payload that occupies nothing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            mask: [0u8; MICRO_BYTES],
            materials: Vec::new(),
        }
    }

    #[must_use]
    pub const fn mask(&self) -> &[u8; MICRO_BYTES] {
        &self.mask
    }

    #[must_use]
    pub fn materials(&self) -> &[u8] {
        &self.materials
    }

    #[must_use]
    pub fn occupied_count(&self) -> usize {
        occupied_count(&self.mask)
    }

    /// The payload as the borrowed view the rules are written on.
    #[must_use]
    pub fn as_ref(&self) -> MicroChunkRef<'_> {
        MicroChunkRef::new(&self.mask, &self.materials)
    }
}

impl Display for MicroChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MaterialCount { occupied, given } => {
                write!(
                    f,
                    "the mask marks {occupied} cells but carries {given} materials"
                )
            }
        }
    }
}

impl Error for MicroChunkError {}

/// Whether a mask and a material list agree: the materials count is the mask's
/// occupancy.
///
/// The rule the constructor enforces, for callers that hold the two parts
/// without owning them.
///
/// # Errors
///
/// Returns [`MicroChunkError::MaterialCount`] naming both counts.
pub fn check_materials(mask: &[u8], materials: &[u8]) -> Result<(), MicroChunkError> {
    let occupied = occupied_count(mask);

    if occupied == materials.len() {
        return Ok(());
    }

    Err(MicroChunkError::MaterialCount {
        occupied,
        given: materials.len(),
    })
}

/// A Micro-chunk's payload borrowed out of wherever it is stored: the
/// Occupancy mask and the materials of the occupied cells in ascending cell
/// order.
#[derive(Clone, Copy, Debug)]
pub struct MicroChunkRef<'a> {
    pub mask: &'a [u8; MICRO_BYTES],
    pub materials: &'a [u8],
}

impl<'a> MicroChunkRef<'a> {
    #[must_use]
    pub const fn new(mask: &'a [u8; MICRO_BYTES], materials: &'a [u8]) -> Self {
        Self { mask, materials }
    }

    /// The cell at `index` of the `x + 8y + 64z` walk.
    #[must_use]
    pub fn cell_offset(index: usize) -> IVec3 {
        self::cell_offset(index)
    }

    /// The cell index of a Micro-chunk-local position.
    #[must_use]
    pub fn mask_index(local: IVec3) -> usize {
        self::mask_index(local)
    }

    /// Writes cell `cell` of a bare mask. The view's own mask is a shared
    /// borrow, so the write rule takes the mask it writes.
    pub fn set_cell(mask: &mut [u8], cell: usize, occupied: bool) {
        self::set_cell(mask, cell, occupied);
    }

    #[must_use]
    pub fn occupied_cell(&self, index: usize) -> bool {
        self::occupied_cell(self.mask, index)
    }

    #[must_use]
    pub fn rank(&self, cell: usize) -> usize {
        self::rank(self.mask, cell)
    }

    #[must_use]
    pub fn occupied_count(&self) -> usize {
        self::occupied_count(self.mask)
    }

    #[must_use]
    pub fn bounds(&self) -> Option<(IVec3, IVec3)> {
        self::bounds(self.mask)
    }
}

/// Whether cell `index` of `mask` is occupied.
#[must_use]
pub fn occupied_cell(mask: &[u8], index: usize) -> bool {
    mask.get(index.strict_div(MICRO_EDGE))
        .is_some_and(|byte| byte & (1u8 << index.strict_rem(MICRO_EDGE)) != 0)
}

/// Writes cell `cell` of `mask`.
pub fn set_cell(mask: &mut [u8], cell: usize, occupied: bool) {
    let Some(slot) = mask.get_mut(cell.strict_div(MICRO_EDGE)) else {
        return;
    };

    let bit = 1u8 << cell.strict_rem(MICRO_EDGE);

    if occupied {
        *slot |= bit;
    } else {
        *slot &= !bit;
    }
}

/// The number of occupied cells in `mask`.
#[must_use]
pub fn occupied_count(mask: &[u8]) -> usize {
    mask.iter().map(|byte| byte.count_ones() as usize).sum()
}

/// The cell at `index` of the `x + 8y + 64z` walk a Micro-chunk's mask and
/// materials both follow.
#[must_use]
pub fn cell_offset(index: usize) -> IVec3 {
    IVec3::new(
        i32::try_from(index % MICRO_EDGE).unwrap_or(0),
        i32::try_from((index / MICRO_EDGE) % MICRO_EDGE).unwrap_or(0),
        i32::try_from(index / MICRO_AREA).unwrap_or(0),
    )
}

/// The cell index of a Micro-chunk-local position.
#[must_use]
pub fn mask_index(local: IVec3) -> usize {
    let x = usize::try_from(local.x).unwrap_or(0);
    let y = usize::try_from(local.y).unwrap_or(0);
    let z = usize::try_from(local.z).unwrap_or(0);

    x.strict_add(y.strict_mul(MICRO_EDGE))
        .strict_add(z.strict_mul(MICRO_AREA))
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
/// indices.
///
/// The 8-byte word holding the cell is read once and masked to the bits below
/// it, so the scan is popcounts of whole words, not of every byte.
#[must_use]
pub fn rank(mask: &[u8], cell: usize) -> usize {
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

/// The exact occupied bounds of `mask` as a Micro-chunk-local range, or `None`
/// for a zero mask.
///
/// Scans the mask's bytes rather than its set bits, so it is bounded by the
/// entry size, not by the cells it holds.
#[must_use]
pub fn bounds(mask: &[u8]) -> Option<(IVec3, IVec3)> {
    let mut min = IVec3::splat(i32::MAX);
    let mut max = IVec3::splat(i32::MIN);
    let mut occupied = false;

    for (index, byte) in mask.iter().copied().enumerate() {
        if byte == 0 {
            continue;
        }

        occupied = true;

        let row = i32::try_from(index).unwrap_or(0);
        let edge = i32::try_from(MICRO_EDGE).unwrap_or(1);
        let y = row.strict_rem(edge);
        let z = row.strict_div(edge);
        let low = i32::try_from(byte.trailing_zeros()).unwrap_or(0);
        let high = i32::try_from(7u32.saturating_sub(byte.leading_zeros())).unwrap_or(0);
        let cell_min = IVec3::new(low, y, z);
        let cell_max = IVec3::new(high, y, z);

        min = min.min(cell_min);
        max = max.max(cell_max);
    }

    occupied.then_some((min, max))
}

#[cfg(test)]
mod tests {
    use glam::IVec3;

    use super::{MICRO_BYTES, MICRO_CELLS, MicroChunk, bounds, rank};

    fn mask_from_cells(cells: &[usize]) -> [u8; MICRO_BYTES] {
        let mut mask = [0u8; MICRO_BYTES];

        for cell in cells {
            let byte = cell.strict_div(8);
            let bit = u32::try_from(cell.strict_rem(8)).unwrap_or(0);

            if let Some(slot) = mask.get_mut(byte) {
                *slot |= 1u8.wrapping_shl(bit);
            }
        }

        mask
    }

    fn mask_from_bytes(bytes: &[(usize, u8)]) -> [u8; MICRO_BYTES] {
        let mut mask = [0u8; MICRO_BYTES];

        for (index, value) in bytes {
            if let Some(slot) = mask.get_mut(*index) {
                *slot = *value;
            }
        }

        mask
    }

    /// `rank` scans 64-bit words. Its GLSL twin `material_rank` in
    /// crates/atlas-rt/shaders/voxel/intersect.rint is the same scan over
    /// 32-bit words, and no test compares the two, so this table is the only
    /// pin on them agreeing.
    #[test]
    fn rank_counts_the_set_bits_below_every_cell() {
        let cases: [&[usize]; 3] = [&[0, 7, 8, 63, 64, 300, 511], &[1, 62, 65, 510], &[255, 256]];

        for cells in cases {
            let mask = mask_from_cells(cells);

            for cell in 0..MICRO_CELLS {
                let below = cells.iter().filter(|&&set| set < cell).count();

                assert_eq!(rank(&mask, cell), below, "cells {cells:?} at cell {cell}");
            }
        }
    }

    #[test]
    fn rank_of_the_named_boundary_cells() {
        let mask = mask_from_cells(&[0, 7, 8, 63, 64, 300, 511]);

        let cases = [
            (0usize, 0usize),
            (7, 1),
            (8, 2),
            (63, 3),
            (64, 4),
            (300, 5),
            (511, 6),
        ];

        for (cell, below) in cases {
            assert_eq!(rank(&mask, cell), below, "cell {cell}");
        }
    }

    /// Cell order `x + 8y + 64z` puts byte `b` at `y = b % 8`, `z = b / 8`, and
    /// bit `k` at `x = k`.
    #[test]
    fn bounds_pins_the_first_and_the_last_mask_byte() {
        let cases: [(&[(usize, u8)], (IVec3, IVec3)); 7] = [
            (
                &[(0, 0b0000_0001)],
                (IVec3::new(0, 0, 0), IVec3::new(0, 0, 0)),
            ),
            (
                &[(1, 0b0000_0001)],
                (IVec3::new(0, 1, 0), IVec3::new(0, 1, 0)),
            ),
            (
                &[(8, 0b0000_0001)],
                (IVec3::new(0, 0, 1), IVec3::new(0, 0, 1)),
            ),
            (
                &[(0, 0b1000_0001)],
                (IVec3::new(0, 0, 0), IVec3::new(7, 0, 0)),
            ),
            (
                &[(0, 0b0001_0010)],
                (IVec3::new(1, 0, 0), IVec3::new(4, 0, 0)),
            ),
            (
                &[(63, 0b1000_0000)],
                (IVec3::new(7, 7, 7), IVec3::new(7, 7, 7)),
            ),
            (
                &[(0, 0b0000_0010), (63, 0b0000_0001)],
                (IVec3::new(0, 0, 0), IVec3::new(1, 7, 7)),
            ),
        ];

        for (bytes, expected) in cases {
            let mask = mask_from_bytes(bytes);

            assert_eq!(bounds(&mask), Some(expected), "{bytes:?}");
        }
    }

    #[test]
    fn bounds_is_none_for_a_zero_mask() {
        assert_eq!(bounds(&[0u8; MICRO_BYTES]), None);
    }

    #[test]
    fn the_constructor_rejects_a_material_count_the_mask_disagrees_with() {
        let mask = mask_from_cells(&[0, 7, 8, 64]);
        let occupied = 4usize;

        for given in [occupied - 1, occupied + 1] {
            let error = MicroChunk::new(mask, vec![0u8; given])
                .expect_err("a count the mask disagrees with must be rejected");

            assert_eq!(
                error.to_string(),
                format!("the mask marks {occupied} cells but carries {given} materials"),
                "the rejection carries the wording the edit path displays"
            );
        }
    }

    #[test]
    fn the_constructor_accepts_one_material_per_occupied_cell() {
        let mask = mask_from_cells(&[0, 7, 8, 64]);
        let chunk = MicroChunk::new(mask, vec![9, 8, 7, 6]).expect("the count matches");

        assert_eq!(chunk.mask(), &mask);
        assert_eq!(chunk.materials(), [9, 8, 7, 6]);
        assert_eq!(chunk.occupied_count(), 4);
    }
}
