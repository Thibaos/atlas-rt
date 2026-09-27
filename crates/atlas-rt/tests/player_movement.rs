mod common;

use std::time::{Duration, Instant};

use atlas_rt::render::camera::mirror_right;
use atlas_rt::sim::{Command, Handle, InputSample, PlayerState};
use atlas_rt::world::update::edit::{VoxelChange, VoxelEdit};
use glam::{IVec3, Quat, Vec2, Vec3};

use common::*;

const EPSILON: f32 = 1.0e-3;
const PARITY: f32 = 1.0e-5;
const WIDE: f32 = 1.0e-2;

const GRAVITY: f32 = 24.0;
const MOVE_SPEED: f32 = 4.0;
const DT: f32 = 1.0 / 30.0;
const CEILING: f32 = 2048.0 - 1.8;

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < EPSILON
}

fn strict(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < PARITY
}

fn wide(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < WIDE
}

/// The height after `ticks` ticks of free fall from rest.
fn fall(start: f32, ticks: u32) -> f32 {
    let n = ticks as f32;

    start - 0.5 * GRAVITY * DT * DT * n * (n + 1.0)
}

/// The height after `ticks` ticks of a jump launched at 8 units per second,
/// the launch tick included and gravity skipped on it.
fn arc(start: f32, ticks: u32) -> f32 {
    let n = ticks as f32;

    start + (8.0 * n - 0.5 * GRAVITY * DT * n * (n - 1.0)) * DT
}

fn clear(x: i32, y: i32, z: i32) -> VoxelEdit {
    VoxelEdit {
        position: IVec3::new(x, y, z),
        change: VoxelChange::Clear,
    }
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

/// A jump rising transition from before the buffer deadline, which no tick
/// may fire.
fn stale() -> InputSample {
    InputSample {
        movement: Vec2::ZERO,
        jump_edge: Some(
            Instant::now()
                .checked_sub(Duration::from_millis(400))
                .expect("the monotonic clock runs forward"),
        ),
    }
}

fn floor(x0: i32, x1: i32, z0: i32, z1: i32) -> Vec<VoxelEdit> {
    (x0..=x1)
        .flat_map(|x| (z0..=z1).map(move |z| set(x, 0, z, 1)))
        .collect()
}

fn open_floor() -> Vec<VoxelEdit> {
    floor(0, 7, 0, 4)
}

fn wall_room() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 7, 0, 4);

    edits.extend((0..=4).map(|z| set(5, 1, z, 1)));
    edits.extend((3..=6).map(|x| set(x, 1, 4, 1)));

    edits
}

fn floor_and_pillar() -> Vec<VoxelEdit> {
    let mut edits = floor(0, 4, 2, 2);

    edits.push(set(2, 3, 2, 1));

    edits
}

fn ledge() -> Vec<VoxelEdit> {
    vec![set(1, 4, 2, 1), set(7, 8, 2, 1)]
}

/// A floor cell under the spawn column, the two cells that bury the player,
/// and a ring at both buried heights, so no sideways escape exists.
fn cage() -> Vec<VoxelEdit> {
    vec![
        set(0, 2045, 0, 1),
        set(0, 2046, 0, 1),
        set(0, 2047, 0, 1),
        set(1, 2046, 0, 1),
        set(1, 2047, 0, 1),
        set(-1, 2046, 0, 1),
        set(-1, 2047, 0, 1),
        set(0, 2046, 1, 1),
        set(0, 2047, 1, 1),
        set(0, 2046, -1, 1),
        set(0, 2047, -1, 1),
    ]
}

/// Runs `ticks` frames of one sample against a fresh sim and reports the
/// pose the last tick ended on.
fn drive(scene: &[VoxelEdit], sample: InputSample, ticks: u32) -> PlayerState {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(scene));
    let mut last = wait_ready(&handle);

    for _ in 0..ticks {
        handle.frame(period(), sample);
        last = expect_tick(recv_push(&handle)).player;
    }

    last
}

