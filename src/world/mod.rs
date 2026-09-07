use std::{
    collections::{HashMap, hash_map::Entry},
    fmt::Display,
    sync::{Mutex, MutexGuard},
};

use dot_vox::{DotVoxData, Voxel};
use glam::IVec3;
use rayon::prelude::*;
use rustc_hash::FxBuildHasher;

use crate::world::scene_graph::{SceneGraphTraverser, VoxelPlacement};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BoundsPolicy {
    Panic,
    Clip,
}

pub mod format;
pub mod grid;
pub mod scene_graph;
pub mod snapshot;

#[cfg(test)]
mod bench;

const SHARD_COUNT: usize = 64;
const SHARD_ROUTE_SHIFT: u32 = 64 - SHARD_COUNT.trailing_zeros();
const BUILD_CHUNK: usize = 8_192;

// every fold input is gated by grid::in_lattice, so the signed cast is exact
#[allow(clippy::as_conversions, clippy::cast_possible_wrap)]
const LATTICE_BIAS: i32 = grid::LATTICE_HALF_EXTENT as i32;
const FOLD_FIELD_BITS: u32 = grid::LATTICE_HALF_EXTENT.trailing_zeros() + 1;
const FOLD_FIELD_MASK: u64 = (1u64 << FOLD_FIELD_BITS) - 1;

type VoxelMap = HashMap<u64, u32, FxBuildHasher>;
type StagedMap = HashMap<u64, u64, FxBuildHasher>;

#[derive(Debug)]
pub struct World {
    shards: [VoxelMap; SHARD_COUNT],
}

impl Default for World {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| HashMap::default()),
        }
    }
}

// Bijective 36-bit fold: three biased 12-bit axis fields, x high.
fn fold(position: IVec3) -> u64 {
    let biased = position.wrapping_add(IVec3::splat(LATTICE_BIAS)).as_uvec3();

    (u64::from(biased.x) << (2 * FOLD_FIELD_BITS))
        | (u64::from(biased.y) << FOLD_FIELD_BITS)
        | u64::from(biased.z)
}

#[allow(clippy::as_conversions, clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
fn unfold(key: u64) -> IVec3 {
    let axis = |field: u64| (field as i32).wrapping_sub(LATTICE_BIAS);

    IVec3::new(
        axis((key >> (2 * FOLD_FIELD_BITS)) & FOLD_FIELD_MASK),
        axis((key >> FOLD_FIELD_BITS) & FOLD_FIELD_MASK),
        axis(key & FOLD_FIELD_MASK),
    )
}

// The golden multiply spreads the packed fold; the top bits always fit usize.
#[allow(clippy::as_conversions)]
const fn shard_index(key: u64) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> SHARD_ROUTE_SHIFT) as usize
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertResult {
    Ok,
    Clipped,
    Existing,
}

impl World {
    fn assert_in_lattice(position: &IVec3) {
        assert!(
            grid::in_lattice(*position),
            "voxel {position} outside the ±{} lattice",
            grid::LATTICE_HALF_EXTENT
        );
    }

    fn shard(&self, position: IVec3) -> &VoxelMap {
        let index = shard_index(fold(position));

        self.shards
            .get(index)
            .unwrap_or_else(|| panic!("shard {index} out of the {SHARD_COUNT} shards"))
    }

    fn shard_mut(&mut self, position: IVec3) -> &mut VoxelMap {
        let index = shard_index(fold(position));

        self.shards
            .get_mut(index)
            .unwrap_or_else(|| panic!("shard {index} out of the {SHARD_COUNT} shards"))
    }

    fn reserve(&mut self, additional: usize) {
        let per_shard = additional / SHARD_COUNT;

        for map in &mut self.shards {
            map.reserve(per_shard);
        }
    }

