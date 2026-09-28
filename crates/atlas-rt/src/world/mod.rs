pub mod diff;
pub mod grid;
pub mod load;
pub mod material;
pub mod palette;
pub mod raycast;
pub mod vox;

#[cfg(test)]
mod bench;

use std::{collections::HashMap, fmt::Display};

use dot_vox::DotVoxData;
use glam::IVec3;
use rustc_hash::FxBuildHasher;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BoundsPolicy {
    Panic,
    Clip,
}

const SHARD_COUNT: usize = 64;
const SHARD_ROUTE_SHIFT: u32 = 64 - SHARD_COUNT.trailing_zeros();

const LATTICE_BIAS: i32 = grid::LATTICE_HALF_EXTENT.cast_signed();
const FOLD_FIELD_BITS: u32 = grid::LATTICE_HALF_EXTENT.trailing_zeros() + 1;
const FOLD_FIELD_MASK: u64 = (1u64 << FOLD_FIELD_BITS) - 1;

type VoxelMap = HashMap<u64, u32, FxBuildHasher>;

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

#[allow(clippy::cast_possible_truncation)]
fn unfold(key: u64) -> IVec3 {
    let axis = |field: u64| (field as i32).wrapping_sub(LATTICE_BIAS);

    IVec3::new(
        axis((key >> (2 * FOLD_FIELD_BITS)) & FOLD_FIELD_MASK),
        axis((key >> FOLD_FIELD_BITS) & FOLD_FIELD_MASK),
        axis(key & FOLD_FIELD_MASK),
    )
}

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
        let (world, clipped) = load::build::load(voxel_data, BoundsPolicy::Panic);
        debug_assert_eq!(clipped, 0);
        world
    }

    #[must_use]
    pub fn new_clipped(voxel_data: &DotVoxData) -> (Self, usize) {
        load::build::load(voxel_data, BoundsPolicy::Clip)
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