/// Activates `scene` on a fresh sim and reports the handle and the pose it
/// spawned at.
fn ready(scene: &[VoxelEdit]) -> (Handle, PlayerState) {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(scene));
    let spawn = wait_ready(&handle);

    (handle, spawn)
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

/// A player twelve ticks into the fall after the pillar is cleared: feet at
/// 2.24, still a cell above the floor's ground band, with no edge buffered.
fn falling_player() -> Handle {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&floor_and_pillar()));
    let mut last = wait_ready(&handle);

    handle.command(Command::Edits(vec![clear(2, 3, 2)]));

    for _ in 0..12 {
        feed(&handle, period());
        last = expect_tick(recv_push(&handle)).player;
    }

    assert!(
        near(last.feet.y, 2.24),
        "the preamble must leave the player mid-fall, got {}",
        last.feet.y
    );
    assert!(!last.grounded, "the floor is still two cells down");

    handle
}

#[test]
fn a_buried_player_keeps_its_overlap_and_the_sweeps_still_move_it() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&cage()));

    let spawn = wait_ready(&handle);

    assert_eq!(spawn.feet.x, 0.5);
    assert_eq!(spawn.feet.z, 0.5);
    assert!(
        near(spawn.feet.y, CEILING),
        "the climb stops at the ceiling, feet.y {}",
        spawn.feet.y
    );
    assert!(spawn.grounded);

    feed(&handle, period());

    let sunk = expect_tick(recv_push(&handle)).player;

    assert_eq!(sunk.feet.x, 0.5, "no sideways input, no sideways exit");
    assert_eq!(sunk.feet.z, 0.5);
    assert!(
        near(sunk.feet.y, 2046.1733),
        "no exit is clear, so the first tick only falls: {}",
        sunk.feet.y
    );
    assert!(
        sunk.grounded,
        "the cell under the box still reports support"
    );

    handle.frame(period(), keys(Vec2::X));

    let shuffled = expect_tick(recv_push(&handle)).player;

    assert!(
        near(shuffled.feet.x, 0.633333),
        "the sweep skips the cells inside the box: {}",
        shuffled.feet.x
    );
    assert!(
        near(shuffled.feet.y, 2046.12),
        "the fall keeps running while buried: {}",
        shuffled.feet.y
    );

    handle.frame(period(), keys(Vec2::X));

    let at_wall = expect_tick(recv_push(&handle)).player;

    assert!(
        near(at_wall.feet.x, 0.7),
        "the ring stops x at its face: {}",
        at_wall.feet.x
    );
    assert!(
        near(at_wall.feet.y, 2046.04),
        "the face below is still a tick away: {}",
        at_wall.feet.y
    );

    for _ in 0..2 {
        handle.frame(period(), keys(Vec2::X));

        let settled = expect_tick(recv_push(&handle)).player;

        assert_eq!(
            settled.feet.y, 2046.0,
            "the fall lands the feet exactly on the cell below"
        );
        assert!(
            near(settled.feet.x, 0.7),
            "the wall still holds x: {}",
            settled.feet.x
        );
        assert_eq!(settled.feet.z, 0.5);
        assert!(settled.grounded);
    }
}

#[test]
fn gravity_falls_the_player_onto_the_floor_with_the_feet_exactly_on_it() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&floor_and_pillar()));
    wait_ready(&handle);
    handle.command(Command::Edits(vec![clear(2, 3, 2)]));

    for tick_number in 1..=16 {
        feed(&handle, period());

        let feet = expect_tick(recv_push(&handle)).player;

        assert_eq!(feet.feet.x, 2.5, "no horizontal input");
        assert_eq!(feet.feet.z, 2.5, "no horizontal input");

        if tick_number == 16 {
            assert_eq!(
                feet.feet.y, 1.0,
                "the sweep lands the feet exactly on the floor face"
            );
            assert!(feet.grounded, "the landing reports grounded");
        } else {
            assert!(
                near(feet.feet.y, fall(4.0, tick_number - 1)),
                "tick {tick_number}: feet.y {}",
                feet.feet.y
            );
        }

        if tick_number == 5 {
            assert!(
                !feet.grounded,
                "the pillar is gone and the floor is two cells down"
            );
        }
    }
}

