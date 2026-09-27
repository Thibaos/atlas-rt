use glam::{IVec3, Vec3};

use crate::world::{World, grid::LATTICE_HALF_EXTENT, material::PhysicalMaterialTable};

use super::contact::{Field, footprint, grounded};
use super::input::PlayerState;
use super::profile::PlayerProfile;
use super::queue::UpdateQueue;

const FLOOR_FEET: Vec3 = Vec3::new(0.0, -(LATTICE_HALF_EXTENT as f32), 0.0);

/// Where an empty `World` stands: on the lattice floor, which holds the
/// collider up with no fall.
#[must_use]
pub const fn floor_pose() -> PlayerState {
    PlayerState {
        feet: FLOOR_FEET,
        grounded: true,
    }
}

/// The spawn pose: feet at the bounding-box center in xz, on top of the
/// highest occupied cell of that column, pushed clear of anything solid.
#[must_use]
pub fn pose(
    world: &World,
    profile: PlayerProfile,
    table: &PhysicalMaterialTable,
    queue: &UpdateQueue,
) -> PlayerState {
    let Some((min, max)) = world.voxel_bounds() else {
        return floor_pose();
    };

    let field = Field::new(world, table, queue);
    let feet = depenetrate(&field, column_feet(world, min, max, profile), profile);

    PlayerState {
        feet,
        grounded: grounded(&field, feet, profile),
    }
}

fn column_feet(world: &World, min: IVec3, max: IVec3, profile: PlayerProfile) -> Vec3 {
    let center = min.saturating_add(max).saturating_add(IVec3::ONE);
    let roofline = column_top(
        world,
        center.x.div_euclid(2),
        center.z.div_euclid(2),
        min.y,
        max.y,
    )
    .unwrap_or(max.y)
    .saturating_add(1);

    let ceiling = LATTICE_HALF_EXTENT as f32 - profile.body_height;
    let mut feet = Vec3::new(center.x as f32 * 0.5, 0.0, center.z as f32 * 0.5);

    feet.y = (roofline as f32).min(ceiling);

    feet
}

/// The highest occupied cell of the column, or `None` for an empty column.
fn column_top(world: &World, x: i32, z: i32, from: i32, to: i32) -> Option<i32> {
    (from..=to)
        .rev()
        .find(|y| world.material_at(&IVec3::new(x, *y, z)).is_some())
}

fn depenetrate(field: &Field, feet: Vec3, profile: PlayerProfile) -> Vec3 {
    let ceiling = LATTICE_HALF_EXTENT as f32 - profile.body_height;
    let mut feet = feet;

    while blocked(field, feet, profile) && feet.y + 1.0 <= ceiling {
        feet.y += 1.0;
    }

    feet
}

fn blocked(field: &Field, feet: Vec3, profile: PlayerProfile) -> bool {
    footprint(feet, profile, 0.0, profile.body_height).any(|cell| field.blocks(cell))
}
