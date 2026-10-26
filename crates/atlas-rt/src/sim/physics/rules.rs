use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::world::World;
use crate::world::diff::edit::{VoxelChange, VoxelEdit};
use crate::world::grid::{MICRO_CHUNK_LENGTH, grid_origin, in_lattice};
use crate::world::material::{PhysicalMaterialTable, Rule};
use crate::world::micro::{self, MICRO_BYTES, MICRO_EDGE, MicroChunkRef};

use super::field::{ChunkSource, Field, Overlay, uniform};
use super::queue::UpdateQueue;
use crate::sim::commit::REWRITE_THRESHOLD;

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

/// A translated chunk's cells where the queue takes them next tick: the chunk,
/// and the chunk above's row 0 when the run continues into it.
struct Translation {
    origin: IVec3,
    above: Option<[u8; MICRO_EDGE]>,
}

/// Drains every cell this tick's queue holds, in ascending y, then x, then z,
/// against `world`: each move goes into an overlay a later cell reads, and
/// nothing here writes to the World. A destination is at `src.y - 1` and the
/// scan serves ascending y, so it needs no moved-this-tick guard. The moves and
/// the wakes queue their cells for the next tick, so a grain moves at most once
/// a tick.
///
/// Under [`DrainPolicy::Runs`] a chunk whose grains all fall straight down is
/// translated whole where the scan reaches its first row, and the boundary
/// cells the translation cannot own move through the per-cell pass.
pub(in crate::sim) fn drain(
    world: &World,
    table: &PhysicalMaterialTable,
    queue: &mut UpdateQueue,
    parity: ParityPolicy,
    policy: DrainPolicy,
) -> Drained {
    queue.begin_tick();

    let mut edits: Vec<VoxelEdit> = Vec::new();
    let mut destinations: Vec<IVec3> = Vec::new();
    let mut woken: Vec<IVec3> = Vec::new();
    let mut translations: Vec<Translation> = Vec::new();
    let mut wakes: Vec<(IVec3, [u8; MICRO_BYTES])> = Vec::new();

    let (processed, overlay) = {
        let mut field = Field::new(world, table, queue);

        let processed = match policy {
            DrainPolicy::PerCell => {
                let served: Vec<IVec3> = field.queued_cells().collect();

                serve(
                    &mut field,
                    parity,
                    &served,
                    &mut edits,
                    &mut destinations,
                    &mut woken,
                )
            }
            DrainPolicy::Runs => serve_runs(
                &mut field,
                world,
                table,
                queue,
                parity,
                &mut edits,
                &mut destinations,
                &mut woken,
                &mut translations,
                &mut wakes,
            ),
        };

        (processed, field.into_overlay())
    };

    for translation in &translations {
        queue.translate(translation.origin, translation.above);
    }

    queue.end_tick();

    for cell in woken {
        queue.wake(cell);
    }

    for (origin, mask) in &wakes {
        queue.queue_mask(*origin, mask);
    }

    for cell in destinations {
        queue.mark_moved(cell);
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
    edits: &mut Vec<VoxelEdit>,
    destinations: &mut Vec<IVec3>,
    woken: &mut Vec<IVec3>,
) -> usize {
    let mut processed = 0usize;

    for src in served {
        processed = processed.saturating_add(1);

        serve_cell(field, parity, *src, edits, destinations, woken);
    }

    processed
}

/// Serves one queued cell: its grain falls straight down while the cell below is
/// open, takes a diagonal once it is stably held, and settles when it has
/// neither, which is when the grain above it is woken for the next tick.
fn serve_cell(
    field: &mut Field,
    parity: ParityPolicy,
    src: IVec3,
    edits: &mut Vec<VoxelEdit>,
    destinations: &mut Vec<IVec3>,
    woken: &mut Vec<IVec3>,
) {
    let Some(material) = field.grain(src) else {
        return;
    };

    let Some(dst) = destination(field, parity, src) else {
        wake(field, woken, src.with_y(src.y.saturating_add(1)));

        return;
    };

    field.apply_move(src, dst, material);
    edits.push(VoxelEdit {
        position: src,
        change: VoxelChange::Clear,
    });
    edits.push(VoxelEdit {
        position: dst,
        change: VoxelChange::Set(material),
    });
    destinations.push(dst);
    wake_above(field, woken, src);
}

/// Serves the queue with the runs first: every chunk whose grains all fall
/// straight down is translated whole at the scan's position of its first row,
/// and the per-cell pass then serves what the translation cannot own. Returns
/// how many cells it evaluated, which counts translated cells as served.
fn serve_runs(
    field: &mut Field,
    world: &World,
    table: &PhysicalMaterialTable,
    queue: &UpdateQueue,
    parity: ParityPolicy,
    edits: &mut Vec<VoxelEdit>,
    destinations: &mut Vec<IVec3>,
    woken: &mut Vec<IVec3>,
    translations: &mut Vec<Translation>,
    wakes: &mut Vec<(IVec3, [u8; MICRO_BYTES])>,
) -> usize {
    let runs = Runs::decide(world, table, queue);
    let skip: FxHashSet<IVec3> = runs.translating.iter().copied().collect();
    let mut processed = 0usize;
    let mut served = 0usize;

    for cell in queue.unserved_except(&skip) {
        while let Some(origin) = runs.translating.get(served).copied()
            && origin.y <= cell.y
        {
            processed = processed.saturating_add(runs.translate_chunk(
                field,
                origin,
                edits,
                destinations,
                translations,
                wakes,
            ));
            served = served.saturating_add(1);
        }

        processed = processed.saturating_add(1);

        serve_cell(field, parity, cell, edits, destinations, woken);
    }

    while let Some(origin) = runs.translating.get(served).copied() {
        processed = processed.saturating_add(runs.translate_chunk(
            field,
            origin,
            edits,
            destinations,
            translations,
            wakes,
        ));
        served = served.saturating_add(1);
    }

    processed
}

/// One tick's run translation: the chunks that move as one, their pre-tick
/// entries, and the column verdicts the row-0 test has already settled.
struct Runs<'a> {
    world: &'a World,
    table: &'a PhysicalMaterialTable,
    queue: &'a UpdateQueue,
    translating: Vec<IVec3>,
    entries: FxHashMap<IVec3, ChunkSource<'a>>,
    columns: FxHashMap<(i32, i32), (i32, bool)>,
}

