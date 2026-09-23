//! The transparency acceptance world, pinned through the real loader.
//!
//! `glass-pane.vox` is the only tracked world that reads a palette slot below
//! 255, so its alpha and placement are the contract the DDA's skip variants
//! and the raygen's blend act on. A regeneration that shifts an alpha by one
//! palette position still loads, still draws, and silently renders every pane
//! opaque, which is what the old `glass-shadow.vox` did.

use atlas_rt::world::{
    World,
    format::{get_palette, open_file},
};
use glam::{IVec3, Vec4};

const GLASS_PANE: &str = "assets/test/glass-pane.vox";

const WALL: u8 = 1;
const PANE: u8 = 2;
const OPENING: u8 = 3;

fn load() -> (World, [Vec4; 256]) {
    let data = open_file(GLASS_PANE);
    (World::new(&data), get_palette(&data))
}

fn material(slot: u8) -> u32 {
    u32::from(slot)
}

fn alpha(palette: &[Vec4; 256], slot: u8) -> f32 {
    palette.get(usize::from(slot)).map_or(0.0, |color| color.w)
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 1.0e-6
}

#[test]
fn palette_carries_the_three_alphas_the_shaders_branch_on() {
    let (_, palette) = load();

    assert!(near(alpha(&palette, WALL), 1.0), "wall stays opaque");
    assert!(
        near(alpha(&palette, PANE), 128.0 / 255.0),
        "pane reads alpha 128, linear coverage 128/255"
    );
    assert!(near(alpha(&palette, OPENING), 0.0), "opening reads alpha 0");
}

#[test]
fn every_slot_outside_the_three_stays_opaque() {
    let (_, palette) = load();
    let authored = [usize::from(WALL), usize::from(PANE), usize::from(OPENING)];

    for (slot, color) in palette.iter().enumerate() {
        if authored.contains(&slot) {
            continue;
        }

        assert!(near(color.w, 1.0), "slot {slot} must stay opaque");
    }
}

#[test]
fn every_voxel_falls_inside_its_models_declared_size() {
    let data = open_file(GLASS_PANE);

    for model in &data.models {
        for voxel in &model.voxels {
            assert!(
                u32::from(voxel.x) < model.size.x
                    && u32::from(voxel.y) < model.size.y
                    && u32::from(voxel.z) < model.size.z,
                "voxel ({}, {}, {}) falls outside the model size {}x{}x{}",
                voxel.x,
                voxel.y,
                voxel.z,
                model.size.x,
                model.size.y,
                model.size.z
            );
        }
    }
}

#[test]
fn the_pane_and_the_opening_sit_in_front_of_the_wall() {
    let (world, _) = load();

    let pane = IVec3::new(3, 5, 8);
    let opening = IVec3::new(7, 5, 8);
    let wall = IVec3::new(7, 5, 10);

    assert_eq!(
        world.get_voxel(&pane),
        Some(&material(PANE)),
        "pane cell carries the alpha-128 slot"
    );
    assert_eq!(
        world.get_voxel(&opening),
        Some(&material(OPENING)),
        "opening cell carries the alpha-0 slot"
    );
    assert_eq!(
        world.get_voxel(&wall),
        Some(&material(WALL)),
        "wall sits two cells behind the opening"
    );
}

#[test]
fn the_wall_fills_the_frame_around_the_window_unit() {
    let (world, _) = load();

    for (cell, label) in [
        (IVec3::new(1, 1, 10), "below-left of the unit"),
        (IVec3::new(10, 10, 10), "above-right of the unit"),
        (IVec3::new(0, 6, 10), "left of the unit"),
        (IVec3::new(11, 6, 10), "right of the unit"),
    ] {
        assert_eq!(
            world.get_voxel(&cell),
            Some(&material(WALL)),
            "wall should back the unit {label}"
        );
    }
}

#[test]
fn the_unit_plane_is_empty_outside_its_footprint() {
    let (world, _) = load();

    for (cell, label) in [
        (IVec3::new(1, 5, 8), "left of the unit"),
        (IVec3::new(10, 5, 8), "right of the unit"),
        (IVec3::new(5, 10, 8), "above the unit"),
        (IVec3::new(5, 1, 8), "below the unit"),
    ] {
        assert!(
            !world.contains(&cell),
            "nothing should float {label} of the unit, but the cell is occupied"
        );
    }
}
