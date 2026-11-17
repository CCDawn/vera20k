//! Emit per-tick parity digests from a headless simulation run.
//!
//! Two modes:
//!
//! * `--map <file> --ra2-dir <path>` loads a real retail scenario with a pinned seed.
//!   This is the mode a cross-engine comparison uses: the same map and the same seed on
//!   both engines is the precondition for a divergence meaning anything.
//! * no `--map` runs a small synthetic two-house setup. It validates that real
//!   `Simulation` state serialises and survives the consumer's strict parsing — it says
//!   nothing about parity, because the scenario has no counterpart in gamemd.
//!
//! **What a real-map run contains (F09).** The scenario is constructed through the same
//! GPU-free funnel the app uses (see `headless_scenario`): map-roster houses, map-placed
//! units and structures, bridge state, wall ownership, smudges, and the seeded RNG
//! streams are all real. What it still lacks versus an app launch is the skirmish
//! *session* — player houses, start-position placement — so digests represent a
//! spectatorless load of the map as authored, not a played skirmish opening.
//!
//! Defense-harness additions (all opt-in, all additive):
//!
//! * `--lobby-slots` brings the map's multiplayer lobby slots (`<Player @ A..H>`)
//!   into the scenario as passive houses, so slot-owned reinforcement teams
//!   resolve an owner and their waves actually spawn.
//! * `--trace-triggers <path.jsonl>` writes one JSON line per observed trigger
//!   fire, action outcome (with the failure reason when an action silently
//!   refuses — the unresolved lobby slot above is the canonical case) and
//!   trigger effect (EVA line, authored text).
//! * `--crate-e2e` runs the money-crate end-to-end probe instead of digests:
//!   it places a crate overlay on the loaded real map, walks an infantry
//!   probe onto it, and asserts the owner's credits rose — the
//!   place → pick up → add credits chain, exercised at map level.

use std::path::{Path, PathBuf};

use vera20k_core::headless_scenario::{self, SIM_TICK_MS};
use vera20k_core::map::entities::EntityCategory;
use vera20k_core::sim::combat::Health;
use vera20k_core::sim::command::{Command, CommandEnvelope};
use vera20k_core::sim::house_state::HouseState;
use vera20k_core::sim::parity_digest::ParityDigestSink;
use vera20k_core::sim::runtime::SimRuntime;
use vera20k_core::sim::world::{FrameEffects, Simulation, TickLane};

const DEFAULT_TICKS: u64 = 600;
/// Arbitrary but fixed, so two synthetic runs are comparable to each other.
const DEFAULT_SEED: u32 = 0x5EED_0001;
/// Frames one crate e2e attempt gets before the probe gives up on that cell.
const CRATE_E2E_ATTEMPT_TICKS: u64 = 180;

struct Args {
    out: PathBuf,
    ticks: u64,
    seed: u32,
    map: Option<String>,
    ra2_dir: Option<PathBuf>,
    lobby_slots: bool,
    trace_triggers: Option<PathBuf>,
    crate_e2e: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut out: Option<PathBuf> = None;
    let mut ticks = DEFAULT_TICKS;
    let mut seed = DEFAULT_SEED;
    let mut map: Option<String> = None;
    let mut ra2_dir: Option<PathBuf> = None;
    let mut lobby_slots = false;
    let mut trace_triggers: Option<PathBuf> = None;
    let mut crate_e2e = false;
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut next = |what: &str| argv.next().ok_or(format!("{flag} needs {what}"));
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(next("a path")?)),
            "--ticks" => {
                ticks = next("a count")?
                    .parse()
                    .map_err(|_| "--ticks must be a positive integer".to_string())?;
            }
            "--seed" => {
                let raw = next("a 32-bit value")?;
                let parsed = raw
                    .strip_prefix("0x")
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .unwrap_or_else(|| raw.parse());
                seed =
                    parsed.map_err(|_| "--seed must be a u32 (decimal or 0x-hex)".to_string())?;
            }
            "--map" => map = Some(next("a map file name")?),
            "--ra2-dir" => ra2_dir = Some(PathBuf::from(next("a path")?)),
            "--lobby-slots" => lobby_slots = true,
            "--trace-triggers" => trace_triggers = Some(PathBuf::from(next("a path")?)),
            "--crate-e2e" => crate_e2e = true,
            other => return Err(format!("unrecognised argument {other}")),
        }
    }
    if map.is_some() && ra2_dir.is_none() {
        return Err("--map also needs --ra2-dir naming the retail install".to_string());
    }
    if crate_e2e && map.is_none() {
        return Err("--crate-e2e runs on a real map: pass --map and --ra2-dir".to_string());
    }
    Ok(Args {
        out: out.ok_or("--out <path> is required".to_string())?,
        ticks,
        seed,
        map,
        ra2_dir,
        lobby_slots,
        trace_triggers,
        crate_e2e,
    })
}

