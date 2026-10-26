use std::cell::Cell;

use glam::IVec3;
use rustc_hash::FxHashMap;

use crate::world::{
    World,
    grid::{MICRO_CHUNK_LENGTH, grid_origin, in_lattice},
    material::{PhysicalMaterialTable, Rule},
    micro::{self, MICRO_AREA, MICRO_BYTES, MICRO_CELLS, MICRO_EDGE, MicroChunkRef},
};

use super::queue::UpdateQueue;

/// A chunk the tick has touched, holding its post-tick mask in the store's
/// layout and the materials its cells hold.
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
        let uniform = uniform(entry.materials);
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
            set_material(per_cell, cell, material);

            return;
        }

        match self.uniform {
            Some(uniform) if uniform == material => {}
            Some(uniform) => {
                let mut per_cell = Box::new([uniform; MICRO_CELLS]);

                set_material(&mut per_cell, cell, material);

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
    origins: Vec<IVec3>,
    states: Vec<ChunkHeld>,
    slots: FxHashMap<IVec3, usize>,
    recent: Cell<Option<(IVec3, usize)>>,
    most_moves: usize,
}

impl Overlay {
    /// The chunk a drain last resolved, which answers for the cells beside it
    /// because a drain reads and writes a handful of neighbouring chunks in a
    /// row.
    fn cached(&self, origin: IVec3) -> Option<usize> {
        let (recent, slot) = self.recent.get()?;

        (recent == origin).then_some(slot)
    }

    fn slot(&self, origin: IVec3) -> Option<usize> {
        if let Some(slot) = self.cached(origin) {
            return Some(slot);
        }

        let slot = self.slots.get(&origin).copied()?;

        self.recent.set(Some((origin, slot)));

        Some(slot)
    }

    fn slot_of_move(&mut self, world: &World, origin: IVec3) -> usize {
        if let Some(slot) = self.cached(origin) {
            return slot;
        }

        let slot = if let Some(slot) = self.slots.get(&origin).copied() {
            slot
        } else {
            let slot = self.states.len();

            self.origins.push(origin);
            self.states.push(ChunkHeld::seed(world, origin));
            self.slots.insert(origin, slot);

            slot
        };

        self.recent.set(Some((origin, slot)));

        slot
    }

    fn held(&self, cell: IVec3) -> Held {
        let origin = chunk_origin(cell);

        let Some(state) = self.slot(origin).and_then(|slot| self.states.get(slot)) else {
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
        let src_slot = self.slot_of_move(world, src_origin);

        if let Some(state) = self.states.get_mut(src_slot) {
            state.vacate(chunk_cell(src, src_origin));
            state.served();

            self.most_moves = self.most_moves.max(state.moves());
        }

        let dst_origin = chunk_origin(dst);
        let dst_slot = self.slot_of_move(world, dst_origin);

        if let Some(state) = self.states.get_mut(dst_slot) {
            state.occupy(chunk_cell(dst, dst_origin), material);
        }
    }

    /// Puts a chunk's whole state in, replacing whatever this tick had put
    /// there, which is what a translated chunk needs.
    fn install(&mut self, origin: IVec3, state: ChunkHeld) {
        self.most_moves = self.most_moves.max(state.moves());

        match self.slots.get(&origin).copied() {
            Some(slot) => self.replace(slot, state),
            None => self.append(origin, state),
        }
    }

    /// Replaces the state of a chunk the overlay already holds.
    fn replace(&mut self, slot: usize, state: ChunkHeld) {
        if let Some(existing) = self.states.get_mut(slot) {
            *existing = state;
        }
    }

    /// Adds a chunk the overlay did not hold before.
    fn append(&mut self, origin: IVec3, state: ChunkHeld) {
        let slot = self.states.len();

        self.origins.push(origin);
        self.states.push(state);
        self.slots.insert(origin, slot);
        self.recent.set(Some((origin, slot)));
    }
}

/// What the commit writes a touched chunk whole from: the chunks this tick's
/// moves have touched, in no particular order.
impl Overlay {
    pub(in crate::sim) fn chunks(&self) -> impl Iterator<Item = (IVec3, &ChunkHeld)> + '_ {
        self.origins.iter().copied().zip(self.states.iter())
    }

    /// Whether this tick's moves have touched no chunk, so the commit has
    /// nothing to write.
    pub(in crate::sim) const fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// The most moves any touched chunk served, which is what decides whether
    /// the commit has a chunk to write whole.
    pub(in crate::sim) const fn most_moves(&self) -> usize {
        self.most_moves
    }

    /// The whole state of `origin`'s chunk, the form the commit writes one entry
    /// from and the one the overlay tests read.
    #[cfg(test)]
    pub(in crate::sim) fn chunk(&self, origin: IVec3) -> Option<&ChunkHeld> {
        self.slots
            .get(&origin)
            .and_then(|slot| self.states.get(*slot))
    }

    #[cfg(test)]
    pub(in crate::sim) fn mask(&self, origin: IVec3) -> Option<&[u8; MICRO_BYTES]> {
        self.chunk(origin).map(ChunkHeld::mask)
    }

    #[cfg(test)]
    pub(in crate::sim) fn material_at(&self, origin: IVec3, cell: usize) -> Option<u8> {
        self.chunk(origin)?.material(cell)
    }
}

/// The three answers [`Overlay::held`] can give, which `Option<Option<u8>>`
/// would spell without naming them.
enum Held {
    Unchanged,
    Empty,
    Material(u8),
}

/// A chunk as the tick found it, before moving it: its entry, and the one
/// material its cells all hold when they hold one. A translation reads both, and
/// the material costs a pass over the entry, so it is found once.
#[derive(Clone, Copy)]
pub(in crate::sim) struct ChunkSource<'a> {
    pub(in crate::sim) entry: MicroChunkRef<'a>,
    pub(in crate::sim) uniform: Option<u8>,
}