#[test]
fn one_press_runs_one_arc_that_lands_back_on_the_pillar() {
    let scene = floor_and_pillar();
    let (handle, _) = ready(&scene);
    let players = press_and_run(&handle, Vec2::ZERO, 21);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;

        assert!(
            near(player.feet.y, arc(4.0, tick)),
            "tick {tick}: feet.y {}",
            player.feet.y
        );
        assert_eq!(player.feet.x, 2.5);
        assert_eq!(player.feet.z, 2.5);
    }

    assert!(
        players.last().expect("twenty one ticks").grounded,
        "the arc ends resting on the pillar"
    );

    for _ in 0..4 {
        feed(&handle, period());

        let held = expect_tick(recv_push(&handle)).player;

        assert_eq!(
            held.feet.y, 4.0,
            "a held key adds no second edge, so nothing launches again"
        );
        assert!(held.grounded);
    }
}

#[test]
fn the_same_scene_and_the_same_input_replay_the_same_arc() {
    let scene = floor_and_pillar();
    let (first, _) = ready(&scene);
    let (second, _) = ready(&scene);

    let one = press_and_run(&first, Vec2::new(0.5, 0.0), 21);
    let two = press_and_run(&second, Vec2::new(0.5, 0.0), 21);

    assert_eq!(one, two, "identical input must replay tick for tick");

    let last = one.last().expect("twenty one ticks");

    assert!(
        near(last.feet.x, 3.9),
        "the run walks the whole way, so it is not trivially empty: {}",
        last.feet.x
    );
    assert!(
        one.get(10).expect("twenty one ticks").feet.y > 5.0,
        "the jump still arcs above the spawn height"
    );
}

#[test]
fn a_three_tick_update_runs_three_ticks_of_arc_after_one_press() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&floor_and_pillar()));
    wait_ready(&handle);

    handle.frame(period().saturating_mul(3), pressed());

    let tick = expect_tick(recv_push(&handle));

    assert_eq!(tick.report.ticks, 3);
    assert!(
        near(tick.player.feet.y, 4.72),
        "three ticks of arc put the feet at {}",
        tick.player.feet.y
    );
}

#[test]
fn horizontal_resolves_first_so_the_ledge_lip_lifts_the_player_up() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&ledge()));

    let spawn = wait_ready(&handle);

    assert_eq!(
        spawn.feet,
        Vec3::new(4.5, 9.0, 2.5),
        "the spawn column is empty, so the fall begins at the roofline"
    );
    assert!(!spawn.grounded);

    for tick_number in 1..=16 {
        handle.frame(period(), keys(Vec2::new(-1.0, 0.0)));

        let player = expect_tick(recv_push(&handle)).player;

        assert!(
            near(player.feet.x, 4.5 - MOVE_SPEED * DT * tick_number as f32),
            "tick {tick_number}: feet.x {}",
            player.feet.x
        );
        assert!(
            near(player.feet.y, fall(9.0, tick_number)),
            "tick {tick_number}: feet.y {}",
            player.feet.y
        );
        assert!(!player.grounded, "the lip is still below the box");
    }

    handle.frame(period(), keys(Vec2::new(-1.0, 0.0)));

    let lip = expect_tick(recv_push(&handle)).player;

    assert_eq!(
        lip.feet.y, 5.0,
        "the x sweep cleared the lip, so the y sweep lands on top of it"
    );
    assert!(
        near(lip.feet.x, 4.5 - MOVE_SPEED * DT * 17.0),
        "the x move ran its full distance: {}",
        lip.feet.x
    );
    assert!(lip.grounded, "the feet rest on the lip");
}