impl<'a> Runs<'a> {
    /// Decides the whole tick before anything is served, in ascending chunk y: a
    /// translating chunk's top row comes from the chunk above it, so the chunk
    /// above has to have its verdict when the lower one is written, and the
    /// row-0 test needs the verdicts below it.
    fn decide(world: &'a World, table: &'a PhysicalMaterialTable, queue: &'a UpdateQueue) -> Self {
        let mut runs = Self {
            world,
            table,
            queue,
            translating: Vec::new(),
            entries: FxHashMap::default(),
            columns: FxHashMap::default(),
        };

        for origin in queue.ready_chunks() {
            runs.consider(origin);
        }

        runs
    }

    /// Whether the chunk translates, keeping its pre-tick entry when it does:
    /// every cell it holds is a grain this tick serves, it holds more cells than
    /// a whole-entry write is cheaper than, and no grain on its row 0 stops.
    fn consider(&mut self, origin: IVec3) {
        let world = self.world;

        let Some(entry) = world.chunk_entry(origin) else {
            return;
        };

        if entry.occupied_count() <= REWRITE_THRESHOLD {
            return;
        }

        let source = ChunkSource::read(entry);

        if !self.grains_only(&source) {
            return;
        }

        if !self.queue.holds_all(origin, entry.mask) {
            return;
        }

        if !self.row_falls(origin, entry.mask) {
            return;
        }

        self.translating.push(origin);
        self.entries.insert(origin, source);
    }

    /// Whether every cell the entry holds is Falling granular, which is what
    /// lets the shift stand for each cell's own decision.
    fn grains_only(&self, source: &ChunkSource<'_>) -> bool {
        if let Some(material) = source.uniform {
            return self.table.get(material).rule == Rule::FallingGranular;
        }

        !source.entry.materials.is_empty()
            && source
                .entry
                .materials
                .iter()
                .all(|material| self.table.get(*material).rule == Rule::FallingGranular)
    }

