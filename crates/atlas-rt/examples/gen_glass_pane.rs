//! Writes `assets/test/glass-pane.vox`, the single-layer transparency world.
//!
//! A back wall two cells thick, with a window unit floating two cells in front
//! of it. The unit's left half is a palette-alpha-128 pane, its right half a
//! palette-alpha-0 run that shows the bare wall through its opening. The wall's
//! front faces point away from the sun, so every one of them stays lit whether
//! the pane or the opening casts a shadow or not. A frame can therefore refute
//! the no-shadow claim: a shadow where the wall was lit would mean the DDA had
//! committed a transparent cell on a shadow ray.
//!
//! A scene-less `.vox` voxel (x, y, z) loads at world (x, z, y), so world
//! coordinates are authored here and the model swaps y and z on the way out.
//!
//! `dot_vox::Voxel::i` is the palette slot, 0-based: the writer emits `i + 1`
//! and the parser reads back `byte - 1`, and `get_palette` fills slot `i` from
//! `palette[i]`. So a voxel painted from `palette[slot]` carries material
//! `slot`. The byte the file stores is one higher, which is the off-by-one the
//! old untracked `glass-shadow.vox` got wrong.

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use dot_vox::{Color, DotVoxData, Model, Size, Voxel};
use glam::IVec3;

const WALL: u8 = 1;
const PANE: u8 = 2;
const OPENING: u8 = 3;

const WALL_FRONT: i32 = 10;
const WALL_SIZE: i32 = 12;
const UNIT_FRONT: i32 = 8;
const UNIT_MIN: i32 = 2;
const UNIT_MAX: i32 = 10;
const PANE_END: i32 = 6;

fn main() -> io::Result<()> {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("test")
        .join("glass-pane.vox");

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut bytes = Vec::new();
    glass_pane_world().write_vox(&mut bytes)?;

    let mut file = fs::File::create(&out)?;
    file.write_all(&bytes)?;
    file.flush()?;

    println!("wrote {}", out.display());

    Ok(())
}

fn glass_pane_world() -> DotVoxData {
    let voxels = world_voxels();
    let size = voxels
        .iter()
        .map(|(position, _)| *position)
        .fold(IVec3::ZERO, IVec3::max)
        .saturating_add(IVec3::ONE);

    DotVoxData {
        version: 150,
        index_map: (0..=255).collect(),
        models: vec![Model {
            size: Size {
                x: size.x.cast_unsigned(),
                y: size.z.cast_unsigned(),
                z: size.y.cast_unsigned(),
            },
            voxels: voxels
                .into_iter()
                .map(|(position, slot)| Voxel {
                    x: u8::try_from(position.x).unwrap_or(0),
                    y: u8::try_from(position.z).unwrap_or(0),
                    z: u8::try_from(position.y).unwrap_or(0),
                    i: slot,
                })
                .collect(),
        }],
        palette: palette(),
        materials: vec![],
        scenes: vec![],
        layers: vec![],
    }
}

fn world_voxels() -> Vec<(IVec3, u8)> {
    let mut voxels = Vec::new();

    for x in 0..WALL_SIZE {
        for y in 0..WALL_SIZE {
            voxels.push((IVec3::new(x, y, WALL_FRONT), WALL));
            voxels.push((IVec3::new(x, y, WALL_FRONT + 1), WALL));
        }
    }

    for x in UNIT_MIN..UNIT_MAX {
        for y in UNIT_MIN..UNIT_MAX {
            let slot = if x < PANE_END { PANE } else { OPENING };
            voxels.push((IVec3::new(x, y, UNIT_FRONT), slot));
        }
    }

    voxels
}

fn palette() -> Vec<Color> {
    let mut colors: Vec<Color> = (0u8..=255)
        .map(|index| Color {
            r: index.saturating_mul(37).saturating_add(11),
            g: index.saturating_mul(71).saturating_add(23),
            b: index.saturating_mul(109).saturating_add(41),
            a: 255,
        })
        .collect();

    let authored = [
        (
            WALL,
            Color {
                r: 170,
                g: 160,
                b: 150,
                a: 255,
            },
        ),
        (
            PANE,
            Color {
                r: 40,
                g: 170,
                b: 220,
                a: 128,
            },
        ),
        (
            OPENING,
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
        ),
    ];

    for (slot, color) in authored {
        if let Some(entry) = colors.get_mut(usize::from(slot)) {
            *entry = color;
        }
    }

    colors
}
