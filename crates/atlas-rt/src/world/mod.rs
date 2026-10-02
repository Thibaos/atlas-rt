pub(crate) mod budget;
pub mod diff;
pub mod grid;
pub mod load;
pub mod material;
pub mod palette;
pub mod raycast;
pub(crate) mod store;
pub mod vox;

#[cfg(test)]
mod bench;

#[cfg(all(test, feature = "map-oracle"))]
mod differential;

use std::fmt::Display;

use dot_vox::DotVoxData;
use glam::IVec3;

use store::{RegionStore, VoxelStore};

#[cfg(feature = "map-oracle")]
use store::ShardedMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BoundsPolicy {
    Panic,
    Clip,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertResult {
    Ok,
    Clipped,
    Existing,
}

/// Which storage backs a [`World`]: the resident Region store, or the sharded
/// map oracle the `map-oracle` feature compiles for the differential tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreKind {
    Region,
    #[cfg(feature = "map-oracle")]
    Map,
}

#[derive(Debug)]
pub struct World {
    store: Box<dyn VoxelStore>,
}

impl Default for World {
    fn default() -> Self {
        Self::empty(StoreKind::Region)
    }
}

impl World {
    fn build_store(kind: StoreKind) -> Box<dyn VoxelStore> {
        match kind {
            StoreKind::Region => Box::new(RegionStore::default()),
            #[cfg(feature = "map-oracle")]
            StoreKind::Map => Box::new(ShardedMap::default()),
        }
    }

    pub(crate) fn empty(kind: StoreKind) -> Self {
        Self::from_store(Self::build_store(kind))
    }

    pub(crate) fn from_store(store: Box<dyn VoxelStore>) -> Self {
        Self { store }
    }

    pub(crate) fn insert(
        &mut self,
        position: IVec3,
        voxel: u32,
        policy: BoundsPolicy,
    ) -> InsertResult {
        self.store.insert(position, voxel, policy)
    }

    #[must_use]
    pub fn new(voxel_data: &DotVoxData) -> Self {
        Self::new_with_store(voxel_data, StoreKind::Region)
    }

    #[must_use]
    pub fn new_clipped(voxel_data: &DotVoxData) -> (Self, usize) {
        Self::new_clipped_with_store(voxel_data, StoreKind::Region)
    }

    #[must_use]
    pub fn new_with_store(voxel_data: &DotVoxData, store: StoreKind) -> Self {
        let (world, clipped) = Self::build(voxel_data, BoundsPolicy::Panic, store);
        debug_assert_eq!(clipped, 0);
        world
    }

    #[must_use]
    pub fn new_clipped_with_store(voxel_data: &DotVoxData, store: StoreKind) -> (Self, usize) {
        Self::build(voxel_data, BoundsPolicy::Clip, store)
    }

    /// Builds a store with no cell budget: a direct constructor has no way to
    /// report a refusal, so it never refuses. The load job reads the budget.
    fn build(voxel_data: &DotVoxData, policy: BoundsPolicy, store: StoreKind) -> (Self, usize) {
        match load::build::load(voxel_data, policy, store, usize::MAX) {
            Ok((world, clipped)) => (world, clipped),
            Err(refused) => panic!("the loader refused {refused} cells with no cell budget set"),
        }
    }

    #[must_use]
    pub fn contains(&self, position: &IVec3) -> bool {
        grid::assert_in_lattice(*position);
        self.store.contains(*position)
    }

    #[must_use]
    pub fn get_voxel(&self, position: &IVec3) -> Option<u8> {
        grid::assert_in_lattice(*position);
        self.store.get(*position)
    }

    pub(crate) fn set_voxel(&mut self, position: IVec3, material: u8) {
        grid::assert_in_lattice(position);
        self.store.set(position, material);
    }

    pub(crate) fn clear_voxel(&mut self, position: IVec3) {
        grid::assert_in_lattice(position);
        self.store.clear(position);
    }

    pub fn iter_voxels(&self) -> impl Iterator<Item = (IVec3, u8)> + '_ {
        self.store.iter()
    }

    #[must_use]
    pub fn voxel_bounds(&self) -> Option<(IVec3, IVec3)> {
        self.store.bounds()
    }

    #[must_use]
    pub fn voxel_count(&self) -> usize {
        self.store.count()
    }

    #[cfg(test)]
    pub(crate) fn storage_size(&self) -> store::StorageSize {
        self.store.storage_size()
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
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(feature = "map-oracle")]
    #[test]
    fn stores_agree_on_a_loaded_world() {
        let data = scene_fixture(&[
            ModelSpec {
                size: (4, 4, 4),
                voxels: vec![
                    dot_vox::Voxel {
                        x: 0,
                        y: 0,
                        z: 0,
                        i: 3,
                    },
                    dot_vox::Voxel {
                        x: 3,
                        y: 2,
                        z: 1,
                        i: 0,
                    },
                    dot_vox::Voxel {
                        x: 1,
                        y: 1,
                        z: 1,
                        i: 9,
                    },
                ],
                rotation: 0b000_0100,
                translation: [16, -16, 8],
            },
            ModelSpec {
                size: (2, 2, 2),
                voxels: vec![dot_vox::Voxel {
                    x: 1,
                    y: 1,
                    z: 1,
                    i: 5,
                }],
                rotation: 0b0001,
                translation: [3000, 0, 0],
            },
        ]);

        let (region, region_clipped) = World::new_clipped_with_store(&data, StoreKind::Region);
        let (map, map_clipped) = World::new_clipped_with_store(&data, StoreKind::Map);

        assert_eq!(region_clipped, map_clipped, "clipped counts");
        test_support::assert_worlds_agree(&region, &map, "loaded world");
    }

    #[cfg(feature = "map-oracle")]
    #[test]
    fn randomized_edits_agree_across_stores() {
        use test_support::{Rng, u8_below};

        let mut rng = Rng::new(0x0505_A0A0);
        let mut region = World::empty(StoreKind::Region);
        let mut map = World::empty(StoreKind::Map);

        for _ in 0..2_000 {
            let position = IVec3::new(
                i32::try_from(rng.below(512)).unwrap_or(0).wrapping_sub(256),
                i32::try_from(rng.below(512)).unwrap_or(0).wrapping_sub(256),
                i32::try_from(rng.below(512)).unwrap_or(0).wrapping_sub(256),
            );

            if rng.below(4) == 0 {
                region.clear_voxel(position);
                map.clear_voxel(position);
            } else {
                let material = u8_below(&mut rng, 256);

                region.set_voxel(position, material);
                map.set_voxel(position, material);
            }
        }

        test_support::assert_worlds_agree(&region, &map, "randomized direct writes");
    }
}