    /// Whether every grain on the chunk's row 0 falls straight down, which is
    /// the row whose cell below sits in the chunk below. A chunk below that
    /// translates has already moved every cell the row can land in.
    fn row_falls(&mut self, origin: IVec3, mask: &[u8; MICRO_BYTES]) -> bool {
        let below = origin.with_y(origin.y.saturating_sub(MICRO_CHUNK_LENGTH.cast_signed()));

        if !in_lattice(below) {
            return false;
        }

        if self.entries.contains_key(&below) {
            return true;
        }

        for z in 0..MICRO_EDGE {
            let Some(byte) = mask.get(z.strict_mul(MICRO_EDGE)).copied() else {
                continue;
            };

            let mut bits = byte;

            while bits != 0 {
                let x = bits.trailing_zeros();
                let cell = origin.saturating_add(IVec3::new(
                    i32::try_from(x).unwrap_or(0),
                    0,
                    i32::try_from(z).unwrap_or(0),
                ));

                bits &= bits.wrapping_sub(1);

                if !self.falls(cell) {
                    return false;
                }
            }
        }

        true
    }

    /// Whether the grain at `cell` falls straight down: the column below it ends
    /// at an open cell or at a chunk that translates, and every grain between is
    /// one this tick serves and moves. A cell held by another material, an
    /// unserved grain, and the lattice edge all stop it.
    ///
    /// Each column remembers the lowest grain it has settled and that grain's
    /// verdict, so a pile under a run pays for the column once rather than once
    /// per chunk.
    fn falls(&mut self, cell: IVec3) -> bool {
        let column = (cell.x, cell.z);
        let mut y = cell.y;

        loop {
            if let Some((probed, verdict)) = self.columns.get(&column).copied()
                && probed == y
            {
                return verdict;
            }

            let below = cell.with_y(y.saturating_sub(1));

            if !in_lattice(below) {
                return self.settle(column, y, false);
            }

            let Some(material) = self.world.get_voxel(&below) else {
                return self.settle(column, y, true);
            };

            if self.table.get(material).rule != Rule::FallingGranular {
                return self.settle(column, y, false);
            }

            if self
                .entries
                .contains_key(&grid_origin(below, MICRO_CHUNK_LENGTH))
            {
                return self.settle(column, y, true);
            }

            if !self.queue.contains(below) {
                return self.settle(column, y, false);
            }

            y = below.y;
        }
    }

    fn settle(&mut self, column: (i32, i32), y: i32, verdict: bool) -> bool {
        self.columns.insert(column, (y, verdict));

        verdict
    }

    /// Serves one translating chunk: its row 0 into the chunk below when that
    /// chunk does not translate with it, the chunk itself as one entry, the wakes
    /// the rows it vacates leave, and the cells the queue takes next tick.
    /// Returns the cells it served.
    fn translate_chunk(
        &self,
        field: &mut Field,
        origin: IVec3,
        edits: &mut Vec<VoxelEdit>,
        destinations: &mut Vec<IVec3>,
        translations: &mut Vec<Translation>,
        wakes: &mut Vec<(IVec3, [u8; MICRO_BYTES])>,
    ) -> usize {
        let Some(source) = self.entries.get(&origin).copied() else {
            return 0;
        };

        let edge = MICRO_CHUNK_LENGTH.cast_signed();
        let above_origin = origin.with_y(origin.y.saturating_add(edge));
        let below_origin = origin.with_y(origin.y.saturating_sub(edge));
        let above = self.entries.get(&above_origin).copied();

        if !self.entries.contains_key(&below_origin) {
            release_row0(field, origin, &source.entry, edits, destinations);
        }

        field.translate(origin, source, above);

        let left = origin.with_x(origin.x.saturating_sub(edge));
        let right = origin.with_x(origin.x.saturating_add(edge));
        let roof = [
            above_origin.with_x(above_origin.x.saturating_sub(edge)),
            above_origin,
            above_origin.with_x(above_origin.x.saturating_add(edge)),
        ];
        let leftward = self.grain_mask(left);
        let rightward = self.grain_mask(right);
        let raised = roof.map(|origin| self.grain_mask(origin));
        let masks = wake_masks(
            source.entry.mask,
            &leftward,
            &rightward,
            [&raised[0], &raised[1], &raised[2]],
        );
        let targets = [origin, left, right, above_origin, roof[0], roof[2]];

        for (origin, mask) in targets
            .into_iter()
            .zip(masks)
            .filter_map(|(origin, mask)| mask.map(|mask| (origin, mask)))
        {
            wakes.push((origin, mask));
        }

        translations.push(Translation {
            origin,
            above: above.map(|above| micro::row(above.entry.mask, 0)),
        });

        self.queue.held(origin)
    }

