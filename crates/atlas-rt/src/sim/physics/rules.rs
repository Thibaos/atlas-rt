use glam::IVec3;

use crate::world::World;
use crate::world::diff::edit::{VoxelChange, VoxelEdit};
use crate::world::material::PhysicalMaterialTable;

use super::field::{Field, Overlay};
use super::queue::UpdateQueue;

/// How a grain breaks a diagonal tie on the axis it picked: by the parity of
/// that coordinate, or always toward the negative direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParityPolicy {
    #[default]
    Alternate,
    AlwaysNegative,
}

/// How a drain serves the queue: one cell at a time, or a whole run of
/// Micro-chunks translated as one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrainPolicy {
    #[default]
    PerCell,
    Runs,
}

/// One drain's outcome: the edits it would commit, the queued cells it
/// evaluated, whether they moved, settled, or were never grains, and the
/// overlay the commit reads.
pub(in crate::sim) struct Drained {
    pub(in crate::sim) edits: Vec<VoxelEdit>,
    pub(in crate::sim) processed: usize,
    pub(in crate::sim) overlay: Overlay,
}

/// Drains every cell this tick's queue holds, in ascending y, then x, then z,
/// against `world`: each move goes into an overlay a later cell reads, and
/// nothing here writes to the World. A destination is at `src.y - 1` and the
/// scan serves ascending y, so it needs no moved-this-tick guard. The moves and
/// the wakes queue their cells for the next tick, so a grain moves at most once
/// a tick.
pub(in crate::sim) fn drain(
    world: &World,
    table: &PhysicalMaterialTable,
    queue: &mut UpdateQueue,
    parity: ParityPolicy,
    policy: DrainPolicy,
) -> Drained {
    queue.begin_tick();

    let mut served: Vec<IVec3> = Vec::new();

    {
        let field = Field::new(world, table, queue);

        served.extend(field.queued_cells());
    }

    let mut moves: Vec<(IVec3, IVec3, u8)> = Vec::new();
    let mut woken: Vec<IVec3> = Vec::new();

    let (processed, overlay) = {
        let mut field = Field::new(world, table, queue);

        let processed = match policy {
            DrainPolicy::PerCell => serve(&mut field, parity, &served, &mut moves, &mut woken),
            DrainPolicy::Runs => {
                // Ticket 06 translates each run of chunks here, and the
                // per-cell pass then moves whatever crosses a run boundary.
                serve(&mut field, parity, &served, &mut moves, &mut woken)
            }
        };

        (processed, field.into_overlay())
    };

    queue.end_tick();

    for cell in woken {
        queue.wake(cell);
    }

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

    Drained {
        edits,
        processed,
        overlay,
    }
}

