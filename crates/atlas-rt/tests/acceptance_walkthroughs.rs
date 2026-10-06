//! The nine acceptance walkthroughs from the falling-sand decision, each one
//! test at the sim seam: the real thread and channel boundary with
//! test-controlled elapsed, so tick counts stay deterministic.

mod common;

use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use atlas_rt::render::region::pack::{RegionData, pack_regions};
use atlas_rt::render::region::queue::RendererInput;
use atlas_rt::sim::{Command, Handle, InputSample, ParityPolicy, PlayerProfile, PlayerState};
use atlas_rt::world::diff::edit::{VoxelChange, VoxelEdit};
use atlas_rt::world::diff::snapshot::{MicroChunkSnapshot, emit_snapshots};
use atlas_rt::world::micro::MicroChunk;
use glam::{IVec3, Vec2, Vec3};

use common::*;

const EPSILON: f32 = 1.0e-3;
const DT: f32 = 1.0 / 30.0;

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < EPSILON
}

fn keys(movement: Vec2) -> InputSample {
    InputSample {
        movement,
        jump_edge: None,
    }
}

fn pressed() -> InputSample {
    InputSample {
        movement: Vec2::ZERO,
        jump_edge: Some(Instant::now()),
    }
}

/// The height after `ticks` ticks of a jump launched at `launch` units per
/// second, the launch tick included and gravity skipped on it.
fn arc(start: f32, ticks: u32, launch: f32, gravity: f32) -> f32 {
    let n = ticks as f32;

    start + (launch * n - 0.5 * gravity * DT * n * (n - 1.0)) * DT
}

/// A nine by nine floor at y = 0, the ground the granular walkthroughs use.
fn floor9() -> Vec<VoxelEdit> {
    let mut edits = Vec::new();

    for x in 0..=8 {
        for z in 0..=8 {
            edits.push(set(x, 0, z, 1));
        }
    }

    edits
}

/// A full depth floor across `x0..=x1` at y = 0.
fn floor(x0: i32, x1: i32) -> Vec<VoxelEdit> {
    (x0..=x1)
        .flat_map(|x| (0..=4).map(move |z| set(x, 0, z, 1)))
        .collect()
}

/// A floor at y = 0 across `x0..=x1`, `z0..=z1`.
fn floor_z(x0: i32, x1: i32, z0: i32, z1: i32) -> Vec<VoxelEdit> {
    (x0..=x1)
        .flat_map(|x| (z0..=z1).map(move |z| set(x, 0, z, 1)))
        .collect()
}

/// A full depth block across `x0..=x1`, `y0..=y1`.
fn fill(x0: i32, x1: i32, y0: i32, y1: i32) -> Vec<VoxelEdit> {
    (x0..=x1)
        .flat_map(|x| (y0..=y1).flat_map(move |y| (0..=4).map(move |z| set(x, y, z, 1))))
        .collect()
}

fn clear(x: i32, y: i32, z: i32) -> VoxelEdit {
    VoxelEdit {
        position: IVec3::new(x, y, z),
        change: VoxelChange::Clear,
    }
}

fn grains(handle: &Handle) -> Vec<IVec3> {
    let guard = handle.world().read().unwrap();
    let mut cells: Vec<IVec3> = guard
        .iter_voxels()
        .filter(|(_, voxel)| *voxel == GRAIN)
        .map(|(position, _)| position)
        .collect();

    cells.sort_unstable_by_key(|cell| cell.to_array());

    cells
}

/// Feeds one whole period, requires exactly one tick, and hands back the
/// batches that tick submitted.
fn one_tick(handle: &Handle) -> Vec<Vec<MicroChunkSnapshot>> {
    let tick = run_tick(handle);

    assert_eq!(tick.report.ticks, 1, "one frame owes one tick");

    tick.report.batches
}

