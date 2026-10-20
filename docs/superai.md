# Super AI — fork development plan

This branch group (`superai/*`) is a fork-only feature line: a command-layer
"super AI" player and a headless win-rate bench. It tracks upstream
`YuriPlanet/vera20k` `main` and keeps its footprint outside `src/sim/` (one
documented exception, below). Licensed GPL like the rest of the tree.

## Positioning

The AI is a **virtual player at the command layer**: it observes the runtime
through the read-only view and issues the same `Command` stream a human
player would. It never mutates simulation state directly. This keeps
determinism, replays and upstream merges intact.

## Difficulty tiers

| Tier   | Strength sources                                             |
|--------|--------------------------------------------------------------|
| Fair   | None. Decisions only. Baseline for win-rate measurement.     |
| Brutal | Fair + money/harvest bonuses (original Brutal-style, later). |
| Super  | Fast build + parallel factories + continuous mass waves.     |

The build-speed multiplier is simulation-side by nature (INI mods change the
same native parameters). Super tier therefore allows exactly one minimal
`src/sim/` change on this fork: a per-house build-time multiplier field read
in `production/factory.rs` `time_to_build`. Nothing else in `src/sim/` may
depend on super-AI code.

## M1 pillars (order of work)

1. **Macro** — build order, power management, economy/harvester escort,
   expansion, tech timing; production queues never idle.
2. **Mass waves** — continuous no-gap attack waves, multi-prong pushes.
3. **Scout & counter** — scout, classify enemy composition, switch unit mix.
4. **Defense & micro** — AA, engineer/spy response, focus fire, kiting,
   ability usage.

## Determinism contract

Same view + same tick must produce the same command sequence. Strategies use
their own seeded RNG (never wall time or global state). The bench replays
bit-identically from its recorded seeds.

## Validation

`sim-bench --scenario super` runs N headless games (super AI vs the built-in
original AI) with per-game seeds, stops at `terminal_score_finalized` (with a
max-ticks guard) and prints a W/L table. Fair tier is the reference line:
strength claims cite these runs.

The super side plays the launch's local house, so the scenario_bootstrap
opening (start waypoint + the country's MCV) gives it a real 1v1 start; a
30000-tick smoke reaches its natural terminal with the idle MCV still
standing well into the game. Strategies start from the next milestone:
deploy the MCV, then macro.

## Upstream sync

`superai/base` merges `origin/main` regularly; each pillar lands on its own
`superai/<pillar>` branch off `base`. Anything generally useful (bench
harness parts, command-layer hooks) is a candidate to PR back upstream;
difficulty knobs and strategies stay fork-only.