/// Two houses with credits and a handful of entities.
///
/// Enough for every digest field to carry a non-trivial value; the runtime is
/// bound to empty resources, so no rules-driven behaviour runs.
fn build_synthetic_simulation(seed: u32) -> Simulation {
    let mut sim = Simulation::with_seed(u64::from(seed));

    for (index, (owner, side)) in [("Americans", 0u8), ("Russians", 1u8)].iter().enumerate() {
        let owner_id = sim.intern(owner);
        let mut house = HouseState::new(owner_id, *side, None, index == 0, 10_000, 10);
        // The structures below are raw-inserted without lifecycle registration, so
        // the defeat scan would read both houses as owning nothing and resolve the
        // match on tick 1, freezing the committed-tick counter. Passive houses are
        // exempt from defeat evaluation, keeping every tick committable.
        house.make_multiplay_passive();
        sim.register_house(house);

        let type_ref = sim.intern("GACNST");
        let base_x = 10 + (index as u16) * 20;
        for slot in 0..3u16 {
            sim.insert_synthetic_techno_for_diagnostics(
                base_x + slot * 2,
                12,
                0,
                0,
                owner_id,
                Health::new(1000),
                type_ref,
                EntityCategory::Structure,
                0,
                6,
                false,
            );
        }
    }
    sim
}

/// A drainable trigger-trace destination.
struct TraceSink {
    writer: std::io::BufWriter<std::fs::File>,
    written: u64,
}

impl TraceSink {
    fn create(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", path.display()))?;
        }
        let file = std::fs::File::create(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        Ok(Self {
            writer: std::io::BufWriter::new(file),
            written: 0,
        })
    }

    fn write(&mut self, tick: u64, event: &vera20k_core::sim::trigger_runtime::HeadlessTriggerEvent) {
        use std::io::Write;
        let line = serde_json::to_string(event).expect("trace event serialises");
        let _ = writeln!(self.writer, "{{\"tick\":{tick},\"event\":{line}}}");
        self.written += 1;
    }

    fn flush(&mut self) {
        use std::io::Write;
        let _ = self.writer.flush();
    }
}

/// One trigger-trace drain, with the counts the run summary reports.
struct TraceSummary {
    fires: u64,
    failures: u64,
    effects: u64,
}

/// Classify drained records: an action outcome is a fire (a failure when it
/// reports `ok = false`), a detail-only record is an effect, everything else
/// is a bare trigger fire.
fn summarize_events(
    events: &[vera20k_core::sim::trigger_runtime::HeadlessTriggerEvent],
) -> TraceSummary {
    let mut summary = TraceSummary {
        fires: 0,
        failures: 0,
        effects: 0,
    };
    for event in events {
        match (event.action, event.ok, event.detail.as_deref()) {
            (Some(_), Some(false), _) => summary.failures += 1,
            (Some(_), _, _) => summary.fires += 1,
            (None, _, Some(_)) => summary.effects += 1,
            _ => summary.fires += 1,
        }
    }
    summary
}

fn drain_trace(
    scenario: &mut headless_scenario::HeadlessScenario,
    sink: Option<&mut TraceSink>,
    tick: u64,
    summary: &mut TraceSummary,
) {
    let events = scenario.runtime.simulation.take_headless_trigger_trace();
    if let Some(sink) = sink {
        for event in &events {
            sink.write(tick, event);
        }
    }
    let counted = summarize_events(&events);
    summary.fires += counted.fires;
    summary.failures += counted.failures;
    summary.effects += counted.effects;
}

