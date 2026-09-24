use std::collections::{HashMap, HashSet};

use anyhow::{Context, anyhow, bail};
use tracing::warn;

/// # Panics
///
/// Panics if `path` cannot be read or parsed as a `.vox` file.
#[must_use]
pub fn open_file(path: &str) -> dot_vox::DotVoxData {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("could not read {path}: {error}"));

    open_bytes(&bytes).unwrap_or_else(|error| panic!("could not load {path}: {error}"))
}

/// # Errors
///
/// Returns an error if the file loading failed, the palette is too large, or
/// the file contains an unsupported `IMAP` map.
pub fn open_bytes(bytes: &[u8]) -> anyhow::Result<dot_vox::DotVoxData> {
    let data = dot_vox::load_bytes(bytes).map_err(|e| anyhow!(e))?;

    if data.palette.len() > 256 {
        bail!("palette length is greater than 256");
    }

    validate_imap(bytes)?;

    Ok(data)
}

struct ChunkSlice {
    kind: [u8; 4],
    content_start: usize,
    content_end: usize,
    children_start: usize,
    end: usize,
}

fn read_u32(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    let end = offset
        .checked_add(4)
        .context("chunk size offset overflowed")?;
    let slice = bytes
        .get(offset..end)
        .with_context(|| format!("chunk header ended at {end}"))?;
    let value: [u8; 4] = slice
        .try_into()
        .map_err(|_| anyhow!("chunk size was not four bytes"))?;

    Ok(u32::from_le_bytes(value))
}

fn chunk_at(bytes: &[u8], offset: usize) -> anyhow::Result<Option<ChunkSlice>> {
    if offset == bytes.len() {
        return Ok(None);
    }

    if offset > bytes.len() {
        bail!("chunk offset {offset} is past the file");
    }

    let kind_end = offset
        .checked_add(4)
        .context("chunk id offset overflowed")?;
    let content_size_offset = offset
        .checked_add(4)
        .context("chunk content size offset overflowed")?;
    let children_size_offset = offset
        .checked_add(8)
        .context("chunk child size offset overflowed")?;
    let header_end = offset
        .checked_add(12)
        .context("chunk header offset overflowed")?;

    if header_end > bytes.len() {
        bail!("chunk header ended at {header_end}");
    }

    let kind: [u8; 4] = bytes
        .get(offset..kind_end)
        .context("chunk id was missing")?
        .try_into()
        .map_err(|_| anyhow!("chunk id was not four bytes"))?;
    let content_size = usize::try_from(read_u32(bytes, content_size_offset)?)
        .context("chunk content size does not fit usize")?;
    let children_size = usize::try_from(read_u32(bytes, children_size_offset)?)
        .context("chunk child size does not fit usize")?;
    let content_start = header_end;
    let content_end = content_start
        .checked_add(content_size)
        .context("chunk content end overflowed")?;
    let children_start = content_end;
    let end = children_start
        .checked_add(children_size)
        .context("chunk end overflowed")?;

    if end > bytes.len() {
        bail!("chunk ended at {end}, past the file");
    }

    Ok(Some(ChunkSlice {
        kind,
        content_start,
        content_end,
        children_start,
        end,
    }))
}

fn validate_child_chunks(bytes: &[u8], start: usize, end: usize) -> anyhow::Result<()> {
    let mut offset = start;

    while offset < end {
        let chunk = chunk_at(bytes, offset)?.ok_or_else(|| anyhow!("missing child chunk"))?;

        if &chunk.kind == b"IMAP" {
            if chunk.children_start != chunk.end {
                bail!("IMAP must not contain child chunks");
            }

            let map_length = chunk
                .content_end
                .checked_sub(chunk.content_start)
                .context("IMAP content bounds were invalid")?;

            if map_length != dot_vox::DEFAULT_INDEX_MAP.len() {
                bail!("IMAP length {map_length} is not 256");
            }

            let map = bytes
                .get(chunk.content_start..chunk.content_end)
                .context("IMAP content was missing")?;

            if map != dot_vox::DEFAULT_INDEX_MAP {
                bail!("non-default IMAP is unsupported");
            }
        }

        offset = chunk.end;
    }

    if offset != end {
        bail!("child chunks ended at {offset}, expected {end}");
    }

    Ok(())
}

