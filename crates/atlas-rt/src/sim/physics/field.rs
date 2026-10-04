use glam::IVec3;

use crate::world::{
    World,
    grid::in_lattice,
    material::{PhysicalMaterialTable, Rule},
};

use super::queue::UpdateQueue;

/// The World one evaluation reads through: the World, the material table, and
/// the update queue. Both the player's contact tests and the voxel rules ask
/// this the same questions, so blocking and rule support cannot disagree.
pub(in crate::sim) struct Field<'a> {
    world: &'a World,
    table: &'a PhysicalMaterialTable,
    queued: &'a UpdateQueue,
}

impl<'a> Field<'a> {
    pub(in crate::sim) const fn new(
        world: &'a World,
        table: &'a PhysicalMaterialTable,
        queued: &'a UpdateQueue,
    ) -> Self {
        Self {
            world,
            table,
            queued,
        }
    }

    /// The coordinates the voxel rules have not evaluated yet, in the order a
    /// tick drains them.
    pub(in crate::sim) fn queued_cells(&self) -> impl Iterator<Item = &IVec3> {
        self.queued.iter()
    }

    /// The material at the cell, or nothing outside the lattice or in an
    /// empty cell.
    pub(in crate::sim) fn material(&self, cell: IVec3) -> Option<u8> {
        if !in_lattice(cell) {
            return None;
        }

        self.world.get_voxel(&cell)
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
    /// a solid cell or a grain the queue has left settled. A queued grain
    /// never holds the column above it, which is what keeps a falling column
    /// coherent.
    pub(in crate::sim) fn supports(&self, cell: IVec3) -> bool {
        if !in_lattice(cell) {
            return true;
        }

        let Some(material) = self.world.get_voxel(&cell) else {
            return false;
        };

        let physical = self.table.get(material);

        if physical.rule == Rule::FallingGranular && self.queued.contains(cell) {
            return false;
        }

        physical.solid || physical.rule == Rule::FallingGranular
    }

    /// Whether the cell blocks the player: outside the lattice, or held by a
    /// player-blocking material the queue has left settled.
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
}
