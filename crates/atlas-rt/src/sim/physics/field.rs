use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::world::{
    World,
    grid::in_lattice,
    material::{PhysicalMaterialTable, Rule},
};

use super::queue::UpdateQueue;

/// The occupancy one tick's moves have changed so far: the cells they vacated,
/// which hold nothing, and the destinations they filled, which carry a
/// material. The last write wins, so a cell sits in one of the two at most.
#[derive(Default)]
struct Overlay {
    vacated: FxHashSet<IVec3>,
    filled: FxHashMap<IVec3, u8>,
}

impl Overlay {
    /// What this tick's moves leave at the cell: the filled material, nothing
    /// for a vacated cell, or the World's own answer when the tick has not
    /// touched it.
    fn held(&self, cell: IVec3) -> Held {
        if let Some(material) = self.filled.get(&cell) {
            return Held::Material(*material);
        }

        if self.vacated.contains(&cell) {
            return Held::Empty;
        }

        Held::Unchanged
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

    /// The coordinates this sweep has not served yet, in the order a tick
    /// drains them.
    pub(in crate::sim) fn queued_cells(&self) -> impl Iterator<Item = &IVec3> {
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
        self.overlay.filled.remove(&src);
        self.overlay.vacated.insert(src);
        self.overlay.vacated.remove(&dst);
        self.overlay.filled.insert(dst, material);
    }

    fn in_flight(&self, cell: IVec3) -> bool {
        self.queued.contains(cell) || self.overlay.filled.contains_key(&cell)
    }
}
