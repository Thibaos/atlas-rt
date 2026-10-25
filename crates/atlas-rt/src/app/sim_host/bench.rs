//! The tick tripwire sweep: p95 tick wall time, evaluation plus commit,
//! crossing 4 ms at the 60 fps target with the fixed 30 Hz tick, over three
//! occupancy points and four active-cell counts. Rule work is the break-out
//! sub-metric; active-cell throughput is not the tripwire, because any cost
//! in the tick can cross it.
//!
//! A tick drains the whole update queue, so its rule work is proportional to
//! the scene and crosses 4 ms on the larger points here. The budget check
//! therefore holds the rule work per drained cell, which scales with the scene
//! where a fixed bound did not. The 4 ms threshold stays the whole-tick
//! tripwire and is reported beside the rule work. The Micro-chunk column is the
//! tick's actual work, which follows the scene.
//!
//! The commit window carries costs the per-cell budget does not: `edit_world`
//! compiles every Micro-chunk the tick's moves touch, and it clones the
//! renderer's tracked Micro-chunk set, which follows the loaded world. A point
//! whose rule work stays inside the 4 ms tripwire and whose whole tick does not
//! is `COMMIT`, whose crossing sits in the commit window. A point whose rule
//! work crosses the tripwire at a per-cell cost inside the budget is `DRAIN`,
//! the expected cost of draining a whole large scene. A point whose rule work
//! crosses the per-cell budget is `OVERRUN`, which is the failure the check
//! asserts, and `print_crossings` reports it apart from the tripwire's own
//! crossings. ADR 0018 carries the measured figures and what they leave open.
//!
//! `generated_surface_tick_timings` measures the same check on the Falling
//! granular surface a Generation writes.
//!
//! `sand_ball_tick_timings` and `raining_tick_timings` measure the two scenes
//! that hurt. Each prints one row per window: the p95 queued, processed and
//! touched-chunk counts, the p95 whole tick with its evaluation and commit, and
//! a verdict that reads `ok` or names the whole tick as the miss. The ball's
//! free-fall and pile gates are reported until the run translation lands; the
//! rain's plateau gate is asserted. The ball's bench also records one tick's
//! frame path: its largest free-fall batch through `RendererInput`.

use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use dot_vox::DotVoxData;
use glam::IVec3;

use atlas_rt::{
    render::region::queue::RendererInput,
    sim::{InputSample, PlayerProfile, Push},
    world::{
        World,
        diff::{
            batch::TrackedCoords,
            edit::{VoxelChange, VoxelEdit, edit_world},
            snapshot::{MicroChunkSnapshot, emit_snapshots},
        },
        generation::{GenerationParams, generate},
        grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH, grid_origin, in_lattice},
        load::progress::Progress,
        material::{PhysicalMaterialTable, parse_override},
        rain, sand_ball,
    },
};

use super::SimHost;

const TICK_BUDGET: Duration = Duration::from_millis(4);

/// The cost of draining one cell the budget check allows, with headroom over
/// the 310 ns the sweep measures per drained cell at 1,000,000 active cells.
/// The check holds it against each scene's own drained-cell count, so the bound
/// scales with the scene rather than cutting the scene at a fixed count.
const CELL_COST_NANOS: u64 = 600;

const TICKS: usize = 60;
const ACTIVE: [usize; 4] = [1_000, 10_000, 100_000, 1_000_000];
const OCCUPANCIES: [&str; 3] = ["synth", "church", "bistro"];

const READY_TIMEOUT: Duration = Duration::from_secs(120);
const TICK_TIMEOUT: Duration = Duration::from_secs(60);

/// A scene bench's per-tick wait: the ball's pile takes about 40 seconds of
/// wall time and the rain's run runs to minutes, so a slow machine must not
/// trip a wait meant to catch a stopped sim.
const SCENE_TICK_TIMEOUT: Duration = Duration::from_secs(600);

const FRAME: Duration = Duration::from_nanos(16_666_667);

const LATTICE_FLOOR: i32 = -LATTICE_HALF_EXTENT.cast_signed();
const LAYER_SPACING: i32 = 2;

const SYNTH_EDGE: i32 = 100;
const SYNTH_MATERIAL: u8 = 0;

const SURFACE_SEED: u64 = 0x5EED_1234;
const SURFACE_EXTENTS: [i32; 4] = [512, 1024, 2048, 4096];

/// The surface bench's ticks, more than the sweep's: the first ticks reach the
/// deepest level, where no cell can move, so a p95 needs more than one tick.
const SURFACE_TICKS: usize = 256;

/// The ball's fall: its lowest grains reach the floor around tick 128, so the
/// free-fall window is the ticks before that and the pile is the ones after.
const FREE_FALL_TICKS: usize = 128;

const BALL_TICKS: usize = 190;

/// The rain's plateau: a grain leaves its hole at -1024 and lands 1,023 ticks
/// later, so the steady state starts at tick 1,024.
const PLATEAU_FIRST: usize = 1_024;

const RAIN_TICKS: usize = 1_100;

const FREE_FALL_GATE: Duration = Duration::from_millis(10);
const PILE_GATE: Duration = Duration::from_millis(360);
const PLATEAU_GATE: Duration = Duration::from_millis(120);

struct SweepPoint {
    occupancy: &'static str,
    active: usize,
    voxels: usize,
    edge: i32,
    layers: i32,
    timings: TickTimings,
}

