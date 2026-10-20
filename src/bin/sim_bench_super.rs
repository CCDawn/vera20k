//! sim-bench's super scenario: the Super AI against one built-in computer.
//!
//! Each game loads the map as a Battle with one computer house of
//! `--builtin-country` at `--builtin-difficulty` (Russia, Hard by default),
//! then adds the Super AI as an extra human house of `--super-country`
//! (`add_human_house`: no start, no starting force, at war with everyone) and
//! plays it through [`SuperAi::orders`] — M0 emits nothing, so the skeleton
//! measures only the load → drive → terminal pipeline. The launch's own local
//! house idles at its start; the match naturally ends when its outcome is
//! decided, which `advance_frame`'s `TickResult::terminal_score_finalized`
//! reports (`advance_frame_for_tooling` discards its output, so this scenario
//! calls the same transaction through `advance_frame` to read the flag).
//! `--max-ticks` is the deadlock backstop; a game that reaches it is recorded
//! as a timeout.
//!
//! Game `i` of `--games N` runs with seed `seed + i`, so a run of N games
//! replays identically. Per game one line prints the winner (from
//! `HouseState::outcome`/`is_defeated`), the tick count and whether the game
//! timed out; the summary prints the Super AI's W-L record and the average
//! ticks per game.

use std::time::Instant;

use vera20k_core::headless_scenario::{self, SIM_TICK_MS};
use vera20k_core::sim::house_state::HouseOutcomeKind;
use vera20k_core::sim::intern::InternedId;
use vera20k_core::sim::runtime::SimRuntime;
use vera20k_core::sim::world::{FrameEffects, TickLane};
use vera20k_core::skirmish_launch::{AiDifficulty, LaunchCountry, SkirmishLaunchSession};
use vera20k_core::super_ai::{SuperAi, SuperAiDifficulty};

/// The `--max-ticks` backstop when the flag is absent: at 66 ms a tick, half
/// an hour of game time before a game is called dead.
pub(crate) const MAX_TICKS: u64 = 30_000;

/// The launch's ten stock countries, by the menu names `LaunchCountry`'s
/// legacy input accepts.
const COUNTRY_NAMES: [(&str, LaunchCountry); 10] = [
    ("America", LaunchCountry::America),
    ("Korea", LaunchCountry::Korea),
    ("France", LaunchCountry::France),
    ("Germany", LaunchCountry::Germany),
    ("GreatBritain", LaunchCountry::GreatBritain),
    ("Libya", LaunchCountry::Libya),
    ("Iraq", LaunchCountry::Iraq),
    ("Cuba", LaunchCountry::Cuba),
    ("Russia", LaunchCountry::Russia),
    ("Yuri", LaunchCountry::Yuri),
];

