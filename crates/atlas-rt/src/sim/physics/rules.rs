use glam::IVec3;

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

/// One drain's outcome: the edits it would commit and the queued cells it
/// evaluated, whether they moved, settled, or were never grains.
pub(in crate::sim) struct Drained {
    pub(in crate::sim) edits: Vec<VoxelEdit>,
    pub(in crate::sim) processed: usize,
}

/// Drains at most `cap` of the cells this sweep has not served, in ascending y,
/// then x, then z, against `world`: each move goes into an overlay a later cell
/// reads, the cells it did not reach stay unserved, and the moves and wakes join
/// the half the sweep's front decides. Nothing here writes to the World.
pub(in crate::sim) fn drain(
    world: &World,
    table: &PhysicalMaterialTable,
    queue: &mut UpdateQueue,
    parity: ParityPolicy,
    cap: usize,
) -> Drained {
    queue.begin_sweep();

    let mut served: Vec<IVec3> = Vec::new();
    let mut boundary = None;

    {
        let field = Field::new(world, table, queue);

        for cell in field.queued_cells().copied() {
            if served.len() >= cap {
                boundary = Some(cell);

                break;
            }

            served.push(cell);
        }
    }

    let mut processed = 0usize;
    let mut moves: Vec<(IVec3, IVec3, u8)> = Vec::new();
    let mut woken: Vec<IVec3> = Vec::new();

    {
        let mut field = Field::new(world, table, queue);

        for src in &served {
            let src = *src;

            processed = processed.saturating_add(1);

            let Some(material) = field.grain(src) else {
                continue;
            };

            let Some(dst) = destination(&field, parity, src) else {
                wake(&field, &mut woken, src.with_y(src.y.saturating_add(1)));

                continue;
            };

            // No moved-this-tick guard: a destination is at src.y - 1 and this
            // scan serves ascending y, so it is a cell this tick has passed.
            field.apply_move(src, dst, material);
            moves.push((src, dst, material));
            wake_above(&field, &mut woken, src);
        }
    }

    // The front is set before the wakes, so a wake behind it waits for the next
    // sweep while a wake ahead of it keeps its turn in this one.
    queue.finish(boundary);

    for cell in woken {
        queue.wake(cell);
    }

    // The mark travels with the grain, so a destination is served next sweep.
    for (_, dst, _) in &moves {
        queue.mark_moved(*dst);
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
    }

    Drained { edits, processed }
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

    /// Five grains stacked in one column above nothing.
    fn column() -> World {
        let mut cells = Vec::new();

        for y in 1..=5 {
            cells.push((IVec3::new(0, y, 0), GRAIN));
        }

        world_of(&cells)
    }

    /// A hand-built sphere of `radius`, centered at y = 8. The shipped sand
    /// ball's radius is fixed at 64, so this is the smallest scene that falls
    /// as a body.
    fn sphere(radius: i32) -> World {
        let mut cells = Vec::new();

        for x in -radius..=radius {
            for y in -radius..=radius {
                for z in -radius..=radius {
                    if x * x + y * y + z * z <= radius * radius {
                        cells.push((IVec3::new(x, y + 8, z), GRAIN));
                    }
                }
            }
        }

        world_of(&cells)
    }

    fn held(world: &World) -> Vec<IVec3> {
        let mut cells: Vec<IVec3> = world
            .iter_voxels()
            .filter(|(_, material)| *material == GRAIN)
            .map(|(position, _)| position)
            .collect();

        cells.sort_unstable_by_key(|cell| cell.to_array());

        cells
    }

    fn step(world: &mut World, queue: &mut UpdateQueue, cap: usize) -> Vec<VoxelEdit> {
        let edits = drain(world, &table(), queue, ParityPolicy::Alternate, cap).edits;

        edit_world(world, &edits, &TrackedCoords::default())
            .unwrap_or_else(|error| panic!("the edits must apply: {error}"));

        edits
    }

    #[test]
    fn a_tick_drains_at_most_the_cap_and_leaves_the_rest_queued() {
        let world = row();
        let mut queue = queued(&world);
        let edits = drain(&world, &table(), &mut queue, ParityPolicy::Alternate, 2).edits;

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
            queue.unserved().copied().collect::<Vec<_>>(),
            vec![
                IVec3::new(2, 5, 0),
                IVec3::new(3, 5, 0),
                IVec3::new(4, 5, 0),
            ],
            "the cells past the cap stay unserved, in drain order"
        );
        assert!(
            queue.contains(IVec3::new(0, 4, 0)) && queue.contains(IVec3::new(1, 4, 0)),
            "the destinations are queued, and the sweep has served them"
        );
    }

    #[test]
    fn the_processed_cells_match_the_uncapped_drain() {
        let world = row();
        let mut capped_queue = queued(&world);
        let mut uncapped_queue = queued(&world);

        let capped = drain(
            &world,
            &table(),
            &mut capped_queue,
            ParityPolicy::Alternate,
            2,
        );
        let uncapped = drain(
            &world,
            &table(),
            &mut uncapped_queue,
            ParityPolicy::Alternate,
            usize::MAX,
        );
        let prefix: Vec<VoxelEdit> = uncapped
            .edits
            .iter()
            .take(capped.edits.len())
            .copied()
            .collect();

        assert_eq!(
            capped.edits, prefix,
            "the cap cuts the uncapped drain short and changes nothing it keeps"
        );
        assert_eq!(
            uncapped.edits.len(),
            10,
            "the uncapped drain moves every grain"
        );
        assert_eq!(capped.processed, 2, "the cap bounds the cells it evaluates");
        assert_eq!(
            uncapped.processed, 5,
            "an uncapped drain evaluates every queued cell"
        );
    }

    #[test]
    fn a_column_advances_every_grain_one_cell_in_one_tick() {
        let mut world = column();
        let mut queue = queued(&world);
        let edits = drain(
            &world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            usize::MAX,
        )
        .edits;

        assert_eq!(
            edits,
            vec![
                clear(0, 1, 0),
                set(0, 0, 0, GRAIN),
                clear(0, 2, 0),
                set(0, 1, 0, GRAIN),
                clear(0, 3, 0),
                set(0, 2, 0, GRAIN),
                clear(0, 4, 0),
                set(0, 3, 0, GRAIN),
                clear(0, 5, 0),
                set(0, 4, 0, GRAIN),
            ],
            "every grain moves one cell: a grain reads the vacancy its neighbour left in the same tick"
        );

        edit_world(&mut world, &edits, &TrackedCoords::default())
            .unwrap_or_else(|error| panic!("the edits must apply: {error}"));

        assert_eq!(
            held(&world),
            (0..5).map(|y| IVec3::new(0, y, 0)).collect::<Vec<_>>(),
            "the column keeps its span and its gaps"
        );
    }

    #[test]
    fn a_sphere_falls_with_an_unchanged_span_and_one_cell_per_grain() {
        let mut world = sphere(3);
        let mut queue = queued(&world);
        let resting = held(&world);
        let span = resting.last().expect("the sphere holds grains").y
            - resting.first().expect("the sphere holds grains").y;

        for tick in 1..=4 {
            step(&mut world, &mut queue, usize::MAX);

            let fallen: Vec<IVec3> = resting
                .iter()
                .map(|cell| cell.with_y(cell.y - tick))
                .collect();

            assert_eq!(
                held(&world),
                fallen,
                "tick {tick}: every grain descends exactly one cell"
            );
            assert_eq!(
                fallen.last().expect("the sphere holds grains").y
                    - fallen.first().expect("the sphere holds grains").y,
                span,
                "tick {tick}: the span is unchanged"
            );
        }
    }

    #[test]
    fn a_grain_takes_the_diagonal_an_earlier_move_vacated_in_the_same_tick() {
        let world = world_of(&[
            (IVec3::new(4, 1, 6), PILLAR),
            (IVec3::new(6, 1, 6), PILLAR),
            (IVec3::new(7, 1, 6), GRAIN),
            (IVec3::new(4, 2, 6), GRAIN),
            (IVec3::new(6, 2, 6), GRAIN),
        ]);
        let mut queue = queued(&world);
        let edits = drain(&world, &table(), &mut queue, ParityPolicy::Alternate, 3).edits;

        assert_eq!(
            edits,
            vec![
                clear(7, 1, 6),
                set(7, 0, 6, GRAIN),
                clear(4, 2, 6),
                set(5, 1, 6, GRAIN),
                clear(6, 2, 6),
                set(7, 1, 6, GRAIN),
            ],
            "the grain over the open column falls, the next takes the open diagonal, and the sealed grain takes the cell the fall vacated"
        );
        assert_eq!(
            order(&queue),
            vec![
                IVec3::new(7, 0, 6),
                IVec3::new(5, 1, 6),
                IVec3::new(7, 1, 6),
                IVec3::new(6, 2, 6),
            ],
            "every destination is queued, and the first move's wake queued (6, 2, 6), which the third grain then left"
        );
    }

    #[test]
    fn a_move_wakes_the_settled_grain_above_it() {
        let world = world_of(&[(IVec3::new(0, 2, 0), GRAIN), (IVec3::new(0, 1, 0), GRAIN)]);
        let mut queue = UpdateQueue::default();

        queue.wake(IVec3::new(0, 1, 0));

        let edits = drain(
            &world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            usize::MAX,
        )
        .edits;

        assert_eq!(
            edits,
            vec![clear(0, 1, 0), set(0, 0, 0, GRAIN)],
            "the queued grain falls into the open cell below it"
        );
        assert!(
            queue.contains(IVec3::new(0, 2, 0)),
            "the move queues the settled grain above the cell it vacated"
        );
    }

    /// Six grains apart at y = 5 with nothing under them, so each falls one cell
    /// per service and no wake reaches another grain.
    fn scattered() -> World {
        let mut cells = Vec::new();

        for x in [0, 4, 8, 12, 16, 20] {
            cells.push((IVec3::new(x, 5, 0), GRAIN));
        }

        world_of(&cells)
    }

    #[test]
    fn a_sweep_serves_every_cell_once_before_serving_any_cell_twice() {
        let mut world = scattered();
        let mut queue = queued(&world);
        let total = queue.len();
        let cap = 3;
        let sweep = total.div_ceil(cap);

        assert_eq!(total, 6, "the scene is six grains");

        for _ in 0..sweep {
            step(&mut world, &mut queue, cap);
        }

        assert_eq!(
            held(&world),
            (0..6)
                .map(|index| IVec3::new(index * 4, 4, 0))
                .collect::<Vec<_>>(),
            "one sweep served every grain once, so every grain fell exactly one cell"
        );
        assert!(
            queue.unserved().next().is_none(),
            "the sweep has served every cell it started with, and none of them twice"
        );

        step(&mut world, &mut queue, cap);

        assert_eq!(
            held(&world).iter().filter(|cell| cell.y == 3).count(),
            cap,
            "the next sweep begins with the cap's worth of destinations"
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
