//! Consuming controls for `install_lobby_slot_houses`: the headless stand-in
//! for the lobby's start-slot house pass. Retail rules only — the constructor
//! resolves launch Countries through the canonical registry.

use super::{LOBBY_SLOT_HOUSE_NAMES, install_lobby_slot_houses};
use crate::sim::world::Simulation;

/// Three slots become three passive houses, and the `PlayerAt` selectors
/// (the literal `<Player @ A..H>` table, `0x117B..0x1182`) resolve them —
/// the resolution a spectatorless load lacks and defense-map reinforcement
/// teams need.
#[test]
fn installed_slots_resolve_the_player_at_selectors() {
    let Some(retail) = crate::rules::retail_ini_fixture::retail_battle_rules_for_map("XMP03T4.MAP")
    else {
        return; // retail INIs unavailable in this checkout
    };
    let mut sim = Simulation::with_seed(31);
    let installed = install_lobby_slot_houses(&mut sim, 3, &retail.rules);
    assert_eq!(installed.len(), 3);
    assert_eq!(
        installed
            .iter()
            .map(|id| sim.interner.resolve(*id).to_string())
            .collect::<Vec<_>>(),
        LOBBY_SLOT_HOUSE_NAMES[..3]
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
    );
    for (slot, owner) in installed.iter().enumerate() {
        let resolved = sim.house_by_trigger_selector(0x117B + slot as i32, &retail.rules);
        assert_eq!(resolved.as_ref(), Some(owner), "slot {slot}");
        let house = sim.houses.get(owner).expect("installed house");
        assert!(house.multiplay_passive(), "slot {slot} is passive");
    }
    // A slot beyond the installed count stays unresolved — visible in the
    // trigger trace instead of silently consuming its trigger.
    assert_eq!(
        sim.house_by_trigger_selector(0x117B + 3, &retail.rules),
        None
    );
}

/// Zero starts install nothing and leave the table empty: a load that never
/// opted in keeps every earlier digest identical.
#[test]
fn zero_starts_install_nothing() {
    let Some(retail) = crate::rules::retail_ini_fixture::retail_battle_rules_for_map("XMP03T4.MAP")
    else {
        return;
    };
    let mut sim = Simulation::with_seed(31);
    assert!(install_lobby_slot_houses(&mut sim, 0, &retail.rules).is_empty());
    assert!(sim.session().start_slot_houses().is_empty());
    assert_eq!(sim.houses().order().len(), 0);
}
