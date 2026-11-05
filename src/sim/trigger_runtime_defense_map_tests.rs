//! The fork-only defense-map classification read: an armed wave team whose
//! `GetHouse` owner resolves outside the session's driven seats reads the
//! defense form; an owner inside them reads the battle form. Each fixture
//! goes through the production ingress the loader takes — map INI, team AI
//! registry, tag classification — never a hand-built definition.

use super::*;
use crate::map::map_file::MapFile;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::rules::team_ai_ini::TeamAiIniRegistry;
use crate::sim::house_state::HouseState;

/// One armed wave trigger (`WAVE`: event 8 — one of PresidentDefense22's
/// own wave-event kinds, action 4 for `WAVETEAM`, `House=<Player @ G>` on a
/// `Repeat=2` tag) installed through the production ingress; start slot G
/// (6) maps to `slot_owner`, whose house is installed exactly as VERA
/// instantiates an unseated slot: present in `houses`, driven by no seat.
fn armed_wave_sim(slot_owner_name: &str) -> Simulation {
    let mut sim = Simulation::with_seed(1);
    let owner = sim.interner.intern(slot_owner_name);
    let country = sim.interner.intern("Americans");
    sim.houses_mut_for_test()
        .register(HouseState::new(owner, 0, Some(country), false, 0, 10));
    sim.session
        .start_slot_houses_mut_for_test()
        .insert(6, owner);
    let teams = IniFile::from_str(
        "[TeamTypes]\n0=WAVETEAM\n[WAVETEAM]\nHouse=<Player @ G>\nScript=S\nTaskForce=F\nMax=-1\n\
         [ScriptTypes]\n0=S\n[S]\n0=5,0\n[TaskForces]\n0=F\n",
    );
    let registry = TeamAiIniRegistry::from_sources(&IniFile::empty(), &teams, false);
    let map = MapFile::from_bytes(
        b"[Map]\nTheater=TEMPERATE\nSize=0,0,2,1\nLocalSize=0,0,2,1\n\
          [IsoMapPack5]\n1=DwALABwBAAIA/////wAAABEAAA==\n\
          [Basic]\nNewINIFormat=4\n\
          [Triggers]\nWAVE=<none>,<none>,Waves,0,1,1,1,0\n\
          [Events]\nWAVE=1,8,0,0\n\
          [Tags]\nTAGW=2,Waves,WAVE\n\
          [Actions]\nWAVE=1,4,1,WAVETEAM,0,0,0,0,A\n",
    )
    .unwrap();
    let rules = RuleSet::empty_for_test();
    sim.initialize_trigger_definitions(&map, &registry, None, None, None);
    let diagnostics = sim
        .install_team_ai_registry(&registry, rules)
        .expect("scenario team AI installs");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    sim.classify_map_triggers(rules);
    sim
}

#[test]
fn wave_owner_outside_the_driven_seats_reads_defense_form() {
    let mut sim = armed_wave_sim("GHOST");
    let player = sim.interner.intern("PLAYER");
    // PresidentDefense22's shape: the wave team's owner is an unseated
    // slot's house — installed, but nobody's opponent, the map's own
    // machinery.
    assert_eq!(
        sim.defense_map_classification(&[player], RuleSet::empty_for_test()),
        (true, 1)
    );
}

#[test]
fn wave_owner_inside_the_driven_seats_reads_battle_form() {
    let mut sim = armed_wave_sim("PLAYER");
    let player = sim.interner.intern("PLAYER");
    // A battle map's reinforcement triggers resolve to a driven AI seat and
    // stay battle form.
    assert_eq!(
        sim.defense_map_classification(&[player], RuleSet::empty_for_test()),
        (false, 1)
    );
}