#[test]
fn angled_movement_slides_along_the_wall_and_stops_at_the_corner() {
    let angle = keys(Vec2::ONE);

    let sliding = drive(&wall_room(), angle, 10);

    assert!(
        near(sliding.feet.x, 4.7),
        "the wall stops x at the face minus half the width: {}",
        sliding.feet.x
    );
    assert!(
        near(sliding.feet.z, 3.442809),
        "z keeps sliding along the wall: {}",
        sliding.feet.z
    );
    assert_eq!(sliding.feet.y, 1.0);
    assert!(sliding.grounded);

    let stopped = drive(&wall_room(), angle, 15);

    assert!(
        near(stopped.feet.x, 4.7),
        "x stays held against the wall: {}",
        stopped.feet.x
    );
    assert!(
        near(stopped.feet.z, 3.7),
        "the corner stops z at the face minus half the depth: {}",
        stopped.feet.z
    );
    assert!(stopped.grounded);
}

#[test]
fn the_lattice_ceiling_stops_the_rise_and_the_platform_catches_the_fall() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&[set(0, 2044, 0, 1)]));

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(0.5, 2045.0, 0.5),
            grounded: true,
        }
    );

    handle.frame(period(), pressed());

    let mut peak = 0.0_f32;

    for tick in 1..=8 {
        if tick > 1 {
            feed(&handle, period());
        }

        let player = expect_tick(recv_push(&handle)).player;

        peak = peak.max(player.feet.y);

        assert!(
            player.feet.y <= CEILING + EPSILON,
            "tick {tick} rose past the lattice ceiling: {}",
            player.feet.y
        );
    }

    assert!(
        near(peak, CEILING),
        "the rise stops at the lattice ceiling plane: {peak}"
    );

    let mut last = None;

    for _ in 0..10 {
        feed(&handle, period());
        last = Some(expect_tick(recv_push(&handle)).player);
    }

    let last = last.expect("ten ticks landed");

    assert_eq!(
        last.feet.y, 2045.0,
        "the fall lands exactly on the platform face"
    );
    assert!(last.grounded);
}

#[test]
fn the_lattice_side_is_an_invisible_wall() {
    let scene = [set(2047, 0, 2047, 1)];
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&scene));

    let spawn = wait_ready(&handle);

    assert_eq!(
        spawn,
        PlayerState {
            feet: Vec3::new(2047.5, 1.0, 2047.5),
            grounded: true,
        }
    );

    let mut last = spawn;

    for _ in 0..5 {
        handle.frame(period(), keys(Vec2::ONE));
        last = expect_tick(recv_push(&handle)).player;

        assert_eq!(last.feet.y, 1.0, "the floor cell still holds");
        assert!(last.grounded);
    }

    assert!(
        near(last.feet.x, 2047.7),
        "the invisible side stops x at the lattice edge: {}",
        last.feet.x
    );
    assert!(
        near(last.feet.z, 2047.7),
        "the invisible side stops z at the lattice edge: {}",
        last.feet.z
    );
}

#[test]
fn the_lattice_floor_is_an_invisible_floor_under_a_jump() {
    let (handle, spawn) = ready(&[]);

    assert_eq!(
        spawn,
        PlayerState {
            feet: Vec3::new(0.0, -2048.0, 0.0),
            grounded: true,
        }
    );

    let players = press_and_run(&handle, Vec2::ZERO, 21);

    for (index, player) in players.iter().enumerate() {
        let tick = index as u32 + 1;

        assert!(
            wide(player.feet.y, arc(-2048.0, tick)),
            "tick {tick}: feet.y {}",
            player.feet.y
        );
    }

    feed(&handle, period());

    let landed = expect_tick(recv_push(&handle)).player;

    assert_eq!(
        landed.feet.y, -2048.0,
        "the sweep snaps the feet onto the lattice floor plane"
    );
    assert!(landed.grounded);
}