/// Steps until the queue stays empty for five straight ticks, and returns
/// the tick that drained it, or panics after sixty.
fn drain(handle: &Handle, what: &str) -> u32 {
    let mut quiet = 0_u32;

    for tick in 1..=60 {
        if one_tick(handle).is_empty() {
            quiet += 1;

            if quiet == 5 {
                return tick;
            }
        } else {
            quiet = 0;
        }
    }

    panic!("{what}: the queue never drained");
}

/// Feeds `ticks` frames of `sample` and reports the pose each tick ended on.
fn walk(handle: &Handle, sample: InputSample, ticks: u32) -> Vec<PlayerState> {
    let mut players = Vec::new();

    for _ in 0..ticks {
        handle.frame(period(), sample);
        players.push(expect_tick(recv_push(handle)).player);
    }

    players
}

/// Presses the jump once with `movement` held, then feeds `ticks - 1` more
/// frames of it, and reports the pose each tick ended on.
fn press_and_run(handle: &Handle, movement: Vec2, ticks: u32) -> Vec<PlayerState> {
    handle.frame(
        period(),
        InputSample {
            movement,
            ..pressed()
        },
    );

    let mut players = vec![expect_tick(recv_push(handle)).player];

    for _ in 1..ticks {
        handle.frame(period(), keys(movement));
        players.push(expect_tick(recv_push(handle)).player);
    }

    players
}

/// Activates `scene` on a fresh sim and reports the handle and the pose it
/// spawned at.
fn ready(scene: &[VoxelEdit]) -> (Handle, PlayerState) {
    let (_world, handle) = spawn_sim();

    handle.activate(granular_activation_of(scene));
    let spawn = wait_ready(&handle);

    (handle, spawn)
}

/// The cell span the box's x face covers, the same slack the contact tests
/// use.
fn span(feet_x: f32) -> RangeInclusive<i32> {
    let half = PlayerProfile::default().width * 0.5;

    ((feet_x - half + 1.0e-3).floor() as i32)..=((feet_x + half - 1.0e-3).floor() as i32)
}

/// The regions the given snapshots pack into, the geometry a renderer holding
/// exactly those snapshots would show.
fn pack_regions_for(snapshots: &[MicroChunkSnapshot]) -> Vec<RegionData> {
    pack_regions(snapshots).expect("the World's own snapshots always pack")
}

/// Whether two packed region lists hold the same geometry.
fn same_geometry(left: &[RegionData], right: &[RegionData]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| {
            a.region_index == b.region_index && a.blocks == b.blocks && a.aabbs == b.aabbs
        })
}

// ---------------------------------------------------------------------------
// Walkthrough 1: empty, solid, and falling granular.
// ---------------------------------------------------------------------------

/// A floor with a standing pillar and a six grain column that falls past them.
fn column_scene() -> Vec<VoxelEdit> {
    let mut edits = floor9();

    for y in 1..=5 {
        edits.push(set(2, y, 4, 1));
    }

    for y in 3..=8 {
        edits.push(set(6, y, 6, GRAIN));
    }

    edits
}

#[test]
fn walkthrough_empty_solid_and_granular_fall() {
    let (_world, handle) = spawn_sim();

    handle.activate(granular_activation_of(&column_scene()));
    wait_ready(&handle);

    let pillar: Vec<IVec3> = (1..=5).map(|y| IVec3::new(2, y, 4)).collect();
    let start = grains(&handle);

    assert_eq!(start.len(), 6, "the column starts six grains tall");

    for _ in 0..10 {
        run_tick(&handle);

        for cell in &pillar {
            assert_eq!(
                held(&handle, *cell),
                Some(1),
                "solid is no rule at all: {cell} never moves"
            );
        }
    }

    let descended = grains(&handle);
    let start_low = start.iter().map(|cell| cell.y).min().expect("six grains");
    let now_low = descended
        .iter()
        .map(|cell| cell.y)
        .min()
        .expect("six grains");

    assert!(
        now_low < start_low,
        "the grains fell through empty cells: {start_low} to {now_low}"
    );

    for _ in 0..30 {
        run_tick(&handle);
    }

    let landed = grains(&handle);
    let mut spread: Vec<i32> = landed.iter().map(|cell| cell.x).collect();

    spread.sort_unstable();
    spread.dedup();

    assert_eq!(
        spread,
        vec![5, 6, 7],
        "tick 40: the landing column spreads both ways: {landed:?}"
    );

    for _ in 0..60 {
        run_tick(&handle);
    }

    let settled = run_tick(&handle);

    assert!(
        settled.report.batches.is_empty(),
        "the queue has drained, so nothing commits"
    );
    assert!(
        settled.report.commit_time.is_zero(),
        "nothing left to commit"
    );

    for cell in &pillar {
        assert_eq!(
            held(&handle, *cell),
            Some(1),
            "the pillar still stands after the fall"
        );
    }
}