    /// The chunk at `origin`, or nothing outside the lattice, where the World
    /// keeps no entry a wake could reach.
    fn neighbour(&self, origin: IVec3) -> Option<MicroChunkRef<'a>> {
        if !in_lattice(origin) {
            return None;
        }

        let world = self.world;

        world.chunk_entry(origin)
    }

    /// The cells of a chunk that hold a grain, since a wake reaches a grain and
    /// nothing else. A chunk that translates holds nothing else, which is the
    /// case nearly every wake is in.
    fn grain_mask(&self, origin: IVec3) -> [u8; MICRO_BYTES] {
        if let Some(source) = self.entries.get(&origin) {
            return *source.entry.mask;
        }

        grain_mask(self.table, self.neighbour(origin))
    }
}

/// Moves the chunk's row 0 into the chunk below it, which is what a run's bottom
/// chunk owns: the translation vacates those cells itself, so only the
/// destination reaches the commit.
fn release_row0(
    field: &mut Field,
    origin: IVec3,
    source: &MicroChunkRef<'_>,
    edits: &mut Vec<VoxelEdit>,
    destinations: &mut Vec<IVec3>,
) {
    for z in 0..MICRO_EDGE {
        let Some(byte) = source.mask.get(z.strict_mul(MICRO_EDGE)).copied() else {
            continue;
        };

        let mut bits = byte;

        while bits != 0 {
            let x = bits.trailing_zeros();
            let material = material_at(
                source,
                usize::try_from(x)
                    .unwrap_or(0)
                    .strict_add(z.strict_mul(MICRO_EDGE * MICRO_EDGE)),
            );

            bits &= bits.wrapping_sub(1);

            let Some(material) = material else {
                continue;
            };

            let offset = IVec3::new(
                i32::try_from(x).unwrap_or(0),
                0,
                i32::try_from(z).unwrap_or(0),
            );
            let src = origin.saturating_add(offset);
            let dst = src.with_y(src.y.saturating_sub(1));

            field.apply_move(src, dst, material);
            edits.push(VoxelEdit {
                position: dst,
                change: VoxelChange::Set(material),
            });
            destinations.push(dst);
        }
    }
}

/// The material the entry holds at cell `index`, which is its rank among the
/// mask's set bits.
fn material_at(entry: &MicroChunkRef<'_>, index: usize) -> Option<u8> {
    if !micro::occupied_cell(entry.mask, index) {
        return None;
    }

    entry.materials.get(micro::rank(entry.mask, index)).copied()
}

/// The cells a translated chunk's rows vacate with a grain above them, split by
/// the chunk they fall in, which the tick queues again so the next one finds
/// them empty. The per-cell pass wakes the same cells one at a time.
///
/// A wake reaches the three cells above the vacated one, so it crosses the side
/// walls of the chunk and, on its top row, its roof.
fn wake_masks(
    mask: &[u8; MICRO_BYTES],
    left: &[u8; MICRO_BYTES],
    right: &[u8; MICRO_BYTES],
    roof: [&[u8; MICRO_BYTES]; 3],
) -> [Option<[u8; MICRO_BYTES]>; 6] {
    let mut own = [0u8; MICRO_BYTES];
    let mut leftward = [0u8; MICRO_BYTES];
    let mut rightward = [0u8; MICRO_BYTES];

    for y in 1..MICRO_EDGE {
        let woke = woke_row(
            wake_sources(mask, left, right, y.saturating_sub(1)),
            micro::row(left, y),
            micro::row(mask, y),
            micro::row(right, y),
        );

        write_row(&mut leftward, y, woke.0);
        write_row(&mut own, y, woke.1);
        write_row(&mut rightward, y, woke.2);
    }

    let woke = woke_row(
        wake_sources(mask, left, right, MICRO_EDGE.saturating_sub(1)),
        micro::row(roof[0], 0),
        micro::row(roof[1], 0),
        micro::row(roof[2], 0),
    );
    let mut overhead = [0u8; MICRO_BYTES];
    let mut overhead_left = [0u8; MICRO_BYTES];
    let mut overhead_right = [0u8; MICRO_BYTES];

    write_row(&mut overhead_left, 0, woke.0);
    write_row(&mut overhead, 0, woke.1);
    write_row(&mut overhead_right, 0, woke.2);

    [
        filled(own),
        filled(leftward),
        filled(rightward),
        filled(overhead),
        filled(overhead_left),
        filled(overhead_right),
    ]
}