/// The money-crate end-to-end probe: place a money crate through the runtime
/// placement owner, walk an infantry probe onto it, and require the probe
/// owner's credits to rise.
///
/// Every step goes through production code on the loaded real map: the crate
/// lands via the runtime placement owner (the Action-108 route), the pickup
/// runs in the movement host (`CellClass::PickupCrate @ 0x00481A00`), and the
/// credits arrive through `Add_Credits @ 0x004F9950`. Returns the report line.
fn run_crate_e2e(
    scenario: &mut headless_scenario::HeadlessScenario,
    ticks: u64,
) -> Result<String, String> {
    const PROBE_TYPE: &str = "E1";
    let registry = scenario.runtime.resources.overlay_registry.clone();

    // The probe: a plain human house at war with everyone, so a multiplayer
    // pickup is admissible for it (passive houses refuse).
    let owner = scenario.add_human_house(
        "CrateProbe",
        vera20k_core::skirmish_launch::LaunchCountry::America,
    );
    let owner_name = "CrateProbe".to_owned();
    let credits_before = scenario
        .runtime
        .simulation
        .houses()
        .get(&owner)
        .map(|house| house.economy().credits())
        .ok_or("probe house missing after creation")?;

    // Place money crates (selection 0 stores the Money arm, so the later
    // pickup spends no selection draw) until one lands beside a cell the
    // probe can stand on: a crate snap may choose water, where infantry
    // cannot spawn. Candidates stride across the whole map so the search
    // does not pool in one corner; each accepted placement keeps its crate,
    // and the walk uses the first matched pair.
    let terrain_cells: Vec<(u16, u16)> = scenario
        .runtime
        .simulation
        .resolved_terrain()
        .iter()
        .flat_map(|terrain| terrain.iter().map(|cell| (cell.rx, cell.ry)))
        .collect();
    let stride = (terrain_cells.len() / 300).max(1);
    let mut crate_cell: Option<(i16, i16)> = None;
    let mut probe_id: Option<u64> = None;
    let mut placements = 0_u32;
    'match_crate: for &(rx, ry) in terrain_cells.iter().step_by(stride) {
        // Infantry standability first: the witness spawn accepts the cell or
        // the cell never hosts a march target. Crates go down under units in
        // real games, so the witness standing there is native legal too.
        let SimRuntime {
            simulation,
            resources,
        } = &mut scenario.runtime;
        let witness = simulation.spawn_object(PROBE_TYPE, &owner_name, rx, ry, 0, &resources.rules);
        let Some(_witness_id) = witness else {
            continue;
        };
        let placed = vera20k_core::sim::crates::place_crate_at_for_tooling(
            &mut scenario.runtime.simulation,
            &scenario.runtime.resources.rules,
            &registry,
            (rx as i16, ry as i16),
            Some(0),
        );
        let Some(cell) = placed else {
            continue;
        };
        placements += 1;
        let cell_x = cell.0.max(0) as u16;
        let cell_y = cell.1.max(0) as u16;
        for (dx, dy) in [
            (-1i32, 0i32),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (1, 1),
            (-1, 1),
            (1, -1),
        ] {
            let (nx, ny) = (i32::from(cell.0) + dx, i32::from(cell.1) + dy);
            let (Ok(nx), Ok(ny)) = (u16::try_from(nx), u16::try_from(ny)) else {
                continue;
            };
            let SimRuntime {
                simulation,
                resources,
            } = &mut scenario.runtime;
            if let Some(id) =
                simulation.spawn_object(PROBE_TYPE, &owner_name, nx, ny, 0, &resources.rules)
            {
                crate_cell = Some(cell);
                probe_id = Some(id);
                break 'match_crate;
            }
        }
        if placements >= 256 {
            break;
        }
    }
    let (Some(crate_cell), Some(probe_id)) = (crate_cell, probe_id) else {
        return Err(format!(
            "crate e2e: {placements} crate placements, none beside a spawnable cell"
        ));
    };

    // The probe house must own a building: a house holding nothing loses at
    // frame one past zero (short game) and Blowup_All takes the probe with
    // it — yet a passive owner is refused the pickup. One keep-alive
    // pillbox, placed as far from the crate as the map allows, satisfies
    // both.
    let keep_alive_types = ["GAPILL", "NASAM", "GAPOWR"];
    let mut by_distance: Vec<&(u16, u16)> = terrain_cells.iter().collect();
    by_distance.sort_by_key(|&&(rx, ry)| {
        std::cmp::Reverse(
            (rx as i64 - crate_cell.0 as i64).pow(2) + (ry as i64 - crate_cell.1 as i64).pow(2),
        )
    });
    let mut keep_alive = false;
    'keep_alive: for &&(rx, ry) in by_distance.iter().step_by(7).take(400) {
        for type_id in keep_alive_types {
            let SimRuntime {
                simulation,
                resources,
            } = &mut scenario.runtime;
            if simulation
                .spawn_object(type_id, &owner_name, rx, ry, 0, &resources.rules)
                .is_some()
            {
                keep_alive = true;
                break 'keep_alive;
            }
        }
    }
    if !keep_alive {
        return Err("crate e2e: the probe house could not gain a keep-alive building".to_string());
    }

    let first_frame = scenario.runtime.simulation.session().tick() + 1;
    let orders = vec![CommandEnvelope::new(
        owner,
        first_frame,
        Command::Move {
            entity_id: probe_id,
            target_rx: crate_cell.0 as u16,
            target_ry: crate_cell.1 as u16,
        },
    )];
    let mut frames_run = 0_u64;
    for _ in 0..ticks.min(CRATE_E2E_ATTEMPT_TICKS) {
        scenario
            .runtime
            .advance_frame(
                &orders,
                SIM_TICK_MS,
                TickLane::Ordinary,
                FrameEffects::default(),
            )
            .map_err(|error| error.to_string())?;
        frames_run += 1;
        let credits_now = scenario
            .runtime
            .simulation
            .houses()
            .get(&owner)
            .map(|house| house.economy().credits())
            .unwrap_or(credits_before);
        if credits_now > credits_before {
            return Ok(format!(
                "crate e2e: probe walked onto the crate at {crate_cell:?} by frame {frames_run}; \
                 credits {credits_before} -> {credits_now}",
            ));
        }
    }
    // The failure report distinguishes a march failure from a pickup
    // failure: the probe's final cell decides which half broke.
    let probe_cell = scenario
        .runtime
        .simulation
        .entities()
        .get(probe_id)
        .map(|entity| (entity.position.rx(), entity.position.ry()));
    let overlay_left = scenario
        .runtime
        .simulation
        .overlay_grid()
        .map(|grid| {
            grid.cell(crate_cell.0 as u16, crate_cell.1 as u16)
                .overlay_id
                .is_some()
        })
        .unwrap_or(false);
    Err(format!(
        "crate e2e: probe did not pick the crate up at {crate_cell:?} within {frames_run} frames \
         (credits unchanged at {credits_before}; probe now at {probe_cell:?}; \
         crate overlay still present: {overlay_left})",
    ))
}

