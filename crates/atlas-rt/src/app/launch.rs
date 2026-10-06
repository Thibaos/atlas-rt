//! The standalone binary's flags: which World supply to build and how to view
//! it.
//!
//! A run with no flags generates a World from a random Seed, so every run opens
//! on a different World. `--world` loads a `.vox` file instead, `--seed` pins
//! the Seed so a run repeats, and `--extent` bounds a Generation's ground.
//! The Seed's bits are reinterpreted from the host's signed integer.
//!
//! `--fly` and `--no-sim` are separate flags. `--fly` frees the camera from the
//! character controller and leaves the simulation running, so the free camera
//! can watch a World settle. `--no-sim` spawns no simulation thread, so no
//! voxel rule runs and the World only changes where the host edits it.
//!
//! The free camera flies on the keys the character controller reads, so under
//! `--fly` those keys also drive the simulated player. `--no-sim` has no
//! character controller to drive, so the camera flies free there too.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
};

use glam::IVec3;

use atlas_rt::world::generation::GenerationParams;

/// The World a run builds at startup: a `.vox` load or a Generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldRequest {
    Load(String),
    Generate(GenerationParams),
}

/// A parsed command line: the World to build, whether the camera flies free of
/// the simulated player, and whether to run without the simulation.
///
/// Without a simulation there is no player pose to follow, so the camera flies
/// free whether or not `free_camera` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub request: WorldRequest,
    pub free_camera: bool,
    pub no_sim: bool,
}

/// Parses the binary's arguments. Unrecognized flags are ignored, as before.
///
/// # Errors
///
/// Returns a reason when a value flag has no value, when a Seed or extent is
/// not an integer, or when `--extent` is given beside `--world`.
pub fn parse(args: &[String]) -> Result<Launch, String> {
    let mut world = None;
    let mut seed = None;
    let mut extent = None;
    let free_camera = args.iter().any(|arg| arg == "--fly");
    let no_sim = args.iter().any(|arg| arg == "--no-sim");

    let mut index = 0;

    while let Some(flag) = args.get(index) {
        match flag.as_str() {
            "--world" => {
                world = Some(value_after(args, index)?.to_owned());
                index = index.strict_add(2);
            }
            "--seed" => {
                seed = Some(parse_seed(value_after(args, index)?)?);
                index = index.strict_add(2);
            }
            "--extent" => {
                extent = Some(parse_extent(value_after(args, index)?)?);
                index = index.strict_add(2);
            }
            _ => index = index.strict_add(1),
        }
    }

    let request = match (world, seed) {
        (_, Some(seed)) => WorldRequest::Generate(generation_params(seed, extent)),
        (Some(_), None) if extent.is_some() => {
            return Err(String::from(
                "--world loads a file, so --extent has nothing to bound",
            ));
        }
        (Some(world), None) => WorldRequest::Load(world),
        (None, None) => WorldRequest::Generate(generation_params(random_seed(), extent)),
    };

    Ok(Launch {
        request,
        free_camera,
        no_sim,
    })
}

/// A Seed for a run that names none, drawn from the random hash keys the
/// standard library takes from the operating system.
fn random_seed() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// The value after the flag at `index`, or a reason naming the flag.
fn value_after(args: &[String], index: usize) -> Result<&str, String> {
    args.get(index.strict_add(1))
        .map(String::as_str)
        .ok_or_else(|| {
            format!(
                "{} needs a value",
                args.get(index).map_or("the flag", String::as_str)
            )
        })
}

/// The Seed as a `u64`, from an unsigned literal or the bits of a signed one,
/// so a negative Seed names the same World Godot's `generate_world` builds.
fn parse_seed(value: &str) -> Result<u64, String> {
    if let Ok(seed) = value.parse::<u64>() {
        return Ok(seed);
    }

    value
        .parse::<i64>()
        .map(i64::cast_unsigned)
        .map_err(|_| format!("the seed {value} is not an integer"))
}

fn parse_extent(value: &str) -> Result<i32, String> {
    value
        .parse::<i32>()
        .map_err(|_| format!("the extent {value} is not an integer"))
}

