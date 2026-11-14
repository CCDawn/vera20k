//! The fork-only defense-map classification read: an armed wave team whose
//! `GetHouse` owner resolves outside the session's driven seats reads the
//! defense form, and so does a driven seat holding a dominant share of the
//! armed spawn triggers (the wave host, returned for the persona roster's
//! stand-down); owners inside the driven seats without that share read the
//! battle form. Each fixture goes through the production ingress the loader
//! takes — map INI, team AI registry, tag classification — never a
//! hand-built definition.

use super::*;
use crate::map::map_file::MapFile;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::rules::team_ai_ini::TeamAiIniRegistry;
use crate::sim::house_state::HouseState;

/// Armed wave triggers installed through the production ingress: one
/// trigger per entry of `wave_teams` (event 8 — one of PresidentDefense22's
/// own wave-event kinds — action 4 for that team, each on its own
/// `Repeat=2` tag), the team's `House=` pinned to a start slot. `slots`
/// maps start slots to house names (G=6, A=0), each house installed exactly
/// as VERA instantiates an unseated slot: present in `houses`, driven by no
/// seat unless the test names it in `driven`.
fn armed_waves_sim(slots: &[(u32, &str)], wave_teams: &[&str]) -> Simulation {
    let mut sim = Simulation::with_seed(1);
    let country = sim.interner.intern("Americans");
    for &(slot, name) in slots {
        let owner = sim.interner.intern(name);
        sim.houses_mut_for_test()
            .register(HouseState::new(owner, 0, Some(country), false, 0, 10));
        sim.session
            .start_slot_houses_mut_for_test()
            .insert(slot, owner);
    }
    let teams = IniFile::from_str(
        "[TeamTypes]\n0=WAVETEAM_G\n1=WAVETEAM_A\n\
         [WAVETEAM_G]\nHouse=<Player @ G>\nScript=S\nTaskForce=F\nMax=-1\n\
         [WAVETEAM_A]\nHouse=<Player @ A>\nScript=S\nTaskForce=F\nMax=-1\n\
         [ScriptTypes]\n0=S\n[S]\n0=5,0\n[TaskForces]\n0=F\n",
    );
    let registry = TeamAiIniRegistry::from_sources(&IniFile::empty(), &teams, false);
    let mut triggers = String::from("[Triggers]\n");
    let mut events = String::from("[Events]\n");
    let mut tags = String::from("[Tags]\n");
    let mut actions = String::from("[Actions]\n");
    for (i, team) in wave_teams.iter().enumerate() {
        triggers.push_str(&format!("WAVE{i}=<none>,<none>,Waves,0,1,1,1,0\n"));
        events.push_str(&format!("WAVE{i}=1,8,0,0\n"));
        tags.push_str(&format!("TAG{i}=2,Waves,WAVE{i}\n"));
        actions.push_str(&format!("WAVE{i}=1,4,1,{team},0,0,0,0,A\n"));
    }
    let map_ini = format!(
        "[Map]\nTheater=TEMPERATE\nSize=0,0,2,1\nLocalSize=0,0,2,1\n\
         [IsoMapPack5]\n1=DwALABwBAAIA/////wAAABEAAA==\n\
         [Basic]\nNewINIFormat=4\n{triggers}{events}{tags}{actions}"
    );
    let map = MapFile::from_bytes(map_ini.as_bytes()).unwrap();
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
    let mut sim = armed_waves_sim(&[(6, "GHOST")], &["WAVETEAM_G"]);
    let player = sim.interner.intern("PLAYER");
    // PresidentDefense22's shape: the wave team's owner is an unseated
    // slot's house — installed, but nobody's opponent, the map's own
    // machinery.
    assert_eq!(
        sim.defense_map_classification(&[player], RuleSet::empty_for_test()),
        (true, 1, None)
    );
}

#[test]
fn wave_owner_inside_the_driven_seats_reads_battle_form() {
    let mut sim = armed_waves_sim(&[(6, "PLAYER")], &["WAVETEAM_G"]);
    let player = sim.interner.intern("PLAYER");
    // A battle map's reinforcement triggers resolve to a driven AI seat and,
    // too few to crown a wave host, stay battle form.
    assert_eq!(
        sim.defense_map_classification(&[player], RuleSet::empty_for_test()),
        (false, 1, None)
    );
}

#[test]
fn a_dominant_driven_wave_host_reads_defense_form_and_returns_for_stand_down() {
    let mut sim = armed_waves_sim(&[(6, "PLAYER"), (0, "ALLY")], &["WAVETEAM_G"; 6]);
    let player = sim.interner.intern("PLAYER");
    let ally = sim.interner.intern("ALLY");
    // A Super tier seated on the wave slot itself: every armed spawn
    // trigger resolves to its driven house, so the read crowns it wave host
    // — defense form however the slot is staffed — and hands it back for
    // the persona roster's stand-down. Six is exactly the dominance floor.
    assert_eq!(
        sim.defense_map_classification(&[player, ally], RuleSet::empty_for_test()),
        (true, 6, Some(player))
    );
}

#[test]
fn two_reinforcing_seats_below_the_share_crown_no_wave_host() {
    let mut sim = armed_waves_sim(
        &[(6, "PLAYER"), (0, "ALLY")],
        &[
            "WAVETEAM_G",
            "WAVETEAM_G",
            "WAVETEAM_G",
            "WAVETEAM_G",
            "WAVETEAM_G",
            "WAVETEAM_G",
            "WAVETEAM_A",
            "WAVETEAM_A",
            "WAVETEAM_A",
            "WAVETEAM_A",
        ],
    );
    let player = sim.interner.intern("PLAYER");
    let ally = sim.interner.intern("ALLY");
    // Six of ten clears the floor but not the two-thirds share: two
    // reinforcing battle seats crown no wave host and read battle form.
    assert_eq!(
        sim.defense_map_classification(&[player, ally], RuleSet::empty_for_test()),
        (false, 10, None)
    );
}
