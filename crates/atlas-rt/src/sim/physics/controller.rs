use std::time::Instant;

use glam::{IVec3, Vec2, Vec3};

use crate::sim::input::PlayerState;
use crate::sim::profile::PlayerProfile;

use super::contact::{self, Axis};
use super::field::Field;

/// The vertical velocity and the jump edge waiting for a grounded tick. The
/// edge dies at its buffer deadline, so only one press can ever launch.
#[derive(Clone, Copy, Debug)]
pub(in crate::sim) struct Controller {
    velocity_y: f32,
    pending_jump: Option<Instant>,
}

impl Controller {
    pub(in crate::sim) const fn new() -> Self {
        Self {
            velocity_y: 0.0,
            pending_jump: None,
        }
    }

    /// The activation reset: a respawned player falls from rest with no edge.
    pub(in crate::sim) const fn reset(&mut self) {
        self.velocity_y = 0.0;
        self.pending_jump = None;
    }

    /// The pause drop: the buffered edge goes with the buffered input, while
    /// the velocity carries through the freeze.
    pub(in crate::sim) const fn discard_jump(&mut self) {
        self.pending_jump = None;
    }

    /// Sticks a rising transition into the buffer, so several transitions in
    /// one update collapse to the newest.
    pub(in crate::sim) fn buffer_jump(&mut self, edge: Instant) {
        self.pending_jump = Some(self.pending_jump.map_or(edge, |held| held.max(edge)));
    }

    /// Resolves one tick: depenetrate, launch or fall, sweep x and z with a
    /// step on grounded contact, then hold or sweep y, and report the
    /// grounded state the sweeps left behind. The y hold rounds the feet
    /// onto the face, the same snap a landing sweep performs.
    pub(in crate::sim) fn advance(
        &mut self,
        field: &Field,
        player: &mut PlayerState,
        profile: PlayerProfile,
        movement: Vec2,
        now: Instant,
    ) {
        let dt = 1.0 / profile.tick_rate as f32;
        let fall = profile.gravity * dt;
        let mut feet = escape(field, player.feet, profile);

        let grounded = contact::grounded(field, feet, profile);

        if !self.launch(profile, grounded, now) {
            self.velocity_y -= fall;
        }

        let (dx, dz) = stride(movement, profile, dt);

        feet = sweep_and_step(field, feet, profile, Axis::X, dx, grounded);
        feet = sweep_and_step(field, feet, profile, Axis::Z, dz, grounded);

        let dy = self.velocity_y * dt;

        if dy < 0.0 && contact::grounded(field, feet, profile) {
            self.velocity_y = 0.0;
            feet.y = feet.y.round();
        } else {
            let (moved, landed) = contact::sweep(field, feet, profile, Axis::Y, dy);

            feet = moved;

            if landed.is_some() {
                self.velocity_y = 0.0;
            }
        }

        player.feet = feet;
        player.grounded = contact::grounded(field, feet, profile);
    }

    /// Consumes the buffered edge on the first grounded tick that runs it
    /// inside the buffer deadline. A tick that is still airborne holds it for
    /// the landing, and one past the deadline drops it unread.
    fn launch(&mut self, profile: PlayerProfile, grounded: bool, now: Instant) -> bool {
        let Some(edge) = self.pending_jump else {
            return false;
        };

        if now.saturating_duration_since(edge) > profile.jump_buffer {
            self.pending_jump = None;

            return false;
        }

        if !grounded {
            return false;
        }

        self.pending_jump = None;
        self.velocity_y = profile.jump_velocity;

        true
    }
}

/// The world-space x and z a frame's key state asks for: normalized to the
/// unit disc, then scaled by the profile move speed over one tick.
fn stride(movement: Vec2, profile: PlayerProfile, dt: f32) -> (f32, f32) {
    let clamped = if movement.length_squared() > 1.0 {
        movement.normalize()
    } else {
        movement
    };

    let scale = profile.move_speed * dt;

    (clamped.x * scale, clamped.y * scale)
}

