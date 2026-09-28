//! Host edit commands at the sim boundary: the raw Micro-chunk and the
//! single-cell ray forms, diffed and applied at commit in arrival order,
//! waking the edited cells so excavated sand falls.

mod common;

use atlas_rt::sim::Command;
use atlas_rt::world::diff::edit::{
    MICRO_AREA, MICRO_BYTES, MICRO_EDGE, MicroChunkEdit, VoxelChange, VoxelEdit,
};
use atlas_rt::world::raycast::Ray;
use glam::{IVec3, Vec3};

use common::*;

fn clear(x: i32, y: i32, z: i32) -> VoxelEdit {
    VoxelEdit {
        position: IVec3::new(x, y, z),
        change: VoxelChange::Clear,
    }
}

fn cell_index(position: IVec3) -> usize {
    position.x as usize + MICRO_EDGE * position.y as usize + MICRO_AREA * position.z as usize
}

/// The raw Micro-chunk at `origin` holding `cells`, materials packed in
/// ascending cell-index order like a Snapshot packs them.
fn chunk_of(origin: IVec3, cells: &[(IVec3, u8)]) -> MicroChunkEdit {
    let mut packed: Vec<(usize, u8)> = cells
        .iter()
        .map(|(position, material)| (cell_index(*position - origin), *material))
        .collect();

    packed.sort_unstable();

    let mut mask = [0u8; MICRO_BYTES];
    let mut materials = Vec::with_capacity(packed.len());

    for (index, material) in packed {
        if let Some(byte) = mask.get_mut(index / MICRO_EDGE) {
            *byte |= 1u8 << (index % MICRO_EDGE);
        }

        materials.push(material);
    }

    MicroChunkEdit {
        origin,
        mask,
        materials,
    }
}

/// The raw Micro-chunk at `origin` with nothing in it, which clears every
/// cell it covers once the sim diffs it.
fn empty_chunk(origin: IVec3) -> MicroChunkEdit {
    MicroChunkEdit {
        origin,
        mask: [0u8; MICRO_BYTES],
        materials: Vec::new(),
    }
}

#[test]
fn a_micro_chunk_command_diffs_against_the_world_at_commit() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[
        set(0, 0, 0, 5),
        set(1, 0, 0, 7),
        set(8, 0, 0, 3),
    ]));
    wait_ready(&handle);

    // the raw chunk keeps (0, 0, 0), drops (1, 0, 0), and claims (2, 0, 0)
    handle.command(Command::MicroChunk(chunk_of(
        IVec3::ZERO,
        &[(IVec3::new(0, 0, 0), 5), (IVec3::new(2, 0, 0), 9)],
    )));

    let tick = run_tick(&handle);

    assert_eq!(tick.report.batches.len(), 1, "the command committed");

    let guard = world.read().unwrap();

    assert_eq!(
        guard.get_voxel(&IVec3::new(0, 0, 0)),
        Some(&5),
        "the cell the chunk keeps is untouched"
    );
    assert_eq!(
        guard.get_voxel(&IVec3::new(1, 0, 0)),
        None,
        "the cell the chunk leaves out is cleared"
    );
    assert_eq!(
        guard.get_voxel(&IVec3::new(2, 0, 0)),
        Some(&9),
        "the cell the chunk claims is set"
    );
    assert_eq!(
        guard.get_voxel(&IVec3::new(8, 0, 0)),
        Some(&3),
        "the neighbouring Micro-chunk is outside the diff"
    );
}

#[test]
fn an_empty_chunk_clears_every_cell_it_covers() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[
        set(0, 0, 0, 5),
        set(9, 7, 0, 6),
        set(0, 8, 0, 7),
    ]));
    wait_ready(&handle);

    handle.command(Command::MicroChunk(empty_chunk(IVec3::ZERO)));

    let tick = run_tick(&handle);

    assert_eq!(tick.report.batches.len(), 1, "the command committed");

    let guard = world.read().unwrap();

    assert_eq!(
        guard.get_voxel(&IVec3::new(0, 0, 0)),
        None,
        "cell (0, 0, 0) sits in the chunk"
    );
    assert_eq!(
        guard.get_voxel(&IVec3::new(9, 7, 0)),
        Some(&6),
        "cell (9, 7, 0) sits in the neighbouring Micro-chunk"
    );
    assert_eq!(
        guard.get_voxel(&IVec3::new(0, 8, 0)),
        Some(&7),
        "cell (0, 8, 0) sits in the Micro-chunk above"
    );
}

#[test]
fn two_writes_in_one_tick_window_apply_in_arrival_order() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 5)]));
    wait_ready(&handle);

    handle.command(Command::Cell(set(0, 0, 0, 9)));
    handle.command(Command::MicroChunk(chunk_of(
        IVec3::ZERO,
        &[(IVec3::new(0, 0, 0), 5)],
    )));

    let tick = run_tick(&handle);

    assert_eq!(
        tick.report.batches.len(),
        1,
        "the two writes merge into one batch"
    );

    let batch = tick.report.batches.first().expect("one batch");

    assert_eq!(
        batch.len(),
        1,
        "both writes touch one Micro-chunk, so the chunk reaches the renderer once"
    );
    forward(&handle, batch);

    assert_eq!(
        held(&handle, IVec3::ZERO),
        Some(5),
        "the chunk claims the cell back, so it diffed against the World the first write left"
    );
}