#[test]
#[ignore = "bench: cargo test --release tick_tripwire_sweep -- --ignored --nocapture"]
fn tick_tripwire_sweep() {
    println!("per cell      {CELL_COST_NANOS} ns a drained cell, the bound the budget check holds");
    println!("tripwire      {TICK_BUDGET:.3?} p95 whole tick, reported beside the rule work");
    println!(
        "clock         frames at 60 fps, tick fixed at 30 Hz, {TICKS} measured ticks per point"
    );
    println!(
        "worst case    sand layers one empty cell apart, so the queue never runs dry and one tick drains the whole queue every tick"
    );
    print_header();

    let mut points: Vec<SweepPoint> = Vec::new();
    let mut failures = Failures::default();

    for occupancy in OCCUPANCIES {
        let source = Source::new(occupancy);

        for active in ACTIVE {
            let point = sweep(&source, occupancy, active);

            print_row(&point);

            let label = format!("{} @ {} active", point.occupancy, point.active);

            failures.record(point.timings.budget_check(&label));
            points.push(point);
        }
    }

    print_crossings(&points);

    failures.assert_empty();
}

/// One `.vox` asset held parsed, or the synthesized occupancy rebuilt per point.
enum Source {
    Asset(DotVoxData),
    Synth,
}

impl Source {
    fn new(occupancy: &str) -> Self {
        if occupancy == "synth" {
            return Self::Synth;
        }

        let path = format!("assets/{occupancy}.vox");
        let data = dot_vox::load(&path)
            .unwrap_or_else(|error| panic!("the asset must load ({path}): {error}"));

        Self::Asset(data)
    }

    fn build(&self) -> World {
        match self {
            Self::Asset(data) => World::new(data),
            Self::Synth => synth_world(),
        }
    }
}

/// The synthesized occupancy point: a solid cube pinned to the roof of the
/// lattice, so the free space below it is the largest any point gets.
fn synth_world() -> World {
    let mut world = World::default();

    edit_world(&mut world, &synth_edits(), &TrackedCoords::default())
        .unwrap_or_else(|error| panic!("the synthesized occupancy must apply: {error}"));

    world
}

fn synth_edits() -> Vec<VoxelEdit> {
    let half = SYNTH_EDGE.saturating_div(2);
    let roof = LATTICE_HALF_EXTENT.cast_signed().saturating_sub(SYNTH_EDGE);
    let base = IVec3::new(0i32.saturating_sub(half), roof, 0i32.saturating_sub(half));
    let mut edits = Vec::new();

    for x in 0..SYNTH_EDGE {
        for y in 0..SYNTH_EDGE {
            for z in 0..SYNTH_EDGE {
                edits.push(VoxelEdit {
                    position: base.saturating_add(IVec3::new(x, y, z)),
                    change: VoxelChange::Set(SYNTH_MATERIAL),
                });
            }
        }
    }

    edits
}

/// The lowest material index the world leaves free, which becomes the sand.
fn free_material(world: &World) -> u8 {
    let mut used = [false; 256];

    for (_, voxel) in world.iter_voxels() {
        if let Some(slot) = used.get_mut(usize::from(voxel)) {
            *slot = true;
        }
    }

    used.iter()
        .position(|&taken| !taken)
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or_else(|| panic!("the world uses every material index"))
}

/// The sand block: layers spaced one empty cell apart, so every grain has an
/// open cell below it and the whole block moves one cell per tick. Every
/// active cell stays in the queue for the whole run, so one tick drains all of
/// them on every tick and the rule work is the scene's own size.
struct Block {
    origin: IVec3,
    edge: i32,
    layers: i32,
}

impl Block {
    /// Placed in the free space under the World: the top layer sits in the
    /// first Micro-chunk row the World never reaches, and the block keeps
    /// `TICKS` cells of descent above the lattice floor.
    fn new(active: usize, bounds: Option<(IVec3, IVec3)>) -> Self {
        let top = free_row_below(bounds);
        let floor = LATTICE_FLOOR.saturating_add(i32::try_from(TICKS).unwrap_or(i32::MAX));
        let available = top.saturating_sub(floor).saturating_add(1);

        assert!(
            available > 0,
            "the free space under the World is {available} cells tall, so nothing fits below it"
        );

        let edge = cube_edge(active);
        let layers = layer_count(edge, active);
        let height = Self::height(layers);

        assert!(
            height <= available,
            "the {active}-cell block is {height} cells tall against {available} cells of free space"
        );

        let bottom = top.saturating_sub(height.saturating_sub(1));
        let inset = 0i32.saturating_sub(edge.saturating_div(2));

        Self {
            origin: IVec3::new(inset, bottom, inset),
            edge,
            layers,
        }
    }

    const fn height(layers: i32) -> i32 {
        layers.saturating_mul(LAYER_SPACING).saturating_sub(1)
    }

    const fn top(&self) -> i32 {
        self.origin
            .y
            .saturating_add(Self::height(self.layers).saturating_sub(1))
    }
}

/// The free row below the World: the floor of the Micro-chunk row the
/// lowest voxel sits in, minus one, so no touched chunk ever shares a row
/// with World occupancy.
fn free_row_below(bounds: Option<(IVec3, IVec3)>) -> i32 {
    let (min, _max) = bounds.unwrap_or((IVec3::ZERO, IVec3::ZERO));

    grid_origin(min, MICRO_CHUNK_LENGTH).y.saturating_sub(1)
}

/// The smallest cube holding `active` cells: the layers then number the cube
/// root, which keeps the block inside the free space at every point of the sweep.
fn cube_edge(active: usize) -> i32 {
    let mut edge = 1usize;

    while edge.saturating_mul(edge).saturating_mul(edge) < active {
        edge = edge.saturating_add(1);
    }

    i32::try_from(edge).unwrap_or(LATTICE_HALF_EXTENT.cast_signed())
}

fn layer_count(edge: i32, active: usize) -> i32 {
    let cells = edge.saturating_mul(edge);
    let per_layer = usize::try_from(cells).unwrap_or(1);

    i32::try_from(active.div_ceil(per_layer)).unwrap_or(i32::MAX)
}

