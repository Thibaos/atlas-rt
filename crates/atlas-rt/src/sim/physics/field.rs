use glam::IVec3;
use rustc_hash::FxHashMap;

use crate::world::{
    World,
    grid::{MICRO_CHUNK_LENGTH, grid_origin, in_lattice},
    material::{PhysicalMaterialTable, Rule},
    micro::{self, MICRO_BYTES, MICRO_CELLS},
};

use super::queue::UpdateQueue;

/// One touched chunk's state after this tick's moves so far: its mask in the
/// store's layout, the one material it holds while every cell agrees, the
/// per-cell array a divergence allocates, and the moves it served.
pub(in crate::sim) struct ChunkHeld {
    mask: [u8; MICRO_BYTES],
    uniform: Option<u8>,
    per_cell: Option<Box<[u8; MICRO_CELLS]>>,
    occupied: usize,
    moves: usize,
}

impl ChunkHeld {
    const fn empty() -> Self {
        Self {
            mask: [0u8; MICRO_BYTES],
            uniform: None,
            per_cell: None,
            occupied: 0,
            moves: 0,
        }
    }

    fn seed(world: &World, origin: IVec3) -> Self {
        let Some(entry) = world.chunk_entry(origin) else {
            return Self::empty();
        };

        let mask = *entry.mask;
        let occupied = micro::occupied_count(&mask);
        let uniform = entry
            .materials
            .first()
            .copied()
            .filter(|first| entry.materials.iter().all(|material| material == first));
        let per_cell = (uniform.is_none() && occupied > 0).then(|| scatter(&mask, entry.materials));

        Self {
            mask,
            uniform,
            per_cell,
            occupied,
            moves: 0,
        }
    }

    pub(in crate::sim) const fn mask(&self) -> &[u8; MICRO_BYTES] {
        &self.mask
    }

    pub(in crate::sim) fn material(&self, cell: usize) -> Option<u8> {
        if !micro::occupied_cell(&self.mask, cell) {
            return None;
        }

        if let Some(per_cell) = &self.per_cell {
            return per_cell.get(cell).copied();
        }

        self.uniform
    }

    pub(in crate::sim) const fn moves(&self) -> usize {
        self.moves
    }

    pub(in crate::sim) const fn is_empty(&self) -> bool {
        self.occupied == 0
    }

    fn vacate(&mut self, cell: usize) {
        if !micro::occupied_cell(&self.mask, cell) {
            return;
        }

        micro::set_cell(&mut self.mask, cell, false);
        self.occupied = self.occupied.saturating_sub(1);

        if self.occupied == 0 && self.per_cell.is_none() {
            self.uniform = None;
        }
    }

    fn occupy(&mut self, cell: usize, material: u8) {
        if !micro::occupied_cell(&self.mask, cell) {
            micro::set_cell(&mut self.mask, cell, true);
            self.occupied = self.occupied.saturating_add(1);
        }

        self.write(cell, material);
    }

    const fn served(&mut self) {
        self.moves = self.moves.saturating_add(1);
    }

    fn write(&mut self, cell: usize, material: u8) {
        if let Some(per_cell) = self.per_cell.as_mut() {
            if let Some(slot) = per_cell.get_mut(cell) {
                *slot = material;
            }

            return;
        }

        match self.uniform {
            Some(uniform) if uniform == material => {}
            Some(uniform) => {
                let mut per_cell = Box::new([uniform; MICRO_CELLS]);

                if let Some(slot) = per_cell.get_mut(cell) {
                    *slot = material;
                }

                self.per_cell = Some(per_cell);
                self.uniform = None;
            }
            None => self.uniform = Some(material),
        }
    }
}

/// Every chunk this tick's moves have touched, seeded from the World on first
/// touch and carried out of the drain as the post-tick truth for that chunk.
#[derive(Default)]
pub(in crate::sim) struct Overlay {
    entries: FxHashMap<IVec3, ChunkHeld>,
}

impl Overlay {
    fn held(&self, cell: IVec3) -> Held {
        let origin = chunk_origin(cell);

        let Some(state) = self.entries.get(&origin) else {
            return Held::Unchanged;
        };

        state
            .material(chunk_cell(cell, origin))
            .map_or(Held::Empty, Held::Material)
    }

