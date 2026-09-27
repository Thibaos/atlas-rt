use glam::{IVec3, Vec3};

use crate::world::{World, grid::in_lattice, material::PhysicalMaterialTable};

use super::profile::PlayerProfile;

/// The face slack every contact test keeps. A sweep snaps the box onto the
/// integer face it stopped at, so this is also the most an overlap can be off
/// by at the lattice's 2048-unit edge, where an f32 has a 2.44e-4 ULP.
const EPSILON: f32 = 1.0e-3;

#[derive(Clone, Copy, Debug)]
pub(super) enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    const fn across(self) -> [Self; 2] {
        match self {
            Self::X => [Self::Y, Self::Z],
            Self::Y => [Self::X, Self::Z],
            Self::Z => [Self::X, Self::Y],
        }
    }

    pub(super) const fn coordinate(self, cell: IVec3) -> i32 {
        match self {
            Self::X => cell.x,
            Self::Y => cell.y,
            Self::Z => cell.z,
        }
    }

    pub(super) const fn get(self, position: Vec3) -> f32 {
        match self {
            Self::X => position.x,
            Self::Y => position.y,
            Self::Z => position.z,
        }
    }

    pub(super) const fn set(self, position: Vec3, value: f32) -> Vec3 {
        match self {
            Self::X => Vec3::new(value, position.y, position.z),
            Self::Y => Vec3::new(position.x, value, position.z),
            Self::Z => Vec3::new(position.x, position.y, value),
        }
    }

    /// Feet to box top: the distance a positive contact steps back over.
    fn ahead(self, profile: PlayerProfile) -> f32 {
        match self {
            Self::X => profile.width * 0.5,
            Self::Y => profile.body_height,
            Self::Z => profile.depth * 0.5,
        }
    }

    /// Box bottom to feet: the distance a negative contact steps back over.
    fn behind(self, profile: PlayerProfile) -> f32 {
        match self {
            Self::X => profile.width * 0.5,
            Self::Y => 0.0,
            Self::Z => profile.depth * 0.5,
        }
    }

    fn min(self, feet: Vec3, profile: PlayerProfile) -> f32 {
        self.get(feet) - self.behind(profile)
    }

    fn max(self, feet: Vec3, profile: PlayerProfile) -> f32 {
        self.get(feet) + self.ahead(profile)
    }
}

/// The collider box the feet anchor: the bottom sits on the feet, the top
/// `body_height` above them, and x and z reach half the width and depth out.
pub(super) fn bounds(feet: Vec3, profile: PlayerProfile) -> (Vec3, Vec3) {
    (
        Vec3::new(
            Axis::X.min(feet, profile),
            Axis::Y.min(feet, profile),
            Axis::Z.min(feet, profile),
        ),
        Vec3::new(
            Axis::X.max(feet, profile),
            Axis::Y.max(feet, profile),
            Axis::Z.max(feet, profile),
        ),
    )
}

/// Moves the feet `delta` along `axis` and stops on the first blocking face
/// the box sweeps into, with the feet left exactly on it.
pub(super) fn sweep(
    world: &World,
    feet: Vec3,
    profile: PlayerProfile,
    table: &PhysicalMaterialTable,
    axis: Axis,
    delta: f32,
) -> (Vec3, bool) {
    if delta == 0.0 {
        return (feet, false);
    }

    let lo = axis.min(feet, profile);
    let hi = axis.max(feet, profile);
    let [first, second] = axis.across();

    let (mut region_min, mut region_max) = (Vec3::ZERO, Vec3::ZERO);

    place(
        &mut region_min,
        &mut region_max,
        axis,
        (
            lo.min(hi + delta).floor() - 1.0,
            hi.max(lo + delta).floor() + 2.0,
        ),
    );
    place(
        &mut region_min,
        &mut region_max,
        first,
        padded(first, feet, profile),
    );
    place(
        &mut region_min,
        &mut region_max,
        second,
        padded(second, feet, profile),
    );

    let positive = delta > 0.0;
    let mut limit = if positive { hi + delta } else { lo + delta };
    let mut contact = false;

    for cell in cells(region_min, region_max) {
        if !blocks(world, &cell, table) {
            continue;
        }

        if !crosses(first, feet, profile, cell) || !crosses(second, feet, profile, cell) {
            continue;
        }

        let face = axis.coordinate(cell) as f32;
        let stop = if positive { face } else { face + 1.0 };
        let reachable = if positive {
            stop >= hi - EPSILON && stop < hi + delta && stop < limit
        } else {
            stop <= lo + EPSILON && stop > lo + delta && stop > limit
        };

        if reachable {
            limit = stop;
            contact = true;
        }
    }

    let feet = if !contact {
        axis.set(feet, axis.get(feet) + delta)
    } else if positive {
        axis.set(feet, limit - axis.ahead(profile))
    } else {
        axis.set(feet, limit + axis.behind(profile))
    };

    (feet, contact)
}