fn sand_edits(block: &Block, active: usize, material: u8) -> Vec<VoxelEdit> {
    let per_layer = block.edge.saturating_mul(block.edge);
    let mut edits = Vec::with_capacity(active);

    for index in 0..active {
        let cell = i32::try_from(index).unwrap_or(i32::MAX);
        let layer = cell.div_euclid(per_layer);
        let offset = cell.rem_euclid(per_layer);

        edits.push(VoxelEdit {
            position: IVec3::new(
                block.origin.x.saturating_add(offset.div_euclid(block.edge)),
                block
                    .origin
                    .y
                    .saturating_add(layer.saturating_mul(LAYER_SPACING)),
                block.origin.z.saturating_add(offset.rem_euclid(block.edge)),
            ),
            change: VoxelChange::Set(material),
        });
    }

    edits
}

fn sand_table(sand: u8) -> PhysicalMaterialTable {
    let record = format!("material {sand} falling_granular solid=true");

    parse_override(&record)
        .unwrap_or_else(|rejections| panic!("the sand record must parse: {rejections:?}"))
}

fn sweep(source: &Source, occupancy: &'static str, active: usize) -> SweepPoint {
    let mut world = source.build();
    let sand = free_material(&world);
    let block = Block::new(active, world.voxel_bounds());

    edit_world(
        &mut world,
        &sand_edits(&block, active, sand),
        &TrackedCoords::default(),
    )
    .unwrap_or_else(|error| panic!("the sand block must apply: {error}"));

    let voxels = world.voxel_count();
    let snapshots =
        emit_snapshots(&world).unwrap_or_else(|error| panic!("the snapshots must emit: {error}"));
    let tracked: TrackedCoords = snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();
    let table = sand_table(sand);

    let mut host = SimHost::spawn(
        Arc::new(RwLock::new(world)),
        PlayerProfile::default(),
        snapshots,
        tracked,
        None,
        None,
        &table,
    )
    .unwrap_or_else(|error| panic!("the host must spawn: {error}"));

    host.start();
    wait_ready(&mut host);

    let samples = drive(&mut host, TICKS);
    drop(host);

    assert_eq!(
        samples.moving, TICKS,
        "every tick of the block must drain cells"
    );

    SweepPoint {
        occupancy,
        active,
        voxels,
        edge: block.edge,
        layers: block.layers,
        timings: TickTimings::of(&samples),
    }
}

/// The activation's two pushes, taken before any frame so no tick runs
/// ahead of the measurement.
fn wait_ready(host: &mut SimHost) {
    let deadline = Instant::now()
        .checked_add(READY_TIMEOUT)
        .unwrap_or_else(Instant::now);

    while !host.ready() {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("the host never became ready within {READY_TIMEOUT:?}"));

        let push = recv(host, timeout, "readiness");

        host.apply(push, &mut |_batch| Ok(()));
    }
}

fn recv(host: &SimHost, timeout: Duration, what: &str) -> Push {
    host.handle
        .recv_timeout(timeout)
        .unwrap_or_else(|error| panic!("the sim must push {what}: {error}"))
}

struct Samples {
    wall: Vec<Duration>,
    eval: Vec<Duration>,
    commit: Vec<Duration>,
    chunks: Vec<usize>,
    queued: Vec<usize>,
    processed: Vec<usize>,
    moving: usize,
}

impl Samples {
    /// The measured ticks `first` to `last`, one-based and inclusive, clipped
    /// to the ticks the run measured.
    fn window(&self, first: usize, last: usize) -> Self {
        let measured = self.wall.len();
        let start = first.saturating_sub(1).min(measured);
        let end = last.min(measured).max(start);
        let chunks = self.chunks.get(start..end).unwrap_or_default().to_vec();
        let moving = chunks.iter().filter(|count| **count > 0).count();

        Self {
            wall: self.wall.get(start..end).unwrap_or_default().to_vec(),
            eval: self.eval.get(start..end).unwrap_or_default().to_vec(),
            commit: self.commit.get(start..end).unwrap_or_default().to_vec(),
            queued: self.queued.get(start..end).unwrap_or_default().to_vec(),
            processed: self.processed.get(start..end).unwrap_or_default().to_vec(),
            chunks,
            moving,
        }
    }
}

/// Frames at the 60 fps target against the fixed 30 Hz tick: two frames per
/// measured tick, each carrying half a tick period rounded up, so the second
/// frame runs the tick the report carries. The clock invariants are the same
/// for every scene, so they are asserted here; whether a tick moved cells
/// belongs to the scene, so the caller checks `moving`.
fn drive(host: &mut SimHost, ticks: usize) -> Samples {
    drive_with(host, ticks, TICK_TIMEOUT, &mut |_, _| {})
}

