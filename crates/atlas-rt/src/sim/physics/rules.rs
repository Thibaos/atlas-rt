use glam::IVec3;
use rustc_hash::FxHashSet;

use crate::world::World;
use crate::world::diff::edit::{VoxelChange, VoxelEdit};
use crate::world::material::PhysicalMaterialTable;

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

/// Drains at most `cap` of the queued cells in ascending y, then x, then z
/// against `world`, records one claim per destination in emission order, folds
/// the moves and their wakes back into `queue`, and leaves the cells it did not
/// reach queued. Nothing here writes to the World.
pub(in crate::sim) fn drain(
    world: &World,
    table: &PhysicalMaterialTable,
    queue: &mut UpdateQueue,
    parity: ParityPolicy,
    cap: usize,
) -> Vec<VoxelEdit> {
    let field = Field::new(world, table, queue);
    let boundary = field.queued_cells().nth(cap).copied();
    let mut claims = FxHashSet::default();
    let mut moves: Vec<(IVec3, IVec3, u8)> = Vec::new();
    let mut requeued: Vec<IVec3> = Vec::new();

    for src in field.queued_cells().take(cap).copied() {
        let Some(material) = field.grain(src) else {
            continue;
        };

        let Some(dst) = destination(&field, parity, src) else {
            wake(&field, &mut requeued, src.with_y(src.y.saturating_add(1)));

            continue;
        };

        if !claims.insert(dst) {
            requeued.push(src);

            continue;
        }

        moves.push((src, dst, material));
        wake_above(&field, &mut requeued, src);
    }

    let mut next = boundary.map_or_else(UpdateQueue::default, |cell| queue.split_off(cell));

    for cell in requeued {
        next.insert(cell);
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
    }

    *queue = next;

    edits
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
fn wake(field: &Field, requeued: &mut Vec<IVec3>, cell: IVec3) {
    if field.grain(cell).is_some() {
        requeued.push(cell);
    }
}

/// The move wake: the three cells above the cell a grain vacated.
fn wake_above(field: &Field, requeued: &mut Vec<IVec3>, vacated: IVec3) {
    let above = vacated.y.saturating_add(1);

    for x in [
        vacated.x,
        vacated.x.saturating_sub(1),
        vacated.x.saturating_add(1),
    ] {
        wake(field, requeued, IVec3::new(x, above, vacated.z));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::diff::batch::TrackedCoords;
    use crate::world::diff::edit::edit_world;
    use crate::world::material::parse_override;

    const GRAIN: u8 = 2;
    const PILLAR: u8 = 1;

    fn table() -> PhysicalMaterialTable {
        parse_override(&format!("material {GRAIN} falling_granular solid=true"))
            .unwrap_or_else(|rejections| panic!("{rejections:?}"))
    }

    fn world_of(cells: &[(IVec3, u8)]) -> World {
        let mut world = World::default();

        for (position, material) in cells {
            world.set_voxel(*position, *material);
        }

        world
    }

    fn queued(world: &World) -> UpdateQueue {
        let mut queue = UpdateQueue::default();

        queue.seed(world, &table(), None);

        queue
    }

    fn order(queue: &UpdateQueue) -> Vec<IVec3> {
        queue.iter().copied().collect()
    }

    fn clear(x: i32, y: i32, z: i32) -> VoxelEdit {
        VoxelEdit {
            position: IVec3::new(x, y, z),
            change: VoxelChange::Clear,
        }
    }

    fn set(x: i32, y: i32, z: i32, material: u8) -> VoxelEdit {
        VoxelEdit {
            position: IVec3::new(x, y, z),
            change: VoxelChange::Set(material),
        }
    }

    /// Five grains in open cells above nothing, so every one of them falls
    /// straight down when the drain reaches it.
    fn row() -> World {
        let mut cells = Vec::new();

        for x in 0..5 {
            cells.push((IVec3::new(x, 5, 0), GRAIN));
        }

        world_of(&cells)
    }

    fn step(world: &mut World, queue: &mut UpdateQueue, cap: usize) -> Vec<VoxelEdit> {
        let edits = drain(world, &table(), queue, ParityPolicy::Alternate, cap);

        edit_world(world, &edits, &TrackedCoords::default())
            .unwrap_or_else(|error| panic!("the edits must apply: {error}"));

        edits
    }

    #[test]
    fn a_tick_drains_at_most_the_cap_and_leaves_the_rest_queued() {
        let world = row();
        let mut queue = queued(&world);
        let edits = drain(&world, &table(), &mut queue, ParityPolicy::Alternate, 2);

        assert_eq!(
            edits,
            vec![
                clear(0, 5, 0),
                set(0, 4, 0, GRAIN),
                clear(1, 5, 0),
                set(1, 4, 0, GRAIN),
            ],
            "only the first two cells in drain order moved"
        );
        assert_eq!(
            order(&queue),
            vec![
                IVec3::new(0, 4, 0),
                IVec3::new(1, 4, 0),
                IVec3::new(2, 5, 0),
                IVec3::new(3, 5, 0),
                IVec3::new(4, 5, 0),
            ],
            "the destinations are queued and the unreached cells stay queued"
        );
    }

    #[test]
    fn the_processed_cells_match_the_uncapped_drain() {
        let world = row();
        let mut capped = queued(&world);
        let mut uncapped = queued(&world);

        let capped_edits = drain(&world, &table(), &mut capped, ParityPolicy::Alternate, 2);
        let uncapped_edits = drain(
            &world,
            &table(),
            &mut uncapped,
            ParityPolicy::Alternate,
            usize::MAX,
        );
        let prefix: Vec<VoxelEdit> = uncapped_edits
            .iter()
            .take(capped_edits.len())
            .copied()
            .collect();

        assert_eq!(
            capped_edits, prefix,
            "the cap cuts the uncapped drain short and changes nothing it keeps"
        );
        assert_eq!(
            uncapped_edits.len(),
            10,
            "the uncapped drain moves every grain"
        );
    }

    #[test]
    fn a_capped_tick_keeps_the_first_claim_winner() {
        let world = world_of(&[
            (IVec3::new(4, 1, 6), PILLAR),
            (IVec3::new(6, 1, 6), PILLAR),
            (IVec3::new(7, 1, 6), GRAIN),
            (IVec3::new(4, 2, 6), GRAIN),
            (IVec3::new(6, 2, 6), GRAIN),
        ]);
        let mut queue = queued(&world);
        let edits = drain(&world, &table(), &mut queue, ParityPolicy::Alternate, 3);

        assert_eq!(
            edits,
            vec![
                clear(7, 1, 6),
                set(7, 0, 6, GRAIN),
                clear(4, 2, 6),
                set(5, 1, 6, GRAIN),
            ],
            "the cell with the open column under it falls, then the first grain to reach (5, 1, 6) takes it"
        );
        assert!(
            queue.contains(IVec3::new(6, 2, 6)),
            "the grain that lost the claim stays queued"
        );
    }

    #[test]
    fn a_capped_sequence_repeats_exactly() {
        let mut first_world = row();
        let mut first = queued(&first_world);
        let mut second_world = row();
        let mut second = queued(&second_world);

        for tick in 0..6 {
            let edits = step(&mut first_world, &mut first, 2);
            let repeat = step(&mut second_world, &mut second, 2);

            assert_eq!(
                edits, repeat,
                "tick {tick}: the same scene drains the same edits"
            );
            assert_eq!(
                order(&first),
                order(&second),
                "tick {tick}: the remainder is queued in the same order"
            );
        }
    }
}
