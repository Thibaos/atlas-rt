//! The tick tripwire sweep: p95 tick wall time, evaluation plus commit,
//! crossing 4 ms at the 60 fps target with the fixed 30 Hz tick, over three
//! occupancy points and four active-cell counts. Commit is the break-out
//! sub-metric; active-cell throughput is not the tripwire, because any cost
//! in the tick can cross it.

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
            snapshot::{MicroChunkSnapshot, emit_snapshots},
        },
        grid::{LATTICE_HALF_EXTENT, MICRO_CHUNK_LENGTH, grid_origin, in_lattice},
        material::{PhysicalMaterialTable, parse_override},
    },
};

use super::SimHost;

const TICK_BUDGET: Duration = Duration::from_millis(4);
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

struct SweepPoint {
    occupancy: &'static str,
    active: usize,
    voxels: usize,
    edge: i32,
    layers: i32,
    p95: Duration,
    eval_p95: Duration,
    commit_p95: Duration,
}

#[test]
#[ignore = "bench: cargo test --release tick_tripwire_sweep -- --ignored --nocapture"]
fn tick_tripwire_sweep() {
    println!("budget        {TICK_BUDGET:.3?} p95 tick wall time, evaluation plus commit");
    println!(
        "clock         frames at 60 fps, tick fixed at 30 Hz, {TICKS} measured ticks per point"
    );
    println!(
        "worst case    sand layers one empty cell apart, so every active cell moves every tick"
    );
    print_header();

    let mut points: Vec<SweepPoint> = Vec::new();
    let mut failures = String::new();

    for occupancy in OCCUPANCIES {
        let source = Source::new(occupancy);

        for active in ACTIVE {
            let point = sweep(&source, occupancy, active);

            print_row(&point);

            if let Err(message) = budget_check(&point) {
                failures.push_str(&message);
                failures.push('\n');
            }

            points.push(point);
        }
    }

    print_crossings(&points);

    assert!(failures.is_empty(), "{failures}");
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
            Self::Asset(data) => {
                let (world, _clipped) = World::new_clipped(data);

                world
            }
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
/// open cell below it and the whole block moves one cell per tick, keeping
/// every active cell in the active queue for the whole run.
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
        &table,
    )
    .unwrap_or_else(|error| panic!("the host must spawn: {error}"));

    host.start();
    wait_ready(&mut host);

    let samples = drive(&mut host, active);
    drop(host);

    SweepPoint {
        occupancy,
        active,
        voxels,
        edge: block.edge,
        layers: block.layers,
        p95: nearest_rank(&samples.wall),
        eval_p95: nearest_rank(&samples.eval),
        commit_p95: nearest_rank(&samples.commit),
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
}

/// Frames at the 60 fps target against the fixed 30 Hz tick: two frames per
/// measured tick, each carrying half a tick period rounded up, so the second
/// frame runs the tick the report carries.
fn drive(host: &mut SimHost, active: usize) -> Samples {
    let mut samples = Samples {
        wall: Vec::with_capacity(TICKS),
        eval: Vec::with_capacity(TICKS),
        commit: Vec::with_capacity(TICKS),
    };

    for _ in 0..TICKS {
        host.frame(FRAME, InputSample::default());
        host.frame(FRAME, InputSample::default());

        let push = recv(host, TICK_TIMEOUT, "a measured tick");

        let Push::Tick(tick) = push else {
            panic!("the frame ran no tick");
        };

        let report = &tick.report;

        assert_eq!(
            report.ticks, 1,
            "the second frame of the pair must run one tick"
        );
        assert_eq!(
            report.discarded, 0,
            "one frame must not outrun the catch-up cap"
        );

        let occupancy = report
            .batches
            .iter()
            .flatten()
            .map(MicroChunkSnapshot::occupied_count)
            .sum::<usize>();

        assert_eq!(
            occupancy, active,
            "every active cell must sit in a touched chunk: {active} expected, {occupancy} reported"
        );

        samples
            .wall
            .push(report.tick_time.saturating_add(report.commit_time));
        samples.eval.push(report.tick_time);
        samples.commit.push(report.commit_time);

        host.apply(Push::Tick(tick), &mut |_batch| Ok(()));
    }

    samples
}

fn nearest_rank(samples: &[Duration]) -> Duration {
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

fn budget_check(point: &SweepPoint) -> Result<(), String> {
    if point.p95 > TICK_BUDGET {
        return Err(format!(
            "{} @ {} active: p95 tick {:.3?} overruns {TICK_BUDGET:.3?}",
            point.occupancy, point.active, point.p95
        ));
    }

    Ok(())
}

fn print_header() {
    println!(
        "{:>9} {:>9} {:>11} {:>9} {:>11} {:>11} {:>11} {:>8}",
        "occupancy", "active", "voxels", "block", "p95 tick", "p95 eval", "p95 commit", "budget"
    );
}

fn print_row(point: &SweepPoint) {
    let block = format!("{}x{}", point.edge, point.layers);
    let verdict = if point.p95 > TICK_BUDGET {
        "OVERRUN"
    } else {
        "ok"
    };

    println!(
        "{:>9} {:>9} {:>11} {:>9} {:>11.3?} {:>11.3?} {:>11.3?} {:>8}",
        point.occupancy,
        point.active,
        point.voxels,
        block,
        point.p95,
        point.eval_p95,
        point.commit_p95,
        verdict
    );
}

fn print_crossings(points: &[SweepPoint]) {
    println!("crossing point, the first active count over budget:");

    for occupancy in OCCUPANCIES {
        let crossing = points
            .iter()
            .find(|point| point.occupancy == occupancy && point.p95 > TICK_BUDGET);

        match crossing {
            Some(point) => println!("  {occupancy:>9} {}", point.active),
            None => println!("  {occupancy:>9} none up to 1M"),
        }
    }
}

#[test]
fn p95_takes_the_nearest_rank() {
    let samples: Vec<Duration> = (1..=60).map(Duration::from_micros).collect();

    assert_eq!(nearest_rank(&samples), Duration::from_micros(57));
    assert_eq!(nearest_rank(&[]), Duration::ZERO);
}

#[test]
fn the_budget_fails_only_over_4ms() {
    let point = SweepPoint {
        occupancy: "synth",
        active: 1_000,
        voxels: 0,
        edge: 10,
        layers: 10,
        p95: TICK_BUDGET,
        eval_p95: Duration::ZERO,
        commit_p95: Duration::ZERO,
    };

    assert!(budget_check(&point).is_ok());

    let mut over = SweepPoint {
        p95: TICK_BUDGET.saturating_add(Duration::from_micros(1)),
        ..point
    };

    assert!(budget_check(&over).is_err());

    over.p95 = Duration::ZERO;
    assert!(budget_check(&over).is_ok());
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