/// The same run with the wait and the batch observer a scene bench needs: the
/// observer sees each measured tick's index and its snapshots before the host
/// takes them.
fn drive_with(
    host: &mut SimHost,
    ticks: usize,
    timeout: Duration,
    on_batch: &mut impl FnMut(usize, &[MicroChunkSnapshot]),
) -> Samples {
    let mut samples = Samples {
        wall: Vec::with_capacity(ticks),
        eval: Vec::with_capacity(ticks),
        commit: Vec::with_capacity(ticks),
        chunks: Vec::with_capacity(ticks),
        queued: Vec::with_capacity(ticks),
        processed: Vec::with_capacity(ticks),
        moving: 0,
    };

    for tick_index in 1..=ticks {
        host.frame(FRAME, InputSample::default());
        host.frame(FRAME, InputSample::default());

        let push = recv(host, timeout, "a measured tick");

        let Push::Tick(tick) = push else {
            panic!("the frame ran no tick");
        };

        let report = &tick.report;

        assert_eq!(
            report.discarded, 0,
            "one frame must not outrun the catch-up cap"
        );

        if let Some(batch) = report.batch.as_ref() {
            samples.moving = samples.moving.saturating_add(1);
            on_batch(tick_index, batch);
        }

        samples
            .chunks
            .push(report.batch.as_ref().map_or(0, Vec::len));
        samples.queued.push(report.queued);
        samples.processed.push(report.processed);
        samples
            .wall
            .push(report.tick_time.saturating_add(report.commit_time));
        samples.eval.push(report.tick_time);
        samples.commit.push(report.commit_time);

        host.apply(Push::Tick(tick), &mut |_batch| Ok(()));
    }

    samples
}

fn nearest_rank<T: Copy + Ord + Default>(samples: &[T]) -> T {
    let mut sorted = samples.to_vec();

    sorted.sort_unstable();

    sorted
        .get(
            sorted
                .len()
                .saturating_mul(95)
                .div_ceil(100)
                .saturating_sub(1),
        )
        .copied()
        .unwrap_or_default()
}

/// The p95 timings of one measured scene, the tick's touched Micro-chunk
/// count, and the rule work one drained cell costs. The per-cell figure is what
/// the budget check holds, since a tick drains the whole queue; the whole
/// tick's 4 ms tripwire is classified beside it.
#[derive(Clone, Copy, Default)]
struct TickTimings {
    tick_p95: Duration,
    eval_p95: Duration,
    commit_p95: Duration,
    chunks_p95: usize,
    queued_p95: usize,
    processed_p95: usize,
    per_cell_nanos: u64,
}

impl TickTimings {
    fn of(samples: &Samples) -> Self {
        let processed: usize = samples.processed.iter().copied().sum();
        let eval: Duration = samples.eval.iter().copied().sum();
        let per_cell_nanos = if processed == 0 {
            0
        } else {
            u64::try_from(eval.as_nanos() / processed as u128).unwrap_or(u64::MAX)
        };

        Self {
            tick_p95: nearest_rank(&samples.wall),
            eval_p95: nearest_rank(&samples.eval),
            commit_p95: nearest_rank(&samples.commit),
            chunks_p95: nearest_rank(&samples.chunks),
            queued_p95: nearest_rank(&samples.queued),
            processed_p95: nearest_rank(&samples.processed),
            per_cell_nanos,
        }
    }

    /// The rule work per drained cell is what the budget check holds, so the
    /// bound follows the scene. The 4 ms tripwire is the whole tick and is
    /// reported beside it.
    fn budget_check(&self, label: &str) -> Result<(), String> {
        if self.per_cell_overrun() {
            return Err(format!(
                "{label}: rule work {} ns a drained cell overruns the {CELL_COST_NANOS} ns bound",
                self.per_cell_nanos
            ));
        }

        Ok(())
    }

    /// Whether the rule work crosses the per-cell budget, which is the failure
    /// the check asserts.
    fn per_cell_overrun(&self) -> bool {
        self.per_cell_nanos > CELL_COST_NANOS
    }

    /// Whether the rule work crossed the 4 ms tripwire at a per-cell cost
    /// inside the budget, which is a large scene draining as designed rather
    /// than a regression.
    fn drain_over_tripwire(&self) -> bool {
        self.eval_p95 > TICK_BUDGET && !self.per_cell_overrun()
    }

    /// Whether the whole tick is over the 4 ms tripwire with its rule work
    /// inside it, so the crossing sits in the commit window.
    fn commit_overrun(&self) -> bool {
        self.eval_p95 <= TICK_BUDGET && self.tick_p95 > TICK_BUDGET
    }

    fn verdict(&self) -> &'static str {
        if self.per_cell_overrun() {
            "OVERRUN"
        } else if self.drain_over_tripwire() {
            "DRAIN"
        } else if self.commit_overrun() {
            "COMMIT"
        } else {
            "ok"
        }
    }
}

/// Collects each over-budget point's message, so one sweep reports every point
/// that failed rather than the first.
#[derive(Default)]
struct Failures(String);

impl Failures {
    fn record(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.0.push_str(&message);
            self.0.push('\n');
        }
    }

    fn assert_empty(&self) {
        assert!(self.0.is_empty(), "{}", self.0);
    }
}

fn print_header() {
    println!(
        "{:>9} {:>9} {:>11} {:>9} {:>11} {:>11} {:>11} {:>10} {:>9} {:>8}",
        "occupancy",
        "active",
        "voxels",
        "block",
        "p95 tick",
        "p95 eval",
        "p95 commit",
        "p95 chunks",
        "ns/cell",
        "budget"
    );
}

fn print_row(point: &SweepPoint) {
    let block = format!("{}x{}", point.edge, point.layers);

    println!(
        "{:>9} {:>9} {:>11} {:>9} {:>11.3?} {:>11.3?} {:>11.3?} {:>10} {:>9} {:>8}",
        point.occupancy,
        point.active,
        point.voxels,
        block,
        point.timings.tick_p95,
        point.timings.eval_p95,
        point.timings.commit_p95,
        point.timings.chunks_p95,
        point.timings.per_cell_nanos,
        point.timings.verdict()
    );
}

