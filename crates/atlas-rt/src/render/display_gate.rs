/// Whether published frames may reach the screen.
///
/// The host suppresses display when it asks for a world to go away, recording
/// the version the renderer had stamped on everything it produced so far. Only
/// a version the renderer raises after that is admitted. The renderer raises it
/// when a batch of Snapshots reaches its store, so the first frame through the
/// gate is one it built from the new content, however long the content took to
/// arrive.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayGate {
    suppressed: bool,
    version: u64,
}

impl DisplayGate {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppressed: false,
            version: 0,
        }
    }

    /// Stops admitting frames of `version` and every version below it.
    pub const fn suppress(&mut self, version: u64) {
        self.suppressed = true;
        self.version = version;
    }

    #[must_use]
    pub const fn admits(&self, version: u64) -> bool {
        !self.suppressed || version > self.version
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

        assert!(gate.admits(showing));

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
    fn a_first_suppression_drops_the_versions_before_any_content_change() {
        let mut gate = DisplayGate::new();

        gate.suppress(0);

        assert!(!gate.admits(0));
        assert!(gate.admits(1));
    }
}
