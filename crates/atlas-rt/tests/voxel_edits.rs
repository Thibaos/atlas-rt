use atlas_rt::{
    render::region::{
        feed::RendererInput,
        pack::{RegionData, pack_regions},
    },
    world::{
        World,
        format::open_file,
        grid::{MICRO_CHUNK_LENGTH, grid_origin, region_index_of},
        update::{
            batch::{Batch, TrackedCoords, plan_clear, plan_load},
            edit::{VoxelChange, VoxelEdit, edit_world},
            snapshot::{MicroChunkSnapshot, emit_snapshots},
        },
    },
};
use glam::IVec3;

const CHUNK: i32 = MICRO_CHUNK_LENGTH as i32;
const SET_SEED: u64 = 0x00ED_1705;
const LOADED_ASSET: &str = "assets/test/edit-seam.vox";

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn set(position: IVec3, material: u8) -> VoxelEdit {
    VoxelEdit {
        position,
        change: VoxelChange::Set(material),
    }
}

fn clear(position: IVec3) -> VoxelEdit {
    VoxelEdit {
        position,
        change: VoxelChange::Clear,
    }
}

/// Drains the feed's packed regions; a second call yields nothing.
fn resident_regions(input: &RendererInput) -> Vec<RegionData> {
    input.packed_regions().unwrap()
}

