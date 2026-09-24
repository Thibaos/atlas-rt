use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use dot_vox::{Color, Dict, DotVoxData, Material, Model, Size, Voxel};
use glam::IVec3;

const WALL: u8 = 1;
const PANE: u8 = 2;
const PRODUCT: u8 = 3;
const FALLBACK: u8 = 4;
const ZERO: u8 = 5;

const WALL_FRONT: i32 = 10;
const WALL_SIZE: i32 = 12;
const PANE_FRONT: i32 = 8;
const PANE_MIN: i32 = 2;
const PANE_MAX: i32 = 10;

fn main() -> io::Result<()> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("test");
    fs::create_dir_all(&directory)?;

    write_data(&directory, "matl-alpha.vox", &valid_data(valid_materials()))?;
    write_data(
        &directory,
        "matl-alpha-invalid.vox",
        &valid_data(vec![material(3, &[("_alpha", "not-a-number")])]),
    )?;
    write_data(
        &directory,
        "matl-alpha-duplicate.vox",
        &valid_data(vec![
            material(3, &[("_alpha", "0.5")]),
            material(3, &[("_alpha", "0.25")]),
        ]),
    )?;

    let mut non_default = dot_vox::DEFAULT_INDEX_MAP.to_vec();

    if let Some(first) = non_default.first_mut() {
        *first = 2;
    }

    write_with_imap(
        &directory,
        "matl-alpha-imap.vox",
        &valid_data(valid_materials()),
        &non_default,
    )?;
    write_with_imap(
        &directory,
        "matl-alpha-imap-default.vox",
        &valid_data(valid_materials()),
        dot_vox::DEFAULT_INDEX_MAP,
    )?;
    write_with_imap(
        &directory,
        "matl-alpha-imap-malformed.vox",
        &valid_data(valid_materials()),
        &[1, 2],
    )?;

    Ok(())
}

fn valid_data(materials: Vec<Material>) -> DotVoxData {
    DotVoxData {
        version: 150,
        index_map: (0..=255).collect(),
        models: vec![Model {
            size: Size {
                x: WALL_SIZE.cast_unsigned(),
                y: WALL_SIZE.cast_unsigned(),
                z: (WALL_FRONT + 2).cast_unsigned(),
            },
            voxels: world_voxels(),
        }],
        palette: palette(),
        materials,
        scenes: vec![],
        layers: vec![],
    }
}

fn world_voxels() -> Vec<Voxel> {
    let mut voxels = Vec::new();

    for x in 0..WALL_SIZE {
        for y in 0..WALL_SIZE {
            voxels.push(world_voxel(IVec3::new(x, y, WALL_FRONT), WALL));
            voxels.push(world_voxel(IVec3::new(x, y, WALL_FRONT + 1), WALL));
        }
    }

    for x in PANE_MIN..PANE_MAX {
        for y in PANE_MIN..PANE_MAX {
            voxels.push(world_voxel(IVec3::new(x, y, PANE_FRONT), PANE));
        }
    }

    voxels
}

fn world_voxel(position: IVec3, slot: u8) -> Voxel {
    Voxel {
        x: u8::try_from(position.x).unwrap_or(0),
        y: u8::try_from(position.z).unwrap_or(0),
        z: u8::try_from(position.y).unwrap_or(0),
        i: slot,
    }
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

    for (slot, color) in [
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
                a: 255,
            },
        ),
        (
            PRODUCT,
            Color {
                r: 220,
                g: 80,
                b: 120,
                a: 128,
            },
        ),
        (
            FALLBACK,
            Color {
                r: 80,
                g: 220,
                b: 120,
                a: 128,
            },
        ),
        (
            ZERO,
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
        ),
    ] {
        if let Some(entry) = colors.get_mut(usize::from(slot)) {
            *entry = color;
        }
    }

    colors
}

fn valid_materials() -> Vec<Material> {
    vec![
        material(2, &[("_alpha", "1.0")]),
        material(3, &[("_alpha", "0.5")]),
        material(4, &[("_alpha", "0.5")]),
        material(6, &[("_alpha", "0.5")]),
    ]
}

fn material(id: u32, properties: &[(&str, &str)]) -> Material {
    let mut values = Dict::new();

    for (key, value) in properties {
        values.insert((*key).to_owned(), (*value).to_owned());
    }

    Material {
        id,
        properties: values,
    }
}

fn write_data(directory: &Path, name: &str, data: &DotVoxData) -> io::Result<()> {
    let mut bytes = Vec::new();
    data.write_vox(&mut bytes)?;
    write_bytes(directory, name, &bytes)
}

fn write_with_imap(directory: &Path, name: &str, data: &DotVoxData, map: &[u8]) -> io::Result<()> {
    let mut bytes = Vec::new();
    data.write_vox(&mut bytes)?;

    let header = bytes
        .get(8..12)
        .ok_or_else(|| io::Error::other("generated VOX file has no MAIN header"))?;

    if header != b"MAIN" {
        return Err(io::Error::other("generated VOX file has no MAIN header"));
    }

    let child_size =
        usize::try_from(read_u32(bytes.get(16..20).ok_or_else(|| {
            io::Error::other("generated VOX file has no MAIN size")
        })?)?)
        .map_err(|_| io::Error::other("MAIN child size does not fit usize"))?;
    let children_end = 20usize
        .checked_add(child_size)
        .ok_or_else(|| io::Error::other("MAIN child size overflowed"))?;
    let children = bytes
        .get(20..children_end)
        .ok_or_else(|| io::Error::other("MAIN children exceed the file"))?;
    let mut main_children = children.to_vec();
    main_children.extend_from_slice(&chunk(*b"IMAP", map, &[]));

    let main = chunk(*b"MAIN", &[], &main_children);
    let mut output = bytes
        .get(..8)
        .ok_or_else(|| io::Error::other("generated VOX file has no header"))?
        .to_vec();
    output.extend_from_slice(&main);

    write_bytes(directory, name, &output)
}

fn chunk(kind: [u8; 4], content: &[u8], children: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&kind);
    bytes.extend_from_slice(
        &u32::try_from(content.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(children.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    bytes.extend_from_slice(content);
    bytes.extend_from_slice(children);
    bytes
}

fn read_u32(bytes: &[u8]) -> io::Result<u32> {
    let value: [u8; 4] = bytes
        .try_into()
        .map_err(|_| io::Error::other("VOX size field was not four bytes"))?;
    Ok(u32::from_le_bytes(value))
}

fn write_bytes(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let path = directory.join(name);
    let mut file = fs::File::create(&path)?;
    file.write_all(bytes)?;
    file.flush()?;
    println!("wrote {}", path.display());
    Ok(())
}
