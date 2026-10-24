//! Build capability recorded by the retained Rules owner and save metadata.
//!
//! This is a VERA compatibility boundary, not a field from gamemd, Ares or
//! Phobos. Keeping one wire variant per compiled capability combination lets a
//! binary explicitly refuse any other mode. Extension catalogs added later
//! belong to the retained Rules registry, not a second mutable policy on
//! RuleSet or Simulation.
//!
//! The three capabilities are orthogonal: Ares, Phobos and NPatch compile
//! independently and combine freely, so every combination needs its own wire
//! variant. A feature added under one capability must keep working when the
//! others are compiled in — the modes differ in what they read, never in
//! whether a feature is available.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RulesCompatibility {
    Retail,
    Ares,
    Phobos,
    Npatch,
    AresPhobos,
    AresNpatch,
    PhobosNpatch,
    AresPhobosNpatch,
}

impl Default for RulesCompatibility {
    fn default() -> Self {
        Self::from_capabilities(
            super::ARES_ENABLED,
            super::PHOBOS_ENABLED,
            super::NPATCH_ENABLED,
        )
    }
}

impl RulesCompatibility {
    pub const fn from_capabilities(ares: bool, phobos: bool, npatch: bool) -> Self {
        match (ares, phobos, npatch) {
            (true, true, true) => Self::AresPhobosNpatch,
            (true, true, false) => Self::AresPhobos,
            (true, false, true) => Self::AresNpatch,
            (true, false, false) => Self::Ares,
            (false, true, true) => Self::PhobosNpatch,
            (false, true, false) => Self::Phobos,
            (false, false, true) => Self::Npatch,
            (false, false, false) => Self::Retail,
        }
    }

    pub(crate) fn matches_build(self) -> bool {
        self == Self::default()
    }

    /// Whether this recorded mode includes the Ares capability.
    pub const fn has_ares(self) -> bool {
        matches!(
            self,
            Self::Ares | Self::AresPhobos | Self::AresNpatch | Self::AresPhobosNpatch
        )
    }

    /// Whether this recorded mode includes the Phobos capability.
    pub const fn has_phobos(self) -> bool {
        matches!(
            self,
            Self::Phobos | Self::AresPhobos | Self::PhobosNpatch | Self::AresPhobosNpatch
        )
    }

    /// Whether this recorded mode includes the NPatch capability.
    pub const fn has_npatch(self) -> bool {
        matches!(
            self,
            Self::Npatch | Self::AresNpatch | Self::PhobosNpatch | Self::AresPhobosNpatch
        )
    }
}

#[cfg(test)]
#[path = "compatibility_tests.rs"]
mod tests;
