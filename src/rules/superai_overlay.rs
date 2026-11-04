//! The Super AI fork's built-in rules overlay (the garrison addendum):
//! retail power plants are not garrisonable — their `CanBeOccupied`,
//! `MaxNumberOccupants` and `CanOccupyFire` keys sit commented out in
//! retail rulesmd.ini (the reactor's garrison reads `InfantryAbsorb`
//! instead) — and this fork's base defense plants troops in its plants.
//! The overlay flips the three occupancy keys on each side's power plant
//! at the source-composition chokepoint, before the personal-rules layer
//! (a user's VERARULES.INI still wins, and the map layers after it).
//!
//! Composed like [`PersonalRules`]: merged into the selected rulesmd root
//! before the Rules readers run — no extra processing pass, whose result
//! would reset omitted fields and repeat native postprocessing.

use crate::rules::ini_parser::IniFile;

/// The occupancy rows the fork adds to each side's power plant: the
/// engine's garrison chain reads the three keys off the type row, five
/// slots — the Battle Bunker's roof — and firing occupants.
pub const GARRISON_PLANT_OVERLAY: &str = "\
[GAPOWR]\n\
CanBeOccupied=yes\n\
MaxNumberOccupants=5\n\
CanOccupyFire=yes\n\
[NAPOWR]\n\
CanBeOccupied=yes\n\
MaxNumberOccupants=5\n\
CanOccupyFire=yes\n\
[YAPOWR]\n\
CanBeOccupied=yes\n\
MaxNumberOccupants=5\n\
CanOccupyFire=yes\n\
";

/// Merge the fork overlay into the selected rules root — before the
/// personal layer, whose rows must win.
pub fn apply_garrison_overlay(root: &mut IniFile) {
    let overlay = IniFile::from_str(GARRISON_PLANT_OVERLAY);
    root.merge_source_sections(&overlay, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::asset_manager::AssetManager;
    use crate::map::source::test_support::TestDirectory;
    use crate::rules::personal_rules::PersonalRules;
    use crate::rules::process_owner::{NativeRulesProcessOwner, NativeScenarioRulesPrefix};
    use crate::rules::retail_sources::RetailRulesSources;

    /// The layered read chain one game launch runs: sources → the fork
    /// overlay → personal layer → scenario layers. A power plant read off
    /// it answers occupiable, five slots, firing occupants.
    fn owner(root: &[u8], personal: &str) -> NativeRulesProcessOwner {
        let directory = TestDirectory::new("superai-overlay");
        directory.write("rulesmd.ini", root);
        directory.write("artmd.ini", b"");
        let assets = AssetManager::from_loose_root_for_test(directory.path());
        RetailRulesSources::select(&assets)
            .unwrap()
            .with_personal_rules(PersonalRules::from_bytes(personal.as_bytes()).unwrap())
            .into_owner(Default::default(), Default::default())
            .unwrap()
    }

    fn scenario_rules(owner: &mut NativeRulesProcessOwner) -> crate::rules::ruleset::RuleSet {
        owner
            .load_scenario(NativeScenarioRulesPrefix::NonCampaign(None), &IniFile::empty())
            .unwrap()
            .into_parts()
            .0
    }

    /// The selected sources carry the overlay into the Rules readers: a
    /// power plant read through the production layered-read chain answers
    /// occupiable, five slots, firing occupants.
    #[test]
    fn the_garrison_overlay_arms_the_plant_types_through_the_layered_read() {
        let root = b"[BuildingTypes]\n0=GAPOWR\n1=NAPOWR\n2=YAPOWR\n\
             [GAPOWR]\nStrength=800\n\
             [NAPOWR]\nStrength=800\n\
             [YAPOWR]\nStrength=600\n";
        let rules = scenario_rules(&mut owner(root, ""));
        for name in ["GAPOWR", "NAPOWR", "YAPOWR"] {
            let plant = rules.object(name).unwrap_or_else(|| panic!("{name} reads"));
            assert!(
                plant.can_be_occupied,
                "{name} is garrisonable through the fork overlay"
            );
            assert_eq!(plant.max_number_occupants, 5, "{name} holds five");
            assert!(plant.can_occupy_fire, "{name}'s occupants fire");
        }
    }

    /// A personal row wins over the fork overlay: VERARULES.INI stays the
    /// user's last word on their rules.
    #[test]
    fn a_personal_row_overrides_the_garrison_overlay() {
        let root = b"[BuildingTypes]\n0=GAPOWR\n[GAPOWR]\nStrength=800\n";
        let rules = scenario_rules(&mut owner(root, "[GAPOWR]\nMaxNumberOccupants=2\n"));
        let plant = rules.object("GAPOWR").unwrap();
        assert_eq!(plant.max_number_occupants, 2, "the personal row wins");
        assert!(plant.can_be_occupied, "the overlay's other rows hold");
        assert!(plant.can_occupy_fire);
    }
}
