use glam::IVec3;
use rustc_hash::FxHashSet;

use crate::world::snapshot::MicroChunkSnapshot;

/// The coordinates the renderer holds content for.
pub type TrackedCoords = FxHashSet<IVec3>;

/// A planned batch of Snapshots and the coordinates it leaves tracked.
///
/// The change queue coalesces last-wins per coordinate, so within `snapshots`
/// the last entry for a coordinate is the content that survives. `tracked`
/// holds the coordinates that carry content once the batch is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub tracked: TrackedCoords,
}

/// The incoming snapshots alone, with no clears for the rest of the world.
#[must_use]
pub fn plan_edit(incoming: Vec<MicroChunkSnapshot>, tracked: &TrackedCoords) -> Batch {
    assemble(tracked, incoming)
}

/// A zero-mask snapshot for every tracked coordinate, and no tracked coordinate
/// left.
#[must_use]
pub fn plan_clear(tracked: &TrackedCoords) -> Batch {
    assemble(tracked, clears(tracked))
}

/// A zero-mask snapshot for every tracked coordinate first, then the incoming
/// snapshots, so a coordinate present in both worlds keeps the incoming content
/// under the queue's last-wins rule.
#[must_use]
pub fn plan_load(incoming: Vec<MicroChunkSnapshot>, tracked: &TrackedCoords) -> Batch {
    let mut snapshots = clears(tracked);
    snapshots.extend(incoming);

    assemble(tracked, snapshots)
}

/// Sorted so an outgoing batch is the same every run.
fn clears(tracked: &TrackedCoords) -> Vec<MicroChunkSnapshot> {
    let mut coords: Vec<IVec3> = tracked.iter().copied().collect();
    coords.sort_unstable_by_key(IVec3::to_array);

    coords.into_iter().map(MicroChunkSnapshot::cleared).collect()
}

