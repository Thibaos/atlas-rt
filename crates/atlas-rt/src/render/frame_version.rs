use anyhow::bail;

/// The renderer's current frame version. Raising it invalidates every frame
/// produced under the version it replaces, whether or not the frame has been
/// published yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameVersion(u64);

impl FrameVersion {
    /// # Errors
    ///
    /// Returns an error on version exhaustion, which would otherwise readmit
    /// frames of an outgoing world
    pub fn bump(&mut self) -> anyhow::Result<u64> {
        let Some(next) = self.0.checked_add(1) else {
            bail!("frame version exhausted");
        };

        self.0 = next;

        Ok(next)
    }

    #[must_use]
    pub const fn get(&self) -> u64 {
        self.0
    }
}

/// Whether published frames may reach the screen.
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

    #[must_use]
    pub const fn is_suppressed(&self) -> bool {
        self.suppressed
    }

    /// Admits only frames carrying `version` or a later one. No frame carries
    /// it yet, so the outgoing world stops being displayed at the call, not at
    /// the next frame.
    pub const fn suppress(&mut self, version: u64) {
        self.suppressed = true;
        self.version = version;
    }

    #[must_use]
    pub const fn admits(&self, version: u64) -> bool {
        !self.suppressed || version >= self.version
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayGate, FrameVersion};

    #[test]
    fn an_unsuppressed_gate_admits_every_frame() {
        let gate = DisplayGate::new();

        assert!(!gate.is_suppressed());
        assert!(gate.admits(0));
        assert!(gate.admits(7));
    }

    #[test]
    fn suppression_drops_the_frames_of_the_outgoing_world() -> anyhow::Result<()> {
        let mut version = FrameVersion::default();
        let mut gate = DisplayGate::new();
        let outgoing = version.get();

        assert!(gate.admits(outgoing));

        gate.suppress(version.bump()?);

        assert!(
            !gate.admits(outgoing),
            "an outgoing frame cannot be wrapped after the call"
        );
        assert!(gate.admits(version.get()));
        assert!(gate.is_suppressed());

        Ok(())
    }

    #[test]
    fn suppression_lifts_without_being_undone() -> anyhow::Result<()> {
        let mut version = FrameVersion::default();
        let mut gate = DisplayGate::new();

        gate.suppress(version.bump()?);

        assert!(gate.admits(version.get()));
        assert!(
            gate.is_suppressed(),
            "the raised version stays, so late frames of the old world stay dropped"
        );

        Ok(())
    }

    #[test]
    fn a_later_suppression_drops_the_frames_of_the_earlier_one() -> anyhow::Result<()> {
        let mut version = FrameVersion::default();
        let mut gate = DisplayGate::new();

        gate.suppress(version.bump()?);

        let outgoing = version.get();

        gate.suppress(version.bump()?);

        assert!(!gate.admits(outgoing));
        assert!(gate.admits(version.get()));

        Ok(())
    }

    #[test]
    fn raising_the_version_never_repeats_one() -> anyhow::Result<()> {
        let mut version = FrameVersion::default();

        assert_ne!(version.bump()?, version.bump()?);

        Ok(())
    }
}