    pub(crate) fn insert(
        &mut self,
        position: IVec3,
        voxel: u32,
        policy: BoundsPolicy,
    ) -> InsertResult {
        if !grid::in_lattice(position) {
            match policy {
                BoundsPolicy::Panic => Self::assert_in_lattice(&position),
                BoundsPolicy::Clip => return InsertResult::Clipped,
            }
        }

        match self.shard_mut(position).insert(fold(position), voxel) {
            Some(_) => InsertResult::Existing,
            None => InsertResult::Ok,
        }
    }

    pub fn new(voxel_data: &DotVoxData) -> Self {
        let (world, clipped) = Self::load(voxel_data, BoundsPolicy::Panic);
        debug_assert_eq!(clipped, 0);
        world
    }

    pub fn new_clipped(voxel_data: &DotVoxData) -> (Self, usize) {
        Self::load(voxel_data, BoundsPolicy::Clip)
    }

    fn load(voxel_data: &DotVoxData, policy: BoundsPolicy) -> (Self, usize) {
        let mut world = Self::default();

        if voxel_data.scenes.is_empty() {
            let direct = voxel_data
                .models
                .iter()
                .map(|model| model.voxels.len())
                .sum();
            world.reserve(direct);
        }

        let mut loader = SceneGraphTraverser {
            world: &mut world,
            policy,
            scene: voxel_data,
            models: vec![],
        };

        let mut clipped = loader.traverse();

        let models = std::mem::take(&mut loader.models);

        let mut placements = Vec::with_capacity(models.len());
        let mut live = 0usize;

        for (translation, rotation, size, voxels) in models {
            let placement = VoxelPlacement::new(translation, rotation, size);

            if policy == BoundsPolicy::Clip && placement.misses_lattice() {
                clipped = clipped.saturating_add(voxels.len());
            } else {
                live = live.saturating_add(voxels.len());
                placements.push((placement, voxels));
            }
        }

        world.reserve(live);

        clipped = clipped.saturating_add(world.build(&placements, live, policy));

        (world, clipped)
    }

    fn build(
        &mut self,
        placements: &[(VoxelPlacement, &[Voxel])],
        live: usize,
        policy: BoundsPolicy,
    ) -> usize {
        if live == 0 {
            return 0;
        }

        let per_shard = live / SHARD_COUNT;
        let staged: Vec<Mutex<StagedMap>> = (0..SHARD_COUNT)
            .map(|_| {
                Mutex::new(StagedMap::with_capacity_and_hasher(per_shard, FxBuildHasher))
            })
            .collect();

        let clipped = placements
            .par_iter()
            .enumerate()
            .flat_map(|(model_index, (placement, voxels))| {
                let model_base = u64::try_from(model_index)
                    .unwrap_or(u64::MAX)
                    .wrapping_mul(1u64 << 40);

                voxels
                    .par_chunks(BUILD_CHUNK)
                    .enumerate()
                    .map(move |(chunk_index, chunk)| {
                        let chunk_base = model_base.wrapping_add(
                            u64::try_from(chunk_index.wrapping_mul(BUILD_CHUNK))
                                .unwrap_or(u64::MAX),
                        );

                        (placement, chunk, chunk_base)
                    })
            })
            .map(|(placement, chunk, sequence_base)| {
                stage_chunk(placement, chunk, sequence_base, policy, &staged)
            })
            .sum();

        let loaded: Vec<VoxelMap> = staged.into_par_iter().map(unstage_shard).collect();

        for (map, staged_map) in self.shards.iter_mut().zip(loaded) {
            *map = staged_map;
        }

        clipped
    }

    pub fn contains(&self, position: &IVec3) -> bool {
        Self::assert_in_lattice(position);
        self.shard(*position).contains_key(&fold(*position))
    }

    pub fn get_voxel(&self, position: &IVec3) -> Option<&u32> {
        Self::assert_in_lattice(position);
        self.shard(*position).get(&fold(*position))
    }

    #[cfg(test)]
    pub(crate) fn insert_voxel_at(&mut self, position: IVec3, material_index: u32) {
        Self::assert_in_lattice(&position);
        self.shard_mut(position).insert(fold(position), material_index);
    }

