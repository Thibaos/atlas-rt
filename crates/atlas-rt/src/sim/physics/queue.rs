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
/// A sweep serves every cell once before serving any cell twice. The ready half
/// holds the cells this sweep has not served and the served half the cells it
/// has, so a tick drains the ready half's lowest cells and the halves swap once
/// that half empties. A mover's destination joins the served half, because the
/// mark travels with the grain, and a wake joins the ready half only when the
/// sweep's front has not passed the cell yet.
#[derive(Clone, Debug, Default)]
pub(in crate::sim) struct UpdateQueue {
    ready: BTreeSet<QueueCell>,
    served: BTreeSet<QueueCell>,
    front: Option<QueueCell>,
    spent: bool,
}

impl UpdateQueue {
    /// Replaces the queue with the Falling granular cells of `world`, the seed
    /// every activation starts from: all of them in the ready half. A
    /// generator's `granular_cells` seeds in time proportional to the cells;
    /// `None` scans the World, which a `.vox` load and a failed rule batch need.
    pub(in crate::sim) fn seed(
        &mut self,
        world: &World,
        table: &PhysicalMaterialTable,
        granular_cells: Option<&[IVec3]>,
    ) {
        self.served = BTreeSet::new();
        self.front = None;
        self.spent = false;

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
        self.ready.len().saturating_add(self.served.len())
    }

    /// Whether either half holds the cell, which is what in flight and blocking
    /// ask: ready and served are both grains the rules have not settled.
    pub(in crate::sim) fn contains(&self, cell: IVec3) -> bool {
        let cell = QueueCell(cell);

        self.ready.contains(&cell) || self.served.contains(&cell)
    }

    /// The whole queue, each half in its own order, which only the tests read:
    /// nothing in the rules walks the served half, because a tick serves the
    /// ready half alone.
    #[cfg(test)]
    pub(in crate::sim) fn iter(&self) -> impl Iterator<Item = &IVec3> {
        let ready = self.ready.iter();
        let served = self.served.iter();

        ready.chain(served).map(|cell| &cell.0)
    }

    /// The cells this sweep has not served, in drain order.
    pub(in crate::sim) fn unserved(&self) -> impl Iterator<Item = &IVec3> {
        self.ready.iter().map(|cell| &cell.0)
    }

    /// Starts a sweep when the ready half is empty, which is the only place the
    /// halves swap. A swap anywhere else would serve a cell twice in one sweep,
    /// and a destination is always one level below its source, so its grain
    /// would move twice in a tick.
    pub(in crate::sim) fn begin_sweep(&mut self) {
        if self.ready.is_empty() {
            mem::swap(&mut self.ready, &mut self.served);
            self.front = None;
            self.spent = false;
        }
    }

    /// Ends the tick: the cells a tick served are the ready half's front, so
    /// everything at or after `boundary` stays unserved and becomes the sweep's
    /// front. A boundary of nothing means the sweep has served every ready cell
    /// it had, which `wake` needs to tell from a sweep that never started. The
    /// served half keeps every cell this sweep has already served.
    pub(in crate::sim) fn finish(&mut self, boundary: Option<IVec3>) {
        self.front = boundary.map(QueueCell);
        self.spent = boundary.is_none();
        self.ready =
            boundary.map_or_else(BTreeSet::new, |cell| self.ready.split_off(&QueueCell(cell)));
    }

    /// Queues a mover's destination: the grain was served this sweep, so the
    /// cell joins the served half and is served again only in the next sweep.
    pub(in crate::sim) fn mark_moved(&mut self, cell: IVec3) {
        let cell = QueueCell(cell);

        self.ready.remove(&cell);
        self.served.insert(cell);
    }

    /// Queues a cell whose support just changed. A cell the sweep has not
    /// reached keeps its turn in the ready half. Every other cell joins the
    /// served half, either because the front has passed it or because the sweep
    /// has no ready cells left at all, so a wake never serves a grain twice in
    /// one sweep and never keeps a finished sweep alive by feeding its ready
    /// half. A wake that is not queued at all is the case the front decides:
    /// ahead of it the sweep has not reached the cell, behind it, or after the
    /// sweep has served everything, it has.
    pub(in crate::sim) fn wake(&mut self, cell: IVec3) {
        let cell = QueueCell(cell);

        if self.ready.contains(&cell) || self.served.contains(&cell) {
            return;
        }

        let ahead = !self.spent && self.front.is_none_or(|front| cell >= front);

        if ahead {
            self.ready.insert(cell);

            return;
        }

        self.served.insert(cell);
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
        assert_eq!(seeded.served, scanned.served);
    }

