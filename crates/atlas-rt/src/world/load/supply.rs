use std::path::PathBuf;

use anyhow::Context;
use glam::{IVec3, Vec4};

use super::progress::{Progress, Stage};
use crate::world::{
    World,
    diff::snapshot::{MicroChunkSnapshot, emit_snapshots_reporting},
    generation::{self, GeneratedWorld, GenerationParams},
    material::{PhysicalMaterialTable, load_table},
    palette::get_effective_palette,
    vox::open_bytes,
};

/// A world's bytes, read on the thread that supplies the World.
pub trait WorldSource: Send {
    fn name(&self) -> String;

    /// # Errors
    ///
    /// Returns a reason the world could not be read.
    fn read(&self) -> Result<Vec<u8>, String>;

    /// The world file's real path, whose sibling `<path>_mat` holds its
    /// Physical material override. `None` for a source with no path, which
    /// reads as an absent override.
    fn filesystem_path(&self) -> Option<PathBuf> {
        None
    }
}

/// A world file on the filesystem, read on the thread that supplies the World.
pub struct FileWorldSource {
    path: PathBuf,
    name: String,
}

impl FileWorldSource {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
        }
    }
}

impl WorldSource for FileWorldSource {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.path).map_err(|error| error.to_string())
    }

    fn filesystem_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

/// A supplied World with the data that describes it. A .vox load and a
/// Generation both deliver this shape, so a host cannot tell the two supplies
/// apart.
#[derive(Debug)]
pub struct SuppliedWorld {
    pub world: World,
    pub snapshots: Vec<MicroChunkSnapshot>,
    pub palette: [Vec4; 256],
    pub materials: PhysicalMaterialTable,
    pub granular_cells: Option<Vec<IVec3>>,
    pub clipped: usize,
}

/// Reads, parses and builds `source`, then emits the World's Snapshots.
///
/// A .vox load delivers no granular cell list, because activation scans the
/// World for Falling granular cells instead.
///
/// # Errors
///
/// Returns an error naming the source when its bytes cannot be read or parsed,
/// when its Palette cannot be built, when the World needs more than `budget`
/// cells, or when its Snapshots cannot be emitted.
pub fn load(
    source: &dyn WorldSource,
    budget: usize,
    progress: &Progress,
) -> anyhow::Result<SuppliedWorld> {
    let name = source.name();

    let bytes = source
        .read()
        .map_err(|reason| anyhow::anyhow!("could not open {name}: {reason}"))?;

    progress.end_stage(Stage::Read);

    let voxel_data = open_bytes(&bytes).with_context(|| format!("could not parse {name}"))?;

    progress.end_stage(Stage::Parse);

    let palette = get_effective_palette(&voxel_data)
        .with_context(|| format!("could not build palette for {name}"))?;

    let materials = load_table(source.filesystem_path().as_deref());

    let (world, clipped) = super::build::load(&voxel_data, budget).map_err(|refused| {
        anyhow::anyhow!("the world needs {refused} cells, above the cell budget of {budget}")
    })?;

    progress.end_stage(Stage::Build);

    let snapshots = emit(progress, &world, &name)?;

    Ok(SuppliedWorld {
        world,
        snapshots,
        palette,
        materials,
        granular_cells: None,
        clipped,
    })
}

/// Generates the World `params` asks for and emits its Snapshots.
///
/// A Generation places nothing outside the Lattice, so it clips nothing, and it
/// delivers its precomputed granular cells, so activation does not scan for
/// them. It never consults the cell budget.
///
/// # Errors
///
/// Returns the generator's own reason when the params are rejected, and an error
/// naming the source when its Snapshots cannot be emitted.
pub fn generate(params: GenerationParams, progress: &Progress) -> anyhow::Result<SuppliedWorld> {
    let GeneratedWorld {
        world,
        palette,
        materials,
        granular_cells,
    } = generation::generate(progress, params).map_err(anyhow::Error::msg)?;

    progress.end_stage(Stage::Build);

    let snapshots = emit(progress, &world, "the generated world")?;

    Ok(SuppliedWorld {
        world,
        snapshots,
        palette,
        materials,
        granular_cells: Some(granular_cells),
        clipped: 0,
    })
}

/// Emits the World's Snapshots through the entry read, naming the source in the
/// failure.
fn emit(progress: &Progress, world: &World, name: &str) -> anyhow::Result<Vec<MicroChunkSnapshot>> {
    emit_snapshots_reporting(world, Some(progress))
        .with_context(|| format!("could not emit {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One voxel at the origin, in the engine's `.vox` dialect.
    fn one_voxel_world() -> Vec<u8> {
        fn chunk(id: [u8; 4], content: &[u8], children: &[u8]) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&id);
            bytes.extend_from_slice(
                &i32::try_from(content.len())
                    .unwrap_or(i32::MAX)
                    .to_le_bytes(),
            );
            bytes.extend_from_slice(
                &i32::try_from(children.len())
                    .unwrap_or(i32::MAX)
                    .to_le_bytes(),
            );
            bytes.extend_from_slice(content);
            bytes.extend_from_slice(children);

            bytes
        }

        let size = chunk(*b"SIZE", &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[]);
        let voxel = chunk(*b"XYZI", &[1, 0, 0, 0, 0, 0, 0, 7], &[]);
        let palette = chunk(*b"RGBA", &[0u8; 1024], &[]);
        let main = chunk(*b"MAIN", &[], &[size, voxel, palette].concat());

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(&main);

        bytes
    }

    /// A world's bytes, standing in for a file on disk.
    struct Bytes(Vec<u8>);

    impl WorldSource for Bytes {
        fn name(&self) -> String {
            String::from("in-memory world")
        }

        fn read(&self) -> Result<Vec<u8>, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn a_load_past_the_cell_budget_is_refused() {
        let budget = crate::world::budget::set_cell_budget(0);
        let source = Bytes(one_voxel_world());
        let error = load(
            &source,
            crate::world::budget::cell_budget(),
            &Progress::load_path(),
        )
        .err()
        .unwrap_or_else(|| panic!("a load above the budget must be refused"));

        assert_eq!(
            format!("{error:#}"),
            "the world needs 1 cells, above the cell budget of 0"
        );

        drop(budget);
    }
}