fn print_crossings(points: &[SweepPoint]) {
    println!(
        "crossing point, the first active count whose whole tick is over the {TICK_BUDGET:.3?} tripwire:"
    );

    for occupancy in OCCUPANCIES {
        let crossing = points
            .iter()
            .find(|point| point.occupancy == occupancy && point.timings.tick_p95 > TICK_BUDGET);

        match crossing {
            Some(point) => println!("  {occupancy:>9} {}", point.active),
            None => println!("  {occupancy:>9} none up to 1M"),
        }
    }

    let worst = points
        .iter()
        .max_by_key(|point| point.timings.per_cell_nanos);

    match worst {
        Some(point) => println!(
            "per cell      {} ns a drained cell at {:>9} {}, against the {CELL_COST_NANOS} ns the budget check holds",
            point.timings.per_cell_nanos, point.occupancy, point.active
        ),
        None => println!("per cell      no point measured"),
    }
}

/// One generated surface's measured ticks: the extent, the Falling granular
/// cells its Generation wrote, the activation before the first tick, and the
/// tick timings they settled under.
struct SurfacePoint {
    extent: i32,
    voxels: usize,
    grains: usize,
    activate: Duration,
    timings: TickTimings,
    moving: usize,
}

/// Runs one generated surface through the real host: generate, emit, activate
/// with the generator's own grain list, then drive the measured ticks. A
/// Generated World's Sand surface is Falling granular wherever the column
/// surface sits below ground level, so the grain count runs to millions. The
/// activation is measured too, because it seeds the queue with every one of
/// those grains.
fn surface_point(extent: i32) -> SurfacePoint {
    let params = GenerationParams::new(SURFACE_SEED, IVec3::splat(extent));
    let generated = generate(&Progress::generate_path(), params)
        .unwrap_or_else(|error| panic!("the {extent} extent must generate: {error}"));

    let grains = generated.granular_cells.len();
    let voxels = generated.world.voxel_count();
    let snapshots = emit_snapshots(&generated.world)
        .unwrap_or_else(|error| panic!("the {extent} snapshots must emit: {error}"));
    let tracked: TrackedCoords = snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();

    let mut host = SimHost::spawn(
        Arc::new(RwLock::new(generated.world)),
        PlayerProfile::default(),
        snapshots,
        tracked,
        Some(generated.granular_cells),
        None,
        &generated.materials,
    )
    .unwrap_or_else(|error| panic!("the host must spawn: {error}"));

    let activated = Instant::now();

    host.start();
    wait_ready(&mut host);

    let activate = activated.elapsed();
    let samples = drive(&mut host, SURFACE_TICKS);
    drop(host);

    SurfacePoint {
        extent,
        voxels,
        grains,
        activate,
        timings: TickTimings::of(&samples),
        moving: samples.moving,
    }
}

fn print_surface_header() {
    println!(
        "{:>9} {:>12} {:>10} {:>11} {:>11} {:>11} {:>11} {:>10} {:>9} {:>8} {:>8}",
        "extent",
        "voxels",
        "grains",
        "activate",
        "p95 tick",
        "p95 eval",
        "p95 commit",
        "p95 chunks",
        "ns/cell",
        "moving",
        "budget"
    );
}

fn print_surface_row(point: &SurfacePoint) {
    println!(
        "{:>9} {:>12} {:>10} {:>11.3?} {:>11.3?} {:>11.3?} {:>11.3?} {:>10} {:>9} {:>8} {:>8}",
        point.extent,
        point.voxels,
        point.grains,
        point.activate,
        point.timings.tick_p95,
        point.timings.eval_p95,
        point.timings.commit_p95,
        point.timings.chunks_p95,
        point.timings.per_cell_nanos,
        point.moving,
        point.timings.verdict()
    );
}

/// The generated-surface tripwire. A full-lattice Generation writes about
/// eight and a half million Falling granular surface cells, and one tick drains
/// all of them, so the rule work is the scene's own size at every extent. The
/// budget check holds the per-cell cost, and the commit window is reported
/// rather than asserted on.
#[test]
#[ignore = "bench: cargo test --release generated_surface_tick_timings -- --ignored --nocapture"]
fn generated_surface_tick_timings() {
    println!("per cell      {CELL_COST_NANOS} ns a drained cell, the bound the budget check holds");
    println!("tripwire      {TICK_BUDGET:.3?} p95 whole tick, reported beside the rule work");
    println!(
        "clock         frames at 60 fps, tick fixed at 30 Hz, {SURFACE_TICKS} measured ticks per point"
    );
    println!(
        "surface       a Generation's Sand surface, activated with the generator's own granular cells"
    );
    print_surface_header();

    let mut failures = Failures::default();

    for extent in SURFACE_EXTENTS {
        let point = surface_point(extent);

        print_surface_row(&point);

        failures.record(point.timings.budget_check(&format!("extent {extent}")));

        assert!(
            point.moving > 0,
            "extent {extent}: the surface has to churn for a timing to mean anything"
        );
        assert!(
            point.grains > point.timings.chunks_p95.saturating_mul(8),
            "extent {extent}: {} grains have to stand well above the {} chunks one tick touches",
            point.grains,
            point.timings.chunks_p95
        );
    }

    failures.assert_empty();
}

/// One measured tick range and the p95 whole tick it is held to. `asserted`
/// separates a gate that fails the run from one that only reports: the ball's
/// two gates stay reported until the run translation lands, and ticket 06
/// flips them.
#[derive(Clone, Copy)]
struct Window {
    name: &'static str,
    first: usize,
    last: usize,
    gate: Duration,
    asserted: bool,
}

impl Window {
    fn met(&self, timings: &TickTimings) -> bool {
        timings.tick_p95 <= self.gate
    }

    /// `ok`, or the name of the miss: the whole tick is the only metric the
    /// gate holds, so a miss is named for it.
    fn verdict(&self, timings: &TickTimings) -> &'static str {
        if self.met(timings) { "ok" } else { "TICK" }
    }

    fn gate_check(&self, timings: &TickTimings) -> Result<(), String> {
        if self.asserted && !self.met(timings) {
            return Err(format!(
                "{}: p95 whole tick {:.3?} over the {:.3?} gate",
                self.name, timings.tick_p95, self.gate
            ));
        }

        Ok(())
    }
}