fn validate_imap(bytes: &[u8]) -> anyhow::Result<()> {
    let main = chunk_at(bytes, 8)?.ok_or_else(|| anyhow!("VOX file has no MAIN chunk"))?;

    if &main.kind != b"MAIN" {
        bail!("VOX file does not start with MAIN");
    }

    if main.end != bytes.len() {
        bail!("VOX file has data after MAIN");
    }

    validate_child_chunks(bytes, main.children_start, main.end)
}

#[must_use]
pub fn get_palette(data: &dot_vox::DotVoxData) -> [glam::Vec4; 256] {
    let mut array = [glam::Vec4::ZERO; 256];

    for (slot, color) in array.iter_mut().zip(data.palette.iter()) {
        *slot = glam::Vec4::new(
            f32::from(color.r) / 255.0,
            f32::from(color.g) / 255.0,
            f32::from(color.b) / 255.0,
            f32::from(color.a) / 255.0,
        );
    }

    array
}

/// # Errors
///
/// Returns an error when `MATL` contains duplicate material IDs or the world
/// carries a non-default `IMAP` map.
pub fn get_effective_palette(data: &dot_vox::DotVoxData) -> anyhow::Result<[glam::Vec4; 256]> {
    if data.index_map != dot_vox::DEFAULT_INDEX_MAP {
        bail!("non-default IMAP is unsupported");
    }

    let mut palette = get_palette(data);
    let mut seen_ids = HashSet::new();
    let mut usable_alphas = HashMap::new();
    let mut invalid_alpha = 0usize;
    let mut unsupported_properties = 0usize;

    for material in &data.materials {
        if !seen_ids.insert(material.id) {
            bail!("duplicate MATL id {}", material.id);
        }

        let Some(slot) = supported_palette_slot(material.id) else {
            continue;
        };

        let alpha = material_alpha(material, &mut invalid_alpha, &mut unsupported_properties);

        if let Some(alpha) = alpha {
            usable_alphas.insert(material.id, alpha);

            if let Some(color) = palette.get_mut(slot) {
                color.w *= alpha;
            }
        }
    }

    let (missing_materials, fallback_materials) = if data.materials.is_empty() {
        (0, 0)
    } else {
        material_fallback_counts(data, &seen_ids, &usable_alphas)
    };

    let warning_count = invalid_alpha
        .saturating_add(unsupported_properties)
        .saturating_add(missing_materials)
        .saturating_add(fallback_materials);
    if warning_count > 0 {
        warn!(
            invalid_alpha,
            unsupported_properties,
            missing_materials,
            fallback_materials,
            "atlas_rt: MATL alpha fell back to the palette"
        );
    }

    Ok(palette)
}

fn material_alpha(
    material: &dot_vox::Material,
    invalid_alpha: &mut usize,
    unsupported_properties: &mut usize,
) -> Option<f32> {
    let Some(value) = material.properties.get("_alpha") else {
        if has_unsupported_properties(&material.properties) {
            *unsupported_properties = unsupported_properties.saturating_add(1);
        }

        return None;
    };

    match value.parse::<f32>() {
        Ok(value) if value.is_finite() && (0.0..=1.0).contains(&value) => Some(value),
        _ => {
            *invalid_alpha = invalid_alpha.saturating_add(1);
            None
        }
    }
}

fn supported_palette_slot(id: u32) -> Option<usize> {
    if id == 0 || id >= 256 {
        return None;
    }

    usize::try_from(id.saturating_sub(1)).ok()
}

fn has_unsupported_properties(properties: &dot_vox::Dict) -> bool {
    ["_trans", "_ior", "_d", "_att"]
        .iter()
        .any(|key| properties.contains_key(*key))
}

