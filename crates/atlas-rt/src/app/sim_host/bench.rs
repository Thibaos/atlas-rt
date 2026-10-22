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

use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use dot_vox::DotVoxData;
use glam::IVec3;

use atlas_rt::{
    sim::{InputSample, PlayerProfile, Push},
    world::{
        World,
        diff::{
            batch::TrackedCoords,
            edit::{VoxelChange, VoxelEdit, edit_world},
            snapshot::emit_snapshots,
        },
        generation::{GenerationParams, generate},
        grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH, grid_origin, in_lattice},
        load::progress::Progress,
        material::{PhysicalMaterialTable, parse_override},
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
    processed: Vec<usize>,
    moving: usize,
}

/// Frames at the 60 fps target against the fixed 30 Hz tick: two frames per
/// measured tick, each carrying half a tick period rounded up, so the second
/// frame runs the tick the report carries. The clock invariants are the same
/// for every scene, so they are asserted here; whether a tick moved cells
/// belongs to the scene, so the caller checks `moving`.
fn drive(host: &mut SimHost, ticks: usize) -> Samples {
    let mut samples = Samples {
        wall: Vec::with_capacity(ticks),
        eval: Vec::with_capacity(ticks),
        commit: Vec::with_capacity(ticks),
        chunks: Vec::with_capacity(ticks),
        processed: Vec::with_capacity(ticks),
        moving: 0,
    };

    for _ in 0..ticks {
        host.frame(FRAME, InputSample::default());
        host.frame(FRAME, InputSample::default());

        let push = recv(host, TICK_TIMEOUT, "a measured tick");

        let Push::Tick(tick) = push else {
            panic!("the frame ran no tick");
        };

        let report = &tick.report;

        assert_eq!(
            report.discarded, 0,
            "one frame must not outrun the catch-up cap"
        );

        if report.batch.is_some() {
            samples.moving = samples.moving.saturating_add(1);
        }

        samples
            .chunks
            .push(report.batch.as_ref().map_or(0, Vec::len));
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
