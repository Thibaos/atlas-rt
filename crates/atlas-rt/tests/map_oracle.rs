//! The map oracle is a second storage implementation selected at construction.
//! Compiled only under the `map-oracle` feature; without it this target is empty.

#![cfg(feature = "map-oracle")]

use atlas_rt::world::{StoreKind, World, vox::open_file};
use glam::IVec3;

const ASSET: &str = "assets/test/edit-seam.vox";

fn sorted(world: &World) -> Vec<(IVec3, u8)> {
    let mut cells: Vec<(IVec3, u8)> = world.iter_voxels().collect();

    cells.sort_unstable_by_key(|(position, _)| position.to_array());

    cells
}

#[test]
fn both_stores_agree_on_a_loaded_asset() {
    let data = open_file(ASSET);

    let (region, region_clipped) = World::new_clipped_with_store(&data, StoreKind::Region);
    let (map, map_clipped) = World::new_clipped_with_store(&data, StoreKind::Map);

    assert_eq!(region_clipped, map_clipped, "clipped counts");
    assert_eq!(region.voxel_count(), map.voxel_count(), "voxel count");
    assert_eq!(region.voxel_bounds(), map.voxel_bounds(), "bounds");

    let region_cells = sorted(&region);
    let map_cells = sorted(&map);

    assert_eq!(region_cells, map_cells, "content");

    for (position, material) in &region_cells {
        assert_eq!(region.get_voxel(position), Some(*material));
        assert_eq!(map.get_voxel(position), Some(*material));
    }
}
