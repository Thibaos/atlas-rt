//! The fixed materials a generated World draws on.
//!
//! A Vocabulary owns the material enum, the Palette colours, the Physical
//! material rules, and the feature tags. The Material enum's discriminants are
//! the Material indices, so a name and the index that paints it have no
//! indirection between them.

use glam::Vec4;

use super::material::{PhysicalMaterial, PhysicalMaterialTable, Rule};

/// A material a generated World draws on. The discriminant is the Material
/// index the Palette and the Physical material table are keyed by.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Bedrock = 0,
    Stone = 1,
    Dirt = 2,
    Grass = 3,
    Sand = 4,
}

impl Material {
    /// The materials a generated World names, one entry per discriminant.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Bedrock,
        Self::Stone,
        Self::Dirt,
        Self::Grass,
        Self::Sand,
    ];
    pub const COUNT: usize = 5;

    /// The Material index this material paints with.
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// The Palette colour this material paints with, opaque and sRGB-encoded.
    #[must_use]
    pub const fn color(self) -> Vec4 {
        match self {
            Self::Bedrock => color(52, 52, 56),
            Self::Stone => color(128, 128, 128),
            Self::Dirt => color(110, 76, 48),
            Self::Grass => color(72, 140, 56),
            Self::Sand => color(198, 178, 116),
        }
    }

    /// The simulation behavior and player solidity this material carries.
    #[must_use]
    pub const fn physical(self) -> PhysicalMaterial {
        match self {
            Self::Sand => PhysicalMaterial {
                rule: Rule::FallingGranular,
                solid: true,
            },
            Self::Bedrock | Self::Stone | Self::Dirt | Self::Grass => PhysicalMaterial {
                rule: Rule::Solid,
                solid: true,
            },
        }
    }
}

const fn color(r: u8, g: u8, b: u8) -> Vec4 {
    Vec4::new(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0,
    )
}

/// A feature a Seed fixes, separated from its siblings by a fixed tag.
///
/// The tag enters the integer hash beside the Seed and the column, so a feature
/// added later leaves the Worlds of the features before it unchanged.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Terrain = 0,
}

impl Feature {
    /// The features a Seed may separate, one entry per discriminant.
    pub const ALL: [Self; Self::COUNT] = [Self::Terrain];
    pub const COUNT: usize = 1;

    /// The fixed tag this feature hashes the Seed with. A tag is never reused.
    #[must_use]
    pub const fn tag(self) -> u64 {
        match self {
            Self::Terrain => 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// The fixed set of materials a generated World draws on.
///
/// It holds their Palette colours, their Physical material rules, and the
/// feature tags. Unused Palette entries stay zero, which is fully transparent,
/// and unused table entries keep the built-in default, which is solid and
/// player-blocking.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vocabulary;

impl Vocabulary {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The Palette a generation's World paints with: every named Material at
    /// its colour, every other entry zero.
    #[must_use]
    pub fn palette(self) -> [Vec4; 256] {
        let mut palette = [Vec4::ZERO; 256];

        for material in Material::ALL {
            if let Some(slot) = palette.get_mut(usize::from(material.index())) {
                *slot = material.color();
            }
        }

        palette
    }

    /// The Physical material table a generation's World simulates with: every
    /// named Material at its rule, every other entry the built-in default.
    #[must_use]
    pub fn materials(self) -> PhysicalMaterialTable {
        let mut table = PhysicalMaterialTable::default();

        for material in Material::ALL {
            table.set(material.index(), material.physical());
        }

        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary() -> Vocabulary {
        Vocabulary::new()
    }

    fn unnamed() -> Vec<u8> {
        let named = Material::ALL.map(Material::index);

        (0..=u8::MAX)
            .filter(|entry| !named.contains(entry))
            .collect()
    }

    #[test]
    fn every_named_material_paints_an_opaque_colour() {
        let palette = vocabulary().palette();

        for material in Material::ALL {
            let Some(color) = palette.get(usize::from(material.index())) else {
                panic!("material {material:?} is inside the Palette");
            };

            assert!(
                color.w > 0.0,
                "material {material:?} must paint, not stay transparent"
            );
        }
    }

    #[test]
    fn unused_palette_entries_stay_zero() {
        let palette = vocabulary().palette();

        for entry in unnamed() {
            let Some(color) = palette.get(usize::from(entry)) else {
                panic!("entry {entry} is inside the Palette");
            };

            assert!(
                *color == Vec4::ZERO,
                "unused entry {entry} must stay zero, found {color:?}"
            );
        }
    }

    #[test]
    fn each_material_carries_its_rule_and_solidity_through_the_table() {
        let materials = vocabulary().materials();

        for material in Material::ALL {
            assert_eq!(
                materials.get(material.index()),
                material.physical(),
                "material {material:?} does not carry its own rule and solidity"
            );
            assert!(
                materials.get(material.index()).solid,
                "material {material:?} blocks the player"
            );
        }

        assert_eq!(
            materials.get(Material::Sand.index()).rule,
            Rule::FallingGranular,
            "Sand settles, so the table is not the built-in default"
        );
    }

    #[test]
    fn unused_table_entries_keep_the_built_in_default() {
        let materials = vocabulary().materials();
        let default = PhysicalMaterialTable::default();

        for entry in unnamed() {
            assert_eq!(
                materials.get(entry),
                default.get(entry),
                "unused entry {entry} must keep the built-in default"
            );
        }
    }

    #[test]
    fn the_discriminants_are_the_distinct_contiguous_indices() {
        assert_eq!(Material::ALL.len(), Material::COUNT);

        let mut indices: Vec<u8> = Material::ALL.map(Material::index).into();

        indices.sort_unstable();

        let contiguous: Vec<u8> = (0..Material::COUNT)
            .map(|index| u8::try_from(index).unwrap_or(u8::MAX))
            .collect();

        assert_eq!(indices, contiguous);
    }

    #[test]
    fn the_feature_tags_are_unique() {
        assert_eq!(Feature::ALL.len(), Feature::COUNT);

        let mut tags: Vec<u64> = Feature::ALL.map(Feature::tag).into();

        tags.sort_unstable();
        tags.dedup();

        assert_eq!(
            tags.len(),
            Feature::COUNT,
            "two features must not share a tag"
        );
    }
}