    #[test]
    fn a_seed_leaves_the_served_half_empty() {
        let mut queue = UpdateQueue::default();

        queue.seed(&mixed_world(), &table(), None);

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(-1, 0, 0), IVec3::new(1, 2, 3)],
            "every seeded cell is unserved, in drain order"
        );
        assert!(queue.served.is_empty(), "no sweep has served anything yet");
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
        ];

        queue.seed(&World::default(), &table(), Some(&cells));

        assert_eq!(
            unserved(&queue),
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
    fn a_destination_waits_for_the_next_sweep() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 5, 0));
        queue.mark_moved(IVec3::new(0, 4, 0));

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 5, 0)],
            "a mover's destination is not this sweep's to serve"
        );
        assert!(
            queue.contains(IVec3::new(0, 4, 0)),
            "the destination is queued and in flight"
        );
        assert_eq!(queue.len(), 2, "both halves count toward the queue");
    }

    #[test]
    fn a_wake_of_an_unserved_cell_keeps_its_place() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 5, 0));
        queue.wake(IVec3::new(0, 7, 0));
        queue.mark_moved(IVec3::new(0, 5, 0));
        queue.finish(Some(IVec3::new(0, 7, 0)));
        queue.wake(IVec3::new(0, 7, 0));

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 7, 0)],
            "a wake of an unserved cell leaves it unserved"
        );
    }

    #[test]
    fn a_wake_behind_the_front_waits_for_the_next_sweep() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 5, 0));
        queue.wake(IVec3::new(0, 7, 0));
        queue.finish(Some(IVec3::new(0, 7, 0)));

        assert_eq!(unserved(&queue), vec![IVec3::new(0, 7, 0)]);

        queue.wake(IVec3::new(0, 3, 0));

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 7, 0)],
            "a cell the front has passed is not served again this sweep"
        );
        assert!(
            queue.contains(IVec3::new(0, 3, 0)),
            "it is queued for the next sweep instead of dropped"
        );
    }

    #[test]
    fn a_wake_after_the_sweep_served_everything_waits_for_the_next_one() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 5, 0));
        queue.finish(None);
        queue.wake(IVec3::new(0, 3, 0));

        assert!(
            unserved(&queue).is_empty(),
            "the finished sweep has no turn left to give, so its ready half stays empty and the \
             next one can swap"
        );
        assert!(
            queue.contains(IVec3::new(0, 3, 0)),
            "the woken cell is queued for the next sweep instead of dropped"
        );
    }

    #[test]
    fn the_sweep_swaps_once_the_ready_half_empties() {
        let mut queue = UpdateQueue::default();

        for cell in [IVec3::new(0, 4, 0), IVec3::new(0, 5, 0)] {
            queue.wake(cell);
            queue.mark_moved(cell);
        }

        queue.finish(None);
        queue.begin_sweep();

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 4, 0), IVec3::new(0, 5, 0)],
            "the served half becomes the next sweep's ready half"
        );
        assert!(
            queue.served.is_empty(),
            "the swap leaves the served half empty"
        );
    }

    #[test]
    fn a_sweep_in_progress_does_not_swap() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 4, 0));
        queue.wake(IVec3::new(0, 5, 0));
        queue.mark_moved(IVec3::new(0, 4, 0));
        queue.finish(Some(IVec3::new(0, 5, 0)));
        queue.begin_sweep();

        assert_eq!(
            unserved(&queue),
            vec![IVec3::new(0, 5, 0)],
            "the unserved tail is still this sweep's to serve"
        );
        assert!(
            queue.contains(IVec3::new(0, 4, 0)),
            "the cell this sweep already served keeps its mark"
        );
    }

    #[test]
    fn a_settled_cell_leaves_the_queue() {
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 4, 0));
        queue.finish(None);

        assert_eq!(queue.len(), 0, "a settled cell loses its place");
        assert!(!queue.contains(IVec3::new(0, 4, 0)));
    }
}