impl<'a> ChunkSource<'a> {
    pub(in crate::sim) fn read(entry: MicroChunkRef<'a>) -> Self {
        Self {
            entry,
            uniform: uniform(entry.materials),
        }
    }
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

    /// Translates `origin`'s chunk one cell down as one entry: rows 0 to 6 from
    /// its rows 1 to 7, and row 7 from `above`'s row 0 when the run continues
    /// into it. Every cell of the source counts as a served move, so the commit
    /// writes the chunk whole.
    pub(in crate::sim) fn translate(
        &mut self,
        origin: IVec3,
        source: ChunkSource<'_>,
        above: Option<ChunkSource<'_>>,
    ) {
        let entry = source.entry;
        let raised = above.map(|above| above.entry);
        let mut mask = [0u8; MICRO_BYTES];

        for z in 0..MICRO_EDGE {
            let base = z.strict_mul(MICRO_EDGE);

            for y in 1..MICRO_EDGE {
                let from = base.strict_add(y);
                let to = base.strict_add(y.saturating_sub(1));

                if let (Some(slot), Some(byte)) = (mask.get_mut(to), entry.mask.get(from)) {
                    *slot = *byte;
                }
            }

            if let Some(above) = raised
                && let (Some(slot), Some(byte)) = (
                    mask.get_mut(base.strict_add(MICRO_EDGE.saturating_sub(1))),
                    above.mask.get(base),
                )
            {
                *slot = *byte;
            }
        }

        let (uniform, per_cell) = translated_materials(&mask, source, above);
        let occupied = micro::occupied_count(&mask);
        let moves = micro::occupied_count(entry.mask);

        self.overlay.install(
            origin,
            ChunkHeld {
                mask,
                uniform,
                per_cell,
                occupied,
                moves,
            },
        );
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

/// Sets one cell's material in a per-cell array, and leaves a cell outside the
/// array's walk alone.
fn set_material(per_cell: &mut [u8; MICRO_CELLS], cell: usize, material: u8) {
    if let Some(slot) = per_cell.get_mut(cell) {
        *slot = material;
    }
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

/// The one material a chunk's cells all hold, or `None` when they disagree.
pub(in crate::sim) fn uniform(materials: &[u8]) -> Option<u8> {
    let first = materials.first().copied()?;

    materials
        .iter()
        .all(|material| *material == first)
        .then_some(first)
}

/// The materials a translated chunk's entry carries: the source's one row down,
/// with the chunk above's row 0 where the source had its row 7, as the uniform
/// marker its cells agree on, or as the per-cell array they do not.
fn translated_materials(
    mask: &[u8; MICRO_BYTES],
    source: ChunkSource<'_>,
    above: Option<ChunkSource<'_>>,
) -> (Option<u8>, Option<Box<[u8; MICRO_CELLS]>>) {
    let entry = source.entry;
    let raised = above.filter(|above| micro::row0(above.entry.mask).iter().any(|byte| *byte != 0));

    if let Some(material) = source.uniform
        && raised.is_none_or(|above| above.uniform == Some(material))
    {
        return (Some(material), None);
    }

    let lowered = scatter(entry.mask, entry.materials);
    let raised = raised.map(|above| scatter(above.entry.mask, above.entry.materials));
    let row = MICRO_CELLS.saturating_sub(MICRO_AREA);
    let mut per_cell = Box::new([0u8; MICRO_CELLS]);

    for (cell, slot) in per_cell.iter_mut().enumerate() {
        if let Some(top) = cell.checked_sub(row) {
            *slot = raised
                .as_ref()
                .and_then(|raised| raised.get(top).copied())
                .unwrap_or(0);

            continue;
        }

        *slot = lowered
            .get(cell.strict_add(MICRO_EDGE))
            .copied()
            .unwrap_or(0);
    }

    let mut material = None;
    let mut mixed = false;

    for cell in 0..MICRO_CELLS {
        if !micro::occupied_cell(mask, cell) {
            continue;
        }

        let held = per_cell.get(cell).copied().unwrap_or(0);

        match material {
            None => material = Some(held),
            Some(current) if current == held => {}
            Some(_) => {
                mixed = true;

                break;
            }
        }
    }

    if mixed {
        return (None, Some(per_cell));
    }

    (material, None)
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
            overlay.slot_of_move(world, *origin);
        }

        overlay
    }

    #[test]
    fn the_overlay_answers_the_most_moves_one_chunk_served() {
        let world = world_of(&[
            (IVec3::new(0, 2, 0), GRAIN),
            (IVec3::new(1, 2, 0), GRAIN),
            (IVec3::new(8, 2, 0), GRAIN),
        ]);
        let table = table();
        let queue = UpdateQueue::default();
        let mut field = Field::new(&world, &table, &queue);

        field.apply_move(IVec3::new(0, 2, 0), IVec3::new(0, 1, 0), GRAIN);

        assert_eq!(
            field.material(IVec3::new(0, 1, 0)),
            Some(GRAIN),
            "one move through the overlay"
        );

        field.apply_move(IVec3::new(1, 2, 0), IVec3::new(1, 1, 0), GRAIN);
        field.apply_move(IVec3::new(8, 2, 0), IVec3::new(8, 1, 0), GRAIN);

        assert_eq!(
            field.into_overlay().most_moves(),
            2,
            "the first chunk served two moves and the second served one"
        );
    }

    #[test]
    fn a_translated_chunk_counts_as_the_moves_it_served() {
        let cells: Vec<(IVec3, u8)> = (0..5).map(|x| (IVec3::new(x, 4, 0), GRAIN)).collect();
        let world = world_of(&cells);
        let table = table();
        let queue = UpdateQueue::default();
        let origin = IVec3::new(0, 0, 0);
        let entry = world
            .chunk_entry(origin)
            .expect("the fixture holds the chunk");
        let mut field = Field::new(&world, &table, &queue);

        field.translate(origin, ChunkSource::read(entry), None);

        assert_eq!(
            field.into_overlay().most_moves(),
            5,
            "a translated chunk serves one move per grain it moves"
        );
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

        let state = overlay.chunk(chunk).expect("the move touched it");

        assert!(
            state.per_cell.is_some(),
            "a second material materializes the per-cell array"
        );
        assert!(state.uniform.is_none(), "the uniform marker is dropped");
        assert_eq!(state.moves(), 0, "the chunk served no move");
        assert_eq!(
            overlay.chunk(source).map_or(0, ChunkHeld::moves),
            1,
            "the source's chunk served it"
        );
        assert_eq!(
            overlay.mask(source),
            Some(&[0u8; MICRO_BYTES]),
            "the source's chunk is left empty"
        );
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

        let mixed_state = overlay.chunk(mixed).expect("the chunk is seeded");

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

        let uniform_state = overlay.chunk(uniform).expect("the chunk is seeded");

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
        assert_eq!(
            overlay.chunk(absent).map_or(0, ChunkHeld::moves),
            0,
            "a seeded chunk served no move"
        );
    }
}
