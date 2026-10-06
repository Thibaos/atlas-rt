use std::cmp::Ordering;
use std::collections::BTreeSet;

use glam::IVec3;

use crate::world::World;
use crate::world::material::{PhysicalMaterialTable, Rule};

/// A cell keyed the way one tick drains the queue: ascending y, then x, then z,
/// which is not the coordinate order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueueCell(IVec3);

impl Ord for QueueCell {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.0.y, self.0.x, self.0.z).cmp(&(other.0.y, other.0.x, other.0.z))
    }
}

impl PartialOrd for QueueCell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The voxel coordinates the voxel rules have not evaluated yet, held in the
/// order a tick drains them and never as World state. Settled means absent, so
/// a Falling granular cell blocks the player only while this holds it.
#[derive(Clone, Debug, Default)]
pub(in crate::sim) struct UpdateQueue {
    cells: BTreeSet<QueueCell>,
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
            self.cells = cells.iter().copied().map(QueueCell).collect();

            return;
        }

        self.cells = world
            .iter_voxels()
            .filter_map(|(position, voxel)| {
                (table.get(voxel).rule == Rule::FallingGranular).then_some(QueueCell(position))
            })
            .collect();
    }

    pub(in crate::sim) fn len(&self) -> usize {
        self.cells.len()
    }

    pub(in crate::sim) fn contains(&self, cell: IVec3) -> bool {
        self.cells.contains(&QueueCell(cell))
    }

    pub(in crate::sim) fn insert(&mut self, cell: IVec3) {
        self.cells.insert(QueueCell(cell));
    }

    /// The queued cells in drain order, ascending y, then x, then z.
    pub(in crate::sim) fn iter(&self) -> impl Iterator<Item = &IVec3> {
        self.cells.iter().map(|cell| &cell.0)
    }

    /// The cells at or after `cell` in drain order, leaving the cells before it
    /// queued.
    pub(in crate::sim) fn split_off(&mut self, cell: IVec3) -> Self {
        Self {
            cells: self.cells.split_off(&QueueCell(cell)),
        }
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

    #[test]
    fn the_queue_iterates_in_drain_order() {
        let mut queue = UpdateQueue::default();

        for cell in [
            IVec3::new(1, 5, 0),
            IVec3::new(0, 4, 9),
            IVec3::new(0, 4, 1),
            IVec3::new(3, 4, 1),
            IVec3::new(0, 6, 0),
            IVec3::new(0, 4, 1),
        ] {
            queue.insert(cell);
        }

        let order: Vec<IVec3> = queue.iter().copied().collect();

        assert_eq!(
            order,
            vec![
                IVec3::new(0, 4, 1),
                IVec3::new(0, 4, 9),
                IVec3::new(3, 4, 1),
                IVec3::new(1, 5, 0),
                IVec3::new(0, 6, 0),
            ],
            "y leads, then x, then z, and a repeated cell stays one entry"
        );
    }

    #[test]
    fn split_off_leaves_the_earlier_cells_queued() {
        let mut queue = UpdateQueue::default();

        for cell in [
            IVec3::new(0, 4, 0),
            IVec3::new(1, 4, 0),
            IVec3::new(0, 5, 0),
            IVec3::new(0, 6, 0),
        ] {
            queue.insert(cell);
        }

        let tail = queue.split_off(IVec3::new(0, 5, 0));
        let head: Vec<IVec3> = queue.iter().copied().collect();
        let tail: Vec<IVec3> = tail.iter().copied().collect();

        assert_eq!(
            head,
            vec![IVec3::new(0, 4, 0), IVec3::new(1, 4, 0)],
            "the cells before the split stay in the queue"
        );
        assert_eq!(
            tail,
            vec![IVec3::new(0, 5, 0), IVec3::new(0, 6, 0)],
            "the split cell leads the taken tail"
        );
    }
}