#[test]
fn depenetration_pushes_up_first_and_retries_every_tick() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&open_floor()));
    wait_ready(&handle);
    handle.command(Command::Edits(vec![set(4, 1, 2, 1)]));

    feed(&handle, period());

    let buried = expect_tick(recv_push(&handle)).player;

    assert_eq!(
        buried.feet,
        Vec3::new(4.0, 1.0, 2.5),
        "the cell commits now"
    );

    for _ in 0..4 {
        feed(&handle, period());

        let lifted = expect_tick(recv_push(&handle)).player;

        assert_eq!(
            lifted.feet,
            Vec3::new(4.0, 2.0, 2.5),
            "up beats the shorter sideways exits, every tick"
        );
        assert!(lifted.grounded, "the lifted box rests on the new cell");
    }
}

#[test]
fn a_jump_pressed_against_a_buried_player_still_launches() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&open_floor()));
    wait_ready(&handle);
    handle.command(Command::Edits(vec![set(4, 1, 2, 1)]));

    feed(&handle, period());
    expect_tick(recv_push(&handle));

    handle.frame(period(), pressed());

    let launched = expect_tick(recv_push(&handle)).player;

    assert!(
        near(launched.feet.y, 2.26667),
        "the depenetration ran before the launch and left the edge alone: {}",
        launched.feet.y
    );
    assert_eq!(launched.feet.x, 4.0);
    assert_eq!(launched.feet.z, 2.5);
}

#[test]
fn a_jump_edge_held_through_the_fall_fires_on_the_grounded_tick() {
    let handle = falling_player();

    handle.frame(period(), pressed());

    let held = expect_tick(recv_push(&handle)).player;

    assert!(
        near(held.feet.y, 1.92),
        "the tick that took the edge starts airborne, so it only falls: {}",
        held.feet.y
    );

    feed(&handle, period());

    let launched = expect_tick(recv_push(&handle)).player;

    assert!(
        near(launched.feet.y, 2.18667),
        "the next tick starts grounded and fires the buffered edge: {}",
        launched.feet.y
    );
}

#[test]
fn a_jump_edge_past_the_buffer_deadline_never_launches() {
    let handle = falling_player();
    let stale = stale();

    handle.frame(period(), stale);

    let held = expect_tick(recv_push(&handle)).player;

    assert!(near(held.feet.y, 1.92), "feet.y {}", held.feet.y);

    feed(&handle, period());

    let fallen = expect_tick(recv_push(&handle)).player;

    assert!(
        near(fallen.feet.y, 1.57333),
        "a stale edge is dropped unread instead of launching: {}",
        fallen.feet.y
    );

    let mut last = fallen;

    for _ in 0..2 {
        feed(&handle, period());
        last = expect_tick(recv_push(&handle)).player;
    }

    assert_eq!(last.feet.y, 1.0, "the player lands on the floor");
    assert!(last.grounded);
}

#[test]
fn the_newest_edge_of_an_update_survives_the_sticky_merge() {
    let handle = falling_player();
    let stale = stale();

    handle.frame(part(9, 10), stale);
    assert_silent(&handle);

    handle.frame(part(2, 10), pressed());

    let held = expect_tick(recv_push(&handle)).player;

    assert!(near(held.feet.y, 1.92), "feet.y {}", held.feet.y);

    feed(&handle, period());

    let launched = expect_tick(recv_push(&handle)).player;

    assert!(
        near(launched.feet.y, 2.18667),
        "the fresh edge replaced the stale one and fired: {}",
        launched.feet.y
    );
}

#[test]
fn pausing_discards_the_pending_jump_edge() {
    let handle = falling_player();

    handle.frame(part(9, 10), pressed());
    handle.set_paused(true);
    handle.frame(part(2, 10), InputSample::default());
    handle.set_paused(false);

    feed(&handle, period());

    let held = expect_tick(recv_push(&handle)).player;

    assert!(near(held.feet.y, 1.92), "feet.y {}", held.feet.y);

    feed(&handle, period());

    let fallen = expect_tick(recv_push(&handle)).player;

    assert!(
        near(fallen.feet.y, 1.57333),
        "the paused edge is gone, so the grounded tick only falls: {}",
        fallen.feet.y
    );
}

