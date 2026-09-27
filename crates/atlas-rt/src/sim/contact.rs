use std::ops::RangeInclusive;

use glam::{IVec3, Vec3};

use crate::world::{
    World,
    grid::in_lattice,
    material::{PhysicalMaterialTable, Rule},
};

use super::profile::PlayerProfile;
use super::queue::UpdateQueue;

/// The face slack every contact test keeps. A sweep snaps the box onto the
/// integer face it stopped at, so this is also the most an overlap can be off
/// by at the lattice's 2048-unit edge, where an f32 has a 2.44e-4 ULP.
const EPSILON: f32 = 1.0e-3;

/// How close to a cell top the feet must sit to rest on it. One gravity
/// step of a resting player is 2.67e-2, so a moving player never reads
/// Grounded and only rests, bridges, and landed sweeps do.
const CONTACT: f32 = 1.1e-2;

/// The World one evaluation reads through: the World, the material table, and
/// the update queue. Both the player's contact tests and the voxel rules ask
/// this the same questions, so blocking and rule support cannot disagree.
pub(super) struct Field<'a> {
    world: &'a World,
    table: &'a PhysicalMaterialTable,
    queued: &'a UpdateQueue,
}

impl<'a> Field<'a> {
    pub(super) const fn new(
        world: &'a World,
        table: &'a PhysicalMaterialTable,
        queued: &'a UpdateQueue,
    ) -> Self {
        Self {
            world,
            table,
            queued,
        }
    }

    /// The coordinates the voxel rules have not evaluated yet, in no order.
    pub(super) fn queued_cells(&self) -> impl Iterator<Item = &IVec3> {
        self.queued.iter()
    }

    /// The material at the cell, or nothing outside the lattice or in an
    /// empty cell.
    pub(super) fn material(&self, cell: IVec3) -> Option<u8> {
        if !in_lattice(cell) {
            return None;
        }

        self.world.material_at(&cell)
    }

    /// The Falling granular material at the cell.
    pub(super) fn grain(&self, cell: IVec3) -> Option<u8> {
        let material = self.material(cell)?;

        (self.table.get(material).rule == Rule::FallingGranular).then_some(material)
    }

    /// Whether the cell is inside the lattice and holds nothing.
    pub(super) fn open(&self, cell: IVec3) -> bool {
        in_lattice(cell) && self.material(cell).is_none()
    }

    /// Whether the cell stably refuses a fall: outside the lattice, or held by
    /// a solid cell or a grain the queue has left settled. A queued grain
    /// never holds the column above it, which is what keeps a falling column
    /// coherent.
    pub(super) fn supports(&self, cell: IVec3) -> bool {
        if !in_lattice(cell) {
            return true;
        }

        let Some(material) = self.world.material_at(&cell) else {
            return false;
        };

        let physical = self.table.get(material);

        if physical.rule == Rule::FallingGranular && self.queued.contains(cell) {
            return false;
        }

        physical.solid || physical.rule == Rule::FallingGranular
    }

