use std::collections::HashMap;
use std::ops::Neg;

use dot_vox::{DotVoxData, Rotation, Voxel};
use glam::{DMat4, DQuat, DVec3, DVec4, IVec3, IVec4, UVec3, Vec4Swizzles};
use tracing::info;

use std::hash::{Hash, Hasher};

#[cfg(feature = "map-oracle")]
use rustc_hash::FxHashMap;
use rustc_hash::FxHasher;

use super::grid;
use super::load::scene_graph::{SceneGraphTraverser, VoxelPlacement};
use super::{BoundsPolicy, ModelSpec, World, scene_fixture};

const TRANSLATIONS: &[i32] = &[
    0, 1, -1, 3, -3, 2047, -2047, 2048, -2048, 2049, -2049, 100_000, -100_000, 1_000_000,
    -1_000_000,
];

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

pub(crate) fn rotation_bytes() -> Vec<u8> {
    (0u8..128)
        .filter(|byte| {
            let first = byte & 0b11;
            let second = (byte >> 2) & 0b11;
            first != 0b11 && second != 0b11 && first != second
        })
        .collect()
}

pub(crate) fn u8_below(rng: &mut Rng, bound: u64) -> u8 {
    u8::try_from(rng.below(bound)).unwrap_or(u8::MAX)
}

pub(super) fn expect_error<T, E>(result: Result<T, E>, message: &str) -> E {
    match result {
        Ok(_) => panic!("{message}"),
        Err(error) => error,
    }
}

/// Asserts two Worlds hold the same content, count, and bounds, whatever store
/// backs each. `case` names the failing input.
#[cfg(feature = "map-oracle")]
pub(crate) fn assert_worlds_agree(region: &World, map: &World, case: &str) {
    assert_eq!(
        region.voxel_count(),
        map.voxel_count(),
        "{case}: region and map voxel counts"
    );
    assert_eq!(
        region.voxel_bounds(),
        map.voxel_bounds(),
        "{case}: region and map bounds"
    );

    let region_content: HashMap<IVec3, u8> = region.iter_voxels().collect();
    let map_content: HashMap<IVec3, u8> = map.iter_voxels().collect();

    assert_eq!(
        region_content, map_content,
        "{case}: region and map content"
    );

    for (position, material) in &region_content {
        assert_eq!(
            region.get_voxel(position),
            Some(*material),
            "{case}: region lookup at {position}"
        );
        assert_eq!(
            map.get_voxel(position),
            Some(*material),
            "{case}: map lookup at {position}"
        );
        assert!(
            region.contains(position),
            "{case}: region misses {position}"
        );
        assert!(map.contains(position), "{case}: map misses {position}");
    }
}

/// The per-Region break-even from the storage spec: a touched Region needs
/// about this many voxels to offset its index and blob.
#[cfg(feature = "map-oracle")]
pub(crate) const REGION_BREAK_EVEN_VOXELS: usize = 4_000;

/// The per-Micro-chunk form of the same break-even, from the storage spec.
#[cfg(feature = "map-oracle")]
pub(crate) const MICRO_CHUNK_BREAK_EVEN_CELLS: usize = 15;

/// Prints the voxels a World holds per touched Region and per touched
/// Micro-chunk, with how many of each fall below the storage spec's break-even,
/// so an asset test records where the asset sits.
#[cfg(feature = "map-oracle")]
pub(crate) fn report_region_density(world: &World, case: &str) {
    let mut per_region = vec![0usize; grid::REGION_COUNT];
    let mut per_chunk: FxHashMap<[i32; 3], usize> = FxHashMap::default();

    for (position, _) in world.iter_voxels() {
        let slot = grid::region_id(grid::region_index_of(position)) as usize;

        if let Some(count) = per_region.get_mut(slot) {
            *count = count.saturating_add(1);
        }

        let chunk = per_chunk
            .entry(grid::grid_origin(position, grid::MICRO_CHUNK_LENGTH).to_array())
            .or_default();

        *chunk = chunk.saturating_add(1);
    }

    let voxels = world.voxel_count();
    let regions: Vec<usize> = per_region.into_iter().filter(|count| *count > 0).collect();
    let chunks: Vec<usize> = per_chunk.into_values().collect();
    let region_count = regions.len();
    let chunk_count = chunks.len();
    let mean_region = voxels as f64 / region_count.max(1) as f64;
    let mean_chunk = voxels as f64 / chunk_count.max(1) as f64;
    let min_region = regions.iter().copied().min().unwrap_or(0);
    let min_chunk = chunks.iter().copied().min().unwrap_or(0);
    let thin_regions = regions
        .iter()
        .filter(|count| **count < REGION_BREAK_EVEN_VOXELS)
        .count();
    let thin_chunks = chunks
        .iter()
        .filter(|count| **count < MICRO_CHUNK_BREAK_EVEN_CELLS)
        .count();

    println!(
        "{case}: {voxels} voxels in {region_count} Regions (mean {mean_region:.0}/Region, min \
         {min_region}/Region, {thin_regions} below {REGION_BREAK_EVEN_VOXELS}) and {chunk_count} \
         Micro-chunks (mean {mean_chunk:.1}/chunk, min {min_chunk}/chunk, {thin_chunks} below \
         {MICRO_CHUNK_BREAK_EVEN_CELLS})"
    );
}

