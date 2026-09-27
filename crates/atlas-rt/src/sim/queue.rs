use glam::IVec3;
use rustc_hash::FxHashSet;

use crate::world::World;
use crate::world::material::{PhysicalMaterialTable, Rule};

/// The voxel coordinates the voxel rules have not evaluated yet, held as
/// single coordinates rather than regions and never as World state. Settled
/// means absent, so a Falling granular cell blocks the player only while this
/// holds it.
#[derive(Clone, Debug, Default)]
pub(super) struct UpdateQueue {
    cells: FxHashSet<IVec3>,
}

impl UpdateQueue {
    /// Replaces the queue with every Falling granular cell of `world`, the
    /// seed every activation starts from.
    pub(super) fn seed(&mut self, world: &World, table: &PhysicalMaterialTable) {
        self.cells = world
            .iter_voxels()
            .filter_map(|(position, voxel)| {
                let material = u8::try_from(*voxel).ok()?;

                (table.get(material).rule == Rule::FallingGranular).then_some(position)
            })
            .collect();
    }

    pub(super) fn contains(&self, cell: IVec3) -> bool {
        self.cells.contains(&cell)
    }

    pub(super) fn insert(&mut self, cell: IVec3) {
        self.cells.insert(cell);
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &IVec3> {
        self.cells.iter()
    }
}