/// The Generation parameters: a square extent when one is given, the full
/// Lattice otherwise.
const fn generation_params(seed: u64, extent: Option<i32>) -> GenerationParams {
    match extent {
        Some(edge) => GenerationParams::new(seed, IVec3::splat(edge)),
        None => GenerationParams::full_lattice(seed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn launch(values: &[&str]) -> Launch {
        parse(&args(values)).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn no_flags_generate_a_world_from_a_random_seed() {
        let launch = launch(&[]);
        let WorldRequest::Generate(params) = launch.request else {
            panic!("a run with no flags generates");
        };

        assert_eq!(params.extent, GenerationParams::full_lattice(0).extent);
        assert!(!launch.free_camera);
        assert!(!launch.no_sim);
    }

    #[test]
    fn the_random_seed_changes_between_runs() {
        let WorldRequest::Generate(first) = launch(&[]).request else {
            panic!("a run with no flags generates");
        };
        let WorldRequest::Generate(second) = launch(&[]).request else {
            panic!("a run with no flags generates");
        };

        assert_ne!(first.seed, second.seed);
    }

    #[test]
    fn a_world_flag_loads_that_file() {
        assert_eq!(
            launch(&["--world", "sponza.vox"]).request,
            WorldRequest::Load(String::from("sponza.vox"))
        );
    }

    #[test]
    fn fly_frees_the_camera_and_leaves_the_simulation_running() {
        let launch = launch(&["--fly"]);

        assert!(launch.free_camera);
        assert!(
            !launch.no_sim,
            "--fly is about the camera, not the simulation"
        );
    }

    #[test]
    fn no_sim_spawns_no_simulation_without_freeing_the_camera() {
        let launch = launch(&["--no-sim"]);

        assert!(launch.no_sim);
        assert!(!launch.free_camera);
    }

    #[test]
    fn fly_and_no_sim_are_independent() {
        let launch = launch(&["--fly", "--no-sim"]);

        assert!(launch.free_camera);
        assert!(launch.no_sim);
    }

    #[test]
    fn a_seed_flag_generates_with_that_seed() {
        assert_eq!(
            launch(&["--seed", "42"]).request,
            WorldRequest::Generate(GenerationParams::full_lattice(42))
        );
    }

    #[test]
    fn a_seed_outranks_a_world() {
        assert_eq!(
            launch(&["--world", "sponza.vox", "--seed", "7"]).request,
            WorldRequest::Generate(GenerationParams::full_lattice(7))
        );
    }

    #[test]
    fn a_negative_seed_names_the_same_bits() {
        assert_eq!(
            launch(&["--seed", "-1"]).request,
            WorldRequest::Generate(GenerationParams::full_lattice(u64::MAX))
        );
    }

    #[test]
    fn an_unsigned_seed_keeps_its_full_range() {
        assert_eq!(
            launch(&["--seed", "18446744073709551615"]).request,
            WorldRequest::Generate(GenerationParams::full_lattice(u64::MAX))
        );
    }

    #[test]
    fn an_extent_flag_builds_a_square_extent() {
        assert_eq!(
            launch(&["--seed", "7", "--extent", "128"]).request,
            WorldRequest::Generate(GenerationParams::new(7, IVec3::splat(128)))
        );
    }

    #[test]
    fn the_extent_defaults_to_the_full_lattice() {
        let request = launch(&["--seed", "7"]).request;
        let WorldRequest::Generate(params) = request else {
            panic!("a Seed generates");
        };

        assert_eq!(params.extent, GenerationParams::full_lattice(7).extent);
    }

    #[test]
    fn an_extent_alone_bounds_a_generated_world() {
        let WorldRequest::Generate(params) = launch(&["--extent", "128"]).request else {
            panic!("an extent alone generates");
        };

        assert_eq!(params.extent, IVec3::splat(128));
    }

    #[test]
    fn an_extent_beside_a_world_is_refused() {
        let refused = parse(&args(&["--world", "sponza.vox", "--extent", "128"]));

        assert!(refused.is_err());
    }

    #[test]
    fn a_flag_without_a_value_is_refused() {
        assert!(parse(&args(&["--seed"])).is_err());
        assert!(parse(&args(&["--extent"])).is_err());
        assert!(parse(&args(&["--world"])).is_err());
    }

    #[test]
    fn a_non_integer_seed_or_extent_is_refused() {
        assert!(parse(&args(&["--seed", "soon"])).is_err());
        assert!(parse(&args(&["--seed", "1", "--extent", "wide"])).is_err());
    }
}
