use glam::IVec3;
use rustc_hash::FxHashSet;

use crate::world::World;
use crate::world::material::{PhysicalMaterialTable, Rule};

/// The voxel coordinates the voxel rules have not evaluated yet, held as
/// single coordinates rather than regions and never as World state. Settled
/// means absent, so a Falling granular cell blocks the player only while this
/// holds it.
#[derive(Clone, Debug, Default)]
pub(in crate::sim) struct UpdateQueue {
    cells: FxHashSet<IVec3>,
}

impl UpdateQueue {
    /// Replaces the queue with the Falling granular cells of `world`, the seed
    /// every activation starts from. A generator's `granular_cells` seeds in
    /// time proportional to the cells; `None` scans the World, which a `.vox`
    /// load and a failed rule batch need.
    pub(in crate::sim) fn seed(
        &mut self,
        world: &World,
        table: &PhysicalMaterialTable,
        granular_cells: Option<&[IVec3]>,
    ) {
        if let Some(cells) = granular_cells {
            self.cells = cells.iter().copied().collect();

            return;
        }

        self.cells = world
            .iter_voxels()
            .filter_map(|(position, voxel)| {
                (table.get(voxel).rule == Rule::FallingGranular).then_some(position)
            })
            .collect();
    }

    pub(in crate::sim) fn contains(&self, cell: IVec3) -> bool {
        self.cells.contains(&cell)
    }

    pub(in crate::sim) fn insert(&mut self, cell: IVec3) {
        self.cells.insert(cell);
    }

    pub(in crate::sim) fn iter(&self) -> impl Iterator<Item = &IVec3> {
        self.cells.iter()
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

    #[test]
    fn a_scan_seeds_every_falling_granular_cell() {
        let mut queue = UpdateQueue::default();

        queue.seed(&mixed_world(), &table(), None);

        assert_eq!(queue.cells.len(), 2);
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

        assert_eq!(seeded.cells, scanned.cells);
    }
}
