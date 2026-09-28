mod common;

use std::time::Instant;

use atlas_rt::sim::{Handle, InputSample, PlayerProfile, PlayerState};
use atlas_rt::world::update::edit::VoxelEdit;
use glam::{Vec2, Vec3};

use common::*;

const EPSILON: f32 = 1.0e-3;
const DT: f32 = 1.0 / 30.0;

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < EPSILON
}

/// The height after `ticks` ticks of a jump launched at `launch` units per
/// second, the launch tick included and `gravity` skipped on it.
fn arc(start: f32, ticks: u32, launch: f32, gravity: f32) -> f32 {
    let n = ticks as f32;

    start + (launch * n - 0.5 * gravity * DT * n * (n - 1.0)) * DT
}

fn pressed() -> InputSample {
    InputSample {
        movement: Vec2::ZERO,
        jump_edge: Some(Instant::now()),
    }
}

fn keys(movement: Vec2) -> InputSample {
    InputSample {
        movement,
        jump_edge: None,
    }
}

/// A full depth floor across `x0..=x1` at y = 0.
fn floor(x0: i32, x1: i32) -> Vec<VoxelEdit> {
    (x0..=x1)
        .flat_map(|x| (0..=4).map(move |z| set(x, 0, z, 1)))
        .collect()
}

/// A full depth block across `x0..=x1` and `y0..=y1`.
fn fill(x0: i32, x1: i32, y0: i32, y1: i32) -> Vec<VoxelEdit> {
    (x0..=x1)
        .flat_map(|x| (y0..=y1).flat_map(move |y| (0..=4).map(move |z| set(x, y, z, 1))))
        .collect()
}

fn staircase() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 27);

    edits.extend(fill(16, 17, 1, 1));
    edits.extend(fill(18, 20, 1, 3));
    edits.extend(fill(21, 23, 1, 6));

    edits
}

/// A staircase for the default profile: a one rise, a two rise, and a five
/// rise wall, all three within the reach of the default stride.
fn default_staircase() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 145);

    edits.extend(fill(93, 112, 1, 1));
    edits.extend(fill(113, 142, 1, 3));
    edits.extend(fill(143, 145, 1, 8));

    edits
}

/// A walkway with a one cell crack at x = 16 and a seven cell trench at
/// x = 18..=24, floored one cell down so the fall lands and climbs back out.
fn walkway() -> Vec<VoxelEdit> {
    let mut edits: Vec<VoxelEdit> = (0..=25)
        .filter(|x| *x != 16 && !(18..=24).contains(x))
        .flat_map(|x| (0..=4).map(move |z| set(x, 0, z, 1)))
        .collect();

    edits.extend(fill(18, 24, -1, -1));

    edits
}

/// A plateau the walk leaves at x = 18 over the floor below.
fn ledge_walkway() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 23);

    edits.extend(fill(0, 17, 1, 1));

    edits
}

/// A step at x = 14 under a ceiling cell over the rise path at x = 13.
fn rise_under_ceiling() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 23);

    edits.extend(fill(14, 14, 1, 1));
    edits.extend(fill(13, 13, 3, 3));

    edits
}

/// The same step with the headroom open.
fn open_step() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 23);

    edits.extend(fill(14, 14, 1, 1));

    edits
}

/// A two cell step at x = 14..=15 under a ceiling cell over the destination.
fn destination_under_ceiling() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 23);

    edits.extend(fill(14, 15, 1, 1));
    edits.extend(fill(15, 15, 3, 3));

    edits
}

/// The same two cell step with the destination clear.
fn wide_step() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 23);

    edits.extend(fill(14, 15, 1, 1));

    edits
}

/// A four cell pillar standing at the end of the floor at x = 72.
fn single_step() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 72);

    edits.extend(fill(72, 72, 1, 4));

    edits
}

/// A floor with a two cell slot cut across it at x = 18..=19 over z = 2
/// only: one cell wide along z, closed on both long sides.
fn one_cell_slot() -> Vec<VoxelEdit> {
    (-70..=93)
        .flat_map(|x| (0..=4).map(move |z| (x, z)))
        .filter(|&(x, z)| !((x == 18 || x == 19) && z == 2))
        .map(|(x, z)| set(x, 0, z, 1))
        .collect()
}