// ---------------------------------------------------------------------------
// Walkthrough 2: two-phase moves, first claim wins, the loser retries.
// ---------------------------------------------------------------------------

#[test]
fn walkthrough_two_phase_claim_with_retry() {
    let (_world, handle) = spawn_sim();
    let mut edits = floor9();

    edits.retain(|edit| edit.position != IVec3::new(7, 0, 6));
    edits.push(set(6, -1, 6, 1));
    edits.push(set(7, -1, 6, 1));
    edits.push(set(8, -1, 6, 1));
    edits.push(set(4, 1, 6, 1));
    edits.push(set(6, 1, 6, 1));
    edits.push(set(4, 2, 6, GRAIN));
    edits.push(set(6, 2, 6, GRAIN));
    edits.push(set(7, 1, 6, GRAIN));

    handle.activate(granular_activation_of(&edits));
    wait_ready(&handle);

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(6, 2, 6),
            IVec3::new(7, 0, 6)
        ],
        "the first claim moved in, the loser kept its cell, and the blocker fell away"
    );

    one_tick(&handle);

    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(7, 0, 6),
            IVec3::new(7, 1, 6)
        ],
        "the loser retried into the diagonal its blocker vacated"
    );

    one_tick(&handle);

    let tick = run_tick(&handle);

    assert!(tick.report.batches.is_empty(), "every grain has settled");
    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(5, 1, 6),
            IVec3::new(7, 0, 6),
            IVec3::new(7, 1, 6)
        ]
    );
}

// ---------------------------------------------------------------------------
// Walkthrough 3: diagonal choices are deterministic under both parities.
// ---------------------------------------------------------------------------

#[test]
fn walkthrough_determinism_under_both_parities() {
    let scene = || {
        let mut edits = floor9();

        edits.push(set(2, 1, 2, 1));
        edits.push(set(5, 1, 5, 1));
        edits.push(set(7, 1, 3, 1));

        for cell in [
            (2, 2, 2),
            (5, 2, 5),
            (7, 2, 3),
            (4, 3, 4),
            (4, 4, 4),
            (4, 5, 4),
            (1, 4, 1),
            (1, 5, 1),
            (6, 2, 6),
            (6, 3, 6),
            (3, 6, 3),
            (8, 4, 8),
            (0, 3, 7),
            (8, 7, 0),
        ] {
            edits.push(set(cell.0, cell.1, cell.2, GRAIN));
        }

        granular_activation_of(&edits)
    };

    let mut by_parity = [0u64; 2];

    for (index, parity) in [ParityPolicy::Alternate, ParityPolicy::AlwaysNegative]
        .into_iter()
        .enumerate()
    {
        let mut runs = [0u64; 2];

        for run in &mut runs {
            let (_world, handle) = spawn_sim_parity(parity);

            handle.activate(scene());
            wait_ready(&handle);

            let rested = world_hash(&handle);

            for _ in 0..90 {
                one_tick(&handle);
            }

            let settled = world_hash(&handle);

            assert_ne!(
                settled, rested,
                "the scene has to churn for a hash match to mean anything"
            );

            *run = settled;
        }

        assert_eq!(
            runs[0], runs[1],
            "two 90-tick sequences under {parity:?} hash the same"
        );

        by_parity[index] = runs[0];
    }

    assert_ne!(
        by_parity[0], by_parity[1],
        "the parity choice leans the spread a different way"
    );
}

