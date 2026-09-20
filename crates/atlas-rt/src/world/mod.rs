use std::{
    collections::{HashMap, hash_map::Entry},
    fmt::Display,
    sync::{Mutex, MutexGuard},
};

use dot_vox::{DotVoxData, Voxel};
use glam::IVec3;
use rayon::prelude::*;
use rustc_hash::FxBuildHasher;

use crate::world::load::scene_graph::{SceneGraphTraverser, VoxelPlacement};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BoundsPolicy {
    Panic,
    Clip,
}

pub mod format;
pub mod grid;
pub mod load;
pub mod raycast;
pub mod update;

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

// call sites gate inputs with grid::in_lattice, so every field fits i32
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]
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

    #[must_use]
    pub fn new(voxel_data: &DotVoxData) -> Self {
        let (world, clipped) = Self::load(voxel_data, BoundsPolicy::Panic);
        debug_assert_eq!(clipped, 0);
        world
    }

    #[must_use]
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
                let attempts = voxels.len() as u64;
                let capacity = placement.in_lattice_capacity(attempts);

                live = live.saturating_add(usize::try_from(capacity).unwrap_or(usize::MAX));
                placements.push((placement, voxels));
            }
        }

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
                Mutex::new(StagedMap::with_capacity_and_hasher(
                    per_shard,
                    FxBuildHasher,
                ))
            })
            .collect();

        let clipped = placements
            .par_iter()
            .enumerate()
            .flat_map(|(model_index, (placement, voxels))| {
                let model_base = (model_index as u64).wrapping_mul(1u64 << 40);

                voxels
                    .par_chunks(BUILD_CHUNK)
                    .enumerate()
                    .map(move |(chunk_index, chunk)| {
                        let chunk_base =
                            model_base.wrapping_add(chunk_index.wrapping_mul(BUILD_CHUNK) as u64);

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

    #[must_use]
    pub fn contains(&self, position: &IVec3) -> bool {
        Self::assert_in_lattice(position);
        self.shard(*position).contains_key(&fold(*position))
    }

    #[must_use]
    pub fn get_voxel(&self, position: &IVec3) -> Option<&u32> {
        Self::assert_in_lattice(position);
        self.shard(*position).get(&fold(*position))
    }

    pub(crate) fn set_voxel(&mut self, position: IVec3, material: u8) {
        Self::assert_in_lattice(&position);
        self.shard_mut(position)
            .insert(fold(position), u32::from(material));
    }

    pub(crate) fn clear_voxel(&mut self, position: IVec3) {
        Self::assert_in_lattice(&position);
        self.shard_mut(position).remove(&fold(position));
    }

    // every write into the shards is a byte, so only an absent voxel is None
    #[must_use]
    pub(crate) fn material_at(&self, position: &IVec3) -> Option<u8> {
        self.get_voxel(position)
            .and_then(|voxel| u8::try_from(*voxel).ok())
    }

    pub fn iter_voxels(&self) -> impl Iterator<Item = (IVec3, &u32)> + '_ {
        self.shards
            .iter()
            .flat_map(|map| map.iter().map(|(key, voxel)| (unfold(*key), voxel)))
    }

    #[must_use]
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

    #[cfg(test)]
    pub(crate) fn reserved_capacity(&self) -> usize {
        self.shards.iter().map(HashMap::capacity).sum()
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
mod placement;

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
        world.set_voxel(IVec3::new(1, 1, 1), 1);
        world.set_voxel(IVec3::new(-8, 19, -15), 2);
        assert!(world.contains(&IVec3::new(1, 1, 1)));
        assert!(world.contains(&IVec3::new(-8, 19, -15)));
        assert!(!world.contains(&IVec3::new(0, 0, 0)));
    }

    #[test]
    fn voxel_count_and_bounds() {
        let mut world = World::default();
        world.set_voxel(IVec3::new(0, 0, 0), 1);
        world.set_voxel(IVec3::new(5, -3, 2), 2);
        assert_eq!(world.voxel_count(), 2);
        assert_eq!(
            world.voxel_bounds(),
            Some((IVec3::new(0, -3, 0), IVec3::new(5, 0, 2)))
        );
    }

    #[test]
    fn world_extent_is_half_open() {
        let mut world = World::default();
        world.set_voxel(IVec3::new(-2048, 0, 0), 1);
        world.set_voxel(IVec3::new(2047, 0, 0), 2);
        assert!(world.contains(&IVec3::new(-2048, 0, 0)));
        assert!(world.contains(&IVec3::new(2047, 0, 0)));
    }

    #[test]
    #[should_panic(expected = "outside the ±2048 lattice")]
    fn insert_rejects_beyond_lattice() {
        let mut world = World::default();
        world.set_voxel(IVec3::new(2048, 0, 0), 1);
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
            rotation: 0b000_0100,
            translation: [0, 0, 0],
        }]);
        let world = World::new(&data);

        assert_eq!(world.voxel_count(), 2);
        assert!(world.contains(&IVec3::new(-1, -1, 0)));
        assert!(world.contains(&IVec3::new(-1, -1, 1)));
    }
}