fn main() -> Result<(), String> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            eprintln!(
                "usage: parity-digest --out <path.jsonl> [--ticks N] [--seed S] \
                 [--map <file> --ra2-dir <retail path>] [--lobby-slots] \
                 [--trace-triggers <path.jsonl>] [--crate-e2e]"
            );
            std::process::exit(2);
        }
    };

    match (&args.map, &args.ra2_dir) {
        (Some(map), Some(ra2_dir)) if args.crate_e2e => {
            let mut scenario = headless_scenario::load(ra2_dir, map, args.seed)?;
            if args.lobby_slots {
                let slots = scenario.install_lobby_slot_houses();
                println!("lobby slot houses installed: {}", slots.len());
            }
            let report = run_crate_e2e(&mut scenario, args.ticks)?;
            println!("{report}");
            return Ok(());
        }
        (Some(map), Some(ra2_dir)) => {
            let mut sink = ParityDigestSink::create(&args.out)
                .map_err(|error| format!("could not open {}: {error}", args.out.display()))?;
            let mut trace_sink = match &args.trace_triggers {
                Some(path) => Some(TraceSink::create(path)?),
                None => None,
            };
            let mut scenario = headless_scenario::load(ra2_dir, map, args.seed)?;
            if args.lobby_slots {
                let slots = scenario.install_lobby_slot_houses();
                println!("lobby slot houses installed: {}", slots.len());
            }
            if trace_sink.is_some() {
                scenario.runtime.simulation.enable_headless_trigger_trace();
            }
            println!(
                "loaded {map} ({}x{}, theater {}) seed 0x{:08X}",
                scenario.sim().session().map_width(),
                scenario.sim().session().map_height(),
                scenario.map.header.theater,
                args.seed
            );
            let mut summary = TraceSummary {
                fires: 0,
                failures: 0,
                effects: 0,
            };
            let registry = scenario.runtime.resources.overlay_registry.clone();
            for _ in 0..args.ticks {
                scenario.tick();
                let tick = scenario.sim().session().tick();
                let digest = scenario.sim().parity_digest_with_overlay_counts(&registry);
                sink.write(&digest)
                    .map_err(|error| format!("digest write failed: {error}"))?;
                drain_trace(&mut scenario, trace_sink.as_mut(), tick, &mut summary);
            }
            if let Some(mut sink) = trace_sink {
                sink.flush();
                println!(
                    "wrote {} trigger trace records to {} ({} action outcomes, {} failures, {} \
                     effects)",
                    sink.written,
                    args.trace_triggers.as_ref().unwrap().display(),
                    summary.fires,
                    summary.failures,
                    summary.effects,
                );

        }
        _ => {
            // F09: the synthetic run advances through the same bound-resource
            // runtime transaction as everything else. Empty resources stand in
            // for the "no rules loaded" contract the synthetic scenario always
            // had; synthetic digests are self-comparable, not parity evidence.
            let mut runtime = vera20k_core::sim::runtime::SimRuntime {
                simulation: build_synthetic_simulation(args.seed),
                resources: std::sync::Arc::new(vera20k_core::sim::runtime::SimResources::empty()),
            };
            for _ in 0..args.ticks {
                runtime
                    .advance_frame_for_tooling(&[], SIM_TICK_MS)
                    .map_err(|error| error.to_string())?;
                let digest = runtime.simulation.parity_digest();
                sink.write(&digest)
                    .map_err(|error| format!("digest write failed: {error}"))?;
            }
            println!("wrote {} digests to {}", sink.written(), args.out.display());
            return Ok(());
        }
        _ => {}
    }

    // F09: the synthetic run advances through the same bound-resource
    // runtime transaction as everything else. Empty resources stand in
    // for the "no rules loaded" contract the synthetic scenario always
    // had; synthetic digests are self-comparable, not parity evidence.
    let mut sink = ParityDigestSink::create(&args.out)
        .map_err(|error| format!("could not open {}: {error}", args.out.display()))?;
    let mut runtime = SimRuntime {
        simulation: build_synthetic_simulation(args.seed),
        resources: vera20k_core::sim::runtime::SimResources::empty(),
    };
    for _ in 0..args.ticks {
        runtime
            .advance_frame_for_tooling(&[], SIM_TICK_MS)
            .map_err(|error| error.to_string())?;
        let digest = runtime.simulation.parity_digest();
        sink.write(&digest)
            .map_err(|error| format!("digest write failed: {error}"))?;
    }

    println!("wrote {} digests to {}", sink.written(), args.out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vera20k_core::sim::trigger_runtime::HeadlessTriggerEvent;

    fn event(action: Option<i32>, ok: Option<bool>, detail: Option<&str>) -> HeadlessTriggerEvent {
        HeadlessTriggerEvent {
            trigger: None,
            event: None,
            action,
            value: None,
            ok,
            detail: detail.map(String::from),
        }
    }

    /// The summary separates fires, failed actions (the silent refusals the
    /// harness exists to surface) and effect records, in the fixed order the
    /// run report prints them.
    #[test]
    fn summary_counts_fires_failures_and_effects() {
        let events = vec![
            event(Some(80), None, None),                     // action executed
            event(None, None, Some("text UI:WaveIncoming")), // effect record
            event(Some(80), Some(false), Some("team x: owner not resolvable")),
            event(Some(107), Some(true), None), // succeeded action
            event(None, None, None),            // bare trigger fire
        ];
        let summary = summarize_events(&events);
        assert_eq!(
            (summary.fires, summary.failures, summary.effects),
            (3, 1, 1)
        );
    }
}