fn filled(mask: [u8; MICRO_BYTES]) -> Option<[u8; MICRO_BYTES]> {
    mask.iter().any(|byte| *byte != 0).then_some(mask)
}

/// One of the store's rows written into a mask.
fn write_row(mask: &mut [u8; MICRO_BYTES], y: usize, row: [u8; MICRO_EDGE]) {
    for (z, byte) in row.iter().copied().enumerate() {
        if let Some(slot) = mask.get_mut(z.strict_mul(MICRO_EDGE).strict_add(y)) {
            *slot = byte;
        }
    }
}

/// The x positions a row of cells wakes on the row above it, as the ten bits one
/// dilation reads: the chunk's own eight, and the one cell beside each wall.
fn wake_sources(
    mask: &[u8; MICRO_BYTES],
    left: &[u8; MICRO_BYTES],
    right: &[u8; MICRO_BYTES],
    y: usize,
) -> [u16; MICRO_EDGE] {
    let own = micro::row(mask, y);
    let led = micro::row(left, y);
    let red = micro::row(right, y);
    let mut sources = [0u16; MICRO_EDGE];

    for z in 0..MICRO_EDGE {
        let (Some(own), Some(led), Some(red)) = (
            own.get(z).copied(),
            led.get(z).copied(),
            red.get(z).copied(),
        ) else {
            continue;
        };

        if let Some(slot) = sources.get_mut(z) {
            *slot = u16::from(own).wrapping_shl(1)
                | u16::from(led.wrapping_shr(7))
                | u16::from(red & 1).wrapping_shl(9);
        }
    }

    sources
}

/// The cells above one row of vacated cells that hold a grain: the three cells
/// above each one, kept where the target row holds a grain, and split into the
/// chunk to the left, the chunk itself and the chunk to the right.
fn woke_row(
    sources: [u16; MICRO_EDGE],
    led: [u8; MICRO_EDGE],
    own: [u8; MICRO_EDGE],
    red: [u8; MICRO_EDGE],
) -> ([u8; MICRO_EDGE], [u8; MICRO_EDGE], [u8; MICRO_EDGE]) {
    let mut out = ([0u8; MICRO_EDGE], [0u8; MICRO_EDGE], [0u8; MICRO_EDGE]);

    for z in 0..MICRO_EDGE {
        let Some(sources) = sources.get(z).copied() else {
            continue;
        };

        let reached = sources | sources.wrapping_shl(1) | sources.wrapping_shr(1);
        let middle = u8::try_from(reached.wrapping_shr(1) & 0xFF).unwrap_or(0);
        let beside = led.get(z).copied().unwrap_or(0) & 0x80;
        let opposite = red.get(z).copied().unwrap_or(0) & 0x01;

        if let Some(slot) = out.0.get_mut(z) {
            *slot = if reached & 0x0001 != 0 { beside } else { 0 };
        }

        if let Some(slot) = out.1.get_mut(z) {
            *slot = middle & own.get(z).copied().unwrap_or(0);
        }

        if let Some(slot) = out.2.get_mut(z) {
            *slot = if reached & 0x0200 != 0 { opposite } else { 0 };
        }
    }

    out
}