    /// Whether the cell holds what this tick's moves put there: set in the
    /// overlay and absent from the World.
    fn arrived(&self, world: &World, cell: IVec3) -> bool {
        matches!(self.held(cell), Held::Material(_)) && world.get_voxel(&cell).is_none()
    }

    fn apply_move(&mut self, world: &World, src: IVec3, dst: IVec3, material: u8) {
        let src_origin = chunk_origin(src);

        {
            let state = self.touch(world, src_origin);

            state.vacate(chunk_cell(src, src_origin));
            state.served();
        }

        let dst_origin = chunk_origin(dst);

        self.touch(world, dst_origin)
            .occupy(chunk_cell(dst, dst_origin), material);
    }

    fn touch(&mut self, world: &World, origin: IVec3) -> &mut ChunkHeld {
        self.entries
            .entry(origin)
            .or_insert_with(|| ChunkHeld::seed(world, origin))
    }
}

/// What the commit reads to write a touched chunk whole, in no particular
/// order. A chunk the tick has not touched answers an absent mask, no moves and
/// empty.
#[allow(dead_code, reason = "the commit reads these, and ticket 05 lands it")]
impl Overlay {
    pub(in crate::sim) fn chunks(&self) -> impl Iterator<Item = (IVec3, &ChunkHeld)> {
        self.entries.iter().map(|(origin, state)| (*origin, state))
    }

    pub(in crate::sim) fn mask(&self, origin: IVec3) -> Option<&[u8; MICRO_BYTES]> {
        self.entries.get(&origin).map(ChunkHeld::mask)
    }

    pub(in crate::sim) fn material_at(&self, origin: IVec3, cell: usize) -> Option<u8> {
        self.entries.get(&origin)?.material(cell)
    }

    pub(in crate::sim) fn moves(&self, origin: IVec3) -> usize {
        self.entries.get(&origin).map_or(0, ChunkHeld::moves)
    }

    pub(in crate::sim) fn is_empty(&self, origin: IVec3) -> bool {
        self.entries.get(&origin).is_none_or(ChunkHeld::is_empty)
    }
}

/// The three answers [`Overlay::held`] can give, which `Option<Option<u8>>`
/// would spell without naming them.
enum Held {
    Unchanged,
    Empty,
    Material(u8),
}

/// The World one evaluation reads through: the World, the material table, the
/// update queue and this tick's moves. Both the player's contact tests and the
/// voxel rules ask this the same questions, so blocking and rule support cannot
/// disagree.
pub(in crate::sim) struct Field<'a> {
    world: &'a World,
    table: &'a PhysicalMaterialTable,
    queued: &'a UpdateQueue,
    overlay: Overlay,
}

impl<'a> Field<'a> {
    pub(in crate::sim) fn new(
        world: &'a World,
        table: &'a PhysicalMaterialTable,
        queued: &'a UpdateQueue,
    ) -> Self {
        Self {
            world,
            table,
            queued,
            overlay: Overlay::default(),
        }
    }