/// A country by its stock menu name.
pub(crate) fn country(name: &str) -> Result<LaunchCountry, String> {
    COUNTRY_NAMES
        .iter()
        .find(|&&(label, _)| label == name)
        .map(|&(_, country)| country)
        .ok_or_else(|| {
            format!(
                "unknown country {name}: one of {}",
                COUNTRY_NAMES
                    .iter()
                    .map(|&(label, _)| label)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// A built-in AI difficulty by its name.
pub(crate) fn difficulty(name: &str) -> Result<AiDifficulty, String> {
    match name {
        "easy" => Ok(AiDifficulty::Easy),
        "normal" => Ok(AiDifficulty::Normal),
        "hard" => Ok(AiDifficulty::Hard),
        other => Err(format!("unknown difficulty {other}: easy, normal or hard")),
    }
}

/// The Battle launch of one super game: the local house against one computer.
fn session(
    map: &str,
    builtin_country: LaunchCountry,
    builtin_difficulty: AiDifficulty,
) -> SkirmishLaunchSession {
    let mut session =
        headless_scenario::battle_session(map, &[(builtin_country, builtin_difficulty)]);
    // No roaming starting units: the match opens with each side's own MCV.
    session.options.unit_count = 0;
    session
}

/// The launch's one computer house.
fn builtin_computer(runtime: &SimRuntime) -> Option<InternedId> {
    let houses = runtime.simulation.houses();
    houses
        .order()
        .iter()
        .filter(|&&id| {
            houses
                .get(&id)
                .is_some_and(|house| !house.is_human() && !house.multiplay_passive())
        })
        .copied()
        .next()
}

/// The house that won, if one did: a defeated opponent decides it, else the
/// Super AI's own Victory outcome.
fn winner(
    runtime: &SimRuntime,
    super_house: InternedId,
    builtin: InternedId,
) -> Option<InternedId> {
    let houses = runtime.simulation.houses();
    let super_defeated = houses.get(&super_house).is_some_and(|h| h.is_defeated());
    let builtin_defeated = houses.get(&builtin).is_some_and(|h| h.is_defeated());
    match (super_defeated, builtin_defeated) {
        (true, false) => Some(builtin),
        (false, true) => Some(super_house),
        _ => houses.get(&super_house).and_then(|house| {
            (house.outcome()?.kind == HouseOutcomeKind::Victory).then_some(super_house)
        }),
    }
}

/// Play every game and print the per-game lines and the summary.
pub(crate) fn run(args: &super::Args) -> Result<(), String> {
    let mut wins = 0u32;
    let mut losses = 0u32;
    let mut timeouts = 0u32;
    let mut total_ticks = 0u64;
    for game in 0..args.games.get() {
        // Game i runs with seed+i, so N games replay identically.
        let seed = args.seed.wrapping_add(game);
        let started = Instant::now();
        let mut scenario = headless_scenario::load_session(
            &args.ra2_dir,
            &args.map,
            seed,
            session(&args.map, args.builtin_country, args.builtin_difficulty),
        )?;
        let super_house = scenario.add_human_house("SuperAI", args.super_country);
        let runtime = &mut scenario.runtime;
        runtime.simulation.batch_path_searches();
        runtime.simulation.batch_target_scans();
        let builtin =
            builtin_computer(runtime).ok_or("the launch created no built-in computer house")?;
        // M0 plays the baseline tier: the skeleton wires the pipeline only.
        let mut ai = SuperAi::new(super_house, SuperAiDifficulty::Fair);

        let mut ticks = 0u64;
        let mut finished = false;
        while ticks < args.max_ticks.get() {
            let batch = ai.orders(runtime, u32::try_from(ticks).unwrap_or(u32::MAX));
            let output = runtime
                .advance_frame(
                    &batch,
                    SIM_TICK_MS,
                    TickLane::Ordinary,
                    FrameEffects::default(),
                )
                .map_err(|error| format!("game {game} tick {ticks}: {error}"))?;
            ticks += 1;
            // The match ends when its score snapshot latches (the local
            // house's outcome is exit-ready) or the frame never commits.
            if output.tick.terminal_score_finalized || !output.tick.frame_committed {
                finished = true;
                break;
            }
        }
        if !finished {
            timeouts += 1;
        }
        let victor = winner(runtime, super_house, builtin);
        match victor {
            Some(id) if id == super_house => wins += 1,
            Some(_) => losses += 1,
            None => {}
        }
        total_ticks += ticks;
        let victor = match victor {
            Some(id) if id == super_house => "SuperAI",
            Some(_) => "Computer",
            None => "nobody",
        };
        println!(
            "game {} seed=0x{:08X}: winner={} ticks={} {} in {:.1}s",
            game + 1,
            seed,
            victor,
            ticks,
            if finished { "finished" } else { "timeout" },
            started.elapsed().as_secs_f64(),
        );
    }
    let games = args.games.get();
    println!(
        "super record: {}-{} over {} game(s), {} timeout(s)",
        wins, losses, games, timeouts
    );
    println!("average ticks per game: {}", total_ticks / u64::from(games));
    Ok(())
}
