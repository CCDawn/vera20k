//! Phobos `SkirmishUnlimitedColors`: skirmish colours indexed into `[Colors]`.
//!
//! Native identity: Phobos `Phobos.INI.cpp`, hook
//! `InitRules_ThingsThatShouldntBeSerailized` (`0x0052D21F`, 6 bytes), reads
//! `[General] SkirmishUnlimitedColors=` with `CCINIClass::ReadBool`, and while
//! the value is set it writes seven bytes over the entry of `Game_GetLinkedColor`
//! (`0x0069A310`):
//!
//! ```text
//! mov eax, [esp+4]   ; the passed colour index
//! shl eax, 1
//! inc eax
//! ```
//!
//! The source comment for that patch is "Disable Ares hook at this address so
//! that our logic can run": Ares hooks the same seven bytes for its customisable
//! dropdown colours, which is why Phobos lists the two as mutually exclusive.
//!
//! The patch has no return of its own, so execution continues into the native
//! body at `0x0069A317`: its `jne` is always taken (`2 * index + 1` is odd, never
//! zero), and the tail at `0x0069A328` still reads the SessionClass table for a
//! value below 9 ([`crate::rules::color_scheme::linked_color_tail`]). From index
//! 4 on, the runtime scheme index is `2 * index + 1`. gamemd builds two schemes
//! per `[Colors]` entry and the odd half is the one a player colour selects
//! ([`crate::rules::color_scheme::scheme_for_priority`]), so the entry is the
//! index itself: a spawner that sends 6 gets the seventh `[Colors]` entry
//! (`Orange` in stock rulesmd.ini). Indices 0..=3 read table entries 1, 3, 5 and
//! 7 instead (schemes 11, 29, 25 and 15), so they resolve to entries 5, 14, 12
//! and 7.
//!
//! Because the hook replaces the whole conversion, this mode also changes the
//! non-spawner callers that pass a fixed priority (trigger message colours,
//! production and rally tints): Phobos documents the feature as spawner-only
//! and warns that the stock launcher's own priorities then draw other colours.
//!
//! ## Dependency rules
//! - Part of rules/ — depends on rules/color_scheme and rules/ini_parser only.

#[cfg(feature = "phobos")]
use crate::rules::ini_parser::IniFile;

/// `[General] SkirmishUnlimitedColors=` and the priority conversion it selects.
///
/// `Default` is the vanilla conversion, which is also what a build without the
/// `phobos` feature produces: the native reader never runs, so no INI value can
/// turn the mode on.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct SkirmishColorMode {
    unlimited: bool,
}

impl SkirmishColorMode {
    /// YR's eight-priority conversion (`PRIORITY_TO_SCHEME_INDEX`).
    pub const VANILLA: Self = Self { unlimited: false };

    /// Phobos `SkirmishUnlimitedColors=true`.
    pub const UNLIMITED: Self = Self { unlimited: true };

    pub fn unlimited(self) -> bool {
        self.unlimited
    }

    /// Native `Game_GetLinkedColor` (`0x0069A310`) as the active build leaves
    /// it: the priority table, or Phobos' `2 * index + 1` run through the
    /// native tail (see the module doc).
    ///
    /// Both return a scheme index, not an entry: callers divide by two
    /// ([`Self::entry_for_priority`]). A negative priority is the lobby's
    /// random/observer marker; the patched function returns a negative index
    /// for it, which addresses no `[Colors]` entry, so VERA clamps it to the
    /// first entry.
    pub fn scheme_index_for_priority(self, priority: i32) -> usize {
        if self.unlimited {
            crate::rules::color_scheme::linked_color_tail(priority.wrapping_mul(2).wrapping_add(1))
        } else {
            crate::rules::color_scheme::scheme_index_for_priority(priority)
        }
    }

    /// `[Colors]` entry a player colour priority selects under this mode.
    ///
    /// The vanilla answer stays owned by
    /// [`crate::rules::color_scheme::scheme_entry_for_priority`].
    pub fn entry_for_priority(self, priority: i32) -> usize {
        if self.unlimited {
            self.scheme_index_for_priority(priority) / 2
        } else {
            crate::rules::color_scheme::scheme_entry_for_priority(priority)
        }
    }

    #[cfg(feature = "phobos")]
    pub(crate) fn from_ini(ini: &IniFile) -> Self {
        // Phobos reads the key with the native ReadBool semantics and its
        // constructor default of false.
        let enabled = ini
            .section_or_empty("General")
            .read_bool("SkirmishUnlimitedColors", false);
        if enabled {
            Self::UNLIMITED
        } else {
            Self::VANILLA
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_mode_runs_the_native_tail_after_the_patch() {
        // Vanilla keeps the SessionClass table: priority 6 is runtime scheme 17,
        // whose doubled pair is [Colors] entry 8.
        assert_eq!(SkirmishColorMode::VANILLA.scheme_index_for_priority(6), 17);
        assert_eq!(SkirmishColorMode::VANILLA.entry_for_priority(6), 8);

        // Oracle: Phobos' seven patch bytes written over gamemd's 0x69A310 and
        // executed in Unicorn. 2 * index + 1 still reads the table below 9, so
        // 0..=3 return schemes 11, 29, 25 and 15; from 4 on the index is the
        // entry.
        let mode = SkirmishColorMode::UNLIMITED;
        for (index, scheme) in [
            (0, 11),
            (1, 29),
            (2, 25),
            (3, 15),
            (4, 9),
            (6, 13),
            (11, 23),
        ] {
            assert_eq!(mode.scheme_index_for_priority(index), scheme, "{index}");
            assert_eq!(mode.entry_for_priority(index), scheme / 2, "{index}");
        }

        // -2 is the lobby's random/observer marker, not an entry: the hook
        // returns -3 there, which addresses no [Colors] entry.
        assert_eq!(mode.entry_for_priority(-2), 0);
    }

    #[cfg(feature = "phobos")]
    #[test]
    fn the_ini_key_defaults_off_and_needs_a_phobos_build() {
        use crate::rules::ini_parser::IniFile;

        let default = SkirmishColorMode::from_ini(&IniFile::from_str("[General]\n"));
        assert_eq!(default, SkirmishColorMode::VANILLA);
        assert!(!default.unlimited());

        let on = SkirmishColorMode::from_ini(&IniFile::from_str(
            "[General]\nSkirmishUnlimitedColors=yes\n",
        ));
        assert_eq!(on, SkirmishColorMode::UNLIMITED);

        // Native ReadBool takes the first character: 'o' is neither a true nor
        // a false token, so `on`/`off` both retain the default.
        let off = SkirmishColorMode::from_ini(&IniFile::from_str(
            "[General]\nSkirmishUnlimitedColors=off\n",
        ));
        assert_eq!(off, SkirmishColorMode::VANILLA);
    }
}