    /// The coordinates this tick serves, in the order a tick drains them.
    pub(in crate::sim) fn queued_cells(&self) -> impl Iterator<Item = IVec3> + '_ {
        self.queued.unserved()
    }

    /// The material at the cell, this tick's moves first, or nothing outside
    /// the lattice or in an empty cell.
    pub(in crate::sim) fn material(&self, cell: IVec3) -> Option<u8> {
        if !in_lattice(cell) {
            return None;
        }

        match self.overlay.held(cell) {
            Held::Unchanged => self.world.get_voxel(&cell),
            Held::Empty => None,
            Held::Material(material) => Some(material),
        }
    }

    /// The Falling granular material at the cell.
    pub(in crate::sim) fn grain(&self, cell: IVec3) -> Option<u8> {
        let material = self.material(cell)?;

        (self.table.get(material).rule == Rule::FallingGranular).then_some(material)
    }

    /// Whether the cell is inside the lattice and holds nothing.
    pub(in crate::sim) fn open(&self, cell: IVec3) -> bool {
        in_lattice(cell) && self.material(cell).is_none()
    }

    /// Whether the cell stably refuses a fall: outside the lattice, or held by
    /// a solid cell or a grain that is not in flight. A grain in flight holds
    /// nothing above it, whether the pre-tick queue holds it or this tick's
    /// moves filled its destination.
    pub(in crate::sim) fn supports(&self, cell: IVec3) -> bool {
        if !in_lattice(cell) {
            return true;
        }

        let Some(material) = self.material(cell) else {
            return false;
        };

        let physical = self.table.get(material);

        if physical.rule == Rule::FallingGranular && self.in_flight(cell) {
            return false;
        }

        physical.solid || physical.rule == Rule::FallingGranular
    }

    /// Whether the cell blocks the player: outside the lattice, or held by a
    /// player-blocking material the queue has left settled. The contact test
    /// runs before the drain, so this reads the World and not the overlay.
    pub(in crate::sim) fn blocks(&self, cell: IVec3) -> bool {
        if !in_lattice(cell) {
            return true;
        }

        self.world.get_voxel(&cell).is_some_and(|material| {
            let physical = self.table.get(material);

            physical.solid
                && !(physical.rule == Rule::FallingGranular && self.queued.contains(cell))
        })
    }

    /// Applies one move to this tick's overlay, so a cell served later reads
    /// the vacancy and the fill the move left behind.
    pub(in crate::sim) fn apply_move(&mut self, src: IVec3, dst: IVec3, material: u8) {
        self.overlay.apply_move(self.world, src, dst, material);
    }

    /// The overlay this tick built, which the commit reads as each touched
    /// chunk's post-tick state.
    pub(in crate::sim) fn into_overlay(self) -> Overlay {
        self.overlay
    }

    fn in_flight(&self, cell: IVec3) -> bool {
        self.queued.contains(cell) || self.overlay.arrived(self.world, cell)
    }
}

/// The Micro-chunk origin holding the cell.
fn chunk_origin(cell: IVec3) -> IVec3 {
    grid_origin(cell, MICRO_CHUNK_LENGTH)
}

/// The cell's index in its Micro-chunk's `x + 8y + 64z` walk, which is its bit
/// in the mask byte `y + 8z`.
fn chunk_cell(cell: IVec3, origin: IVec3) -> usize {
    micro::mask_index(cell.saturating_sub(origin))
}