/// Sweeps `axis` by `delta`, then climbs the contact when the tick started
/// grounded: the raised sweep when both refusal boxes stay clear, the sweep
/// alone when any of them refuses.
fn sweep_and_step(
    field: &Field,
    feet: Vec3,
    profile: PlayerProfile,
    axis: Axis,
    delta: f32,
    grounded: bool,
) -> Vec3 {
    let (swept, contact) = contact::sweep(field, feet, profile, axis, delta);

    if !grounded {
        return swept;
    }

    let Some(cell) = contact else {
        return swept;
    };

    step(field, feet, profile, axis, delta, cell).unwrap_or(swept)
}

/// The whole step: refused outright, never committed and cleaned up, when
/// the rise box or the destination body box blocks, or when the raised
/// sweep does not land resting. The rise box is the footprint as tall as
/// this step's actual rise on the head at the current position, and the
/// destination box is the full body at the advanced feet.
fn step(
    field: &Field,
    feet: Vec3,
    profile: PlayerProfile,
    axis: Axis,
    delta: f32,
    cell: IVec3,
) -> Option<Vec3> {
    let rise = contact::step_rise(field, feet, profile, cell)?;
    let (min, max) = contact::bounds(feet, profile);
    let rise_min = Vec3::new(min.x, max.y, min.z);
    let rise_max = Vec3::new(max.x, max.y + rise, max.z);

    if contact::blocked(field, rise_min, rise_max) {
        return None;
    }

    let raised = Axis::Y.set(feet, feet.y + rise);
    let (dest_min, dest_max) = contact::bounds(raised, profile);
    let dest_min = axis.set(dest_min, axis.get(dest_min) + delta);
    let dest_max = axis.set(dest_max, axis.get(dest_max) + delta);

    if contact::blocked(field, dest_min, dest_max) {
        return None;
    }

    let (advanced, _) = contact::sweep(field, raised, profile, axis, delta);

    contact::grounded(field, advanced, profile).then_some(advanced)
}

/// The nearest clear position the overlapping cells allow, up first and the
/// five remaining exits ordered by distance. No clear exit keeps the overlap.
fn escape(field: &Field, feet: Vec3, profile: PlayerProfile) -> Vec3 {
    let hits: Vec<IVec3> = contact::overlapping(field, feet, profile).collect();

    if hits.is_empty() {
        return feet;
    }

    let (min, max) = contact::bounds(feet, profile);
    let up = exit(Axis::Y, true, &hits, min.y, max.y);
    let mut sides = [
        exit(Axis::X, true, &hits, min.x, max.x),
        exit(Axis::X, false, &hits, min.x, max.x),
        exit(Axis::Z, true, &hits, min.z, max.z),
        exit(Axis::Z, false, &hits, min.z, max.z),
        exit(Axis::Y, false, &hits, min.y, max.y),
    ];

    sides.sort_by(|a, b| a.distance.total_cmp(&b.distance));

    for candidate in std::iter::once(up).chain(sides) {
        let moved = candidate.apply(feet);

        if contact::overlapping(field, moved, profile).next().is_none() {
            return moved;
        }
    }

    feet
}

/// One way out of a set of overlapping cells: the distance that lifts the box
/// clear of them along one axis, in one direction.
struct Exit {
    axis: Axis,
    positive: bool,
    distance: f32,
}

impl Exit {
    const fn apply(self, feet: Vec3) -> Vec3 {
        let value = self.axis.get(feet);
        let moved = if self.positive {
            value + self.distance
        } else {
            value - self.distance
        };

        self.axis.set(feet, moved)
    }
}

fn exit(axis: Axis, positive: bool, hits: &[IVec3], min: f32, max: f32) -> Exit {
    let coordinates = hits.iter().map(|cell| axis.coordinate(*cell));

    let distance = if positive {
        let top = coordinates.fold(i32::MIN, i32::max);

        top.saturating_add(1) as f32 - min
    } else {
        let bottom = coordinates.fold(i32::MAX, i32::min);

        max - bottom as f32
    };

    Exit {
        axis,
        positive,
        distance,
    }
}