fn assemble(tracked: &TrackedCoords, snapshots: Vec<MicroChunkSnapshot>) -> Batch {
    let mut next = tracked.clone();

    for snapshot in &snapshots {
        if snapshot.occupied_count() == 0 {
            next.remove(&snapshot.global_coords);
        } else {
            next.insert(snapshot.global_coords);
        }
    }

    Batch {
        snapshots,
        tracked: next,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn snapshot(coords: IVec3, material: u8) -> MicroChunkSnapshot {
        let mut mask = [0u8; 64];
        mask[0] = 1;

        MicroChunkSnapshot {
            global_coords: coords,
            mask,
            materials: vec![material],
        }
    }

    /// The change queue's coalescing: the last write for a coordinate wins.
    fn last_wins(snapshots: &[MicroChunkSnapshot]) -> HashMap<IVec3, MicroChunkSnapshot> {
        let mut resident = HashMap::new();

        for snapshot in snapshots {
            resident.insert(snapshot.global_coords, snapshot.clone());
        }

        resident
    }

    fn coords_set(coords: &[IVec3]) -> TrackedCoords {
        coords.iter().copied().collect()
    }

    #[test]
    fn an_edit_on_empty_tracking_submits_only_the_edit() {
        let coords = IVec3::new(8, 0, 0);
        let incoming = vec![snapshot(coords, 3)];

        let batch = plan_edit(incoming.clone(), &TrackedCoords::default());

        assert_eq!(batch.snapshots, incoming);
        assert_eq!(batch.tracked, coords_set(&[coords]));
    }

    #[test]
    fn an_edit_never_synthesises_clears() {
        let outgoing = [IVec3::new(0, 0, 0), IVec3::new(8, 0, 0)];
        let incoming_coords = IVec3::new(16, 0, 0);

        let batch = plan_edit(vec![snapshot(incoming_coords, 5)], &coords_set(&outgoing));

        assert_eq!(batch.snapshots, vec![snapshot(incoming_coords, 5)]);
        assert_eq!(
            batch.tracked,
            coords_set(&[outgoing[0], outgoing[1], incoming_coords])
        );
    }

    #[test]
    fn an_edit_that_empties_a_coordinate_stops_tracking_it() {
        let coords = IVec3::new(8, 0, 0);

        let batch = plan_edit(
            vec![MicroChunkSnapshot::cleared(coords)],
            &coords_set(&[coords]),
        );

        assert!(batch.tracked.is_empty());
    }

    #[test]
    fn a_clear_of_empty_tracking_submits_nothing() {
        let batch = plan_clear(&TrackedCoords::default());

        assert!(batch.snapshots.is_empty());
        assert!(batch.tracked.is_empty());
    }

    #[test]
    fn a_clear_empties_every_tracked_coordinate() {
        let tracked = coords_set(&[IVec3::new(8, 0, 0), IVec3::new(0, 0, 0)]);

        let batch = plan_clear(&tracked);

        assert_eq!(batch.snapshots.len(), tracked.len());
        assert!(
            batch.snapshots.iter().all(|s| s.occupied_count() == 0),
            "every submitted snapshot must be a zero-mask clear"
        );
        assert_eq!(
            batch
                .snapshots
                .iter()
                .map(|s| s.global_coords)
                .collect::<TrackedCoords>(),
            tracked
        );
        assert!(batch.tracked.is_empty());
    }

    #[test]
    fn a_load_on_empty_tracking_submits_the_world() {
        let incoming = vec![
            snapshot(IVec3::new(0, 0, 0), 1),
            snapshot(IVec3::new(8, 0, 0), 2),
        ];

        let batch = plan_load(incoming.clone(), &TrackedCoords::default());

        assert_eq!(batch.snapshots, incoming);
        assert_eq!(
            batch.tracked,
            coords_set(&[IVec3::new(0, 0, 0), IVec3::new(8, 0, 0)])
        );
    }

    #[test]
    fn a_load_of_an_empty_world_clears_the_outgoing_world() {
        let outgoing = [IVec3::new(8, 0, 0), IVec3::new(0, 0, 0)];

        let batch = plan_load(Vec::new(), &coords_set(&outgoing));

        assert_eq!(
            batch.snapshots,
            vec![
                MicroChunkSnapshot::cleared(IVec3::new(0, 0, 0)),
                MicroChunkSnapshot::cleared(IVec3::new(8, 0, 0)),
            ]
        );
        assert!(batch.tracked.is_empty());
    }

    #[test]
    fn a_load_clears_the_disjoint_outgoing_coordinates_first() {
        let outgoing = [IVec3::new(8, 0, 0), IVec3::new(0, 0, 0)];
        let incoming_coords = IVec3::new(16, 0, 0);

        let batch = plan_load(vec![snapshot(incoming_coords, 4)], &coords_set(&outgoing));

        assert_eq!(
            batch.snapshots,
            vec![
                MicroChunkSnapshot::cleared(IVec3::new(0, 0, 0)),
                MicroChunkSnapshot::cleared(IVec3::new(8, 0, 0)),
                snapshot(incoming_coords, 4),
            ]
        );
        assert_eq!(batch.tracked, coords_set(&[incoming_coords]));
    }

    #[test]
    fn a_load_keeps_the_incoming_content_for_a_shared_coordinate() {
        let shared = IVec3::new(0, 0, 0);
        let exclusive = IVec3::new(8, 0, 0);

        let batch = plan_load(
            vec![snapshot(shared, 7)],
            &coords_set(&[shared, exclusive]),
        );

        let cleared = batch
            .snapshots
            .iter()
            .filter(|s| s.global_coords == shared && s.occupied_count() == 0)
            .count();

        assert_eq!(
            cleared, 1,
            "the outgoing clear for {shared} must be submitted too"
        );

        let resident = last_wins(&batch.snapshots);

        assert_eq!(
            resident[&shared],
            snapshot(shared, 7),
            "the incoming snapshot is the last write for the shared coordinate"
        );
        assert_eq!(resident[&exclusive].occupied_count(), 0);
        assert_eq!(batch.tracked, coords_set(&[shared]));
    }
}