fn random_size(rng: &mut Rng) -> (u32, u32, u32) {
    let mut axis = || {
        if rng.below(8) == 0 {
            250 + rng.below(7) as u32
        } else {
            1 + rng.below(8) as u32
        }
    };
    (axis(), axis(), axis())
}

fn random_voxels(rng: &mut Rng, size: (u32, u32, u32)) -> Vec<Voxel> {
    let count = 1 + rng.below(12) as usize;
    (0..count)
        .map(|_| Voxel {
            x: rng.below(size.0 as u64) as u8,
            y: rng.below(size.1 as u64) as u8,
            z: rng.below(size.2 as u64) as u8,
            i: rng.below(256) as u8,
        })
        .collect()
}

fn random_translation(rng: &mut Rng) -> [i32; 3] {
    let mut pick = || {
        if rng.below(2) == 0 {
            TRANSLATIONS[rng.below(TRANSLATIONS.len() as u64) as usize]
        } else {
            (rng.below(4_000_001) as i64 - 2_000_000) as i32
        }
    };
    [pick(), pick(), pick()]
}

fn collected_models(data: &DotVoxData) -> Vec<(IVec3, Rotation, UVec3, &[Voxel])> {
    let mut world = World::default();
    let mut loader = SceneGraphTraverser {
        world: &mut world,
        policy: BoundsPolicy::Clip,
        scene: data,
        models: Vec::new(),
    };
    loader.traverse();
    std::mem::take(&mut loader.models)
}

fn rounded(value: f64) -> i32 {
    (value + 0.5).floor() as i32
}

fn exact_oracle_transform(translation: IVec3, rotation: Rotation, size: UVec3) -> DMat4 {
    let (quat, scale) = rotation.to_quat_scale();
    let quat = DQuat::from_xyzw(
        f64::from(quat[0]),
        f64::from(quat[1]),
        f64::from(quat[2]),
        f64::from(quat[3]),
    );
    let quat = DQuat::from_xyzw(quat.x, quat.z, -quat.y, quat.w);
    let scale = DVec3::new(
        f64::from(scale[0]),
        f64::from(scale[2]),
        f64::from(scale[1]),
    );

    let translation = DVec3::new(
        f64::from(translation.x),
        f64::from(translation.z),
        -f64::from(translation.y),
    );

    let mut offset = DVec3::new(
        if size.x & 1 == 1 { 0.5 } else { 0.0 },
        if size.z & 1 == 1 { 0.5 } else { 0.0 },
        if size.y & 1 == 1 { -0.5 } else { 0.0 },
    );
    offset = quat.mul_vec3(offset);

    let center =
        quat.mul_vec3(DVec3::new(f64::from(size.x), f64::from(size.z), f64::from(size.y)) * 0.5);

    DMat4::from_scale_rotation_translation(scale, quat, translation - center * scale + offset)
}

fn exact_oracle_map(data: &DotVoxData) -> HashMap<IVec3, u8> {
    let mut map = HashMap::new();
    for (translation, rotation, size, voxels) in collected_models(data) {
        let transform = exact_oracle_transform(translation, rotation, size);
        for voxel in voxels {
            let local = DVec3::new(
                f64::from(voxel.x),
                f64::from(voxel.z),
                f64::from(size.y) - f64::from(voxel.y) - 1.0,
            );
            let placed = transform.mul_vec4(DVec4::new(local.x, local.y, local.z, 1.0));
            let position = IVec3::new(rounded(placed.x), rounded(placed.y), -rounded(placed.z));
            if grid::in_lattice(position) {
                map.insert(position, voxel.i);
            }
        }
    }
    map
}

