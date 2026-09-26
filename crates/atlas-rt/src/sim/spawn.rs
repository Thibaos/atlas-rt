use glam::{IVec3, Vec3};

use crate::world::{
    World,
    grid::{LATTICE_HALF_EXTENT, in_lattice},
    material::PhysicalMaterialTable,
};

use super::input::PlayerState;
use super::profile::PlayerProfile;

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
pub fn pose(world: &World, profile: PlayerProfile, table: &PhysicalMaterialTable) -> PlayerState {
    let Some((min, max)) = world.voxel_bounds() else {
        return floor_pose();
    };

    let feet = depenetrate(world, column_feet(world, min, max, profile), profile, table);

    PlayerState {
        feet,
        grounded: grounded(world, feet, profile, table),
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

fn depenetrate(
    world: &World,
    feet: Vec3,
    profile: PlayerProfile,
    table: &PhysicalMaterialTable,
) -> Vec3 {
    let ceiling = LATTICE_HALF_EXTENT as f32 - profile.body_height;
    let mut feet = feet;

    while blocked(world, feet, profile, table) && feet.y + 1.0 <= ceiling {
        feet.y += 1.0;
    }

    feet
}

fn blocked(
    world: &World,
    feet: Vec3,
    profile: PlayerProfile,
    table: &PhysicalMaterialTable,
) -> bool {
    footprint(feet, profile, 0.0, profile.body_height).any(|cell| blocks(world, &cell, table))
}

/// Whether blocking cells rest the feet under any part of the player
/// collider, one cell below them.
pub(super) fn grounded(
    world: &World,
    feet: Vec3,
    profile: PlayerProfile,
    table: &PhysicalMaterialTable,
) -> bool {
    footprint(feet, profile, 1.0, 0.0).any(|cell| blocks(world, &cell, table))
}

/// The cells the player collider spans, from `bottom` under the feet to `top`
/// above them.
fn footprint(
    feet: Vec3,
    profile: PlayerProfile,
    bottom: f32,
    top: f32,
) -> impl Iterator<Item = IVec3> {
    let half_width = profile.width * 0.5;
    let half_depth = profile.depth * 0.5;
    let min = Vec3::new(feet.x - half_width, feet.y - bottom, feet.z - half_depth);
    let max = Vec3::new(feet.x + half_width, feet.y + top, feet.z + half_depth);

    cells(min, max)
}

fn cells(min: Vec3, max: Vec3) -> impl Iterator<Item = IVec3> {
    let start = min.floor().as_ivec3();
    let end = max.ceil().as_ivec3().saturating_sub(IVec3::ONE);

    (start.x..=end.x).flat_map(move |x| {
        (start.y..=end.y).flat_map(move |y| (start.z..=end.z).map(move |z| IVec3::new(x, y, z)))
    })
}

fn blocks(world: &World, cell: &IVec3, table: &PhysicalMaterialTable) -> bool {
    !in_lattice(*cell)
        || world
            .material_at(cell)
            .is_some_and(|material| table.get(material).solid)
}