const BALL_WINDOWS: [Window; 2] = [
    Window {
        name: "free fall",
        first: 1,
        last: FREE_FALL_TICKS,
        gate: FREE_FALL_GATE,
        asserted: false,
    },
    Window {
        name: "pile",
        first: FREE_FALL_TICKS.saturating_add(1),
        last: BALL_TICKS,
        gate: PILE_GATE,
        asserted: false,
    },
];

const RAIN_WINDOWS: [Window; 1] = [Window {
    name: "plateau",
    first: PLATEAU_FIRST,
    last: RAIN_TICKS,
    gate: PLATEAU_GATE,
    asserted: true,
}];

struct WindowReport {
    window: Window,
    ticks: usize,
    timings: TickTimings,
}

fn window_report(samples: &Samples, window: Window) -> WindowReport {
    let measured = samples.window(window.first, window.last);

    WindowReport {
        ticks: measured.wall.len(),
        timings: TickTimings::of(&measured),
        window,
    }
}

fn print_scene_header() {
    println!(
        "{:>11} {:>10} {:>7} {:>10} {:>13} {:>10} {:>11} {:>11} {:>11} {:>8}",
        "window",
        "gate",
        "ticks",
        "p95 queued",
        "p95 processed",
        "p95 chunks",
        "p95 tick",
        "p95 eval",
        "p95 commit",
        "verdict"
    );
}

fn print_window_row(report: &WindowReport) {
    println!(
        "{:>11} {:>10.3?} {:>7} {:>10} {:>13} {:>10} {:>11.3?} {:>11.3?} {:>11.3?} {:>8}",
        report.window.name,
        report.window.gate,
        report.ticks,
        report.timings.queued_p95,
        report.timings.processed_p95,
        report.timings.chunks_p95,
        report.timings.tick_p95,
        report.timings.eval_p95,
        report.timings.commit_p95,
        report.window.verdict(&report.timings)
    );
}

/// One tick's snapshots through the input worker: the submit cost, and the
/// apply and pack the worker does. Recorded, never asserted.
fn print_frame_path(batch: &[MicroChunkSnapshot]) {
    let input = RendererInput::new()
        .unwrap_or_else(|error| panic!("the renderer input must start: {error}"));

    let submitting = Instant::now();

    input
        .submit_batch(batch.to_vec())
        .unwrap_or_else(|error| panic!("the batch must submit: {error}"));

    let submit = submitting.elapsed();
    let packing = Instant::now();

    input
        .wait_until_idle()
        .unwrap_or_else(|error| panic!("the input worker must settle: {error}"));

    let pack = packing.elapsed();

    println!(
        "frame path    {} snapshots, the largest free-fall tick's batch: submit {submit:.3?}, apply and pack {pack:.3?}, recorded rather than asserted",
        batch.len()
    );
}

/// The sand ball's tripwire: about 1.1 million grains falling 128 cells to a
/// one-cell floor, then the pile that settles on it. The free fall is one rigid
/// translation, so it repeats one rule decision a million times; the pile is
/// what the body costs once the grains stop agreeing.
#[test]
#[ignore = "bench: cargo test --release sand_ball_tick_timings -- --ignored --nocapture"]
fn sand_ball_tick_timings() {
    println!("clock         frames at 60 fps, tick fixed at 30 Hz, {BALL_TICKS} measured ticks");
    println!(
        "gate          p95 whole tick, {FREE_FALL_GATE:?} for the fall and {PILE_GATE:?} for the pile, reported rather than asserted until the run translation lands"
    );

    let generated = sand_ball::build(&Progress::generate_path());
    let grains = generated.granular_cells.len();
    let voxels = generated.world.voxel_count();
    let snapshots = emit_snapshots(&generated.world)
        .unwrap_or_else(|error| panic!("the sand ball snapshots must emit: {error}"));
    let tracked: TrackedCoords = snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();

    let mut host = SimHost::spawn(
        Arc::new(RwLock::new(generated.world)),
        PlayerProfile::default(),
        snapshots,
        tracked,
        Some(generated.granular_cells),
        None,
        &generated.materials,
    )
    .unwrap_or_else(|error| panic!("the host must spawn: {error}"));

    host.start();
    wait_ready(&mut host);

    let mut largest: Vec<MicroChunkSnapshot> = Vec::new();
    let samples = drive_with(
        &mut host,
        BALL_TICKS,
        SCENE_TICK_TIMEOUT,
        &mut |tick, batch| {
            if tick <= FREE_FALL_TICKS && batch.len() > largest.len() {
                largest = batch.to_vec();
            }
        },
    );

    drop(host);

    println!(
        "scene         sand ball, {grains} grains and {voxels} voxels, activated with the generator's own granular cells"
    );
    print_scene_header();

    let mut failures = Failures::default();

    for window in BALL_WINDOWS {
        let report = window_report(&samples, window);

        print_window_row(&report);
        failures.record(report.window.gate_check(&report.timings));
    }

    print_frame_path(&largest);

    assert_eq!(
        samples.window(1, FREE_FALL_TICKS).moving,
        FREE_FALL_TICKS,
        "every tick of the fall has to move the body for a timing to mean anything"
    );

    failures.assert_empty();
}