/// A step at x = 14 whose column ends there, so the destination a full
/// stride would reach sits over open air.
fn unrested_destination() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 13);

    edits.extend(fill(14, 14, 1, 1));

    edits
}

/// The same step with one cell filling the destination column.
fn rested_destination() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 13);

    edits.extend(fill(14, 15, 1, 1));

    edits
}

fn fast_profile() -> PlayerProfile {
    PlayerProfile::new(0.6, 0.6, 1.8, 1.62, 45.0, 24.0, 8.0, 2.0, 0.15, 30.0)
        .expect("the fast profile is valid")
}

fn sprint_profile() -> PlayerProfile {
    PlayerProfile::new(0.6, 0.6, 1.8, 1.62, 60.0, 24.0, 8.0, 2.0, 0.15, 30.0)
        .expect("the sprint profile is valid")
}

fn stepped_profile(step_height: u32) -> PlayerProfile {
    PlayerProfile::new(
        0.6,
        0.6,
        1.8,
        1.62,
        4.0,
        24.0,
        8.0,
        step_height as f32,
        0.15,
        30.0,
    )
    .expect("the stepping profile is valid")
}

/// Activates `scene` on a fresh sim running `profile` and reports the handle
/// and the pose it spawned at.
fn ready(scene: &[VoxelEdit], profile: PlayerProfile) -> (Handle, PlayerState) {
    let (_world, handle) = spawn_sim_with(profile);

    handle.activate(activation_of(scene));
    let spawn = wait_ready(&handle);

    (handle, spawn)
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

#[test]
fn the_staircase_climbs_one_rise_and_two_rises_and_stops_at_the_wall() {
    let (handle, spawn) = ready(&default_staircase(), PlayerProfile::default());

    assert_eq!(spawn.feet, Vec3::new(73.0, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 60);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;

        assert!(
            player.grounded,
            "tick {tick} lost its support at y {}",
            player.feet.y
        );
    }

    for player in players.iter().take(12) {
        assert_eq!(player.feet.y, 1.0, "the walk starts flat");
    }

    let rise = players.get(12).expect("thirteen ticks");

    assert!(
        near(rise.feet.x, 90.33333),
        "the contact carries the stride onto the step: {}",
        rise.feet.x
    );
    assert_eq!(rise.feet.y, 2.0, "the first step rises one");

    for player in players.iter().skip(13).take(14) {
        assert_eq!(player.feet.y, 2.0, "the walk holds the first step");
    }

    let twice = players.get(27).expect("twenty eight ticks");

    assert!(
        near(twice.feet.x, 110.33333),
        "the two voxel rise climbs in one move: {}",
        twice.feet.x
    );
    assert_eq!(twice.feet.y, 4.0, "the second step rises two");

    for player in players.iter().skip(28).take(22) {
        assert_eq!(player.feet.y, 4.0, "the walk holds the platform");
    }

    let wall = players.get(50).expect("fifty one ticks");

    assert!(
        near(wall.feet.x, 140.0),
        "a five voxel stack stops the walk as a wall: {}",
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
}

#[test]
fn step_height_one_climbs_one_rise_and_takes_the_two_rise_as_a_wall() {
    let (handle, spawn) = ready(&staircase(), stepped_profile(1));

    assert_eq!(spawn.feet, Vec3::new(14.0, 1.0, 2.5));

    let players = walk(&handle, keys(Vec2::X), 60);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;

        assert!(
            player.grounded,
            "tick {tick} lost its support at y {}",
            player.feet.y
        );
    }

    let rise = players.get(12).expect("thirteen ticks");

    assert!(
        near(rise.feet.x, 15.73333),
        "the one voxel step still climbs: {}",
        rise.feet.x
    );
    assert_eq!(rise.feet.y, 2.0, "one voxel is within the step height");

    let wall = players.get(27).expect("twenty eight ticks");

    assert!(
        near(wall.feet.x, 17.7),
        "the two voxel rise stops the walk at the face: {}",
        wall.feet.x
    );
    assert_eq!(wall.feet.y, 2.0, "rise two exceeds the step height");

    for player in players.iter().skip(27) {
        assert!(
            near(player.feet.x, 17.7),
            "the walk stays against the rise: {}",
            player.feet.x
        );
        assert_eq!(player.feet.y, 2.0, "the wall is never climbed");
    }
}

#[test]
fn step_height_zero_disables_stepping_but_not_jumping() {
    let (handle, spawn) = ready(&staircase(), stepped_profile(0));

    assert_eq!(spawn.feet, Vec3::new(14.0, 1.0, 2.5));

    let players = walk(&handle, keys(Vec2::X), 20);

    for player in players.iter().take(12) {
        assert_eq!(player.feet.y, 1.0, "the walk starts flat");
    }

    for (index, player) in players.iter().skip(12).enumerate() {
        let tick = index as u32 + 13;

        assert!(
            near(player.feet.x, 15.7),
            "tick {tick} holds the step face: {}",
            player.feet.x
        );
        assert_eq!(
            player.feet.y, 1.0,
            "tick {tick} never rises without a step height"
        );
        assert!(player.grounded, "tick {tick} keeps the floor");
    }

    handle.frame(period(), pressed());

    let mut jumped = vec![expect_tick(recv_push(&handle)).player];

    for _ in 0..21 {
        handle.frame(period(), InputSample::default());
        jumped.push(expect_tick(recv_push(&handle)).player);
    }

    for (index, player) in jumped.iter().take(20).enumerate() {
        let tick = index as u32 + 1;

        assert!(
            near(player.feet.y, arc(1.0, tick, 8.0, 24.0)),
            "jump tick {tick}: feet.y {}",
            player.feet.y
        );
        assert!(!player.grounded, "jump tick {tick} flies");
    }

    let landed = jumped.get(20).expect("twenty one ticks");

    assert!(
        near(landed.feet.y, 1.0),
        "the jump lands back on the floor: {}",
        landed.feet.y
    );
    assert!(landed.grounded, "the landing reports grounded");

    let held = jumped.last().expect("twenty two ticks");

    assert_eq!(held.feet.y, 1.0, "the floor holds after the landing");
    assert!(held.grounded, "the landed tick stays grounded");
}

#[test]
fn a_one_cell_crack_holds_the_walk_and_a_seven_cell_trench_swallows() {
    let (handle, spawn) = ready(&walkway(), PlayerProfile::default());

    assert_eq!(spawn.feet, Vec3::new(13.0, 1.0, 2.5));
    assert!(spawn.grounded, "the walkway floor holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 12);

    for (index, player) in players.iter().take(5).enumerate() {
        let tick = index as u32 + 1;

        assert_eq!(player.feet.y, 1.0, "tick {tick} of the crack stays level");
        assert!(player.grounded, "tick {tick} of the crack holds");
    }

    let leaving = players.get(5).expect("six ticks");

    assert!(
        near(leaving.feet.y, 0.7777778),
        "the trench starts the fall: {}",
        leaving.feet.y
    );
    assert!(!leaving.grounded, "a seven cell gap is not ground");

    let falling = players.get(6).expect("seven ticks");

    assert!(!falling.grounded, "the fall runs before the trench floor");

    let trench = players.get(7).expect("eight ticks");

    assert_eq!(trench.feet.y, 0.0, "the trench floor catches the fall");
    assert!(trench.grounded, "the trench floor is ground");

    let climbed = players.get(8).expect("nine ticks");

    assert!(
        near(climbed.feet.x, 23.33333),
        "the climb out carries the stride: {}",
        climbed.feet.x
    );
    assert_eq!(climbed.feet.y, 1.0, "the walkway height rises in one step");
    assert!(climbed.grounded, "the climb holds");

    for player in players.iter().skip(9) {
        assert_eq!(player.feet.y, 1.0, "the climb holds its height");
        assert!(player.grounded, "the climbed walkway stays ground");
    }

    let last = players.last().expect("twelve ticks");

    assert!(
        near(last.feet.x, 27.33333),
        "the walk resumes after the climb: {}",
        last.feet.x
    );
}

#[test]
fn walking_off_a_ledge_falls_and_reports_ungrounded() {
    let (handle, spawn) = ready(&ledge_walkway(), PlayerProfile::default());

    assert_eq!(spawn.feet, Vec3::new(12.0, 2.0, 2.5));
    assert!(spawn.grounded, "the plateau holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 11);

    for player in players.iter().take(6) {
        assert_eq!(player.feet.y, 2.0, "the plateau holds the walk");
        assert!(player.grounded, "the plateau is ground");
    }

    let leaving = players.get(6).expect("seven ticks");

    assert!(
        near(leaving.feet.y, 1.7777778),
        "the ledge drop starts the fall: {}",
        leaving.feet.y
    );
    assert!(!leaving.grounded, "the ledge is gone under the feet");

    let falling = players.get(7).expect("eight ticks");

    assert!(!falling.grounded, "the fall runs to the floor");

    let landed = players.get(8).expect("nine ticks");

    assert_eq!(landed.feet.y, 1.0, "the floor catches the fall");
    assert!(landed.grounded, "the landing reports grounded");

    for player in players.iter().skip(9) {
        assert_eq!(player.feet.y, 1.0, "the landing holds");
        assert!(player.grounded, "the floor stays ground");
    }

    let last = players.last().expect("eleven ticks");

    assert!(
        near(last.feet.x, 26.66667),
        "the walk resumes on the floor: {}",
        last.feet.x
    );
}

#[test]
fn a_ceiling_over_the_head_refuses_the_step_without_moving() {
    let (handle, spawn) = ready(&rise_under_ceiling(), fast_profile());

    assert_eq!(spawn.feet, Vec3::new(12.0, 1.0, 2.5));

    let players = walk(&handle, keys(Vec2::X), 12);

    let first = players.first().expect("one tick");

    assert!(
        near(first.feet.x, 13.5),
        "the first stride runs its full distance: {}",
        first.feet.x
    );
    assert_eq!(first.feet.y, 1.0, "the walk starts flat");
    assert!(first.grounded, "the floor holds");

    for player in players.iter().skip(1) {
        assert!(
            near(player.feet.x, 13.7),
            "the refused step holds the face: {}",
            player.feet.x
        );
        assert_eq!(player.feet.y, 1.0, "the refused step never rises");
        assert!(player.grounded, "the floor still holds");
    }
}

#[test]
fn the_same_step_under_an_open_ceiling_rises() {
    let (handle, spawn) = ready(&open_step(), fast_profile());

    assert_eq!(spawn.feet, Vec3::new(12.0, 1.0, 2.5));

    let players = walk(&handle, keys(Vec2::X), 2);

    let first = players.first().expect("one tick");

    assert!(
        near(first.feet.x, 13.5),
        "the first stride runs its full distance: {}",
        first.feet.x
    );
    assert_eq!(first.feet.y, 1.0, "the walk starts flat");

    let risen = players.get(1).expect("two ticks");

    assert!(
        near(risen.feet.x, 15.0),
        "the step carries the stride: {}",
        risen.feet.x
    );
    assert_eq!(risen.feet.y, 2.0, "the open headroom lets the rise through");
    assert!(risen.grounded, "the step top holds");
}

#[test]
fn a_ceiling_over_the_destination_refuses_the_step_and_never_bobs() {
    let (handle, spawn) = ready(&destination_under_ceiling(), fast_profile());

    assert_eq!(spawn.feet, Vec3::new(12.0, 1.0, 2.5));

    let players = walk(&handle, keys(Vec2::X), 25);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;
        let held = if tick == 1 { 13.5 } else { 13.7 };

        assert!(
            near(player.feet.x, held),
            "tick {tick} never crosses: {}",
            player.feet.x
        );
        assert_eq!(player.feet.y, 1.0, "tick {tick} never rises to bob");
        assert!(player.grounded, "tick {tick} keeps the floor");
    }
}

#[test]
fn the_same_step_without_a_destination_ceiling_crosses() {
    let (handle, spawn) = ready(&wide_step(), fast_profile());

    assert_eq!(spawn.feet, Vec3::new(12.0, 1.0, 2.5));

    let players = walk(&handle, keys(Vec2::X), 2);

    let first = players.first().expect("one tick");

    assert!(
        near(first.feet.x, 13.5),
        "the first stride runs its full distance: {}",
        first.feet.x
    );
    assert_eq!(first.feet.y, 1.0, "the walk starts flat");

    let crossed = players.get(1).expect("two ticks");

    assert!(
        near(crossed.feet.x, 15.0),
        "the clear destination lets the stride cross: {}",
        crossed.feet.x
    );
    assert_eq!(crossed.feet.y, 2.0, "the step rises");
    assert!(crossed.grounded, "the step lands on the platform");
}

#[test]
fn an_airborne_contact_never_starts_a_step() {
    let (handle, spawn) = ready(&single_step(), PlayerProfile::default());

    assert_eq!(spawn.feet, Vec3::new(36.5, 1.0, 2.5));

    let players = press_and_run(&handle, Vec2::X, 26);

    for (index, player) in players.iter().take(24).enumerate() {
        let tick = index as u32 + 1;

        assert!(
            near(player.feet.y, arc(1.0, tick, 80.0, 200.0)),
            "tick {tick} follows the arc: {}",
            player.feet.y
        );
        assert!(!player.grounded, "tick {tick} flies");
    }

    let contact = players.get(24).expect("twenty five ticks");

    assert!(
        near(contact.feet.x, 69.0),
        "the airborne contact stops at the face: {}",
        contact.feet.x
    );
    assert!(
        near(contact.feet.y, 1.0),
        "the airborne contact lands instead of stepping: {}",
        contact.feet.y
    );
    assert!(contact.grounded, "the landing reports grounded");

    let stepped = players.get(25).expect("twenty six ticks");

    assert!(
        near(stepped.feet.x, 70.33333),
        "the grounded contact steps four: {}",
        stepped.feet.x
    );
    assert_eq!(stepped.feet.y, 5.0, "the step climbs onto the pillar");
    assert!(stepped.grounded, "the pillar top holds");
}

#[test]
fn the_walk_holds_over_a_one_cell_wide_slot() {
    let (handle, spawn) = ready(&one_cell_slot(), PlayerProfile::default());

    assert_eq!(spawn.feet, Vec3::new(12.0, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 62);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;

        assert_eq!(player.feet.y, 1.0, "tick {tick} stays level over the slot");
        assert!(
            player.grounded,
            "tick {tick} spans the slot one cell wide in z"
        );
    }

    let across = players.get(52).expect("fifty three ticks");

    assert!(
        near(across.feet.x, 82.66667),
        "the slot never stalls the stride: {}",
        across.feet.x
    );
}

#[test]
fn a_destination_with_nothing_to_rest_on_refuses_the_whole_step() {
    let (handle, spawn) = ready(&unrested_destination(), sprint_profile());

    assert_eq!(spawn.feet, Vec3::new(7.5, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 6);

    for player in players.iter().take(3) {
        assert_eq!(player.feet.y, 1.0, "the walk runs flat to the step");
        assert!(player.grounded, "the floor holds the stride");
    }

    let approach = players.get(2).expect("three ticks");

    assert!(
        near(approach.feet.x, 13.5),
        "the stride stops short of the rise: {}",
        approach.feet.x
    );

    for (index, player) in players.iter().skip(3).enumerate() {
        let tick = index as u32 + 4;

        assert!(
            near(player.feet.x, 13.7),
            "tick {tick} never crosses: {}",
            player.feet.x
        );
        assert_eq!(
            player.feet.y, 1.0,
            "tick {tick} refuses instead of rising over air"
        );
        assert!(player.grounded, "tick {tick} keeps the floor");
    }
}

#[test]
fn the_same_step_commits_when_the_destination_rests() {
    let (handle, spawn) = ready(&rested_destination(), sprint_profile());

    assert_eq!(spawn.feet, Vec3::new(8.0, 1.0, 2.5));
    assert!(spawn.grounded, "the floor holds the spawn");

    let players = walk(&handle, keys(Vec2::X), 4);

    for player in players.iter().take(2) {
        assert_eq!(player.feet.y, 1.0, "the walk runs flat to the step");
    }

    let risen = players.get(2).expect("three ticks");

    assert!(
        near(risen.feet.x, 14.0),
        "the step carries the stride: {}",
        risen.feet.x
    );
    assert_eq!(risen.feet.y, 2.0, "the destination cell takes the rise");
    assert!(risen.grounded, "the destination holds");

    let held = players.get(3).expect("four ticks");

    assert!(
        near(held.feet.x, 16.0),
        "the walk resumes past the step: {}",
        held.feet.x
    );
    assert_eq!(held.feet.y, 2.0, "the step top holds");
    assert!(held.grounded, "the destination cell stays under the feet");
}
