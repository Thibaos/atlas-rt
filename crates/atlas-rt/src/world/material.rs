use std::{
    fmt::Display,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use tracing::warn;

/// A Material index's simulation behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Solid,
    FallingGranular,
}

/// A Material index's simulation behavior and whether the occupied voxel
/// blocks the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalMaterial {
    pub rule: Rule,
    pub solid: bool,
}

const BUILT_IN: PhysicalMaterial = PhysicalMaterial {
    rule: Rule::Solid,
    solid: true,
};

/// The mapping from Material index to simulation behavior and player
/// solidity. It sits beside the Palette rather than inside it, and occupancy
/// still decides whether a voxel exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalMaterialTable {
    entries: [PhysicalMaterial; 256],
}

impl Default for PhysicalMaterialTable {
    fn default() -> Self {
        Self {
            entries: [BUILT_IN; 256],
        }
    }
}

impl PhysicalMaterialTable {
    /// The entry for `index`, solid and player-blocking unless an override
    /// named it.
    #[must_use]
    #[allow(clippy::indexing_slicing)] // a u8 reaches all 256 entries
    pub const fn get(self, index: u8) -> PhysicalMaterial {
        self.entries[index as usize]
    }

    #[allow(clippy::indexing_slicing)] // a u8 reaches all 256 entries
    const fn set(&mut self, index: u8, material: PhysicalMaterial) {
        self.entries[index as usize] = material;
    }
}

/// One invalid record in a rejected override: the line the author has to fix
/// and the reason it failed. `line` is `None` when the whole file failed, as
/// an unreadable file has no line to name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub line: Option<usize>,
    pub reason: String,
}

impl Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { line, reason } = self;

        match line {
            Some(line) => write!(f, "line {line}: {reason}"),
            None => f.write_str(reason),
        }
    }
}

/// A world's Physical material override, decided by presence: absent is the
/// normal case, applied is a parsed file, rejected is a present file that
/// failed for any reason.
#[derive(Debug, PartialEq, Eq)]
pub enum Override {
    Absent,
    Applied(Box<PhysicalMaterialTable>),
    Rejected(Vec<Rejection>),
}

/// Parses a `.vox_mat` body, one `material <index> <rule> solid=<bool>`
/// record per line, ignoring comments and blank lines.
///
/// # Errors
///
/// Returns every invalid record with its line number and reason instead of
/// stopping at the first, so the caller can discard the whole file and report
/// all of an author's typos at once.
pub fn parse_override(text: &str) -> Result<PhysicalMaterialTable, Vec<Rejection>> {
    let mut table = PhysicalMaterialTable::default();
    let mut rejections = Vec::new();

    for (offset, line) in text.lines().enumerate() {
        let record = line.trim();

        if record.is_empty() || record.starts_with('#') {
            continue;
        }

        match parse_record(record) {
            Ok((index, material)) => table.set(index, material),
            Err(reason) => rejections.push(Rejection {
                line: Some(offset.saturating_add(1)),
                reason,
            }),
        }
    }

    if rejections.is_empty() {
        Ok(table)
    } else {
        Err(rejections)
    }
}

fn parse_record(record: &str) -> Result<(u8, PhysicalMaterial), String> {
    let mut fields = record.split_whitespace();
    let keyword = fields
        .next()
        .ok_or_else(|| String::from("expected a `material` record"))?;

    if keyword != "material" {
        return Err(format!("expected a `material` record, found `{keyword}`"));
    }

    let index = fields
        .next()
        .ok_or_else(|| String::from("missing the material index"))
        .and_then(parse_index)?;
    let rule = fields
        .next()
        .ok_or_else(|| String::from("missing the rule"))
        .and_then(parse_rule)?;
    let solid = fields
        .next()
        .ok_or_else(|| String::from("missing the `solid=` flag"))
        .and_then(parse_solid)?;

    if let Some(extra) = fields.next() {
        return Err(format!("unexpected `{extra}` after the `solid=` flag"));
    }

    Ok((index, PhysicalMaterial { rule, solid }))
}

fn parse_index(field: &str) -> Result<u8, String> {
    let index = field
        .parse::<i64>()
        .map_err(|_| format!("material index `{field}` is not a number"))?;

    u8::try_from(index).map_err(|_| format!("material index {index} is outside 0 through 255"))
}

fn parse_rule(field: &str) -> Result<Rule, String> {
    match field {
        "solid" => Ok(Rule::Solid),
        "falling_granular" => Ok(Rule::FallingGranular),
        _ => Err(format!("unknown rule `{field}`")),
    }
}

fn parse_solid(field: &str) -> Result<bool, String> {
    let Some(flag) = field.strip_prefix("solid=") else {
        return Err(format!("expected `solid=<true-or-false>`, found `{field}`"));
    };

    match flag {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("`{field}` is not `true` or `false`")),
    }
}