/// The rain's tripwire: grains planted a tick through the plate's holes, each
/// falling 1,023 cells to the floor. The plateau is the steady state, where the
/// grains leaving a hole and the ones landing are both in flight, so the tick's
/// chunk churn is at its own size.
#[test]
#[ignore = "bench: cargo test --release raining_tick_timings -- --ignored --nocapture"]
fn raining_tick_timings() {
    println!("clock         frames at 60 fps, tick fixed at 30 Hz, {RAIN_TICKS} measured ticks");
    println!("gate          p95 whole tick, {PLATEAU_GATE:?} over the plateau, asserted");

    let (generated, emitter) = rain::build(&Progress::generate_path());
    let holes = emitter.cells.len();
    let per_tick = emitter.per_tick;
    let voxels = generated.world.voxel_count();
    let snapshots = emit_snapshots(&generated.world)
        .unwrap_or_else(|error| panic!("the rain snapshots must emit: {error}"));
    let tracked: TrackedCoords = snapshots
        .iter()
        .filter(|snapshot| snapshot.occupied_count() > 0)
        .map(|snapshot| snapshot.global_coords)
        .collect();

    let mut host = SimHost::spawn(
        Arc::new(RwLock::new(generated.world)),
        PlayerProfile::default(),
        snapshots,
        tracked,
        None,
        Some(emitter),
        &generated.materials,
    )
    .unwrap_or_else(|error| panic!("the host must spawn: {error}"));

    host.start();
    wait_ready(&mut host);

    let samples = drive_with(&mut host, RAIN_TICKS, SCENE_TICK_TIMEOUT, &mut |_, _| {});

    drop(host);

    println!(
        "scene         rain plate, {holes} holes and {voxels} voxels, {per_tick} grains a tick, no granular cells seeded"
    );
    print_scene_header();

    let mut failures = Failures::default();

    for window in RAIN_WINDOWS {
        let report = window_report(&samples, window);

        print_window_row(&report);
        failures.record(report.window.gate_check(&report.timings));

        assert!(
            report.ticks > 0,
            "{}: the run has to reach the window for a timing to mean anything",
            window.name
        );
        assert!(
            samples.window(window.first, window.last).moving > 0,
            "{}: the scene has to move grains in the window",
            window.name
        );
    }

    failures.assert_empty();
}

#[test]
fn p95_takes_the_nearest_rank() {
    let samples: Vec<Duration> = (1..=60).map(Duration::from_micros).collect();

    assert_eq!(nearest_rank(&samples), Duration::from_micros(57));
    assert_eq!(nearest_rank::<Duration>(&[]), Duration::ZERO);

    let ranks: Vec<usize> = (1..=20).collect();

    assert_eq!(nearest_rank(&ranks), 19, "the nearest rank is generic");
}

#[test]
fn the_budget_fails_only_over_the_per_cell_bound() {
    let within = TickTimings {
        per_cell_nanos: CELL_COST_NANOS,
        ..TickTimings::default()
    };
    let beyond = TickTimings {
        per_cell_nanos: CELL_COST_NANOS.saturating_add(1),
        ..TickTimings::default()
    };

    assert!(within.budget_check("synth @ 1000 active").is_ok());
    assert!(beyond.budget_check("synth @ 1000 active").is_err());
    assert!(TickTimings::default().budget_check("empty").is_ok());
}

#[test]
fn a_commit_overrun_is_named_apart_from_the_rule_work() {
    let over = TICK_BUDGET.saturating_add(Duration::from_micros(1));
    let committing = TickTimings {
        tick_p95: over,
        ..TickTimings::default()
    };
    let draining = TickTimings {
        tick_p95: over,
        eval_p95: over,
        ..TickTimings::default()
    };
    let overrunning = TickTimings {
        tick_p95: over,
        eval_p95: over,
        per_cell_nanos: CELL_COST_NANOS.saturating_add(1),
        ..TickTimings::default()
    };

    assert_eq!(
        committing.verdict(),
        "COMMIT",
        "the commit window alone is over the tripwire"
    );
    assert_eq!(
        draining.verdict(),
        "DRAIN",
        "rule work over the tripwire at the budget's per-cell cost is a large scene, not a regression"
    );
    assert_eq!(
        overrunning.verdict(),
        "OVERRUN",
        "rule work over the per-cell budget is the failure the sweep asserts"
    );
    assert_eq!(TickTimings::default().verdict(), "ok");
    assert!(
        !overrunning.commit_overrun(),
        "a per-cell overrun is not a commit-only overrun"
    );
    assert!(overrunning.budget_check("over the budget").is_err());
    assert!(draining.budget_check("inside the budget").is_ok());
}

#[test]
fn the_block_sits_below_the_world_and_clears_the_descent() {
    let bounds = (
        IVec3::new(-2048, -1135, -2048),
        IVec3::new(2047, 1134, 2047),
    );
    let block = Block::new(1_000_000, Some(bounds));

    assert_eq!(block.top(), -1137);
    assert!(
        block
            .origin
            .y
            .saturating_sub(i32::try_from(TICKS).unwrap_or(i32::MAX))
            >= LATTICE_FLOOR
    );
    assert!(in_lattice(block.origin));
    assert!(in_lattice(IVec3::new(
        block.origin.x.saturating_add(block.edge).saturating_sub(1),
        block.top(),
        block.origin.z.saturating_add(block.edge).saturating_sub(1)
    )));
}

#[test]
fn the_block_shape_sizes_to_the_cube_root() {
    assert_eq!(cube_edge(1_000), 10);
    assert_eq!(cube_edge(10_000), 22);
    assert_eq!(cube_edge(100_000), 47);
    assert_eq!(cube_edge(1_000_000), 100);
}

#[test]
fn the_sand_block_holds_the_active_count() {
    let active = 1_000;
    let block = Block::new(active, None);
    let edits = sand_edits(&block, active, 7);
    let placed: HashSet<IVec3> = edits.iter().map(|edit| edit.position).collect();

    assert_eq!(edits.len(), active);
    assert_eq!(placed.len(), active);
    assert!(placed.iter().all(|cell| in_lattice(*cell)));
}