/// Serves every cell in drain order, one cell at a time, and returns how many
/// it evaluated.
fn serve(
    field: &mut Field,
    parity: ParityPolicy,
    served: &[IVec3],
    moves: &mut Vec<(IVec3, IVec3, u8)>,
    woken: &mut Vec<IVec3>,
) -> usize {
    let mut processed = 0usize;

    for src in served {
        let src = *src;

        processed = processed.saturating_add(1);

        let Some(material) = field.grain(src) else {
            continue;
        };

        let Some(dst) = destination(field, parity, src) else {
            wake(field, woken, src.with_y(src.y.saturating_add(1)));

            continue;
        };

        field.apply_move(src, dst, material);
        moves.push((src, dst, material));
        wake_above(field, woken, src);
    }

    processed
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

    /// The solid slab the falling sphere rests on. The sphere's lowest cell
    /// starts at y = 5, so it falls 12 cells before it lands.
    const FLOOR_Y: i32 = -8;

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
        queue.iter().collect()
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

    /// A hand-built sphere of `radius`, centered at y = 8 and standing above a
    /// solid slab at `floor_y`. The shipped sand ball's radius is fixed at 64,
    /// so this is the smallest scene that falls as a body.
    fn sphere(radius: i32, floor_y: i32) -> World {
        let mut cells = Vec::new();

        for x in -radius..=radius {
            for z in -radius..=radius {
                cells.push((IVec3::new(x, floor_y, z), PILLAR));
            }
        }

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

    /// The vertical span of the grains: the highest y minus the lowest.
    fn vertical_span(cells: &[IVec3]) -> i32 {
        let lowest = cells
            .iter()
            .map(|cell| cell.y)
            .min()
            .expect("cells hold grains");
        let highest = cells
            .iter()
            .map(|cell| cell.y)
            .max()
            .expect("cells hold grains");

        highest - lowest
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

    fn step(world: &mut World, queue: &mut UpdateQueue) -> Vec<VoxelEdit> {
        let edits = drain(
            world,
            &table(),
            queue,
            ParityPolicy::Alternate,
            DrainPolicy::PerCell,
        )
        .edits;

        edit_world(world, &edits, &TrackedCoords::default())
            .unwrap_or_else(|error| panic!("the edits must apply: {error}"));

        edits
    }

    #[test]
    fn a_tick_serves_every_queued_cell() {
        let world = row();
        let mut queue = queued(&world);
        let drained = drain(
            &world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            DrainPolicy::PerCell,
        );

        assert_eq!(drained.processed, 5, "the tick evaluates every queued cell");
        assert_eq!(drained.edits.len(), 10, "every grain moves one cell");
        assert!(
            queue.unserved().next().is_none(),
            "the tick leaves none of its own cells unserved"
        );
        assert_eq!(
            queue.len(),
            5,
            "only the five destinations wait for the next tick"
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
            DrainPolicy::PerCell,
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
        let mut world = sphere(3, FLOOR_Y);
        let mut queue = queued(&world);
        let resting = held(&world);
        let resting_span = vertical_span(&resting);
        let lowest = resting
            .iter()
            .map(|cell| cell.y)
            .min()
            .expect("the sphere holds grains");
        let landing = lowest - (FLOOR_Y + 1);

        for tick in 1..=landing {
            step(&mut world, &mut queue);

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
                vertical_span(&fallen),
                resting_span,
                "tick {tick}: the span is unchanged"
            );
        }

        assert_eq!(
            held(&world)
                .iter()
                .map(|cell| cell.y)
                .min()
                .expect("the sphere holds grains"),
            FLOOR_Y + 1,
            "the lowest grain rests on the floor after {landing} ticks"
        );
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
        let edits = drain(
            &world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            DrainPolicy::PerCell,
        )
        .edits;

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
            DrainPolicy::PerCell,
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
    fn a_tick_serves_every_grain_once_and_queues_their_destinations() {
        let mut world = scattered();
        let mut queue = queued(&world);

        assert_eq!(queue.len(), 6, "the scene is six grains");

        step(&mut world, &mut queue);

        assert_eq!(
            held(&world),
            (0..6)
                .map(|index| IVec3::new(index * 4, 4, 0))
                .collect::<Vec<_>>(),
            "every grain fell exactly one cell in the one tick"
        );
        assert_eq!(
            order(&queue),
            (0..6)
                .map(|index| IVec3::new(index * 4, 4, 0))
                .collect::<Vec<_>>(),
            "the queue holds exactly the six destinations, and no source"
        );

        step(&mut world, &mut queue);

        assert!(
            held(&world).iter().all(|cell| cell.y == 3),
            "the next tick falls the same six grains one cell further"
        );
    }

    #[test]
    fn a_sequence_repeats_exactly() {
        let mut first_world = row();
        let mut first = queued(&first_world);
        let mut second_world = row();
        let mut second = queued(&second_world);

        for tick in 0..6 {
            let edits = step(&mut first_world, &mut first);
            let repeat = step(&mut second_world, &mut second);

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

    #[test]
    fn the_runs_policy_serves_what_the_per_cell_pass_serves() {
        let world = row();
        let mut per_cell = queued(&world);
        let mut runs = queued(&world);

        let served = drain(
            &world,
            &table(),
            &mut per_cell,
            ParityPolicy::Alternate,
            DrainPolicy::PerCell,
        );
        let translated = drain(
            &world,
            &table(),
            &mut runs,
            ParityPolicy::Alternate,
            DrainPolicy::Runs,
        );

        assert_eq!(
            translated.edits, served.edits,
            "the run pass moves what the per-cell pass moves"
        );
        assert_eq!(translated.processed, served.processed);
        assert_eq!(order(&per_cell), order(&runs));
    }
}