    pub fn iter_voxels(&self) -> impl Iterator<Item = (IVec3, &u32)> + '_ {
        self.shards
            .iter()
            .flat_map(|map| map.iter().map(|(key, voxel)| (unfold(*key), voxel)))
    }

    pub fn voxel_bounds(&self) -> Option<(IVec3, IVec3)> {
        let mut min: Option<IVec3> = None;
        let mut max: Option<IVec3> = None;

        for (position, _) in self.iter_voxels() {
            min = Some(min.map_or(position, |m| m.min(position)));
            max = Some(max.map_or(position, |m| m.max(position)));
        }

        min.zip(max)
    }

    pub fn voxel_count(&self) -> usize {
        self.shards.iter().map(HashMap::len).sum()
    }
}

fn stage_chunk(
    placement: &VoxelPlacement,
    chunk: &[Voxel],
    sequence_base: u64,
    policy: BoundsPolicy,
    staged: &[Mutex<StagedMap>],
) -> usize {
    let mut routed: Vec<Vec<(u64, u64)>> = (0..SHARD_COUNT).map(|_| Vec::new()).collect();
    let mut sequence = sequence_base;
    let mut clipped = 0usize;

    for voxel in chunk {
        let position = placement.place(*voxel);

        if grid::in_lattice(position) {
            let key = fold(position);
            let value = sequence.wrapping_mul(256) | u64::from(voxel.i);

            match routed.get_mut(shard_index(key)) {
                Some(bucket) => bucket.push((key, value)),
                None => panic!("shard route out of the {SHARD_COUNT} shards"),
            }
        } else {
            match policy {
                BoundsPolicy::Panic => World::assert_in_lattice(&position),
                BoundsPolicy::Clip => clipped = clipped.saturating_add(1),
            }
        }

        sequence = sequence.wrapping_add(1);
    }

    for (staged_map, bucket) in staged.iter().zip(&routed) {
        if bucket.is_empty() {
            continue;
        }

        let mut map = lock_stage(staged_map);

        for (key, value) in bucket {
            match map.entry(*key) {
                Entry::Vacant(entry) => {
                    entry.insert(*value);
                }
                Entry::Occupied(mut entry) => {
                    if *value > *entry.get() {
                        entry.insert(*value);
                    }
                }
            }
        }
    }

    clipped
}

fn lock_stage(staged: &Mutex<StagedMap>) -> MutexGuard<'_, StagedMap> {
    match staged.lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn unstage_shard(staged: Mutex<StagedMap>) -> VoxelMap {
    let map = match Mutex::into_inner(staged) {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };

    map.into_iter()
        .map(|(position, value)| {
            let [material, ..] = value.to_le_bytes();
            (position, u32::from(material))
        })
        .collect()
}

impl Display for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "World {{ voxels: {} }}", self.voxel_count())
    }
}

#[cfg(test)]
#[derive(Debug)]
struct ModelSpec {
    size: (u32, u32, u32),
    voxels: Vec<dot_vox::Voxel>,
    rotation: u8,
    translation: [i32; 3],
}

#[cfg(test)]
fn scene_fixture(specs: &[ModelSpec]) -> DotVoxData {
    use dot_vox::{Dict, Frame, Model, SceneNode, ShapeModel, Size};

    let children: Vec<u32> = (0..specs.len()).map(|k| (2 * k + 1) as u32).collect();
    let mut scenes = vec![SceneNode::Group {
        attributes: Dict::new(),
        children,
    }];

    let mut models = Vec::new();
    for (k, spec) in specs.iter().enumerate() {
        let mut frame = Dict::new();
        frame.insert("_r".to_owned(), spec.rotation.to_string());
        frame.insert(
            "_t".to_owned(),
            format!(
                "{} {} {}",
                spec.translation[0], spec.translation[1], spec.translation[2]
            ),
        );

        scenes.push(SceneNode::Transform {
            attributes: Dict::new(),
            frames: vec![Frame::new(frame)],
            child: (2 * k + 2) as u32,
            layer_id: 0,
        });
        scenes.push(SceneNode::Shape {
            attributes: Dict::new(),
            models: vec![ShapeModel {
                model_id: k as u32,
                attributes: Dict::new(),
            }],
        });
        models.push(Model {
            size: Size {
                x: spec.size.0,
                y: spec.size.1,
                z: spec.size.2,
            },
            voxels: spec.voxels.clone(),
        });
    }

    DotVoxData {
        version: 200,
        index_map: Vec::new(),
        models,
        palette: Vec::new(),
        materials: Vec::new(),
        scenes,
        layers: Vec::new(),
    }
}