fn material_fallback_counts(
    data: &dot_vox::DotVoxData,
    seen_ids: &HashSet<u32>,
    usable_alphas: &HashMap<u32, f32>,
) -> (usize, usize) {
    let mut missing: usize = 0;
    let mut fallback: usize = 0;

    for slot in used_material_slots(data) {
        if slot >= 255 {
            continue;
        }

        let Ok(slot) = u32::try_from(slot) else {
            continue;
        };

        let Some(id) = slot.checked_add(1) else {
            continue;
        };

        if !seen_ids.contains(&id) {
            missing = missing.saturating_add(1);
        } else if !usable_alphas.contains_key(&id) {
            fallback = fallback.saturating_add(1);
        }
    }

    (missing, fallback)
}

fn used_material_slots(data: &dot_vox::DotVoxData) -> HashSet<usize> {
    data.models
        .iter()
        .flat_map(|model| model.voxels.iter().map(|voxel| usize::from(voxel.i)))
        .collect()
}

#[cfg(test)]
mod tests {
    use dot_vox::{Color, Dict, DotVoxData, Material, Model, Size, Voxel};
    use glam::Vec4;

    use super::{get_effective_palette, get_palette, open_bytes};

    fn material(id: u32, alpha: Option<&str>) -> Material {
        let mut properties = Dict::new();

        if let Some(alpha) = alpha {
            properties.insert(String::from("_alpha"), String::from(alpha));
        }

        Material { id, properties }
    }

    fn material_with_properties(id: u32, properties: &[(&str, &str)]) -> Material {
        let mut values = Dict::new();

        for (key, value) in properties {
            values.insert((*key).to_owned(), (*value).to_owned());
        }

        Material {
            id,
            properties: values,
        }
    }

    fn data(materials: Vec<Material>) -> DotVoxData {
        DotVoxData {
            version: 150,
            index_map: dot_vox::DEFAULT_INDEX_MAP.to_vec(),
            models: vec![Model {
                size: Size { x: 1, y: 1, z: 1 },
                voxels: vec![Voxel {
                    x: 0,
                    y: 0,
                    z: 0,
                    i: 1,
                }],
            }],
            palette: vec![
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                Color {
                    r: 0,
                    g: 255,
                    b: 0,
                    a: 128,
                },
            ],
            materials,
            scenes: vec![],
            layers: vec![],
        }
    }

    fn alpha(palette: &[Vec4; 256], slot: usize) -> f32 {
        palette.get(slot).map_or(0.0, |color| color.w)
    }

    fn half(value: f32) -> f32 {
        value.mul_add(0.5, 0.0)
    }