fn geometry_matches(actual: &[RegionData], expected: &[MicroChunkSnapshot]) -> bool {
    let Ok(want) = pack_regions(expected) else {
        return false;
    };

    if actual.len() != want.len() {
        return false;
    }

    actual.iter().zip(&want).all(|(got, want)| {
        got.region_index == want.region_index
            && got.blocks == want.blocks
            && got.aabbs == want.aabbs
    })
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

fn assert_matches_world(input: &RendererInput, world: &World) {
    let emitted = emit_snapshots(world).unwrap();

    assert_geometry(&resident_regions(input), &emitted);
}

fn submit_edit(
    input: &RendererInput,
    world: &mut World,
    edits: &[VoxelEdit],
    tracked: &TrackedCoords,
) -> Batch {
    let batch = edit_world(world, edits, tracked).unwrap();

    input.submit_batch(batch.snapshots.clone()).unwrap();
    input.wait_until_idle().unwrap();

    batch
}

fn submit_load(
    input: &RendererInput,
    incoming: Vec<MicroChunkSnapshot>,
    tracked: &TrackedCoords,
) -> TrackedCoords {
    let planned = plan_load(incoming, tracked);

    input.submit_batch(planned.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    planned.tracked
}

fn chunk_cells(origin: IVec3) -> Vec<IVec3> {
    let mut cells = Vec::with_capacity(512);

    for z in 0..CHUNK {
        for y in 0..CHUNK {
            for x in 0..CHUNK {
                cells.push(origin + IVec3::new(x, y, z));
            }
        }
    }

    cells
}

/// Chunk origins clustered well inside the lattice, so every cell of every
/// chunk is a legal edit position.
fn chunk_origins(rng: &mut Rng, count: usize) -> Vec<IVec3> {
    let mut origins = Vec::with_capacity(count);

    while origins.len() < count {
        let origin = IVec3::new(
            (rng.below(4) as i32 - 1).saturating_mul(CHUNK),
            (rng.below(4) as i32 - 1).saturating_mul(CHUNK),
            (rng.below(4) as i32 - 1).saturating_mul(CHUNK),
        );

        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }

    origins
}

fn random_position(rng: &mut Rng, origins: &[IVec3]) -> IVec3 {
    let origin = origins[rng.below(origins.len() as u64) as usize];
    let local = IVec3::new(
        rng.below(CHUNK as u64) as i32,
        rng.below(CHUNK as u64) as i32,
        rng.below(CHUNK as u64) as i32,
    );

    origin + local
}

fn random_set_edits(rng: &mut Rng, origins: &[IVec3]) -> Vec<VoxelEdit> {
    let count = rng.below(24).saturating_add(8) as usize;

    (0..count)
        .map(|_| {
            let material = u8::try_from(rng.below(256)).unwrap_or(u8::MAX);

            set(random_position(rng, origins), material)
        })
        .collect()
}

fn random_mixed_edits(rng: &mut Rng, origins: &[IVec3]) -> Vec<VoxelEdit> {
    let count = rng.below(24).saturating_add(8) as usize;

    (0..count)
        .map(|_| {
            let position = random_position(rng, origins);

            if rng.below(3) == 0 {
                clear(position)
            } else {
                let material = u8::try_from(rng.below(256)).unwrap_or(u8::MAX);

                set(position, material)
            }
        })
        .collect()
}

#[test]
fn randomized_edits_match_a_fresh_emission() {
    let mut rng = Rng::new(SET_SEED);

    for case in 0..16u32 {
        let input = RendererInput::new().unwrap();
        let mut world = World::default();
        let mut tracked = TrackedCoords::default();
        let origins = chunk_origins(&mut rng, 3);
        let context = format!("case {case}");

        let created = random_set_edits(&mut rng, &origins);
        let batch = submit_edit(&input, &mut world, &created, &tracked);
        tracked = batch.tracked;

        let mixed = random_mixed_edits(&mut rng, &origins);
        let batch = submit_edit(&input, &mut world, &mixed, &tracked);
        tracked = batch.tracked;

        let emitted = emit_snapshots(&world).unwrap();

        assert!(
            geometry_matches(&resident_regions(&input), &emitted),
            "{context}: resident regions do not match pack_regions of a fresh emission"
        );
        assert!(
            !tracked.is_empty(),
            "{context}: the sequence must leave tracked content"
        );
    }
}

#[test]
fn a_loaded_world_edited_matches_a_fresh_emission() {
    let input = RendererInput::new().unwrap();
    let data = open_file(LOADED_ASSET);
    let (mut world, _) = World::new_clipped(&data);

    let mut tracked = submit_load(&input, emit_snapshots(&world).unwrap(), &TrackedCoords::default());

    let Some(first) = world.iter_voxels().next().map(|(position, _)| position) else {
        panic!("the loaded asset must hold at least one voxel");
    };

    let origin = grid_origin(first, MICRO_CHUNK_LENGTH);
    let cells = chunk_cells(origin);
    let edits: Vec<VoxelEdit> = cells
        .iter()
        .enumerate()
        .map(|(index, position)| {
            if index % 5 == 0 {
                clear(*position)
            } else {
                set(*position, u8::try_from((index % 200) as u64 + 1).unwrap_or(u8::MAX))
            }
        })
        .collect();

    let batch = submit_edit(&input, &mut world, &edits, &tracked);
    tracked = batch.tracked;

    let added = origin + IVec3::new(CHUNK, 0, 0);
    let batch = submit_edit(&input, &mut world, &[set(added, 9)], &tracked);

    assert!(
        batch.tracked.contains(&grid_origin(added, MICRO_CHUNK_LENGTH)),
        "the new chunk beside the loaded content is tracked"
    );
    assert_matches_world(&input, &world);
}

#[test]
fn clearing_every_voxel_of_a_chunk_drops_it_from_the_packed_region() {
    let input = RendererInput::new().unwrap();
    let mut world = World::default();
    let kept = IVec3::ZERO;
    let emptied = IVec3::new(CHUNK, 0, 0);

    let fills: Vec<VoxelEdit> = chunk_cells(kept)
        .into_iter()
        .chain(chunk_cells(emptied))
        .enumerate()
        .map(|(index, position)| set(position, u8::try_from(index % 200 + 1).unwrap_or(u8::MAX)))
        .collect();

    let mut tracked = TrackedCoords::default();
    let batch = submit_edit(&input, &mut world, &fills, &tracked);
    tracked = batch.tracked;

    let drops: Vec<VoxelEdit> = chunk_cells(emptied).into_iter().map(clear).collect();
    let batch = edit_world(&mut world, &drops, &tracked).unwrap();

    assert!(
        batch
            .snapshots
            .iter()
            .any(|snapshot| snapshot.global_coords == emptied && snapshot.occupied_count() == 0),
        "the emptied chunk must be submitted as a zero-mask snapshot"
    );

    tracked = batch.tracked;
    input.submit_batch(batch.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        tracked.contains(&kept) && !tracked.contains(&emptied),
        "only the surviving chunk stays tracked"
    );

    let regions = resident_regions(&input);
    let region = region_index_of(kept);

    assert_eq!(regions.len(), 1, "the region still holds the kept chunk");
    assert_eq!(regions[0].region_index, region);

    let emitted = emit_snapshots(&world).unwrap();

    assert_geometry(&regions, &emitted);

    let want = pack_regions(&emitted).unwrap();

    assert!(
        !want[0].aabbs.iter().any(|aabb| {
            let min = aabb.min;
            let origin = IVec3::new(min[0] as i32, min[1] as i32, min[2] as i32);

            grid_origin(origin, MICRO_CHUNK_LENGTH) == emptied
        }),
        "the emptied chunk has no AABB left in the pack"
    );
}

#[test]
fn clearing_every_chunk_of_a_region_yields_none_from_packed_region() {
    let input = RendererInput::new().unwrap();
    let mut world = World::default();
    let origin = IVec3::ZERO;
    let region = region_index_of(origin);

    let fills: Vec<VoxelEdit> = chunk_cells(origin)
        .into_iter()
        .enumerate()
        .map(|(index, position)| set(position, u8::try_from(index % 200 + 1).unwrap_or(u8::MAX)))
        .collect();

    let batch = submit_edit(&input, &mut world, &fills, &TrackedCoords::default());
    let mut tracked = batch.tracked;

    assert!(
        input.packed_region(region).unwrap().is_some(),
        "the filled region packs"
    );

    let drops: Vec<VoxelEdit> = chunk_cells(origin).into_iter().map(clear).collect();
    let batch = edit_world(&mut world, &drops, &tracked).unwrap();

    assert!(
        batch
            .snapshots
            .iter()
            .any(|snapshot| snapshot.global_coords == origin && snapshot.occupied_count() == 0),
        "the last chunk's clear is a zero-mask snapshot"
    );

    tracked = batch.tracked;
    input.submit_batch(batch.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert!(tracked.is_empty(), "the emptied region leaves no tracked chunk");
    assert!(
        input.packed_region(region).unwrap().is_none(),
        "a region emptied by the edits must yield None"
    );
    assert!(resident_regions(&input).is_empty());
}

#[test]
fn a_rejected_batch_changes_nothing() {
    let input = RendererInput::new().unwrap();
    let mut world = World::default();
    let held = IVec3::new(4, 4, 4);

    let batch = submit_edit(
        &input,
        &mut world,
        &[set(held, 6), set(held + IVec3::X, 7)],
        &TrackedCoords::default(),
    );
    let tracked = batch.tracked;
    let voxels_before = world.voxel_count();
    let tracked_before = tracked.clone();
    let emitted_before = emit_snapshots(&world).unwrap();

    let error = edit_world(
        &mut world,
        &[set(IVec3::new(8, 0, 0), 1), set(IVec3::new(2048, 0, 0), 1)],
        &tracked,
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "voxel [2048, 0, 0] is outside the lattice",
        "the error names the offending position"
    );
    assert_eq!(world.voxel_count(), voxels_before, "nothing was mutated");
    assert_eq!(
        emit_snapshots(&world).unwrap(),
        emitted_before,
        "the emission is unchanged"
    );

    // tracked is only borrowed, so rejection cannot change it; the assert
    // pins the contract rather than probing a possible mutation
    assert_eq!(tracked, tracked_before, "the tracked set is unchanged");

    assert_geometry(&resident_regions(&input), &emitted_before);
}

#[test]
fn tracked_edits_converge_under_plan_clear() {
    let input = RendererInput::new().unwrap();
    let mut world = World::default();
    let mut tracked = TrackedCoords::default();

    let first = submit_edit(
        &input,
        &mut world,
        &[
            set(IVec3::new(0, 0, 0), 1),
            set(IVec3::new(CHUNK, 0, 0), 2),
            set(IVec3::new(0, CHUNK, 0), 3),
        ],
        &tracked,
    );
    tracked = first.tracked;

    let second = submit_edit(
        &input,
        &mut world,
        &[
            clear(IVec3::new(0, 0, 0)),
            set(IVec3::new(1, 1, 1), 9),
            set(IVec3::new(200, 5, 5), 4),
        ],
        &tracked,
    );
    tracked = second.tracked;

    assert!(!tracked.is_empty(), "the edits leave content to clear");

    let planned = plan_clear(&tracked);

    assert!(!planned.snapshots.is_empty());
    assert!(
        planned
            .snapshots
            .iter()
            .all(|snapshot| snapshot.occupied_count() == 0),
        "every clear is a zero-mask snapshot"
    );
    assert!(planned.tracked.is_empty(), "tracking is dropped");

    input.submit_batch(planned.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        resident_regions(&input).is_empty(),
        "plan_clear of the tracked set empties the feed"
    );
}

#[test]
fn a_shifted_material_fails_the_oracle_and_the_revert_passes() {
    let input = RendererInput::new().unwrap();
    let mut world = World::default();
    let origin = IVec3::ZERO;

    let batch = edit_world(
        &mut world,
        &[set(origin, 3), set(origin + IVec3::X, 4)],
        &TrackedCoords::default(),
    )
    .unwrap();

    let emitted = emit_snapshots(&world).unwrap();

    input.submit_batch(batch.snapshots.clone()).unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        geometry_matches(&resident_regions(&input), &emitted),
        "the unperturbed batch has to match"
    );

    let mut shifted = batch.snapshots.clone();

    for snapshot in &mut shifted {
        if let Some(material) = snapshot.materials.first_mut() {
            *material = material.wrapping_add(1);
            break;
        }
    }

    input.submit_batch(shifted).unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        !geometry_matches(&resident_regions(&input), &emitted),
        "one material shifted by a slot must fail the oracle"
    );

    input.submit_batch(batch.snapshots).unwrap();
    input.wait_until_idle().unwrap();

    assert!(
        geometry_matches(&resident_regions(&input), &emitted),
        "reverting the shift restores the oracle"
    );
}