#[cfg(test)]
mod placement_differential {
    use std::collections::HashMap;
    use std::ops::Neg;

    use dot_vox::{DotVoxData, Rotation, Voxel};
    use glam::{DMat4, DQuat, DVec3, DVec4, IVec3, IVec4, UVec3, Vec4Swizzles};

    use std::hash::{Hash, Hasher};

    use rustc_hash::FxHasher;

    use super::grid;
    use super::scene_graph::{SceneGraphTraverser, VoxelPlacement};
    use super::{BoundsPolicy, ModelSpec, World, scene_fixture};

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    const TRANSLATIONS: &[i32] = &[
        0, 1, -1, 3, -3, 2047, -2047, 2048, -2048, 2049, -2049, 100_000, -100_000, 1_000_000,
        -1_000_000,
    ];

    fn rotation_bytes() -> Vec<u8> {
        (0u8..128)
            .filter(|byte| {
                let first = byte & 0b11;
                let second = (byte >> 2) & 0b11;
                first != 0b11 && second != 0b11 && first != second
            })
            .collect()
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

        let center = quat
            .mul_vec3(DVec3::new(f64::from(size.x), f64::from(size.z), f64::from(size.y)) * 0.5);

        DMat4::from_scale_rotation_translation(scale, quat, translation - center * scale + offset)
    }

