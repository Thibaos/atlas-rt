//! A World supply, driven through its public interface from outside the crate.
//!
//! A supply is synchronous and returns plain data, so a test reaches it with no
//! window, no GPU and no thread of its own. The load entry point is covered
//! here; a Generation is supplied through the World job and keeps its own test.

use std::path::PathBuf;

use atlas_rt::world::{
    diff::snapshot::{MicroChunkSnapshot, emit_snapshots},
    load::{
        progress::Progress,
        supply::{SuppliedWorld, WorldSource, load},
    },
    material::PhysicalMaterialTable,
};

/// A tracked fixture inside the lattice. Its `MATL` chunks put `_alpha` 1.0 on
/// material 2 and 0.5 on materials 3, 4 and 6, so the effective Palette it
/// produces carries content that can be told from the raw one.
const WORLD: &str = "assets/test/matl-alpha.vox";

/// The names the in-memory sources report, so an error can be checked to name
/// the source that produced it.
const MALFORMED: &str = "in-memory malformed world";
const UNREADABLE: &str = "in-memory unreadable world";

/// A world file on the real filesystem, read with its sibling override path.
struct WorldFile {
    path: PathBuf,
}

impl WorldSource for WorldFile {
    fn name(&self) -> String {
        self.path.display().to_string()
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.path).map_err(|error| error.to_string())
    }

    fn filesystem_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

/// Bytes held in memory, standing in for a file on disk.
struct Bytes {
    name: &'static str,
    bytes: Vec<u8>,
}

impl WorldSource for Bytes {
    fn name(&self) -> String {
        self.name.to_owned()
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        Ok(self.bytes.clone())
    }
}

/// A source whose read fails, standing in for a missing file.
struct Unreadable;

impl WorldSource for Unreadable {
    fn name(&self) -> String {
        UNREADABLE.to_owned()
    }

    fn read(&self) -> Result<Vec<u8>, String> {
        Err(String::from("no such file"))
    }
}

fn fixture() -> WorldFile {
    WorldFile {
        path: PathBuf::from(WORLD),
    }
}

fn supplied(source: &dyn WorldSource) -> SuppliedWorld {
    load(source, usize::MAX, &Progress::load_path())
        .unwrap_or_else(|error| panic!("{} must load: {error:#}", source.name()))
}

fn refused(source: &dyn WorldSource) -> String {
    let error = load(source, usize::MAX, &Progress::load_path())
        .err()
        .unwrap_or_else(|| panic!("{} must fail", source.name()));

    format!("{error:#}")
}

#[test]
fn a_vox_load_delivers_the_world_its_palette_its_material_table_and_its_snapshots() {
    let supplied = supplied(&fixture());

    assert!(supplied.world.voxel_count() > 0, "the fixture holds voxels");
    assert!(!supplied.snapshots.is_empty(), "the world emits snapshots");

    let occupied: usize = supplied
        .snapshots
        .iter()
        .map(MicroChunkSnapshot::occupied_count)
        .sum();

    assert_eq!(
        supplied.world.voxel_count(),
        occupied,
        "every voxel reaches the renderer"
    );

    // The fixture's RGBA chunk holds 170, 160, 150, 255 at slot 1, which
    // material 2 leaves at full alpha.
    let full = supplied
        .palette
        .get(1)
        .unwrap_or_else(|| panic!("slot 1 is inside the Palette"));

    assert!((full.x - 170.0_f32 / 255.0_f32).abs() < 1.0e-6, "{full:?}");
    assert!((full.w - 1.0).abs() < 1.0e-6, "material 2 keeps its alpha");

    // Material 3 halves slot 2's alpha of 255 and material 4 halves slot 3's
    // alpha of 128.
    for (slot, raw) in [(2usize, 255.0_f32), (3, 128.0)] {
        let color = supplied
            .palette
            .get(slot)
            .unwrap_or_else(|| panic!("slot {slot} is inside the Palette"));
        let expected = (raw / 255.0) * 0.5;

        assert!(
            (color.w - expected).abs() < 1.0e-6,
            "slot {slot} does not carry its Material alpha: {color:?}"
        );
    }

    assert_eq!(
        supplied.materials,
        PhysicalMaterialTable::default(),
        "no override sits beside the fixture"
    );
}

#[test]
fn the_supplied_snapshots_equal_a_fresh_emission_of_the_world() {
    let supplied = supplied(&fixture());
    let fresh = emit_snapshots(&supplied.world)
        .unwrap_or_else(|error| panic!("the world emits a second time: {error:#}"));

    assert_eq!(
        supplied.snapshots, fresh,
        "the Snapshots are emission of the World they arrive with"
    );
}

#[test]
fn an_in_lattice_load_reports_no_clipping_and_no_granular_cells() {
    let supplied = supplied(&fixture());

    assert_eq!(supplied.clipped, 0, "the fixture sits inside the Lattice");
    assert!(
        supplied.granular_cells.is_none(),
        "a .vox load seeds the queue by scanning, not from a list"
    );
}

#[test]
fn a_malformed_world_fails_with_an_error_that_names_the_source() {
    let source = Bytes {
        name: MALFORMED,
        bytes: vec![0xde, 0xad, 0xbe, 0xef],
    };
    let text = refused(&source);

    assert!(
        text.contains(MALFORMED),
        "the error names the source: {text}"
    );
    assert!(text.contains("could not parse"), "{text}");
}

#[test]
fn a_source_that_cannot_be_read_fails_with_an_error_that_names_the_source() {
    let text = refused(&Unreadable);

    assert!(
        text.contains(UNREADABLE),
        "the error names the source: {text}"
    );
    assert!(text.contains("could not open"), "{text}");
    assert!(text.contains("no such file"), "the cause survives: {text}");
}
