use std::fmt::{self, Formatter};
use std::time::Duration;

/// A second in nanoseconds. The tick period truncates to whole nanoseconds,
/// so a rate above this one has no period.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// The controller's immutable dimensions and motion values in voxel units. The
/// sim copies one profile and it survives every Activation, so a different
/// profile means a new sim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerProfile {
    pub width: f32,
    pub depth: f32,
    pub body_height: f32,
    pub eye_offset: f32,
    pub move_speed: f32,
    pub gravity: f32,
    pub jump_velocity: f32,
    pub step_height: u32,
    pub jump_buffer: Duration,
    pub tick_rate: u32,
}

impl Default for PlayerProfile {
    fn default() -> Self {
        Self {
            width: 6.0,
            depth: 6.0,
            body_height: 18.0,
            eye_offset: 16.2,
            move_speed: 40.0,
            gravity: 200.0,
            jump_velocity: 80.0,
            step_height: 4,
            jump_buffer: Duration::from_secs_f32(0.15),
            tick_rate: 30,
        }
    }
}

/// A configuration value the validated constructor refused, named for the
/// field and the rule it broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    NotFinite(&'static str),
    NotPositive(&'static str),
    Negative(&'static str),
    EyeOffsetOutsideBody,
    Fractional(&'static str),
    OutOfRange(&'static str),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFinite(field) => write!(f, "{field} must be a finite number"),
            Self::NotPositive(field) => write!(f, "{field} must be positive"),
            Self::Negative(field) => write!(f, "{field} must not be negative"),
            Self::EyeOffsetOutsideBody => {
                f.write_str("eye_offset must fall inside the body height")
            }
            Self::Fractional(field) => write!(f, "{field} must count whole units"),
            Self::OutOfRange(field) => write!(f, "{field} is out of range"),
        }
    }
}

impl PlayerProfile {
    /// Validates raw configuration: linear values in voxel units,
    /// `jump_buffer` in seconds, and `tick_rate` in Hz.
    ///
    /// # Errors
    ///
    /// Returns the first value that fails its check, named by field, rather
    /// than clamping it into range.
    pub fn new(
        width: f32,
        depth: f32,
        body_height: f32,
        eye_offset: f32,
        move_speed: f32,
        gravity: f32,
        jump_velocity: f32,
        step_height: f32,
        jump_buffer: f32,
        tick_rate: f32,
    ) -> Result<Self, ProfileError> {
        let width = positive("width", width)?;
        let depth = positive("depth", depth)?;
        let body_height = positive("body_height", body_height)?;
        let eye_offset = inside_body("eye_offset", eye_offset, body_height)?;
        let move_speed = positive("move_speed", move_speed)?;
        let gravity = positive("gravity", gravity)?;
        let jump_velocity = nonnegative("jump_velocity", jump_velocity)?;
        let step_height = whole("step_height", nonnegative("step_height", step_height)?)?;
        let jump_buffer = nonnegative("jump_buffer", jump_buffer)?;
        let tick_rate = whole("tick_rate", positive("tick_rate", tick_rate)?)?;

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole and nonnegative
        let step_height = u32::try_from(step_height as u64)
            .map_err(|_| ProfileError::OutOfRange("step_height"))?;
        let jump_buffer = Duration::try_from_secs_f32(jump_buffer)
            .map_err(|_| ProfileError::OutOfRange("jump_buffer"))?;

        if tick_rate > NANOS_PER_SECOND as f32 {
            return Err(ProfileError::OutOfRange("tick_rate"));
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // positive and bounded
        let tick_rate = tick_rate as u32;

        Ok(Self {
            width,
            depth,
            body_height,
            eye_offset,
            move_speed,
            gravity,
            jump_velocity,
            step_height,
            jump_buffer,
            tick_rate,
        })
    }

    /// One fixed tick at `tick_rate`, truncated to whole nanoseconds.
    ///
    /// # Panics
    ///
    /// Panics when `tick_rate` is zero, which the validated constructor
    /// rejects.
    #[must_use]
    pub fn tick_period(&self) -> Duration {
        assert!(self.tick_rate > 0, "a tick rate of zero has no period");

        Duration::from_nanos(
            NANOS_PER_SECOND
                .checked_div(u64::from(self.tick_rate))
                .unwrap_or(0),
        )
    }
}

fn finite(field: &'static str, value: f32) -> Result<f32, ProfileError> {
    value
        .is_finite()
        .then_some(value)
        .ok_or(ProfileError::NotFinite(field))
}

fn positive(field: &'static str, value: f32) -> Result<f32, ProfileError> {
    let value = finite(field, value)?;

    (value > 0.0)
        .then_some(value)
        .ok_or(ProfileError::NotPositive(field))
}

fn nonnegative(field: &'static str, value: f32) -> Result<f32, ProfileError> {
    let value = finite(field, value)?;

    (value >= 0.0)
        .then_some(value)
        .ok_or(ProfileError::Negative(field))
}

fn inside_body(field: &'static str, value: f32, body_height: f32) -> Result<f32, ProfileError> {
    let value = finite(field, value)?;

    (0.0..=body_height)
        .contains(&value)
        .then_some(value)
        .ok_or(ProfileError::EyeOffsetOutsideBody)
}

fn whole(field: &'static str, value: f32) -> Result<f32, ProfileError> {
    (value.fract() == 0.0)
        .then_some(value)
        .ok_or(ProfileError::Fractional(field))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default configuration as raw values, so a test can break one.
    #[derive(Clone, Copy, Debug)]
    struct Values {
        width: f32,
        depth: f32,
        body_height: f32,
        eye_offset: f32,
        move_speed: f32,
        gravity: f32,
        jump_velocity: f32,
        step_height: f32,
        jump_buffer: f32,
        tick_rate: f32,
    }

    impl Default for Values {
        fn default() -> Self {
            Self {
                width: 6.0,
                depth: 6.0,
                body_height: 18.0,
                eye_offset: 16.2,
                move_speed: 40.0,
                gravity: 200.0,
                jump_velocity: 80.0,
                step_height: 4.0,
                jump_buffer: 0.15,
                tick_rate: 30.0,
            }
        }
    }

    impl Values {
        fn build(self) -> Result<PlayerProfile, ProfileError> {
            PlayerProfile::new(
                self.width,
                self.depth,
                self.body_height,
                self.eye_offset,
                self.move_speed,
                self.gravity,
                self.jump_velocity,
                self.step_height,
                self.jump_buffer,
                self.tick_rate,
            )
        }
    }

    fn rejected(values: Values) -> ProfileError {
        match values.build() {
            Ok(profile) => panic!("the profile must be rejected, got {profile:?}"),
            Err(error) => error,
        }
    }

    fn accepted(values: Values) {
        assert!(values.build().is_ok(), "{values:?} must be accepted");
    }

    #[test]
    fn the_default_profile_is_the_documented_one() {
        assert_eq!(
            PlayerProfile::default(),
            PlayerProfile {
                width: 6.0,
                depth: 6.0,
                body_height: 18.0,
                eye_offset: 16.2,
                move_speed: 40.0,
                gravity: 200.0,
                jump_velocity: 80.0,
                step_height: 4,
                jump_buffer: Duration::from_secs_f32(0.15),
                tick_rate: 30,
            }
        );
        assert_eq!(
            PlayerProfile::default().tick_period(),
            Duration::from_nanos(33_333_333)
        );
    }

    #[test]
    fn the_default_values_pass_the_validated_constructor() {
        assert_eq!(Values::default().build(), Ok(PlayerProfile::default()));
    }

    #[test]
    fn a_dimension_speed_or_gravity_that_is_not_positive_and_finite_is_rejected() {
        let base = Values::default();
        let cases = [
            (
                Values { width: 0.0, ..base },
                ProfileError::NotPositive("width"),
            ),
            (
                Values {
                    width: f32::NAN,
                    ..base
                },
                ProfileError::NotFinite("width"),
            ),
            (
                Values {
                    depth: -0.6,
                    ..base
                },
                ProfileError::NotPositive("depth"),
            ),
            (
                Values {
                    depth: f32::INFINITY,
                    ..base
                },
                ProfileError::NotFinite("depth"),
            ),
            (
                Values {
                    body_height: 0.0,
                    ..base
                },
                ProfileError::NotPositive("body_height"),
            ),
            (
                Values {
                    body_height: f32::NEG_INFINITY,
                    ..base
                },
                ProfileError::NotFinite("body_height"),
            ),
            (
                Values {
                    move_speed: 0.0,
                    ..base
                },
                ProfileError::NotPositive("move_speed"),
            ),
            (
                Values {
                    move_speed: f32::NAN,
                    ..base
                },
                ProfileError::NotFinite("move_speed"),
            ),
            (
                Values {
                    gravity: -24.0,
                    ..base
                },
                ProfileError::NotPositive("gravity"),
            ),
            (
                Values {
                    gravity: f32::INFINITY,
                    ..base
                },
                ProfileError::NotFinite("gravity"),
            ),
        ];

        for (values, expected) in cases {
            assert_eq!(rejected(values), expected);
        }
    }

    #[test]
    fn an_eye_offset_outside_the_body_or_not_finite_is_rejected() {
        let base = Values::default();

        for values in [
            Values {
                eye_offset: base.body_height + 0.1,
                ..base
            },
            Values {
                eye_offset: -0.1,
                ..base
            },
        ] {
            assert_eq!(rejected(values), ProfileError::EyeOffsetOutsideBody);
        }

        assert_eq!(
            rejected(Values {
                eye_offset: f32::NAN,
                ..base
            }),
            ProfileError::NotFinite("eye_offset")
        );
    }

    #[test]
    fn the_eye_offset_reaching_either_end_of_the_body_is_accepted() {
        let base = Values::default();

        for values in [
            Values {
                eye_offset: 0.0,
                ..base
            },
            Values {
                eye_offset: base.body_height,
                ..base
            },
        ] {
            accepted(values);
        }
    }

    #[test]
    fn a_jump_velocity_or_buffer_that_is_negative_or_not_finite_is_rejected() {
        let base = Values::default();
        let cases = [
            (
                Values {
                    jump_velocity: -1.0,
                    ..base
                },
                ProfileError::Negative("jump_velocity"),
            ),
            (
                Values {
                    jump_velocity: f32::NAN,
                    ..base
                },
                ProfileError::NotFinite("jump_velocity"),
            ),
            (
                Values {
                    jump_buffer: -0.1,
                    ..base
                },
                ProfileError::Negative("jump_buffer"),
            ),
            (
                Values {
                    jump_buffer: f32::INFINITY,
                    ..base
                },
                ProfileError::NotFinite("jump_buffer"),
            ),
            (
                Values {
                    jump_buffer: 1.0e30,
                    ..base
                },
                ProfileError::OutOfRange("jump_buffer"),
            ),
        ];

        for (values, expected) in cases {
            assert_eq!(rejected(values), expected);
        }
    }

    #[test]
    fn a_step_height_that_is_negative_fractional_or_too_large_is_rejected() {
        let base = Values::default();
        let cases = [
            (
                Values {
                    step_height: -1.0,
                    ..base
                },
                ProfileError::Negative("step_height"),
            ),
            (
                Values {
                    step_height: 1.5,
                    ..base
                },
                ProfileError::Fractional("step_height"),
            ),
            (
                Values {
                    step_height: 1.0e12,
                    ..base
                },
                ProfileError::OutOfRange("step_height"),
            ),
        ];

        for (values, expected) in cases {
            assert_eq!(rejected(values), expected);
        }
    }

    #[test]
    fn a_tick_rate_that_is_not_a_positive_whole_rate_is_rejected() {
        let base = Values::default();
        let cases = [
            (
                Values {
                    tick_rate: 0.0,
                    ..base
                },
                ProfileError::NotPositive("tick_rate"),
            ),
            (
                Values {
                    tick_rate: -30.0,
                    ..base
                },
                ProfileError::NotPositive("tick_rate"),
            ),
            (
                Values {
                    tick_rate: 30.5,
                    ..base
                },
                ProfileError::Fractional("tick_rate"),
            ),
            (
                Values {
                    tick_rate: 2.0e9,
                    ..base
                },
                ProfileError::OutOfRange("tick_rate"),
            ),
            (
                Values {
                    tick_rate: f32::NAN,
                    ..base
                },
                ProfileError::NotFinite("tick_rate"),
            ),
        ];

        for (values, expected) in cases {
            assert_eq!(rejected(values), expected);
        }
    }

    #[test]
    fn the_zero_values_the_profile_allows_are_accepted() {
        let base = Values::default();

        for values in [
            Values {
                step_height: 0.0,
                ..base
            },
            Values {
                jump_buffer: 0.0,
                ..base
            },
            Values {
                jump_velocity: 0.0,
                ..base
            },
            Values {
                eye_offset: 0.0,
                ..base
            },
        ] {
            accepted(values);
        }
    }

    #[test]
    fn the_slowest_tick_rate_still_has_a_period() {
        let one_hz = Values {
            tick_rate: 1.0,
            ..Values::default()
        }
        .build()
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(one_hz.tick_period(), Duration::from_secs(1));
    }
}