/// A world file's Physical material override, `<path>_mat` beside it, so
/// `castle.vox` reads `castle.vox_mat`.
#[must_use]
pub fn override_path(world_path: &Path) -> PathBuf {
    let mut path = world_path.as_os_str().to_os_string();
    path.push("_mat");

    PathBuf::from(path)
}

/// Reads a world's Physical material override. A missing file is `Absent`; a
/// present file that cannot be read or parsed is `Rejected`.
#[must_use]
pub fn load_override(path: &Path) -> Override {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Override::Absent,
        Err(error) => {
            return Override::Rejected(vec![Rejection {
                line: None,
                reason: format!("the file could not be read: {error}"),
            }]);
        }
    };

    let Ok(text) = String::from_utf8(bytes) else {
        return Override::Rejected(vec![Rejection {
            line: None,
            reason: String::from("the file is not text"),
        }]);
    };

    match parse_override(&text) {
        Ok(table) => Override::Applied(Box::new(table)),
        Err(rejections) => Override::Rejected(rejections),
    }
}

/// The world's Physical material table: its sibling override when that file
/// loads, the built-in table otherwise, with one terminal warning per rejection.
#[must_use]
pub fn load_table(world_path: Option<&Path>) -> PhysicalMaterialTable {
    let Some(world_path) = world_path else {
        return PhysicalMaterialTable::default();
    };

    let path = override_path(world_path);

    match load_override(&path) {
        Override::Absent => PhysicalMaterialTable::default(),
        Override::Applied(table) => *table,
        Override::Rejected(rejections) => {
            let file = path.display().to_string();
            let rejected = rejections
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");

            warn!(
                file = file.as_str(),
                rejected = ?rejections,
                "atlas_rt: rejected {file}: {rejected}; using the built-in table"
            );

            PhysicalMaterialTable::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "\
# material 9 liquid solid=true

material 0 solid solid=false
material 7 falling_granular solid=true
material 255 falling_granular solid=false";

    const INVALID: &str = "\
material 0 solid solid=false
material 300 solid solid=true
material 4 liquid solid=true
material 5 solid maybe=true
material 6 solid
castle 1 solid solid=true
material 2 solid solid=true extra";

    fn blocking() -> PhysicalMaterial {
        PhysicalMaterial {
            rule: Rule::Solid,
            solid: true,
        }
    }

    fn parse(text: &str) -> PhysicalMaterialTable {
        parse_override(text).unwrap_or_else(|rejections| panic!("{rejections:?}"))
    }

    fn rejections_of(text: &str) -> Vec<String> {
        match parse_override(text) {
            Ok(_) => panic!("the file must be rejected"),
            Err(rejections) => rejections.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn the_built_in_table_is_solid_and_player_blocking_at_every_index() {
        let table = PhysicalMaterialTable::default();

        for index in u8::MIN..=u8::MAX {
            assert_eq!(table.get(index), blocking(), "material {index}");
        }
    }

    #[test]
    fn a_valid_override_changes_only_the_indices_it_names() {
        let table = parse(VALID);

        assert_eq!(
            table.get(0),
            PhysicalMaterial {
                rule: Rule::Solid,
                solid: false,
            }
        );
        assert_eq!(
            table.get(7),
            PhysicalMaterial {
                rule: Rule::FallingGranular,
                solid: true,
            }
        );
        assert_eq!(
            table.get(255),
            PhysicalMaterial {
                rule: Rule::FallingGranular,
                solid: false,
            }
        );

        for index in [1u8, 6, 8, 254] {
            assert_eq!(table.get(index), blocking(), "material {index}");
        }

        assert_ne!(table, PhysicalMaterialTable::default());
    }

    #[test]
    fn every_invalid_record_is_collected_with_its_line_and_reason() {
        assert_eq!(
            rejections_of(INVALID),
            [
                "line 2: material index 300 is outside 0 through 255",
                "line 3: unknown rule `liquid`",
                "line 4: expected `solid=<true-or-false>`, found `maybe=true`",
                "line 5: missing the `solid=` flag",
                "line 6: expected a `material` record, found `castle`",
                "line 7: unexpected `extra` after the `solid=` flag",
            ]
        );
    }

    #[test]
    fn one_invalid_record_rejects_the_whole_file() {
        let rejections = rejections_of("material 0 solid solid=false\nmaterial 1 nope solid=true");

        assert_eq!(rejections, ["line 2: unknown rule `nope`"]);
    }

    #[test]
    fn a_missing_index_reports_a_missing_index() {
        assert_eq!(
            rejections_of("material"),
            ["line 1: missing the material index"]
        );
    }

    #[test]
    fn a_non_numeric_index_is_rejected() {
        assert_eq!(
            rejections_of("material sand solid solid=true"),
            ["line 1: material index `sand` is not a number"]
        );
    }

    #[test]
    fn an_override_sits_beside_its_world_file() {
        assert_eq!(
            override_path(Path::new("castle.vox")),
            PathBuf::from("castle.vox_mat")
        );
    }
}