    fn exact_oracle_map(data: &DotVoxData) -> HashMap<IVec3, u32> {
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
                    map.insert(position, u32::from(voxel.i));
                }
            }
        }
        map
    }

    fn legacy_float_map(data: &DotVoxData) -> HashMap<IVec3, u32> {
        let mut map = HashMap::new();
        for (translation, rotation, size, voxels) in collected_models(data) {
            let transform =
                SceneGraphTraverser::legacy_float_transform(translation, rotation, size);
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
                    map.insert(position, u32::from(voxel.i));
                }
            }
        }
        map
    }

    fn production_map(data: &DotVoxData) -> HashMap<IVec3, u32> {
        let (world, _) = World::new_clipped(data);
        world
            .iter_voxels()
            .map(|(position, voxel)| (position, *voxel))
            .collect()
    }

    fn random_specs(rng: &mut Rng, rotation: u8) -> Vec<ModelSpec> {
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
        let mut rng = Rng::new(0xC0FFEE);

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
            Some(&7),
            "the legacy pipeline evaluates this origin voxel to m = (5, 8, -6), so the world position after the final z negation is (5, 8, 6)"
        );
    }

    fn serial_map(data: &DotVoxData) -> HashMap<IVec3, u32> {
        let mut map = HashMap::new();

        for (translation, rotation, size, voxels) in collected_models(data) {
            let placement = VoxelPlacement::new(translation, rotation, size);

            for voxel in voxels {
                let position = placement.place(*voxel);

                if grid::in_lattice(position) {
                    map.insert(position, u32::from(voxel.i));
                }
            }
        }

        map
    }

    fn content_hash(map: &HashMap<IVec3, u32>) -> u64 {
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
        let mut rng = Rng::new(0x5011_0AD);
        let valid_rotations = rotation_bytes();

        for rotation in &valid_rotations {
            for _ in 0..4 {
                let specs = random_specs(&mut rng, *rotation);
                let data = scene_fixture(&specs);
                assert_eq!(
                    production_map(&data),
                    serial_map(&data),
                    "rotation {rotation:#010b}, specs {specs:?}"
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
            assert_eq!(production_map(&data), serial_map(&data), "specs {specs:?}");
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
        println!(
            "{path}: {} voxels, content hash {parallel_hash:016x}",
            parallel_map.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_round_trips_lattice_extremes() {
        for position in [
            IVec3::new(-2048, -2048, -2048),
            IVec3::new(2047, 2047, 2047),
            IVec3::new(-2048, 2047, 0),
            IVec3::new(0, -2048, 2047),
            IVec3::new(123, -456, 789),
            IVec3::ZERO,
        ] {
            assert!(grid::in_lattice(position));
            assert_eq!(unfold(fold(position)), position);
        }

        assert_ne!(
            fold(IVec3::new(-2048, -2048, -2048)),
            fold(IVec3::new(-2048, -2048, -2047))
        );
    }

    #[test]
    fn insert_and_contains() {
        let mut world = World::default();
        world.insert_voxel_at(IVec3::new(1, 1, 1), 1);
        world.insert_voxel_at(IVec3::new(-8, 19, -15), 2);
        assert!(world.contains(&IVec3::new(1, 1, 1)));
        assert!(world.contains(&IVec3::new(-8, 19, -15)));
        assert!(!world.contains(&IVec3::new(0, 0, 0)));
    }

    #[test]
    fn voxel_count_and_bounds() {
        let mut world = World::default();
        world.insert_voxel_at(IVec3::new(0, 0, 0), 1);
        world.insert_voxel_at(IVec3::new(5, -3, 2), 2);
        assert_eq!(world.voxel_count(), 2);
        assert_eq!(
            world.voxel_bounds(),
            Some((IVec3::new(0, -3, 0), IVec3::new(5, 0, 2)))
        );
    }

    #[test]
    fn world_extent_is_half_open() {
        let mut world = World::default();
        world.insert_voxel_at(IVec3::new(-2048, 0, 0), 1);
        world.insert_voxel_at(IVec3::new(2047, 0, 0), 2);
        assert!(world.contains(&IVec3::new(-2048, 0, 0)));
        assert!(world.contains(&IVec3::new(2047, 0, 0)));
    }

    #[test]
    #[should_panic(expected = "outside the ±2048 lattice")]
    fn insert_rejects_beyond_lattice() {
        let mut world = World::default();
        world.insert_voxel_at(IVec3::new(2048, 0, 0), 1);
    }

    #[test]
    fn clip_drops_out_of_lattice_voxels() {
        let mut world = World::default();
        assert_eq!(
            world.insert(IVec3::new(0, 0, 0), 1, BoundsPolicy::Clip),
            InsertResult::Ok
        );
        assert_eq!(
            world.insert(IVec3::new(3000, 0, 0), 2, BoundsPolicy::Clip),
            InsertResult::Clipped
        );
        assert_eq!(
            world.insert(IVec3::new(0, -3000, 0), 3, BoundsPolicy::Clip),
            InsertResult::Clipped
        );
        assert_eq!(world.voxel_count(), 1);
        assert_eq!(
            world.iter_voxels().next().map(|(p, _)| p),
            Some(IVec3::new(0, 0, 0))
        );
    }

    #[test]
    fn scene_path_flips_rows_without_underflow() {
        let data = scene_fixture(&[ModelSpec {
            size: (2, 2, 2),
            voxels: vec![
                dot_vox::Voxel {
                    x: 0,
                    y: 0,
                    z: 0,
                    i: 0,
                },
                dot_vox::Voxel {
                    x: 0,
                    y: 1,
                    z: 0,
                    i: 0,
                },
            ],
            rotation: 0b0000100,
            translation: [0, 0, 0],
        }]);
        let world = World::new(&data);

        assert_eq!(world.voxel_count(), 2);
        assert!(world.contains(&IVec3::new(-1, -1, 0)));
        assert!(world.contains(&IVec3::new(-1, -1, 1)));
    }
}
