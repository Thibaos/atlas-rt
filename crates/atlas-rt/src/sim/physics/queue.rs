use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::mem;

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
///
/// A tick serves every cell the queue holds. One half is the cells this tick
/// serves and the other is the cells its moves and wakes queue for the next, so
/// a grain moves at most once a tick and no cell is served twice in one.
#[derive(Clone, Debug, Default)]
pub(in crate::sim) struct UpdateQueue {
    ready: BTreeSet<QueueCell>,
    next: BTreeSet<QueueCell>,
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
        self.next = BTreeSet::new();

        if let Some(cells) = granular_cells {
            self.ready = cells.iter().copied().map(QueueCell).collect();

            return;
        }

        self.ready = world
            .iter_voxels()
            .filter_map(|(position, voxel)| {
                (table.get(voxel).rule == Rule::FallingGranular).then_some(QueueCell(position))
            })
            .collect();
    }

    pub(in crate::sim) fn len(&self) -> usize {
        self.ready.len().saturating_add(self.next.len())
    }

    /// Whether the queue holds the cell, which is what in flight and blocking
    /// ask: both halves are grains the rules have not settled.
    pub(in crate::sim) fn contains(&self, cell: IVec3) -> bool {
        let cell = QueueCell(cell);

        self.ready.contains(&cell) || self.next.contains(&cell)
    }

    /// Every queued cell, this tick's in their order and then the next tick's,
    /// which only the tests read.
    #[cfg(test)]
    pub(in crate::sim) fn iter(&self) -> impl Iterator<Item = &IVec3> {
        let ready = self.ready.iter();
        let next = self.next.iter();

        ready.chain(next).map(|cell| &cell.0)
    }

    /// The cells this tick serves, in drain order.
    pub(in crate::sim) fn unserved(&self) -> impl Iterator<Item = &IVec3> {
        self.ready.iter().map(|cell| &cell.0)
    }

    /// Makes the cells the last tick queued this tick's own. A tick ends once
    /// its ready half is served, so the swap waits only for the first tick
    /// after a seed, which already holds its cells.
    pub(in crate::sim) fn begin_tick(&mut self) {
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
        self.next.insert(QueueCell(cell));
    }

    /// Queues a cell whose support changed for the next tick, unless the queue
    /// holds it already. A cell served in this tick is gone from the ready half
    /// by the time the wakes arrive, so its wake queues it for the next.
    pub(in crate::sim) fn wake(&mut self, cell: IVec3) {
        let cell = QueueCell(cell);

        if self.ready.contains(&cell) || self.next.contains(&cell) {
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
        queue.unserved().copied().collect()
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

        assert_eq!(seeded.ready, scanned.ready);
        assert_eq!(seeded.next, scanned.next);
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