fn legacy_float_map(data: &DotVoxData) -> HashMap<IVec3, u8> {
    let mut map = HashMap::new();
    for (translation, rotation, size, voxels) in collected_models(data) {
        let transform = SceneGraphTraverser::legacy_float_transform(translation, rotation, size);
        for voxel in voxels.iter().copied() {
            let local = UVec3::new(
                u32::from(voxel.x),
                u32::from(voxel.z),
                size.y.strict_sub(u32::from(voxel.y)).strict_sub(1),
            )
            .as_ivec3();
            let position = IVec4::new(local.x, local.y, local.z, 1).as_vec4();
            let position = (transform.mul_vec4(position)).xyz().as_ivec3();
            let position = IVec3::new(position.x, position.y, position.z.neg());
            if grid::in_lattice(position) {
                map.insert(position, voxel.i);
            }
        }
    }
    map
}

fn production_map(data: &DotVoxData) -> HashMap<IVec3, u8> {
    let (world, _) = World::new_clipped(data);
    world.iter_voxels().collect()
}

pub(crate) fn random_specs(rng: &mut Rng, rotation: u8) -> Vec<ModelSpec> {
    let mut specs = Vec::new();
    for _ in 0..(1 + rng.below(4)) {
        let size = random_size(rng);
        specs.push(ModelSpec {
            size,
            voxels: random_voxels(rng, size),
            rotation,
            translation: random_translation(rng),
        });
    }
    specs
}

#[test]
fn integer_placement_matches_exact_oracle_on_randomized_scenes() {
    let mut rng = Rng::new(0x5EED_2024);
    let valid_rotations = rotation_bytes();

    for rotation in &valid_rotations {
        for _ in 0..8 {
            let specs = random_specs(&mut rng, *rotation);
            let data = scene_fixture(&specs);
            assert_eq!(
                production_map(&data),
                exact_oracle_map(&data),
                "rotation {rotation:#010b}, specs {specs:?}"
            );
        }
    }

    for _ in 0..32 {
        let specs: Vec<ModelSpec> = (0..(1 + rng.below(4)))
            .map(|_| {
                let size = random_size(&mut rng);
                ModelSpec {
                    size,
                    voxels: random_voxels(&mut rng, size),
                    rotation: valid_rotations[rng.below(valid_rotations.len() as u64) as usize],
                    translation: random_translation(&mut rng),
                }
            })
            .collect();
        let data = scene_fixture(&specs);
        assert_eq!(
            production_map(&data),
            exact_oracle_map(&data),
            "specs {specs:?}"
        );
    }
}

#[test]
fn integer_placement_matches_legacy_float_on_exact_quaternion_classes() {
    let exact_pairs = [(0u8, 1u8), (1, 2), (2, 0)];
    let mut rng = Rng::new(0x00C0_FFEE);

    for rotation in rotation_bytes() {
        let pair = (rotation & 0b11, (rotation >> 2) & 0b11);
        if !exact_pairs.contains(&pair) {
            continue;
        }

        for _ in 0..8 {
            let specs = random_specs(&mut rng, rotation);
            let data = scene_fixture(&specs);
            assert_eq!(
                production_map(&data),
                legacy_float_map(&data),
                "rotation {rotation:#010b}, specs {specs:?}"
            );
        }
    }
}

#[test]
#[ignore = "asset: cargo test --release church_matches_legacy_float_path -- --ignored --nocapture"]
fn church_matches_legacy_float_path() {
    let data = dot_vox::load("assets/church.vox").unwrap();
    assert_eq!(production_map(&data), legacy_float_map(&data));
}

#[test]
#[ignore = "asset: cargo test --release bistro_matches_legacy_float_path -- --ignored --nocapture"]
fn bistro_matches_legacy_float_path() {
    let data = dot_vox::load("assets/bistro.vox").unwrap();
    assert_eq!(production_map(&data), legacy_float_map(&data));
}

#[test]
fn placement_known_example_from_legacy_pipeline() {
    let specs = [ModelSpec {
        size: (1, 1, 1),
        voxels: vec![Voxel {
            x: 0,
            y: 0,
            z: 0,
            i: 7,
        }],
        rotation: 0b0001,
        translation: [5, 7, 9],
    }];
    let world = World::new(&scene_fixture(&specs));

    assert_eq!(world.voxel_count(), 1);
    assert_eq!(
        world.get_voxel(&IVec3::new(5, 8, 6)),
        Some(7),
        "the legacy pipeline evaluates this origin voxel to m = (5, 8, -6), so the world position after the final z negation is (5, 8, 6)"
    );
}

fn serial_map(data: &DotVoxData) -> HashMap<IVec3, u8> {
    let mut map = HashMap::new();

    for (translation, rotation, size, voxels) in collected_models(data) {
        let placement = VoxelPlacement::new(translation, rotation, size);

        for voxel in voxels {
            let position = placement.place(*voxel);

            if grid::in_lattice(position) {
                map.insert(position, voxel.i);
            }
        }
    }

    map
}