#[test]
fn a_repeated_clear_resolves_as_a_no_op() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(3, 3, 3, 4)]));
    wait_ready(&handle);

    handle.command(Command::Cell(clear(3, 3, 3)));
    handle.command(Command::Cell(clear(3, 3, 3)));

    let tick = run_tick(&handle);

    assert_eq!(
        tick.report.batches.len(),
        1,
        "only the first clear changes the World"
    );
    assert_eq!(held(&handle, IVec3::new(3, 3, 3)), None);

    handle.command(Command::Cell(clear(3, 3, 3)));

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the cell is already clear, so nothing commits"
    );
}

#[test]
fn commands_buffer_while_paused_and_apply_in_order_after_resume() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(1, 1, 1, 2)]));
    wait_ready(&handle);

    handle.set_paused(true);
    handle.command(Command::Cell(set(4, 1, 1, 6)));
    handle.command(Command::Cell(set(4, 1, 1, 7)));

    assert_silent(&handle);

    handle.set_paused(false);

    let tick = run_tick(&handle);

    assert_eq!(
        tick.report.batches.len(),
        1,
        "the buffered commands commit as one batch"
    );
    assert_eq!(
        held(&handle, IVec3::new(4, 1, 1)),
        Some(7),
        "the buffer applies in arrival order"
    );
}

#[test]
fn a_command_waits_for_a_frame_and_commits_at_the_next_one() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[]));
    wait_ready(&handle);

    handle.command(Command::Cell(set(6, 6, 6, 4)));

    assert_silent(&handle);

    feed(&handle, part(9, 10));
    assert_silent(&handle);

    let tick = run_tick(&handle);

    assert_eq!(tick.report.ticks, 1, "the short frame owed no tick");
    assert_eq!(
        tick.report.batches.len(),
        1,
        "the command commits with the first tick that runs after it"
    );
    assert_eq!(held(&handle, IVec3::new(6, 6, 6)), Some(4));
}

#[test]
fn an_out_of_lattice_command_is_dropped_and_the_sim_keeps_running() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 0, 0, 1)]));
    wait_ready(&handle);

    handle.command(Command::Cell(set(2048, 0, 0, 3)));

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the out-of-lattice cell commits nothing"
    );
    assert_eq!(held(&handle, IVec3::ZERO), Some(1));

    handle.command(Command::MicroChunk(MicroChunkEdit {
        origin: IVec3::new(2048, 0, 0),
        mask: [0u8; MICRO_BYTES],
        materials: Vec::new(),
    }));

    let tick = run_tick(&handle);

    assert!(tick.report.batches.is_empty());

    let tick = run_tick(&handle);

    assert_eq!(tick.report.ticks, 1, "the sim keeps ticking");
    assert_eq!(held(&handle, IVec3::ZERO), Some(1));
}

/// A solid floor at `height`, wide enough that a grain standing on its
/// centre has no diagonal to slide into.
fn floor_at(height: i32) -> Vec<VoxelEdit> {
    let mut edits = Vec::new();

    for x in 3..=5 {
        for z in 3..=5 {
            edits.push(set(x, height, z, 1));
        }
    }

    edits
}

#[test]
fn a_dig_wakes_the_cell_it_left_and_the_three_above() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor_at(3);

    edits.push(set(4, 4, 4, GRAIN));
    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    let tick = run_tick(&handle);

    assert!(
        tick.report.batches.is_empty(),
        "the grain rests on the floor"
    );

    handle.command(Command::Cell(clear(4, 3, 4)));

    let tick = run_tick(&handle);

    assert_eq!(tick.report.batches.len(), 1, "the dig committed");
    assert_eq!(held(&handle, IVec3::new(4, 3, 4)), None);

    let tick = run_tick(&handle);

    assert_eq!(
        tick.report.batches.len(),
        1,
        "the woken grain falls without a world scan"
    );
    assert_eq!(held(&handle, IVec3::new(4, 4, 4)), None);
    assert_eq!(
        held(&handle, IVec3::new(4, 3, 4)),
        Some(u32::from(GRAIN)),
        "the grain took the cell the dig opened"
    );
}

#[test]
fn a_raycast_through_the_read_lock_sees_the_world_the_dig_left() {
    let (world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(4, 1, 4, 1), set(4, 1, 8, 1)]));
    wait_ready(&handle);

    let ray = Ray::new(Vec3::new(4.5, 1.5, 0.0), Vec3::Z);

    let before = world.read().unwrap();
    let hits_before = before.raycast(ray);
    let count_before = before.voxel_count();

    drop(before);

    assert_eq!(
        hits_before.map(|hit| hit.voxel),
        Some(IVec3::new(4, 1, 4)),
        "the wall is in the way to start with"
    );

    handle.command(Command::Cell(clear(4, 1, 4)));

    let tick = run_tick(&handle);

    assert_eq!(tick.report.batches.len(), 1, "the dig committed");

    let after = world.read().unwrap();
    let hits_after = after.raycast(ray);
    let count_after = after.voxel_count();

    assert_eq!(
        hits_after.map(|hit| hit.voxel),
        Some(IVec3::new(4, 1, 8)),
        "the ray passes the dug cell and reaches the backdrop"
    );
    assert_eq!(
        count_after,
        count_before.saturating_sub(1),
        "the count read through the lock follows the dig"
    );
}
