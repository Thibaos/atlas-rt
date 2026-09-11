//! The load, edit, and clear paths driven through the real region feed.

#![allow(clippy::unwrap_used)]

use atlas_rt::{
    render::region::{
        feed::RendererInput,
        pack::{RegionData, pack_regions},
    },
    world::{
        batch::{TrackedCoords, plan_clear, plan_edit, plan_load},
        grid::region_index_of,
        snapshot::MicroChunkSnapshot,
    },
};
use glam::IVec3;

fn snapshot(coords: IVec3, material: u8) -> MicroChunkSnapshot {
    let mut mask = [0u8; 64];
    mask[0] = 1;

    MicroChunkSnapshot {
        global_coords: coords,
        mask,
        materials: vec![material],
    }
}

/// Drains the feed's packed regions; a second call yields nothing.
fn resident_regions(input: &RendererInput) -> Vec<RegionData> {
    input.packed_regions().unwrap()
}

fn coords_set(coords: &[IVec3]) -> TrackedCoords {
    coords.iter().copied().collect()
}

fn assert_geometry(actual: &[RegionData], expected: &[MicroChunkSnapshot]) {
    let want = pack_regions(expected).unwrap();

    assert_eq!(actual.len(), want.len(), "resident region count");

    for (got, want) in actual.iter().zip(&want) {
        assert_eq!(got.region_index, want.region_index);
        assert_eq!(
            got.blocks.len(),
            want.blocks.len(),
            "region {} block byte count",
            got.region_index
        );
        assert!(
            got.blocks == want.blocks,
            "region {} blocks differ from the direct pack of the expected snapshots",
            got.region_index
        );
        assert_eq!(got.aabbs, want.aabbs, "region {} aabbs", got.region_index);
    }
}

fn load(
    input: &RendererInput,
    tracked: &TrackedCoords,
    incoming: Vec<MicroChunkSnapshot>,
) -> TrackedCoords {
    let planned = plan_load(incoming, tracked);

    input.submit_batch(planned.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    planned.tracked
}

#[test]
fn a_second_load_replaces_the_first() {
    let input = RendererInput::new().unwrap();

    let shared = IVec3::new(0, 0, 0);
    let first_only = IVec3::new(256, 0, 0);
    let second_only = IVec3::new(8, 0, 0);

    let tracked = load(
        &input,
        &TrackedCoords::default(),
        vec![snapshot(shared, 1), snapshot(first_only, 2)],
    );

    let tracked = load(
        &input,
        &tracked,
        vec![snapshot(shared, 7), snapshot(second_only, 3)],
    );

    let regions = resident_regions(&input);

    assert_geometry(&regions, &[snapshot(shared, 7), snapshot(second_only, 3)]);
    assert!(
        !regions
            .iter()
            .any(|region| region.region_index == region_index_of(first_only)),
        "the outgoing world's exclusive region must be gone"
    );
    assert_eq!(
        tracked,
        coords_set(&[shared, second_only]),
        "the tracked set afterwards describes the incoming world exactly"
    );
}

#[test]
fn a_clear_before_a_snapshot_for_the_same_coordinate_keeps_the_snapshot() {
    let shared = IVec3::new(0, 0, 0);
    let incoming = vec![snapshot(shared, 7)];

    let planned = plan_load(incoming.clone(), &coords_set(&[shared]));

    assert_eq!(
        planned.snapshots,
        vec![MicroChunkSnapshot::cleared(shared), snapshot(shared, 7)],
        "the outgoing clear is submitted ahead of the incoming snapshot"
    );

    let input = RendererInput::new().unwrap();

    input.submit_batch(planned.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert_geometry(&resident_regions(&input), &incoming);
}

#[test]
fn a_clear_after_a_snapshot_for_the_same_coordinate_leaves_nothing_resident() {
    let shared = IVec3::new(0, 0, 0);
    let input = RendererInput::new().unwrap();

    input
        .submit_batch([snapshot(shared, 7), MicroChunkSnapshot::cleared(shared)])
        .unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        resident_regions(&input).is_empty(),
        "the queue coalesces last-wins, so a clear after a snapshot erases it"
    );
}

#[test]
fn a_batch_edit_leaves_the_rest_of_the_world_resident() {
    let input = RendererInput::new().unwrap();

    let edited = IVec3::new(0, 0, 0);
    let untouched = IVec3::new(8, 0, 0);
    let added = IVec3::new(16, 0, 0);

    let tracked = load(
        &input,
        &TrackedCoords::default(),
        vec![snapshot(edited, 1), snapshot(untouched, 2)],
    );

    let planned = plan_edit(vec![snapshot(edited, 9), snapshot(added, 4)], &tracked);

    assert_eq!(
        planned.snapshots,
        vec![snapshot(edited, 9), snapshot(added, 4)],
        "an edit submits only the chunks it was given"
    );

    input.submit_batch(planned.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert_geometry(
        &resident_regions(&input),
        &[
            snapshot(edited, 9),
            snapshot(untouched, 2),
            snapshot(added, 4),
        ],
    );
}

#[test]
fn a_clear_leaves_nothing_for_the_next_load() {
    let input = RendererInput::new().unwrap();

    let tracked = load(
        &input,
        &TrackedCoords::default(),
        vec![
            snapshot(IVec3::new(0, 0, 0), 1),
            snapshot(IVec3::new(256, 0, 0), 2),
        ],
    );

    let planned = plan_clear(&tracked);

    input.submit_batch(planned.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert!(planned.tracked.is_empty(), "no coordinate stays tracked");
    assert!(
        resident_regions(&input).is_empty(),
        "no region stays resident"
    );

    let fresh = IVec3::new(8, 0, 0);
    let tracked = load(&input, &planned.tracked, vec![snapshot(fresh, 3)]);

    assert_eq!(tracked, coords_set(&[fresh]));
    assert_geometry(&resident_regions(&input), &[snapshot(fresh, 3)]);
}
