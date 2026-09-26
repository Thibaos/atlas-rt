//! The Physical material override, driven through the world load seam.
//!
//! A world file's sibling `<path>_mat` replaces the Physical material table
//! when it parses. The parse exposes its rejection reasons as data, the load
//! hands the resulting table over with its snapshots and palette, and nothing
//! of the rejection reaches the view or the Palette.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use atlas_rt::world::{
    World,
    format::{get_effective_palette, open_file},
    material::{
        Override, PhysicalMaterial, PhysicalMaterialTable, Rule, load_override, load_table,
        override_path,
    },
    update::job::{Finished, LoadedWorld, Status, WorldSource, WorldUpdateJob},
};

const WORLD: &str = "assets/test/matl-alpha.vox";

const VALID: &str = "\
# material 9 liquid solid=true

material 0 solid solid=false
material 7 falling_granular solid=true
material 255 falling_granular solid=false";

const INVALID: &str = "\
material 0 solid solid=false
material 300 solid solid=true
material 4 liquid solid=true";

/// A copy of the test world in a private directory, with the given override
/// beside it. The directory leaves with the test.
struct Fixture {
    directory: PathBuf,
    world: PathBuf,
}

impl Fixture {
    fn new(name: &str, override_text: Option<&str>) -> Self {
        let directory = std::env::temp_dir().join(format!("atlas-rt-physical-{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("could not make {}: {error}", directory.display()));

        let world = directory.join("world.vox");
        let bytes =
            std::fs::read(WORLD).unwrap_or_else(|error| panic!("could not read {WORLD}: {error}"));
        std::fs::write(&world, bytes)
            .unwrap_or_else(|error| panic!("could not write {}: {error}", world.display()));

        if let Some(text) = override_text {
            let path = override_path(&world);

            std::fs::write(&path, text)
                .unwrap_or_else(|error| panic!("could not write {}: {error}", path.display()));
        }

        Self { directory, world }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// The world file the loader reads, with its sibling override beside it.
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

/// Loads one world through the real job and hands back the work it produces
/// with the status it settles at.
fn load(path: &Path) -> (LoadedWorld, Status) {
    let mut job = WorldUpdateJob::new();

    job.load(
        Box::new(WorldFile {
            path: path.to_path_buf(),
        }),
        0,
    )
    .unwrap_or_else(|error| panic!("the load was refused: {error:?}"));

    let deadline = Instant::now() + Duration::from_secs(5);

    let finished = loop {
        if let Some(finished) = job.poll() {
            break finished;
        }

        assert!(
            Instant::now() < deadline,
            "the load did not settle within five seconds"
        );

        std::thread::sleep(Duration::from_millis(1));
    };

    assert_eq!(finished, Finished::Loaded, "the world loads either way");

    let loaded = job
        .take_loaded()
        .unwrap_or_else(|| panic!("the load must hand over its work"));
    job.arrive();

    (loaded, job.status())
}

/// The world and palette an override must leave untouched.
fn reference() -> (World, [glam::Vec4; 256]) {
    let data = open_file(WORLD);
    let palette = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error:#}"));

    (World::new(&data), palette)
}

fn blocking() -> PhysicalMaterial {
    PhysicalMaterial {
        rule: Rule::Solid,
        solid: true,
    }
}

fn rejections(path: &Path) -> Vec<String> {
    let Override::Rejected(rejections) = load_override(&override_path(path)) else {
        panic!("the present file must be rejected");
    };

    rejections.iter().map(ToString::to_string).collect()
}

#[test]
fn a_valid_override_reaches_the_loaded_world_and_the_load_still_readies() {
    let fixture = Fixture::new("valid", Some(VALID));
    let (loaded, status) = load(&fixture.world);

    assert_eq!(status, Status::Ready);
    assert_eq!(
        loaded.materials.get(0),
        PhysicalMaterial {
            rule: Rule::Solid,
            solid: false,
        }
    );
    assert_eq!(
        loaded.materials.get(7),
        PhysicalMaterial {
            rule: Rule::FallingGranular,
            solid: true,
        }
    );
    assert_eq!(
        loaded.materials.get(255),
        PhysicalMaterial {
            rule: Rule::FallingGranular,
            solid: false,
        }
    );
    assert_eq!(
        loaded.materials.get(1),
        blocking(),
        "an unnamed index keeps the built-in default"
    );
}

#[test]
fn a_rejected_override_reports_every_bad_line_and_falls_back_at_the_seam() {
    let fixture = Fixture::new("rejected", Some(INVALID));

    assert_eq!(
        rejections(&fixture.world),
        [
            "line 2: material index 300 is outside 0 through 255",
            "line 3: unknown rule `liquid`",
        ]
    );

    let (loaded, status) = load(&fixture.world);

    assert_eq!(status, Status::Ready, "a rejected override still readies");
    assert_eq!(
        loaded.materials,
        PhysicalMaterialTable::default(),
        "the valid record on line 1 is discarded with the rest of the file"
    );
}

#[test]
fn an_absent_override_is_the_normal_case_and_still_readies() {
    let fixture = Fixture::new("absent", None);

    assert_eq!(
        load_override(&override_path(&fixture.world)),
        Override::Absent
    );
    assert_eq!(load_table(None), PhysicalMaterialTable::default());

    let (loaded, status) = load(&fixture.world);

    assert_eq!(status, Status::Ready);
    assert_eq!(loaded.materials, PhysicalMaterialTable::default());
}

#[test]
fn a_present_override_that_cannot_be_read_is_rejected() {
    let fixture = Fixture::new("unreadable", None);
    let path = override_path(&fixture.world);
    std::fs::create_dir(&path)
        .unwrap_or_else(|error| panic!("could not make {}: {error}", path.display()));

    let reported = rejections(&fixture.world);
    let Some(reason) = reported.first() else {
        panic!("a present file that fails must produce a reason");
    };

    assert!(reason.starts_with("the file could not be read"), "{reason}");
    assert_eq!(reported.len(), 1, "one failure, one reason");
}

#[test]
fn a_present_override_that_is_not_text_is_rejected() {
    let fixture = Fixture::new("binary", None);
    let path = override_path(&fixture.world);
    std::fs::write(&path, [0x6d, 0x61, 0x74, 0xff])
        .unwrap_or_else(|error| panic!("could not write {}: {error}", path.display()));

    let reported = rejections(&fixture.world);
    let Some(reason) = reported.first() else {
        panic!("a present file that fails must produce a reason");
    };

    assert!(reason.starts_with("the file is not text"), "{reason}");
    assert_eq!(reported.len(), 1, "one failure, one reason");
}

#[test]
fn a_valid_override_changes_neither_the_palette_nor_the_occupancy() {
    let fixture = Fixture::new("palette-valid", Some(VALID));
    let (loaded, _) = load(&fixture.world);
    let (world, palette) = reference();

    assert_eq!(
        loaded.palette, palette,
        "the override never touches the Palette"
    );
    assert_eq!(
        loaded.world.voxel_count(),
        world.voxel_count(),
        "occupancy alone still decides whether a voxel exists"
    );
}

#[test]
fn a_rejected_override_changes_neither_the_palette_nor_the_occupancy() {
    let fixture = Fixture::new("palette-rejected", Some(INVALID));
    let (loaded, _) = load(&fixture.world);
    let (world, palette) = reference();

    assert_eq!(loaded.palette, palette, "the fallback world draws the same");
    assert_eq!(
        loaded.world.voxel_count(),
        world.voxel_count(),
        "occupancy alone still decides whether a voxel exists"
    );
}
