//! The observer read surface (the defense AI's input signals): armed
//! triggers and their elapsed-time timers, trigger variables, TeamType
//! definitions with their initial waypoints, and the object-tag chains a
//! defended structure carries. Every fixture installs through the production
//! scenario-init readers.

use super::tests::{event_ingress_fixture, install_event_ingress_map};
use super::*;
use crate::rules::ini_parser::IniFile;
use crate::rules::object_type::ObjectCategory;
use crate::rules::team_ai_ini::TeamAiIniRegistry;
use crate::sim::team_script_vm::{TeamMemberTypeIdentity, TeamTaskForceEntry};

/// One ordinary production master frame polling the installed triggers —
/// the local copy of the shared fixture loop, so the shared fixture file
/// stays untouched.
fn polled_master_frame(
    sim: &mut Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    overlay: &crate::rules::overlay_types::OverlayTypeRegistry,
) {
    let _ = sim
        .advance_master_frame(
            &[],
            rules,
            Some(overlay),
            67,
            crate::sim::world::TickLane::Ordinary,
            true,
            FrameEffects::default(),
        )
        .expect("the observer fixture frame completes");
}

/// One wave trigger the way a defense map authors it: a repeating
/// elapsed-time gate (event 13, two seconds) spawning a reinforcement team
/// (action 80) whose TaskForce and spawn waypoint are authored beside it.
const WAVE_BODY: &str = "[Triggers]\nWAVE=<none>,<none>,Wave,0,1,1,1,0\n\
     [Events]\nWAVE=1,13,0,2\n\
     [Actions]\nWAVE=1,80,1,TT,0,0,0,0,A\n\
     [Tags]\nWAVE_TAG=1,Wave,WAVE\n\
     [TeamTypes]\n0=TT\n[TT]\nHouse=Country0\nScript=S\nTaskForce=F\nWaypoint=A\nMax=1\n\
     [ScriptTypes]\n0=S\n[S]\n0=5,0\n\
     [TaskForces]\n0=F\n[F]\n0=1,TAGUNIT\n";

#[test]
fn armed_trigger_view_reads_the_wave_timer_and_team() {
    let (mut sim, rules, _, _) = event_ingress_fixture();
    install_event_ingress_map(&mut sim, &rules, WAVE_BODY);
    let frame = |sim: &Simulation| sim.session().binary_frame() as i32;
    let views = sim.trigger_runtime().armed_trigger_views(frame(&sim));
    assert_eq!(views.len(), 1, "the armed wave trigger registers once");
    let wave = &views[0];
    assert_eq!(wave.trigger_id, "WAVE");
    assert!(wave.enabled);
    assert_eq!(wave.completed_event_bits, 0);
    assert_eq!(
        wave.timer_remaining_frames,
        Some(30),
        "event 13's value 2 arms 2*15 logic frames at the instance's construction"
    );
    assert_eq!(
        wave.definition.events,
        vec![TriggerEventView { kind: 13, value: 2 }]
    );
    assert_eq!(wave.definition.actions.len(), 1);
    let spawn = &wave.definition.actions[0];
    assert_eq!(spawn.kind, 80);
    assert_eq!(spawn.team_type.as_deref(), Some("TT"));
    assert_eq!(
        spawn.waypoint,
        Some(0),
        "the A waypoint token reads index 0"
    );

    sim.session.set_binary_frame_for_test(15);
    assert_eq!(
        sim.trigger_runtime().armed_trigger_views(frame(&sim))[0].timer_remaining_frames,
        Some(15)
    );
    sim.session.set_binary_frame_for_test(30);
    assert_eq!(
        sim.trigger_runtime().armed_trigger_views(frame(&sim))[0].timer_remaining_frames,
        Some(0),
        "the timer reads its exact zero boundary"
    );
}