/// The blocking cells the box spans with slack on every face, so a box
/// resting exactly on a face does not count as overlapping the cell under it.
pub(super) fn overlapping<'a>(
    world: &'a World,
    feet: Vec3,
    profile: PlayerProfile,
    table: &'a PhysicalMaterialTable,
) -> impl Iterator<Item = IVec3> + 'a {
    let (min, max) = bounds(feet, profile);

    cells(min, max).filter(move |cell| inside(*cell, min, max) && blocks(world, cell, table))
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

fn padded(axis: Axis, feet: Vec3, profile: PlayerProfile) -> (f32, f32) {
    (
        axis.min(feet, profile).floor() - 1.0,
        axis.max(feet, profile).floor() + 2.0,
    )
}

const fn place(min: &mut Vec3, max: &mut Vec3, axis: Axis, range: (f32, f32)) {
    let (low, high) = range;

    *min = axis.set(*min, low);
    *max = axis.set(*max, high);
}

/// Whether the cell spans the box on one of the sweep's face axes, the test
/// that lets a sweep skip cells already inside the box.
fn crosses(axis: Axis, feet: Vec3, profile: PlayerProfile, cell: IVec3) -> bool {
    let value = axis.coordinate(cell) as f32;

    value < axis.max(feet, profile) - EPSILON && value + 1.0 > axis.min(feet, profile) + EPSILON
}

fn inside(cell: IVec3, min: Vec3, max: Vec3) -> bool {
    (cell.x as f32) < max.x - EPSILON
        && (cell.x.saturating_add(1)) as f32 > min.x + EPSILON
        && (cell.y as f32) < max.y - EPSILON
        && (cell.y.saturating_add(1)) as f32 > min.y + EPSILON
        && (cell.z as f32) < max.z - EPSILON
        && (cell.z.saturating_add(1)) as f32 > min.z + EPSILON
}

/// The cells the player collider spans, from `bottom` under the feet to `top`
/// above them.
pub(super) fn footprint(
    feet: Vec3,
    profile: PlayerProfile,
    bottom: f32,
    top: f32,
) -> impl Iterator<Item = IVec3> {
    let (min, max) = bounds(feet, profile);

    cells(
        Vec3::new(min.x, feet.y - bottom, min.z),
        Vec3::new(max.x, feet.y + top, max.z),
    )
}

fn cells(min: Vec3, max: Vec3) -> impl Iterator<Item = IVec3> {
    let start = min.floor().as_ivec3();
    let end = max.ceil().as_ivec3().saturating_sub(IVec3::ONE);

    (start.x..=end.x).flat_map(move |x| {
        (start.y..=end.y).flat_map(move |y| (start.z..=end.z).map(move |z| IVec3::new(x, y, z)))
    })
}

pub(super) fn blocks(world: &World, cell: &IVec3, table: &PhysicalMaterialTable) -> bool {
    !in_lattice(*cell)
        || world
            .material_at(cell)
            .is_some_and(|material| table.get(material).solid)
}