// ---------------------------------------------------------------------------
// Walkthrough 4: gravity and a clean jump.
// ---------------------------------------------------------------------------

/// A floor with a one cell pillar the jump lands back on.
fn floor_and_pillar() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 4);

    edits.push(set(2, 3, 2, 1));

    edits
}

#[test]
fn walkthrough_gravity_and_jump_arc() {
    let scene = floor_and_pillar();
    let (handle, spawn) = ready(&scene);

    assert_eq!(
        spawn.feet,
        Vec3::new(2.5, 4.0, 2.5),
        "the spawn stands on the pillar"
    );
    assert!(spawn.grounded, "the pillar holds the spawn");

    let players = press_and_run(&handle, Vec2::ZERO, 25);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;

        assert!(
            near(player.feet.y, arc(4.0, tick, 80.0, 200.0)),
            "tick {tick}: feet.y {}",
            player.feet.y
        );
        assert_eq!(player.feet.x, 2.5, "no horizontal input");
        assert_eq!(player.feet.z, 2.5, "no horizontal input");
    }

    assert!(
        players.last().expect("twenty five ticks").grounded,
        "the arc ends resting on the pillar"
    );

    for _ in 0..4 {
        feed(&handle, period());

        let settled_pose = expect_tick(recv_push(&handle)).player;

        assert_eq!(
            settled_pose.feet.y, 4.0,
            "no jump edge arrives, so nothing launches again"
        );
        assert!(settled_pose.grounded, "the pillar keeps holding");
    }
}

// ---------------------------------------------------------------------------
// Walkthrough 5: the ceiling stops the rise, the wall takes the slide.
// ---------------------------------------------------------------------------

/// The ceiling plane the jump rides, one cell above the highest rise the
/// corridor leaves open.
const CEILING: i32 = 29;

/// A floor room with a ceiling over the approach and a tall wall at the end,
/// so the walk runs level under the ceiling, stops at the wall, and the jump
/// it takes there rides the ceiling and comes back down the wall's face.
fn corridor() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 40);

    for x in 25..=40 {
        for z in 0..=4 {
            edits.push(set(x, CEILING, z, 1));
        }
    }

    for y in 1..=(CEILING - 1) {
        for z in 0..=4 {
            edits.push(set(35, y, z, 1));
        }
    }

    edits
}