#[test]
fn elapsed_scenario_time_event_has_no_timer_and_sets_its_variables() {
    let (mut sim, rules, overlay, _) = event_ingress_fixture();
    install_event_ingress_map(
        &mut sim,
        &rules,
        "[Triggers]\nSURV=<none>,<none>,Survive,0,1,1,1,0\n\
         [Events]\nSURV=1,47,0,10\n\
         [Actions]\nSURV=2,28,0,5,0,0,0,0,A,56,0,7,0,0,0,0,A\n\
         [Tags]\nSURV_TAG=2,Survive,SURV\n",
    );
    let runtime = sim.trigger_runtime();
    let views = runtime.armed_trigger_views(sim.session().binary_frame() as i32);
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].timer_remaining_frames, None,
        "event 47 owns no CdTimer; its deadline is the event value"
    );
    assert_eq!(
        views[0].definition.events,
        vec![TriggerEventView {
            kind: 47,
            value: 10
        }]
    );
    assert!(!sim.global_variables()[5]);
    assert!(!runtime.local_variable_is_set(7));

    // The tenth authored second is frame 150 (15 logic frames per second);
    // read back the frame the flip lands on.
    let mut fired_at = None;
    for _ in 0..400 {
        polled_master_frame(&mut sim, &rules, &overlay);
        if sim.global_variables()[5] {
            fired_at = Some(sim.session().binary_frame());
            break;
        }
    }
    assert_eq!(
        fired_at,
        Some(151),
        "event 47's tenth second admits frame 150 and the next poll reads it"
    );
    assert!(sim.trigger_runtime().local_variable_is_set(7));
}

#[test]
fn teamtype_definition_reads_waypoint_and_composition() {
    let (mut sim, rules, _, _) = event_ingress_fixture();
    let map = install_event_ingress_map(
        &mut sim,
        &rules,
        &format!("{WAVE_BODY}[Waypoints]\n0=16014\n"),
    );
    let teams = TeamAiIniRegistry::from_sources(&IniFile::from_str(""), &map.ini, true);
    sim.install_team_ai_registry(&teams, &rules).unwrap();
    sim.session.install_waypoints(&map.waypoints);
    let tt = sim.interner().get("TT").unwrap();
    let definition = sim
        .team_script_vm()
        .team_types()
        .get(&tt)
        .expect("the authored TeamType installs");
    assert_eq!(
        sim.team_script_vm().team_type_waypoint(tt),
        0,
        "Waypoint=A retains letter index 0"
    );
    let force = sim
        .team_script_vm()
        .task_force_of(tt)
        .expect("the TaskForce reference resolves");
    assert_eq!(
        force.entries,
        vec![TeamTaskForceEntry {
            member_type: TeamMemberTypeIdentity {
                category: ObjectCategory::Vehicle,
                id: sim.interner().get("TAGUNIT").unwrap(),
            },
            count: 1,
        }]
    );
    assert_eq!(
        definition.task_force_id,
        sim.interner().get("F").unwrap(),
        "the definition keeps its resolved TaskForce reference"
    );
    assert_eq!(
        sim.session().waypoint_cell(0),
        Some((14, 16)),
        "the packed authored waypoint decodes to its cell"
    );
}

#[test]
fn object_tag_chain_view_reads_the_defended_object() {
    let (mut sim, rules, _, houses) = event_ingress_fixture();
    install_event_ingress_map(
        &mut sim,
        &rules,
        "[Triggers]\nLOSE=Country0,<none>,Lost,0,1,1,1,0\n\
         [Events]\nLOSE=1,7,0,0\n\
         [Actions]\nLOSE=1,2,0,0,0,0,0,0,A\n\
         [Tags]\nLOSE_TAG=0,Lost,LOSE\n",
    );
    let owner = sim.interner().resolve(houses[0]).to_string();
    let defended = sim
        .spawn_object_at_height("TAGUNIT", &owner, 15, 15, 0, 0, &rules)
        .unwrap();
    let bystander = sim
        .spawn_object_at_height("TAGUNIT", &owner, 16, 15, 0, 0, &rules)
        .unwrap();
    sim.attach_object_trigger_tag(defended, Some("LOSE_TAG"));
    let chains = sim.trigger_runtime().object_tag_definition_views(defended);
    assert_eq!(chains.len(), 1, "the attached Tag registers one chain");
    assert_eq!(
        chains[0].events,
        vec![TriggerEventView { kind: 7, value: 0 }],
        "the destroyed-by-anything event is readable"
    );
    assert_eq!(chains[0].actions.len(), 1);
    assert_eq!(chains[0].actions[0].kind, 2, "the lose action is readable");
    assert!(
        sim.trigger_runtime()
            .object_tag_definition_views(bystander)
            .is_empty(),
        "an object with no attached Tag reads empty"
    );
}