    fn expect_error<T, E>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}"),
            Err(error) => error,
        }
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

    fn vox_with_imap(map: Option<&[u8]>) -> Vec<u8> {
        vox_with_imap_children(map, &[])
    }

    fn vox_with_imap_children(map: Option<&[u8]>, imap_children: &[u8]) -> Vec<u8> {
        let size = chunk(*b"SIZE", &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[]);
        let voxel = chunk(*b"XYZI", &[1, 0, 0, 0, 0, 0, 0, 2], &[]);
        let palette = chunk(*b"RGBA", &[0; 1024], &[]);
        let mut children = [size, voxel, palette].concat();

        if let Some(map) = map {
            children.extend_from_slice(&chunk(*b"IMAP", map, imap_children));
        }

        let main = chunk(*b"MAIN", &[], &children);

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(&main);
        bytes
    }

    #[test]
    fn matl_alpha_multiplies_palette_alpha() {
        let data = data(vec![material(2, Some("0.5"))]);
        let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

        assert!((alpha(&palette, 0) - 1.0).abs() < 1.0e-6);
        assert!((alpha(&palette, 1) - (half(128.0 / 255.0))).abs() < 1.0e-6);
    }

    #[test]
    fn invalid_matl_alpha_falls_back_to_palette_alpha() {
        let data = data(vec![material(2, Some("not-a-number"))]);
        let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

        assert!((alpha(&palette, 1) - 128.0 / 255.0).abs() < 1.0e-6);
    }

    #[test]
    fn explicit_alpha_does_not_require_a_glass_type() {
        let data = data(vec![material_with_properties(
            2,
            &[("_type", "_metal"), ("_alpha", "0.5")],
        )]);
        let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

        assert!((alpha(&palette, 1) - (half(128.0 / 255.0))).abs() < 1.0e-6);
    }

    #[test]
    fn unsupported_properties_fall_back_when_alpha_is_absent() {
        let data = data(vec![material_with_properties(
            2,
            &[("_trans", "0.5"), ("_ior", "0.3")],
        )]);
        let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

        assert!((alpha(&palette, 1) - 128.0 / 255.0).abs() < 1.0e-6);
    }

    #[test]
    fn missing_matl_records_fall_back_without_changing_the_palette() {
        let data = data(vec![]);
        let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

        assert!((alpha(&palette, 1) - 128.0 / 255.0).abs() < 1.0e-6);
    }

    #[test]
    fn unpaintable_matl_ids_are_ignored() {
        let data = data(vec![material(0, Some("0")), material(256, Some("0"))]);
        let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

        assert!((alpha(&palette, 1) - 128.0 / 255.0).abs() < 1.0e-6);
    }

    #[test]
    fn duplicate_matl_ids_are_rejected() {
        let data = data(vec![material(2, Some("0.5")), material(2, Some("0.25"))]);
        let error = expect_error(get_effective_palette(&data), "duplicate ids must reject");

        assert!(error.to_string().contains("duplicate MATL id 2"));
    }

    #[test]
    fn default_imap_is_accepted_as_a_no_op() {
        let bytes = vox_with_imap(Some(dot_vox::DEFAULT_INDEX_MAP));

        assert!(open_bytes(&bytes).is_ok());
    }

    #[test]
    fn non_default_imap_is_rejected_by_effective_palette() {
        let mut data = data(vec![]);

        if let Some(first) = data.index_map.first_mut() {
            *first = 2;
        }

        let error = expect_error(get_effective_palette(&data), "non-default IMAP must reject");

        assert!(error.to_string().contains("non-default IMAP"));
    }

    #[test]
    fn non_default_imap_is_rejected() {
        let mut map = dot_vox::DEFAULT_INDEX_MAP.to_vec();

        if let Some(first) = map.first_mut() {
            *first = 2;
        }

        let bytes = vox_with_imap(Some(&map));
        let error = expect_error(open_bytes(&bytes), "non-default IMAP must reject");

        assert!(error.to_string().contains("non-default IMAP"));
    }

    #[test]
    fn malformed_imap_is_rejected() {
        let bytes = vox_with_imap(Some(&[1, 2]));
        let error = expect_error(open_bytes(&bytes), "malformed IMAP must reject");

        assert!(error.to_string().contains("IMAP length"));
    }

    #[test]
    fn imap_with_child_chunks_is_rejected() {
        let bytes =
            vox_with_imap_children(Some(dot_vox::DEFAULT_INDEX_MAP), &chunk(*b"NOPE", &[], &[]));
        let error = expect_error(open_bytes(&bytes), "IMAP child chunks must reject");

        assert!(
            error
                .to_string()
                .contains("IMAP must not contain child chunks")
        );
    }

    #[test]
    fn data_after_main_is_rejected() {
        let mut bytes = vox_with_imap(None);
        bytes.extend_from_slice(b"trailing");
        let error = expect_error(open_bytes(&bytes), "trailing data must reject");

        assert!(error.to_string().contains("data after MAIN"));
    }

    #[test]
    fn raw_palette_does_not_include_matl_alpha() {
        let data = data(vec![material(2, Some("0.5"))]);
        let palette = get_palette(&data);

        assert!((alpha(&palette, 1) - 128.0 / 255.0).abs() < 1.0e-6);
    }
}