    /// Whether the cell blocks the player: outside the lattice, or held by a
    /// player-blocking material the queue has left settled.
    pub(super) fn blocks(&self, cell: IVec3) -> bool {
        if !in_lattice(cell) {
            return true;
        }

        self.world.material_at(&cell).is_some_and(|material| {
            let physical = self.table.get(material);

            physical.solid
                && !(physical.rule == Rule::FallingGranular && self.queued.contains(cell))
        })
    }
}

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
/// the box sweeps into, with the feet left exactly on it, reported as the
/// cell that owns the face.
pub(super) fn sweep(
    field: &Field,
    feet: Vec3,
    profile: PlayerProfile,
    axis: Axis,
    delta: f32,
) -> (Vec3, Option<IVec3>) {
    if delta == 0.0 {
        return (feet, None);
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
    let mut contact: Option<IVec3> = None;

    for cell in cells(region_min, region_max) {
        if !field.blocks(cell) {
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
            contact = Some(cell);
        }
    }

    let feet = if contact.is_none() {
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
    field: &'a Field<'a>,
    feet: Vec3,
    profile: PlayerProfile,
) -> impl Iterator<Item = IVec3> + 'a {
    let (min, max) = bounds(feet, profile);

    hits(field, min, max)
}

/// The blocking cells inside the box, with the same face slack.
fn hits<'a>(field: &'a Field<'a>, min: Vec3, max: Vec3) -> impl Iterator<Item = IVec3> + 'a {
    cells(min, max).filter(move |cell| inside(*cell, min, max) && field.blocks(*cell))
}

/// Whether any blocking cell sits inside the box, with the same face slack.
pub(super) fn blocked(field: &Field, min: Vec3, max: Vec3) -> bool {
    hits(field, min, max).next().is_some()
}

/// Whether blocking cells hold the feet: any cell under the footprint whose
/// top sits within contact tolerance, or a one cell crack the footprint
/// bridges across.
pub(super) fn grounded(field: &Field, feet: Vec3, profile: PlayerProfile) -> bool {
    let face = feet.y.round();

    if (face - feet.y).abs() >= CONTACT {
        return false;
    }

    let (min, max) = bounds(feet, profile);

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // the face is whole
    let row = (face - 1.0) as i32;
    let xs = span(min.x, max.x);
    let zs = span(min.z, max.z);

    let held = xs
        .clone()
        .any(|x| zs.clone().any(|z| field.blocks(IVec3::new(x, row, z))));

    held || bridged(field, row, &xs, &zs)
}

/// Whether the empty footprint sits over a one cell crack: the span on one
/// axis is a single cell and both neighbor lines over the other axis block.
fn bridged(field: &Field, row: i32, xs: &RangeInclusive<i32>, zs: &RangeInclusive<i32>) -> bool {
    let over_z = |x: i32| zs.clone().all(|z| field.blocks(IVec3::new(x, row, z)));
    let over_x = |z: i32| xs.clone().all(|x| field.blocks(IVec3::new(x, row, z)));

    let gap_x = xs.start() == xs.end()
        && over_z(xs.start().saturating_sub(1))
        && over_z(xs.end().saturating_add(1));
    let gap_z = zs.start() == zs.end()
        && over_x(zs.start().saturating_sub(1))
        && over_x(zs.end().saturating_add(1));

    gap_x || gap_z
}

/// The cells a box face spans, slack pulled in from both ends so a face
/// sitting exactly on an integer stays inside the cell it rests on.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // lattice faces are whole numbers
fn span(min: f32, max: f32) -> RangeInclusive<i32> {
    ((min + EPSILON).floor() as i32)..=((max - EPSILON).floor() as i32)
}

/// The height a step would climb in the contacted column: the top of the
/// blocking stack the feet run into, within the step height, or no step.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // feet and step heights sit in the lattice
pub(super) fn step_rise(
    field: &Field,
    feet: Vec3,
    profile: PlayerProfile,
    contact: IVec3,
) -> Option<f32> {
    if profile.step_height < 1 {
        return None;
    }

    let column = IVec3::new(contact.x, 0, contact.z);
    let lo = feet.y.round() as i32;
    let hi = (feet.y + profile.step_height as f32 - 1.0 + EPSILON).floor() as i32;

    let mut bottom = lo;

    while bottom <= hi && !field.blocks(column.with_y(bottom)) {
        bottom = bottom.saturating_add(1);
    }

    if bottom > hi {
        return None;
    }

    let mut top = bottom.saturating_add(1);

    while in_lattice(column.with_y(top)) && field.blocks(column.with_y(top)) {
        top = top.saturating_add(1);
    }

    let rise = top as f32 - feet.y;

    if rise < EPSILON || rise > profile.step_height as f32 + EPSILON {
        return None;
    }

    Some(rise)
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