#[test]
fn a_pause_ignores_a_jump_edge_pressed_while_it_holds() {
    let handle = falling_player();

    handle.set_paused(true);
    handle.frame(period(), pressed());
    handle.set_paused(false);

    feed(&handle, period());

    let held = expect_tick(recv_push(&handle)).player;

    assert!(near(held.feet.y, 1.92), "feet.y {}", held.feet.y);

    feed(&handle, period());

    let fallen = expect_tick(recv_push(&handle)).player;

    assert!(
        near(fallen.feet.y, 1.57333),
        "the paused press never reached the buffer, so the grounded tick only falls: {}",
        fallen.feet.y
    );
}

#[test]
fn activation_discards_the_pending_jump_edge() {
    let (_world, handle) = spawn_sim();

    handle.activate(activation_of(&floor_and_pillar()));
    wait_ready(&handle);

    handle.frame(part(9, 10), pressed());
    handle.activate(activation_of(&floor_and_pillar()));

    assert_eq!(
        wait_ready(&handle),
        PlayerState {
            feet: Vec3::new(2.5, 4.0, 2.5),
            grounded: true,
        }
    );

    feed(&handle, period());

    let tick = expect_tick(recv_push(&handle)).player;

    assert_eq!(
        tick.feet.y, 4.0,
        "a surviving edge would read 4.26667 here, so the respawn only fell"
    );
}

#[test]
fn movement_is_scaled_and_clamped_to_the_profile_move_speed() {
    let east = drive(&open_floor(), keys(Vec2::X), 10);

    assert!(
        near(east.feet.x, 5.333333),
        "one key runs the whole move speed: {}",
        east.feet.x
    );
    assert!(near(east.feet.z, 2.5));
    assert_eq!(east.feet.y, 1.0);
    assert!(east.grounded);

    let diagonal = drive(&open_floor(), keys(Vec2::ONE), 10);

    assert!(
        near(diagonal.feet.x, 4.942809),
        "two keys normalize instead of adding up: {}",
        diagonal.feet.x
    );
    assert!(
        near(diagonal.feet.z, 3.442809),
        "the diagonal splits the speed evenly: {}",
        diagonal.feet.z
    );

    let half = drive(&open_floor(), keys(Vec2::new(0.5, 0.0)), 10);

    assert!(
        near(half.feet.x, 4.666667),
        "a partial key scales without normalizing: {}",
        half.feet.x
    );
}

#[test]
fn from_local_matches_the_axes_the_drawn_frame_uses() {
    for yaw in [-1.2_f32, -0.4, 0.0, 0.35, 2.9] {
        let forward = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
        let [right, _, ahead] = mirror_right([forward.cross(Vec3::Y), Vec3::Y, forward]);

        let strafe = InputSample::from_local(yaw, 1.0, 0.0, None).movement;
        let push = InputSample::from_local(yaw, 0.0, 1.0, None).movement;

        assert!(
            strict(strafe.x, right.x) && strict(strafe.y, right.z),
            "yaw {yaw}: strafe {strafe:?} against the mirrored right {right:?}"
        );
        assert!(
            strict(push.x, ahead.x) && strict(push.y, ahead.z),
            "yaw {yaw}: forward {push:?} against the view forward {ahead:?}"
        );
    }
}

#[test]
fn a_local_key_sample_drives_the_player_along_the_view() {
    let strafe = drive(
        &open_floor(),
        InputSample::from_local(0.0, 1.0, 0.0, None),
        10,
    );

    assert!(
        near(strafe.feet.x, 2.666667),
        "strafe right at yaw zero runs to world -x: {}",
        strafe.feet.x
    );
    assert!(near(strafe.feet.z, 2.5));

    let ahead = drive(
        &open_floor(),
        InputSample::from_local(0.0, 0.0, 1.0, None),
        10,
    );

    assert!(near(ahead.feet.x, 4.0));
    assert!(
        near(ahead.feet.z, 1.166667),
        "forward at yaw zero runs to world -z: {}",
        ahead.feet.z
    );
}
