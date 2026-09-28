use glam::IVec3;
use rustc_hash::FxHashSet;

use crate::world::update::edit::{VoxelChange, VoxelEdit};

use super::field::Field;
use super::queue::UpdateQueue;

/// How a grain breaks a diagonal tie on the axis it picked: by the parity of
/// that coordinate, or always toward the negative direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParityPolicy {
    #[default]
    Alternate,
    AlwaysNegative,
}

/// One tick's rule outcome: the pending edits and the queue the next tick
/// starts from.
pub(in crate::sim) struct Outcome {
    pub(in crate::sim) edits: Vec<VoxelEdit>,
    pub(in crate::sim) queue: UpdateQueue,
}

/// Drains the queued cells in ascending y, then x, then z against `field`,
/// records one claim per destination in emission order, and rebuilds the queue
/// from the moves and their wakes. Nothing here writes to the World.
pub(in crate::sim) fn drain(field: &Field, parity: ParityPolicy) -> Outcome {
    let mut order: Vec<IVec3> = field.queued_cells().copied().collect();
    order.sort_unstable_by_key(|cell| (cell.y, cell.x, cell.z));

    let mut next = UpdateQueue::default();
    let mut claims = FxHashSet::default();
    let mut moves: Vec<(IVec3, IVec3, u8)> = Vec::new();

    for src in order {
        let Some(material) = field.grain(src) else {
            continue;
        };

        let Some(dst) = destination(field, parity, src) else {
            wake(field, &mut next, src.with_y(src.y.saturating_add(1)));

            continue;
        };

        if !claims.insert(dst) {
            next.insert(src);

            continue;
        }

        moves.push((src, dst, material));
    }

    let mut edits = Vec::with_capacity(moves.len().saturating_mul(2));

    for (src, dst, material) in moves {
        edits.push(VoxelEdit {
            position: src,
            change: VoxelChange::Clear,
        });
        edits.push(VoxelEdit {
            position: dst,
            change: VoxelChange::Set(material),
        });

        next.insert(dst);
        wake_above(field, &mut next, src);
    }

    Outcome { edits, queue: next }
}

/// Where the grain goes this tick: straight down while the cell below is
/// open, a diagonal once it is stably held, and nowhere when the grain has no
/// diagonal to take, which is what settling is.
fn destination(field: &Field, parity: ParityPolicy, src: IVec3) -> Option<IVec3> {
    let below = src.with_y(src.y.saturating_sub(1));

    if field.open(below) {
        return Some(below);
    }

    if !field.supports(below) {
        return None;
    }

    diagonal(field, src, parity)
}

/// The first open diagonal: the axis follows the parity of x plus z so
/// neither axis is preferred, then the tie follows `parity`, then the other
/// axis. The flank cell at the grain's own height has to be open too, or the
/// corner squeeze is sealed.
fn diagonal(field: &Field, src: IVec3, parity: ParityPolicy) -> Option<IVec3> {
    let order = if src.x.rem_euclid(2) == src.z.rem_euclid(2) {
        [Side::X, Side::Z]
    } else {
        [Side::Z, Side::X]
    };

    for side in order {
        for step in steps(side, src, parity) {
            let flank = side.shift(src, step);
            let target = flank.with_y(src.y.saturating_sub(1));

            if field.open(target) && field.open(flank) {
                return Some(target);
            }
        }
    }

    None
}

/// The near direction first: odd coordinates prefer the negative one under
/// `Alternate`, and every coordinate does under `AlwaysNegative`.
const fn steps(side: Side, cell: IVec3, parity: ParityPolicy) -> [i32; 2] {
    let near_first = match parity {
        ParityPolicy::Alternate => side.coordinate(cell).rem_euclid(2) == 1,
        ParityPolicy::AlwaysNegative => true,
    };

    if near_first { [-1, 1] } else { [1, -1] }
}

/// The settle wake: one cell straight up from a grain that failed to move.
fn wake(field: &Field, next: &mut UpdateQueue, cell: IVec3) {
    if field.grain(cell).is_some() {
        next.insert(cell);
    }
}

/// The move wake: the three cells above the cell a grain vacated.
fn wake_above(field: &Field, next: &mut UpdateQueue, vacated: IVec3) {
    let above = vacated.y.saturating_add(1);

    for x in [
        vacated.x,
        vacated.x.saturating_sub(1),
        vacated.x.saturating_add(1),
    ] {
        wake(field, next, IVec3::new(x, above, vacated.z));
    }
}

/// The horizontal axes a diagonal runs along, the Y axis never being one.
#[derive(Clone, Copy)]
enum Side {
    X,
    Z,
}

impl Side {
    const fn coordinate(self, cell: IVec3) -> i32 {
        match self {
            Self::X => cell.x,
            Self::Z => cell.z,
        }
    }

    fn shift(self, cell: IVec3, step: i32) -> IVec3 {
        match self {
            Self::X => cell.with_x(cell.x.saturating_add(step)),
            Self::Z => cell.with_z(cell.z.saturating_add(step)),
        }
    }
}
