use std::collections::{HashMap, HashSet};

use anyhow::bail;
use tracing::warn;

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
    let used_ids = used_material_ids(data);
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

        let alpha = material_alpha(
            material,
            used_ids.contains(&material.id),
            &mut invalid_alpha,
            &mut unsupported_properties,
        );

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
        material_fallback_counts(&used_ids, &seen_ids, &usable_alphas)
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
    is_referenced: bool,
    invalid_alpha: &mut usize,
    unsupported_properties: &mut usize,
) -> Option<f32> {
    let Some(value) = material.properties.get("_alpha") else {
        if is_referenced && has_unsupported_properties(&material.properties) {
            *unsupported_properties = unsupported_properties.saturating_add(1);
        }

        return None;
    };

    match value.parse::<f32>() {
        Ok(value) if value.is_finite() && (0.0..=1.0).contains(&value) => Some(value),
        _ => {
            if is_referenced {
                *invalid_alpha = invalid_alpha.saturating_add(1);
            }

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
    used_ids: &HashSet<u32>,
    seen_ids: &HashSet<u32>,
    usable_alphas: &HashMap<u32, f32>,
) -> (usize, usize) {
    let mut missing: usize = 0;
    let mut fallback: usize = 0;

    for id in used_ids {
        if !seen_ids.contains(id) {
            missing = missing.saturating_add(1);
        } else if !usable_alphas.contains_key(id) {
            fallback = fallback.saturating_add(1);
        }
    }

    (missing, fallback)
}

fn used_material_ids(data: &dot_vox::DotVoxData) -> HashSet<u32> {
    data.models
        .iter()
        .flat_map(|model| model.voxels.iter())
        .filter_map(|voxel| {
            let slot = u32::from(voxel.i);
            if slot >= 255 {
                return None;
            }

            slot.checked_add(1)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use dot_vox::{Color, Dict, DotVoxData, Material, Model, Size, Voxel};
    use glam::Vec4;

    use super::{get_effective_palette, get_palette};

    use crate::world::test_support::expect_error;

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
    fn non_default_imap_is_rejected_by_effective_palette() {
        let mut data = data(vec![]);

        if let Some(first) = data.index_map.first_mut() {
            *first = 2;
        }

        let error = expect_error(get_effective_palette(&data), "non-default IMAP must reject");

        assert!(error.to_string().contains("non-default IMAP"));
    }

    #[test]
    fn raw_palette_does_not_include_matl_alpha() {
        let data = data(vec![material(2, Some("0.5"))]);
        let palette = get_palette(&data);

        assert!((alpha(&palette, 1) - 128.0 / 255.0).abs() < 1.0e-6);
    }
}
