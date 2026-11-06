//! Consuming controls for the headless trigger observation
//! (`Simulation::enable_headless_trigger_trace` / `take_headless_trigger_trace`). The test
//! traces are untouched by it: enabling the observation must not alter any
//! dispatch outcome, and the drained records report the fire, the action
//! execution and — through `note_trigger_failure` — a refusal reason.

use super::tests::{
    event_ingress_damage, event_ingress_fixture, install_event_ingress_map,
    spawn_event_ingress_unit,
};
use crate::sim::world::FrameEffects;

const HIT_MAP: &str = "[Triggers]\nHIT=Country0,<none>,Hit,0,1,1,1,0\n\
     [Tags]\nTAG=0,Hit,HIT\n[Events]\nHIT=1,41,0,0\n\
     [Actions]\nHIT=1,28,0,7,0,0,0,0,A\n";

fn fire_damage(
    sim: &mut crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    overlays: &crate::rules::overlay_types::OverlayTypeRegistry,
    victim: u64,
) {
    event_ingress_damage(
        sim,
        rules,
        overlays,
        victim,
        crate::sim::combat::RAD_NO_ATTACKER,
        1,
        FrameEffects::default(),
    );
}

/// Without opting in, nothing records: a default run stays byte-identical.
#[test]
fn disabled_trace_records_nothing() {
    let (mut sim, rules, overlays, houses) = event_ingress_fixture();
    let victim = spawn_event_ingress_unit(&mut sim, &rules, houses[0], (15, 15));
    install_event_ingress_map(&mut sim, &rules, HIT_MAP);
    sim.attach_object_trigger_tag(victim, Some("TAG"));
    fire_damage(&mut sim, &rules, &overlays, victim);
    assert!(sim.take_headless_trigger_trace().is_empty());
}

/// Enabled, one dispatch drains one fire record and one action record, and a
/// second drain is empty — the bins write exactly what a tick observed.
#[test]
fn enabled_trace_records_fire_and_action_then_drains_empty() {
    let (mut sim, rules, overlays, houses) = event_ingress_fixture();
    let victim = spawn_event_ingress_unit(&mut sim, &rules, houses[0], (15, 15));
    install_event_ingress_map(&mut sim, &rules, HIT_MAP);
    sim.attach_object_trigger_tag(victim, Some("TAG"));
    sim.enable_headless_trigger_trace();
    fire_damage(&mut sim, &rules, &overlays, victim);

    let events = sim.take_headless_trigger_trace();
    assert_eq!(events.len(), 2, "one fire record plus one action record");
    assert_eq!(events[0].trigger.as_deref(), Some("HIT"));
    assert_eq!(events[0].event, Some(41));
    assert_eq!(events[0].action, None);
    assert_eq!(events[1].action, Some(28));
    assert_eq!(events[1].value, Some(7));
    assert!(events[1].ok.is_none(), "28 has no outcome record");

    assert!(sim.take_headless_trigger_trace().is_empty(), "drained once");
}

/// The observation never disturbs the simulation's own state bookkeeping:
/// the action still ran (it set global variable 7) while traced.
#[test]
fn tracing_leaves_the_dispatch_outcome_in_place() {
    let (mut sim, rules, overlays, houses) = event_ingress_fixture();
    let victim = spawn_event_ingress_unit(&mut sim, &rules, houses[0], (15, 15));
    install_event_ingress_map(&mut sim, &rules, HIT_MAP);
    sim.attach_object_trigger_tag(victim, Some("TAG"));
    sim.enable_headless_trigger_trace();
    fire_damage(&mut sim, &rules, &overlays, victim);
    assert!(
        sim.trigger_runtime.globals_set.contains(&7),
        "the traced action still set global 7"
    );
}