fn serial_bounds(map: &HashMap<IVec3, u8>) -> Option<(IVec3, IVec3)> {
    map.keys().copied().fold(None, |bounds, position| {
        Some(match bounds {
            Some((min, max)) => (min.min(position), max.max(position)),
            None => (position, position),
        })
    })
}

/// Asserts the live Region build equals the serial placement oracle in content,
/// count, and bounds.
fn assert_matches_serial(data: &DotVoxData, case: &str) {
    let (world, _) = World::new_clipped(data);
    let serial = serial_map(data);

    assert_eq!(world.voxel_count(), serial.len(), "voxel count, {case}");
    assert_eq!(
        world.voxel_bounds(),
        serial_bounds(&serial),
        "bounds, {case}"
    );
    assert_eq!(
        world.iter_voxels().collect::<HashMap<IVec3, u8>>(),
        serial,
        "content, {case}"
    );
}

fn content_hash(map: &HashMap<IVec3, u8>) -> u64 {
    let mut hash = 0u64;

    for (position, voxel) in map {
        let mut hasher = FxHasher::default();
        position.hash(&mut hasher);
        hash ^= hasher.finish().wrapping_add(u64::from(*voxel));
    }

    hash
}

fn load_asset(path: &str) -> DotVoxData {
    match dot_vox::load(path) {
        Ok(data) => data,
        Err(error) => panic!("failed to load {path}: {error}"),
    }
}

#[test]
fn parallel_load_matches_serial_oracle_on_randomized_scenes() {
    let mut rng = Rng::new(0x0501_10AD);
    let valid_rotations = rotation_bytes();

    for rotation in &valid_rotations {
        for _ in 0..4 {
            let specs = random_specs(&mut rng, *rotation);
            let data = scene_fixture(&specs);
            assert_matches_serial(
                &data,
                &format!("rotation {rotation:#010b}, specs {specs:?}"),
            );
        }
    }

    let mut rotations = valid_rotations.iter().copied().cycle();

    for _ in 0..64 {
        let specs: Vec<ModelSpec> = (0..(1 + rng.below(4)))
            .map(|_| {
                let size = random_size(&mut rng);
                ModelSpec {
                    size,
                    voxels: random_voxels(&mut rng, size),
                    rotation: rotations.next().unwrap_or(0b0001),
                    translation: random_translation(&mut rng),
                }
            })
            .collect();
        let data = scene_fixture(&specs);
        assert_matches_serial(&data, &format!("specs {specs:?}"));
    }
}

#[test]
fn overlapping_models_keep_the_last_serial_write() {
    let specs = [
        ModelSpec {
            size: (2, 2, 2),
            voxels: vec![Voxel {
                x: 1,
                y: 1,
                z: 1,
                i: 3,
            }],
            rotation: 0b0001,
            translation: [0, 0, 0],
        },
        ModelSpec {
            size: (2, 2, 2),
            voxels: vec![Voxel {
                x: 1,
                y: 1,
                z: 1,
                i: 9,
            }],
            rotation: 0b0001,
            translation: [0, 0, 0],
        },
    ];
    let data = scene_fixture(&specs);
    let map = production_map(&data);

    assert_eq!(map, serial_map(&data));
    assert_eq!(
        map.values().collect::<Vec<_>>(),
        vec![&9],
        "the second model's material wins the shared position"
    );
}

#[test]
#[ignore = "asset: cargo test --release church_matches_parallel_load -- --ignored --nocapture"]
fn church_matches_parallel_load() {
    asset_differential("assets/church.vox");
}

#[test]
#[ignore = "asset: cargo test --release bistro_matches_parallel_load -- --ignored --nocapture"]
fn bistro_matches_parallel_load() {
    asset_differential("assets/bistro.vox");
}

fn asset_differential(path: &str) {
    let data = load_asset(path);
    let parallel_map = production_map(&data);
    let serial = serial_map(&data);

    assert_eq!(
        parallel_map.len(),
        serial.len(),
        "voxel count diverged from the serial oracle for {path}"
    );
    assert_eq!(
        parallel_map, serial,
        "content diverged from the serial oracle for {path}"
    );

    let parallel_hash = content_hash(&parallel_map);
    assert_eq!(
        parallel_hash,
        content_hash(&serial),
        "content hash diverged from the serial oracle for {path}"
    );
    info!(
        "{path}: {} voxels, content hash {parallel_hash:016x}",
        parallel_map.len()
    );
}
