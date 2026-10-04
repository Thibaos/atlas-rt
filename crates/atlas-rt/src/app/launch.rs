//! The standalone binary's flags: which World supply to build and how to view
//! it.
//!
//! A run with no flags loads the default `.vox` file, exactly as before. A
//! `--seed` selects a Generation instead, and `--footprint` bounds its ground.
//! The Seed's bits are reinterpreted from the host's signed integer.

use glam::IVec3;

use atlas_rt::world::generation::GenerationParams;

/// The `.vox` file a run with no `--world` loads.
const DEFAULT_WORLD: &str = "castle.vox";

/// The World a run builds at startup: a `.vox` load or a Generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldRequest {
    Load(String),
    Generate(GenerationParams),
}

/// A parsed command line: the World to build and whether to run without the
/// simulation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub request: WorldRequest,
    pub fly: bool,
}

/// Parses the binary's arguments. Unrecognized flags are ignored, as before.
///
/// # Errors
///
/// Returns a reason when a value flag has no value, when a Seed or footprint is
/// not an integer, or when `--footprint` is given without `--seed`.
pub fn parse(args: &[String]) -> Result<Launch, String> {
    let mut world = None;
    let mut seed = None;
    let mut footprint = None;
    let fly = args.iter().any(|arg| arg == "--fly");

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
            "--footprint" => {
                footprint = Some(parse_footprint(value_after(args, index)?)?);
                index = index.strict_add(2);
            }
            _ => index = index.strict_add(1),
        }
    }

    let request = match seed {
        Some(seed) => WorldRequest::Generate(generation_params(seed, footprint)),
        None if footprint.is_some() => {
            return Err(String::from("--footprint needs --seed to generate a World"));
        }
        None => WorldRequest::Load(world.unwrap_or_else(|| DEFAULT_WORLD.to_owned())),
    };

    Ok(Launch { request, fly })
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

fn parse_footprint(value: &str) -> Result<i32, String> {
    value
        .parse::<i32>()
        .map_err(|_| format!("the footprint {value} is not an integer"))
}

/// The Generation parameters: a square footprint when one is given, the full
/// Lattice otherwise.
const fn generation_params(seed: u64, footprint: Option<i32>) -> GenerationParams {
    match footprint {
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
    fn no_flags_load_the_default_world() {
        let launch = launch(&[]);

        assert_eq!(
            launch.request,
            WorldRequest::Load(String::from(DEFAULT_WORLD))
        );
        assert!(!launch.fly);
    }

    #[test]
    fn a_world_flag_loads_that_file() {
        assert_eq!(
            launch(&["--world", "sponza.vox"]).request,
            WorldRequest::Load(String::from("sponza.vox"))
        );
    }

    #[test]
    fn fly_is_read() {
        assert!(launch(&["--fly"]).fly);
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
    fn a_footprint_flag_builds_a_square_footprint() {
        assert_eq!(
            launch(&["--seed", "7", "--footprint", "128"]).request,
            WorldRequest::Generate(GenerationParams::new(7, IVec3::splat(128)))
        );
    }

    #[test]
    fn the_footprint_defaults_to_the_full_lattice() {
        let request = launch(&["--seed", "7"]).request;
        let WorldRequest::Generate(params) = request else {
            panic!("a Seed generates");
        };

        assert_eq!(
            params.footprint,
            GenerationParams::full_lattice(7).footprint
        );
    }

    #[test]
    fn a_footprint_without_a_seed_is_refused() {
        let refused = parse(&args(&["--footprint", "128"]));

        assert!(refused.is_err());
    }

    #[test]
    fn a_flag_without_a_value_is_refused() {
        assert!(parse(&args(&["--seed"])).is_err());
        assert!(parse(&args(&["--footprint"])).is_err());
        assert!(parse(&args(&["--world"])).is_err());
    }

    #[test]
    fn a_non_integer_seed_or_footprint_is_refused() {
        assert!(parse(&args(&["--seed", "soon"])).is_err());
        assert!(parse(&args(&["--seed", "1", "--footprint", "wide"])).is_err());
    }
}