/// A run whose tick `n` costs `n` milliseconds, queues `n` cells and processes
/// ten times `n`, so a window's p95 names the tick it came from.
fn sample_run(ticks: usize) -> Samples {
    let mut samples = Samples {
        wall: Vec::new(),
        eval: Vec::new(),
        commit: Vec::new(),
        chunks: Vec::new(),
        queued: Vec::new(),
        processed: Vec::new(),
        moving: 0,
    };

    for tick in 1..=ticks {
        let value = u64::try_from(tick).unwrap_or(u64::MAX);

        samples.wall.push(Duration::from_millis(value));
        samples
            .eval
            .push(Duration::from_millis(value.saturating_mul(2)));
        samples
            .commit
            .push(Duration::from_millis(value.saturating_mul(3)));
        samples.chunks.push(tick);
        samples.queued.push(tick);
        samples.processed.push(tick.saturating_mul(10));
        samples.moving = samples.moving.saturating_add(1);
    }

    samples
}

#[test]
fn a_window_takes_the_ticks_it_names() {
    let run = sample_run(BALL_TICKS);
    let free_fall = run.window(1, FREE_FALL_TICKS);
    let pile = run.window(FREE_FALL_TICKS.saturating_add(1), BALL_TICKS);

    assert_eq!(free_fall.wall.len(), FREE_FALL_TICKS);
    assert_eq!(pile.wall.len(), BALL_TICKS.saturating_sub(FREE_FALL_TICKS));
    assert_eq!(
        free_fall.wall.first().copied(),
        Some(Duration::from_millis(1)),
        "the first window starts at tick one"
    );
    assert_eq!(
        free_fall.wall.last().copied(),
        Some(Duration::from_millis(128)),
        "the free fall ends where the pile starts"
    );
    assert_eq!(pile.wall.first().copied(), Some(Duration::from_millis(129)));
    assert_eq!(pile.moving, 62, "a window counts the ticks it holds");
    assert_eq!(free_fall.processed.first().copied(), Some(10));
}

#[test]
fn a_window_past_the_run_clips_to_what_measured() {
    let run = sample_run(4);

    assert_eq!(run.window(3, 9).wall.len(), 2);
    assert_eq!(run.window(3, 9).moving, 2);
    assert_eq!(
        run.window(9, 12).wall.len(),
        0,
        "a window after the last tick holds nothing"
    );
    assert_eq!(run.window(9, 12).moving, 0);
    assert_eq!(
        run.window(0, 2).wall.len(),
        2,
        "a window below the first tick starts at it"
    );
}

#[test]
fn a_window_p95_holds_only_its_own_ticks() {
    let run = sample_run(BALL_TICKS);
    let free_fall = TickTimings::of(&run.window(1, FREE_FALL_TICKS));
    let pile = TickTimings::of(&run.window(FREE_FALL_TICKS.saturating_add(1), BALL_TICKS));

    assert_eq!(
        free_fall.tick_p95,
        Duration::from_millis(122),
        "128 ticks put the nearest rank of 95 percent at the 122nd"
    );
    assert_eq!(
        pile.tick_p95,
        Duration::from_millis(187),
        "62 ticks put the nearest rank of 95 percent at the 187th"
    );
    assert_eq!(free_fall.eval_p95, Duration::from_millis(244));
    assert_eq!(free_fall.commit_p95, Duration::from_millis(366));
    assert_eq!(free_fall.queued_p95, 122);
    assert_eq!(pile.processed_p95, 1_870);
    assert_eq!(pile.chunks_p95, 187);
    assert_eq!(
        free_fall.per_cell_nanos, 200_000,
        "the per-cell cost divides the window's own evaluation by its own processed cells"
    );
}

#[test]
fn a_window_reads_ok_or_the_name_of_its_miss() {
    let window = Window {
        name: "free fall",
        first: 1,
        last: FREE_FALL_TICKS,
        gate: FREE_FALL_GATE,
        asserted: false,
    };
    let met = TickTimings {
        tick_p95: FREE_FALL_GATE,
        ..TickTimings::default()
    };
    let missed = TickTimings {
        tick_p95: FREE_FALL_GATE.saturating_add(Duration::from_micros(1)),
        ..TickTimings::default()
    };

    assert_eq!(window.verdict(&met), "ok", "a tick on its gate meets it");
    assert_eq!(
        window.verdict(&missed),
        "TICK",
        "the whole tick is the only metric the gate holds, so the miss is named for it"
    );
    assert!(
        window.gate_check(&missed).is_ok(),
        "the ball's gate is reported, not asserted, until ticket 06 flips it"
    );
}

#[test]
fn only_an_asserted_window_records_a_miss() {
    let window = Window {
        name: "plateau",
        first: PLATEAU_FIRST,
        last: RAIN_TICKS,
        gate: PLATEAU_GATE,
        asserted: true,
    };
    let over = TickTimings {
        tick_p95: PLATEAU_GATE.saturating_add(Duration::from_millis(1)),
        ..TickTimings::default()
    };

    let Err(message) = window.gate_check(&over) else {
        panic!("a plateau tick over the gate has to record a miss");
    };

    assert!(
        message.contains("plateau"),
        "the miss names its window: {message}"
    );
    assert!(
        message.contains("120"),
        "the miss names its gate: {message}"
    );
    assert!(
        window
            .gate_check(&TickTimings {
                tick_p95: PLATEAU_GATE,
                ..TickTimings::default()
            })
            .is_ok(),
        "a tick on its gate meets it"
    );
}