/// The cells of a chunk that hold a Falling granular material, since a wake
/// reaches a grain and nothing else. A chunk of one material answers with its
/// own mask.
fn grain_mask(
    table: &PhysicalMaterialTable,
    entry: Option<MicroChunkRef<'_>>,
) -> [u8; MICRO_BYTES] {
    let Some(entry) = entry else {
        return [0u8; MICRO_BYTES];
    };

    let uniform = uniform(entry.materials);

    if let Some(material) = uniform {
        return if table.get(material).rule == Rule::FallingGranular {
            *entry.mask
        } else {
            [0u8; MICRO_BYTES]
        };
    }

    let mut mask = [0u8; MICRO_BYTES];
    let mut ascending = entry.materials.iter();

    for cell in 0..micro::MICRO_CELLS {
        if !micro::occupied_cell(entry.mask, cell) {
            continue;
        }

        let Some(material) = ascending.next().copied() else {
            break;
        };

        if table.get(material).rule != Rule::FallingGranular {
            continue;
        }

        let byte = cell.strict_div(MICRO_EDGE);

        if let Some(slot) = mask.get_mut(byte) {
            *slot |= 1u8.wrapping_shl(u32::try_from(cell.strict_rem(MICRO_EDGE)).unwrap_or(0));
        }
    }

    mask
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

    /// The moves a chunk served, which is what the commit's threshold reads.
    fn moves(overlay: &Overlay, origin: IVec3) -> usize {
        overlay
            .chunks()
            .find(|(touched, _)| *touched == origin)
            .map_or(0, |(_, held)| held.moves())
    }

    fn occupied(overlay: &Overlay, origin: IVec3, local: IVec3) -> bool {
        overlay
            .mask(origin)
            .is_some_and(|mask| micro::occupied_cell(mask, micro::mask_index(local)))
    }

    #[test]
    fn a_chunk_whose_grains_all_fall_translates_as_one_entry() {
        let world = row();
        let mut queue = queued(&world);
        let chunk = IVec3::new(0, 0, 0);

        let drained = drain(
            &world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            DrainPolicy::Runs,
        );

        assert!(
            drained.edits.is_empty(),
            "a translated chunk reaches the World as one entry and emits no per-cell edit"
        );
        assert_eq!(
            drained.processed, 5,
            "every translated cell counts as served"
        );
        assert_eq!(
            moves(&drained.overlay, chunk),
            5,
            "the chunk served one move per grain, which takes it past the commit's threshold"
        );

        for x in 0..5 {
            assert!(
                occupied(&drained.overlay, chunk, IVec3::new(x, 4, 0)),
                "column {x} is one row down"
            );
            assert!(
                !occupied(&drained.overlay, chunk, IVec3::new(x, 5, 0)),
                "column {x} left the row it was on"
            );
        }

        assert_eq!(
            order(&queue),
            (0..5).map(|x| IVec3::new(x, 4, 0)).collect::<Vec<_>>(),
            "the translated chunk queues its cells at their new positions"
        );
    }

    #[test]
    fn a_chunk_of_four_grains_stays_on_the_per_cell_path() {
        let mut cells = Vec::new();

        for x in 0..4 {
            cells.push((IVec3::new(x, 1, 0), GRAIN));
        }

        let world = world_of(&cells);
        let mut per_cell = queued(&world);
        let mut runs = queued(&world);

        let served = drain(
            &world,
            &table(),
            &mut per_cell,
            ParityPolicy::Alternate,
            DrainPolicy::PerCell,
        );

        assert_eq!(served.edits.len(), 8, "four grains fall as eight edits");

        let translated = drain(
            &world,
            &table(),
            &mut runs,
            ParityPolicy::Alternate,
            DrainPolicy::Runs,
        );

        assert_eq!(
            translated.edits, served.edits,
            "four cells cost less as edits than as one entry write, so the chunk is not translated"
        );
        assert_eq!(translated.processed, served.processed);
        assert_eq!(order(&per_cell), order(&runs));
    }

    #[test]
    fn a_translated_chunk_wakes_the_grain_settled_on_its_top_row() {
        let mut cells = Vec::new();

        for x in 0..5 {
            cells.push((IVec3::new(x, 0, 0), GRAIN));
            cells.push((IVec3::new(x, 7, 0), GRAIN));
        }

        let settled = IVec3::new(2, 8, 0);

        cells.push((settled, GRAIN));

        let world = world_of(&cells);
        let served: Vec<IVec3> = cells
            .iter()
            .map(|(position, _)| *position)
            .filter(|position| *position != settled)
            .collect();
        let mut queue = UpdateQueue::default();

        queue.seed(&world, &table(), Some(&served));

        assert!(
            !queue.contains(settled),
            "the fixture starts with one settled grain above the chunk"
        );

        let drained = drain(
            &world,
            &table(),
            &mut queue,
            ParityPolicy::Alternate,
            DrainPolicy::Runs,
        );

        assert_eq!(
            drained.edits.len(),
            5,
            "the run's bottom row leaves five destinations in the chunk below"
        );
        assert!(
            drained
                .edits
                .iter()
                .all(|edit| matches!(edit.change, VoxelChange::Set(_))),
            "the translation vacates the sources itself, so only destinations reach the commit"
        );
        assert!(
            queue.contains(settled),
            "the row the run vacated wakes the grain settled above it"
        );
    }
}