/// A chunk's materials spread over its mask's set bits in ascending cell order.
fn scatter(mask: &[u8; MICRO_BYTES], materials: &[u8]) -> Box<[u8; MICRO_CELLS]> {
    let mut per_cell = Box::new([0u8; MICRO_CELLS]);
    let mut ascending = materials.iter();

    for (cell, slot) in per_cell.iter_mut().enumerate() {
        if !micro::occupied_cell(mask, cell) {
            continue;
        }

        if let Some(material) = ascending.next() {
            *slot = *material;
        }
    }

    per_cell
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::material::parse_override;

    const GRAIN: u8 = 2;
    const OTHER_GRAIN: u8 = 3;

    fn table() -> PhysicalMaterialTable {
        parse_override(&format!(
            "material {GRAIN} falling_granular solid=true\n\
             material {OTHER_GRAIN} falling_granular solid=true"
        ))
        .unwrap_or_else(|rejections| panic!("{rejections:?}"))
    }

    fn world_of(cells: &[(IVec3, u8)]) -> World {
        let mut world = World::default();

        for (position, material) in cells {
            world.set_voxel(*position, *material);
        }

        world
    }

    fn seeded(world: &World, chunks: &[IVec3]) -> Overlay {
        let mut overlay = Overlay::default();

        for origin in chunks {
            overlay.touch(world, *origin);
        }

        overlay
    }

    #[test]
    fn a_diagonal_move_carrying_a_second_material_materializes_the_per_cell_array() {
        let world = world_of(&[
            (IVec3::new(0, 0, 0), GRAIN),
            (IVec3::new(8, 1, 0), OTHER_GRAIN),
        ]);
        let table = table();
        let queue = UpdateQueue::default();
        let mut field = Field::new(&world, &table, &queue);

        field.apply_move(IVec3::new(8, 1, 0), IVec3::new(7, 0, 0), OTHER_GRAIN);

        assert_eq!(
            field.material(IVec3::new(7, 0, 0)),
            Some(OTHER_GRAIN),
            "the diagonal's destination holds the material it carried"
        );
        assert_eq!(
            field.material(IVec3::new(0, 0, 0)),
            Some(GRAIN),
            "the grain the chunk already held is unchanged"
        );
        assert_eq!(
            field.material(IVec3::new(1, 0, 0)),
            None,
            "a cell the tick left empty answers empty"
        );

        let overlay = field.into_overlay();
        let chunk = IVec3::new(0, 0, 0);
        let source = IVec3::new(8, 0, 0);

        assert_eq!(overlay.material_at(chunk, 0), Some(GRAIN));
        assert_eq!(overlay.material_at(chunk, 7), Some(OTHER_GRAIN));
        assert_eq!(overlay.material_at(chunk, 1), None);
        assert_eq!(
            overlay.mask(chunk).map(|mask| mask[0]),
            Some(0b1000_0001),
            "the mask puts x 0 and x 7 in the byte for y 0 and z 0, as the store does"
        );

        let state = overlay.entries.get(&chunk).expect("the move touched it");

        assert!(
            state.per_cell.is_some(),
            "a second material materializes the per-cell array"
        );
        assert!(state.uniform.is_none(), "the uniform marker is dropped");
        assert_eq!(overlay.moves(chunk), 0, "the chunk served no move");
        assert_eq!(overlay.moves(source), 1, "the source's chunk served it");
        assert!(overlay.is_empty(source), "the source's chunk is left empty");
    }

    #[test]
    fn an_entry_seeded_from_the_world_answers_its_cells_and_keeps_a_uniform_marker() {
        let world = world_of(&[
            (IVec3::new(0, 0, 0), GRAIN),
            (IVec3::new(3, 0, 0), OTHER_GRAIN),
            (IVec3::new(0, 4, 2), OTHER_GRAIN),
            (IVec3::new(8, 0, 0), GRAIN),
            (IVec3::new(9, 1, 0), GRAIN),
            (IVec3::new(8, 7, 7), GRAIN),
        ]);
        let mixed = IVec3::new(0, 0, 0);
        let uniform = IVec3::new(8, 0, 0);
        let absent = IVec3::new(0, 0, 8);
        let overlay = seeded(&world, &[mixed, uniform, absent]);

        assert_eq!(overlay.material_at(mixed, 0), Some(GRAIN));
        assert_eq!(overlay.material_at(mixed, 3), Some(OTHER_GRAIN));
        assert_eq!(
            overlay.material_at(mixed, 160),
            Some(OTHER_GRAIN),
            "the last cell of the x + 8y + 64z walk keeps its own material"
        );
        assert_eq!(overlay.material_at(mixed, 1), None);

        let mixed_state = overlay.entries.get(&mixed).expect("the chunk is seeded");

        assert!(
            mixed_state.per_cell.is_some(),
            "a mixed chunk carries the per-cell array"
        );
        assert!(
            mixed_state.uniform.is_none(),
            "a mixed chunk has no uniform marker"
        );

        assert_eq!(overlay.material_at(uniform, 0), Some(GRAIN));
        assert_eq!(overlay.material_at(uniform, 9), Some(GRAIN));
        assert_eq!(overlay.material_at(uniform, 504), Some(GRAIN));

        let uniform_state = overlay.entries.get(&uniform).expect("the chunk is seeded");

        assert!(
            uniform_state.per_cell.is_none(),
            "a uniform chunk allocates no per-cell array"
        );
        assert_eq!(
            uniform_state.uniform,
            Some(GRAIN),
            "a uniform chunk keeps the marker"
        );

        assert_eq!(
            overlay.mask(absent),
            Some(&[0u8; MICRO_BYTES]),
            "a chunk the World keeps no entry for starts as a zero mask"
        );
        assert!(overlay.is_empty(absent));
    }
}
