/// Whether published frames may reach the screen.
///
/// The host records a version as it asks for a world to go away, and admits
/// only frames the renderer produced after it.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayGate {
    suppressed: bool,
    version: u64,
    armed: bool,
}

impl DisplayGate {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppressed: false,
            version: 0,
            armed: false,
        }
    }

    /// Stops admitting frames of `version` and every version below it.
    pub const fn suppress(&mut self, version: u64) {
        self.suppressed = true;
        self.version = version;
    }

    /// Arms the gate to report the first frame it admits past `version`, so the
    /// host can tell when the content it asked for has reached the screen.
    pub const fn arm(&mut self, version: u64) {
        self.suppress(version);
        self.armed = true;
    }

    #[must_use]
    pub const fn admits(&self, version: u64) -> bool {
        !self.suppressed || version > self.version
    }

    /// Whether `version` is both admissible and the first frame past the
    /// suppression the gate was armed on. Reports a frame once: the arm is spent
    /// by the answer.
    #[must_use]
    pub const fn admitted(&mut self, version: u64) -> bool {
        if !self.armed || !self.admits(version) {
            return false;
        }

        self.armed = false;

        true
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayGate;

    #[test]
    fn an_unsuppressed_gate_admits_every_frame() {
        let gate = DisplayGate::new();

        assert!(gate.admits(0));
        assert!(gate.admits(7));
    }

    #[test]
    fn suppression_drops_the_frames_of_the_outgoing_content() {
        let mut gate = DisplayGate::new();
        let showing = 3;

        gate.suppress(showing);

        for version in 0..=showing {
            assert!(
                !gate.admits(version),
                "a frame the renderer built before the content changed cannot be wrapped"
            );
        }
    }

    #[test]
    fn a_frame_of_the_version_the_content_change_raised_is_admitted() {
        let mut gate = DisplayGate::new();

        gate.suppress(3);

        assert!(gate.admits(4));
    }

    #[test]
    fn suppression_lifts_without_being_undone() {
        let mut gate = DisplayGate::new();

        gate.suppress(3);

        assert!(gate.admits(9_000));
        assert!(
            !gate.admits(3),
            "the recorded version stays, so a late frame of the outgoing content stays dropped"
        );
    }

    #[test]
    fn a_later_suppression_drops_the_frames_of_the_earlier_one() {
        let mut gate = DisplayGate::new();

        gate.suppress(3);
        gate.suppress(5);

        assert!(!gate.admits(4));
        assert!(gate.admits(6));
    }

    #[test]
    fn an_unarmed_gate_reports_no_admission() {
        let mut gate = DisplayGate::new();

        assert!(!gate.admitted(0));
    }

    #[test]
    fn an_armed_gate_reports_the_first_frame_past_the_suppression() {
        let mut gate = DisplayGate::new();

        gate.arm(3);

        assert!(!gate.admitted(3), "the outgoing content stays out");
        assert!(gate.admitted(4));
        assert!(
            !gate.admitted(5),
            "the arm is spent, so a later frame is not a second completion"
        );
    }

    #[test]
    fn a_suppression_after_the_arm_leaves_the_arm_standing() {
        let mut gate = DisplayGate::new();

        gate.arm(3);
        gate.suppress(7);

        assert!(!gate.admitted(4), "the recorded version rose to 7");
        assert!(!gate.admitted(7));
        assert!(gate.admitted(8));
    }
}