#[test]
fn walkthrough_ceiling_stops_the_rise_and_the_wall_takes_the_slide() {
    let (handle, spawn) = ready(&corridor());

    assert_eq!(spawn.feet, Vec3::new(20.5, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    let walked = walk(&handle, keys(Vec2::X), 30);

    for (index, player) in walked.iter().enumerate() {
        let tick = index as u32 + 1;

        assert_eq!(
            player.feet.y, 1.0,
            "tick {tick} runs level under the ceiling"
        );
        assert!(player.grounded, "tick {tick} keeps the floor");
    }

    let at_wall = walked.last().expect("thirty ticks");

    assert!(
        near(at_wall.feet.x, 32.0),
        "the wall face stops the walk: {}",
        at_wall.feet.x
    );

    let jumped = press_and_run(&handle, Vec2::X, 45);

    for (index, player) in jumped.iter().enumerate() {
        let tick = index as u32 + 1;

        assert!(
            near(player.feet.x, 32.0),
            "tick {tick} left the wall face: {}",
            player.feet.x
        );
    }

    let peak = jumped
        .iter()
        .map(|player| player.feet.y)
        .fold(0.0_f32, f32::max);
    let headroom = CEILING as f32 - PlayerProfile::default().body_height;

    assert!(
        near(peak, headroom),
        "the rise stops at the ceiling plane, not the jump apex: {peak}"
    );
    assert!(
        peak < arc(1.0, 13, 80.0, 200.0),
        "the ceiling holds the rise below the free apex: {peak}"
    );

    let peak_index = jumped
        .iter()
        .position(|player| near(player.feet.y, peak))
        .expect("a tick reached the peak");
    let after_peak = &jumped[peak_index + 1..];

    assert!(
        after_peak.iter().any(|player| player.feet.y < peak - 1.0),
        "the fall runs down the wall's face"
    );
    assert!(
        after_peak.iter().any(|player| !player.grounded),
        "the slide down the face is airborne"
    );

    let landed = jumped.last().expect("forty five ticks");

    assert_eq!(landed.feet.y, 1.0, "the fall returns to the floor");
    assert!(landed.grounded, "the landing reports grounded");
}

// ---------------------------------------------------------------------------
// Walkthrough 6: the obstacle run in one scenario.
// ---------------------------------------------------------------------------

/// The one cell gap the walk has to bridge.
const CRACK: i32 = 78;

/// The trench the walk crosses on settled sand, from its first cell to its
/// last.
const TRENCH_START: i32 = 84;
const TRENCH_END: i32 = 90;

/// A floor holding every obstacle in one run: a one cell crack, a seven cell
/// trench filled to the brim with sand, a one voxel step, a two voxel step,
/// and a wall taller than the step height. The floor spans the whole run, so
/// the spawn lands clear of the crack.
fn obstacle_run() -> Vec<VoxelEdit> {
    let mut edits: Vec<VoxelEdit> = (0..=145)
        .filter(|x| *x != CRACK && !(TRENCH_START..=TRENCH_END).contains(x))
        .flat_map(|x| (0..=4).map(move |z| set(x, 0, z, 1)))
        .collect();

    for x in TRENCH_START..=TRENCH_END {
        for z in -1..=5 {
            edits.push(set(x, -1, z, 1));
        }

        for z in 0..=4 {
            edits.push(set(x, 0, z, GRAIN));
        }
    }

    edits.extend(fill(93, 112, 1, 1));
    edits.extend(fill(113, 142, 1, 3));
    edits.extend(fill(143, 145, 1, 8));

    edits
}

#[test]
fn walkthrough_the_obstacle_run() {
    let (handle, spawn) = ready(&obstacle_run());

    assert_eq!(spawn.feet, Vec3::new(73.0, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 60);
    let mut over_trench = None;

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;
        let cells = span(player.feet.x);

        assert!(
            player.grounded,
            "tick {tick} lost its support at y {}",
            player.feet.y
        );

        if cells.contains(&CRACK) {
            assert_eq!(
                player.feet.y, 1.0,
                "tick {tick} bridges the crack instead of falling in"
            );
        }

        if *cells.start() >= TRENCH_START && *cells.end() <= TRENCH_END {
            over_trench = Some(index);
        }
    }

    let across = players
        .get(over_trench.expect("the walk crosses the trench with both feet over sand"))
        .expect("the trench tick");

    assert_eq!(across.feet.y, 1.0, "settled sand is a floor, not a gap");
    assert!(across.grounded, "the sand holds the walk");

    for x in span(across.feet.x) {
        for z in 0..=4 {
            assert_eq!(
                held(&handle, IVec3::new(x, 0, z)),
                Some(GRAIN),
                "cell ({x}, 0, {z}) under the walk still holds sand"
            );
            assert_eq!(
                held(&handle, IVec3::new(x, -1, z)),
                Some(1),
                "cell ({x}, -1, {z}) still floors the trench"
            );
        }
    }

    let rise = players.get(12).expect("thirteen ticks");

    assert!(
        near(rise.feet.x, 90.33333),
        "the stride carries onto the step: {}",
        rise.feet.x
    );
    assert_eq!(rise.feet.y, 2.0, "the first step climbs one without a jump");

    let twice = players.get(27).expect("twenty eight ticks");

    assert!(
        near(twice.feet.x, 110.3334),
        "the stride carries onto the platform: {}",
        twice.feet.x
    );
    assert_eq!(
        twice.feet.y, 4.0,
        "the two voxel step climbs in one move without a jump"
    );

    let mut rises = vec![spawn.feet.y];

    for player in &players {
        if !near(player.feet.y, *rises.last().expect("the walk starts flat")) {
            rises.push(player.feet.y);
        }
    }

    assert_eq!(
        rises,
        vec![1.0, 2.0, 4.0],
        "the run climbs one rise then two, and never jumps"
    );

    let wall = players.get(50).expect("fifty one ticks");

    assert!(
        near(wall.feet.x, 140.0),
        "the taller wall stops the walk at its face: {}",
        wall.feet.x
    );
    assert_eq!(wall.feet.y, 4.0, "the wall never becomes a step");

    for player in players.iter().skip(50) {
        assert!(
            near(player.feet.x, 140.0),
            "the walk holds against the wall: {}",
            player.feet.x
        );
        assert_eq!(player.feet.y, 4.0, "the wall never becomes a step");
    }

    for x in TRENCH_START..=TRENCH_END {
        for z in 0..=4 {
            assert_eq!(
                held(&handle, IVec3::new(x, 0, z)),
                Some(GRAIN),
                "the trench keeps its sand at ({x}, 0, {z})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Walkthrough 7: sand passes through the player, then lifts them out, and
// the settled pile climbs one rise at a time.
// ---------------------------------------------------------------------------

/// A floor under a settled sand staircase, one rise taller than the last,
/// with a wall at the tall edge so the last column keeps its stack.
fn pile_walk() -> Vec<VoxelEdit> {
    let mut edits = floor_z(-20, 40, -6, 10);

    for step in 0..4 {
        for x in (19 + step * 3)..=(21 + step * 3) {
            for y in 1..=(step + 1) {
                for z in 1..=3 {
                    edits.push(set(x, y, z, GRAIN));
                }
            }
        }
    }

    edits.extend(fill(31, 31, 1, 10));

    edits
}

#[test]
fn walkthrough_sand_through_the_player_then_pile_climb() {
    let scene = pile_walk();
    let seeded = scene
        .iter()
        .filter(|edit| matches!(edit.change, VoxelChange::Set(GRAIN)))
        .count();

    let (handle, spawn) = ready(&scene);

    assert_eq!(spawn.feet, Vec3::new(10.5, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    drain(&handle, "the seeded pile settles");

    assert_eq!(
        grains(&handle).len(),
        seeded,
        "every seeded grain rests in the pile"
    );

    handle.command(Command::Cell(set(11, 20, 2, GRAIN)));

    let mut lifted_at = None;
    let mut lifted = None;

    for tick in 1..=40 {
        let pose = run_tick(&handle).player;

        if pose.feet.y > 1.0 {
            lifted_at = Some(tick);
            lifted = Some(pose);
            break;
        }

        assert_eq!(
            pose.feet,
            Vec3::new(10.5, 1.0, 2.5),
            "tick {tick}: the moving grain passes through without blocking"
        );
        assert!(pose.grounded, "tick {tick} keeps the floor");
    }

    let lifted_at = lifted_at.expect("depenetration lifts the player");
    let lifted = lifted.expect("the lift tick reports a pose");

    assert!(
        lifted_at > 10,
        "the grain had to fall the length of the box first: tick {lifted_at}"
    );

    assert_eq!(
        lifted.feet,
        Vec3::new(10.5, 2.0, 2.5),
        "up-first depenetration lifts the player straight out of the overlap"
    );
    assert!(lifted.grounded, "the settled grain under the feet holds");

    assert_eq!(
        held(&handle, IVec3::new(11, 1, 2)),
        Some(GRAIN),
        "the grain came to rest inside the box"
    );

    let drained_at = drain(&handle, "the settle around the player drains the queue");

    assert_eq!(
        grains(&handle).len(),
        seeded + 1,
        "no grain leaves the pile while it settles: drained at tick {drained_at}"
    );

    let climbed = walk(&handle, keys(Vec2::X), 60);
    let settled_at = climbed
        .iter()
        .rposition(|player| near(player.feet.y, 1.0))
        .expect("the lift drops back to the floor before the climb");

    let mut rises = Vec::new();
    let mut held_up = 1.0_f32;

    for player in &climbed[settled_at..] {
        assert!(
            player.grounded,
            "the pile holds the walk at y {}",
            player.feet.y
        );

        if !near(player.feet.y, held_up) {
            rises.push(player.feet.y);
            held_up = player.feet.y;
        }
    }

    assert_eq!(
        rises,
        vec![2.0, 3.0, 4.0, 5.0],
        "the pile climbs exactly one rise at a time"
    );

    let top = climbed.last().expect("sixty ticks");

    assert!(
        near(top.feet.x, 28.0),
        "the walk crosses the whole pile: {}",
        top.feet.x
    );
    assert_eq!(top.feet.y, 5.0, "the wall stops the walk on the top rise");
    assert!(top.grounded, "the top of the pile holds");
}

// ---------------------------------------------------------------------------
// Walkthrough 8: a dig wakes the cell it left and the three above.
// ---------------------------------------------------------------------------

/// A floor under a three by three platform, with a sand column standing on
/// the platform's centre.
fn dig_slab() -> Vec<VoxelEdit> {
    let mut edits = floor9();

    for x in 3..=5 {
        for z in 3..=5 {
            edits.push(set(x, 3, z, 1));
        }
    }

    edits.push(set(4, 4, 4, GRAIN));
    edits.push(set(4, 5, 4, GRAIN));
    edits.push(set(4, 6, 4, GRAIN));

    edits
}

#[test]
fn walkthrough_a_dig_wakes_the_edited_cell_and_the_three_above() {
    let (_world, handle) = spawn_sim();

    handle.activate(granular_activation_of(&dig_slab()));
    wait_ready(&handle);

    let resting = run_tick(&handle);

    assert!(
        resting.report.batches.is_empty(),
        "the slab and its sand settle first"
    );
    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(4, 4, 4),
            IVec3::new(4, 5, 4),
            IVec3::new(4, 6, 4)
        ],
        "the sand rests on the platform"
    );

    handle.command(Command::Cell(clear(4, 3, 4)));

    let tick = run_tick(&handle);

    assert_eq!(tick.report.batches.len(), 1, "the dig committed");
    assert_eq!(held(&handle, IVec3::new(4, 3, 4)), None, "the cell is open");
    assert_eq!(
        grains(&handle),
        vec![
            IVec3::new(4, 4, 4),
            IVec3::new(4, 6, 4),
            IVec3::new(5, 4, 4)
        ],
        "the dig wakes the column and its neighbour slides off the platform"
    );

    let falling = [
        vec![
            IVec3::new(4, 3, 4),
            IVec3::new(4, 5, 4),
            IVec3::new(6, 3, 4),
        ],
        vec![
            IVec3::new(4, 2, 4),
            IVec3::new(4, 4, 4),
            IVec3::new(6, 2, 4),
        ],
        vec![
            IVec3::new(4, 1, 4),
            IVec3::new(4, 3, 4),
            IVec3::new(6, 1, 4),
        ],
    ];

    for (index, expected) in falling.iter().enumerate() {
        run_tick(&handle);

        let step = index + 1;

        assert_eq!(
            &grains(&handle),
            expected,
            "step {step}: the woken grains land exactly where the rules move them"
        );
    }

    for _ in 0..40 {
        run_tick(&handle);
    }

    let settled = run_tick(&handle);

    assert!(
        settled.report.batches.is_empty(),
        "the queue drains back to zero once the hole has eaten its column"
    );

    let rest = grains(&handle);

    assert_eq!(rest.len(), 3, "every grain survived the fall: {rest:?}");
    assert!(
        rest.iter().all(|cell| cell.y <= 3),
        "the sand piled on the floor: {rest:?}"
    );
}

// ---------------------------------------------------------------------------
// Walkthrough 9: the renderer trails the World, and a failing batch is
// dropped while the frame keeps running.
// ---------------------------------------------------------------------------

/// A floor with a grain that commits a batch every tick until it lands.
fn trail_scene() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 8);

    edits.push(set(7, 7, 2, GRAIN));

    edits
}

/// Submits every batch and waits, the way a caught-up renderer applies them.
fn forward_all(input: &RendererInput, batches: &[Vec<MicroChunkSnapshot>]) {
    for batch in batches {
        input.submit_batch(batch.iter().cloned()).unwrap();
    }

    input.wait_until_idle().unwrap();
}

#[test]
fn walkthrough_the_renderer_trails_and_drops_a_failing_batch() {
    let (handle, _spawn) = ready(&trail_scene());
    let input = RendererInput::new().unwrap();

    let initial = emit_snapshots(&handle.world().read().unwrap()).unwrap();

    forward_all(&input, &[initial.clone()]);
    assert_geometry(&input.packed_regions().unwrap(), &initial);

    let rested = world_hash(&handle);
    let mut withheld = Vec::new();

    for _ in 0..3 {
        withheld.extend(one_tick(&handle));
    }

    assert!(
        !withheld.is_empty(),
        "the falling grain commits batches to forward"
    );
    assert_ne!(
        world_hash(&handle),
        rested,
        "the World kept changing while the renderer trailed"
    );

    let trailing = input.packed_regions().unwrap();
    let current = emit_snapshots(&handle.world().read().unwrap()).unwrap();
    let caught_up = pack_regions_for(&current);

    assert!(
        !same_geometry(&trailing, &caught_up),
        "the renderer shows an older state while the World runs ahead"
    );

    forward_all(&input, &withheld);

    assert_geometry(&input.packed_regions().unwrap(), &current);

    let frame = run_tick(&handle);
    let mut failing = frame
        .report
        .batches
        .first()
        .expect("the grain still commits")
        .clone();

    failing.push(MicroChunkSnapshot {
        global_coords: IVec3::new(2048, 0, 0),
        chunk: MicroChunk::empty(),
    });

    let queue_before = input.packed_regions().unwrap();
    let world_before = world_hash(&handle);

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        input.submit_batch(failing.iter().cloned())
    }));

    let payload = outcome.expect_err("the out-of-lattice snapshot must assert");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("the panic carried no message");

    assert!(
        message.contains("exceeds the renderer lattice"),
        "the batch failed on the lattice bound: {message}"
    );

    let queue_after = input.packed_regions().unwrap();

    assert!(
        same_geometry(&queue_before, &queue_after),
        "the rejected batch left the queue untouched"
    );
    assert_eq!(
        world_hash(&handle),
        world_before,
        "the World keeps the edits the renderer never saw"
    );

    let world_now = emit_snapshots(&handle.world().read().unwrap()).unwrap();

    assert!(
        !same_geometry(&pack_regions_for(&world_now), &queue_after),
        "the whole batch is dropped, so the renderer never caught the World"
    );

    let resume = run_tick(&handle);
    let resume_batch = resume
        .report
        .batches
        .first()
        .expect("the grain still commits");

    input.submit_batch(resume_batch.iter().cloned()).unwrap();
    input.wait_until_idle().unwrap();

    let after_resume = input.packed_regions().unwrap();
    let world_now = emit_snapshots(&handle.world().read().unwrap()).unwrap();

    assert!(
        !same_geometry(&queue_after, &after_resume),
        "the next valid batch lands and moves the renderer"
    );
    assert_geometry(&after_resume, &world_now);
}
