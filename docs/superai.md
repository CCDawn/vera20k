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
opening (start waypoint + the country's MCV) gives it a real 1v1 start.

The first strategy slice is landed: the Fair tier deploys the MCV and walks
the retail opening (power, refinery, barracks, war factory, then a second
refinery and harvesters) as pure decisions — one command per tick, gated by
the production cameo state. A retail regression test drives a real Battle
from the launch MCV to a standing construction yard, power plant and
refinery (the idle-MCV baseline terminated at 12904 ticks).

The mass-production slice arms the opening: the vehicle queue builds miners
to a target of five and then the side's main battle tank (Grizzly, Rhino,
Lasher) without pause, the war factory's rally sits on the construction
yard, and the barracks runs the side's basic soldier continuously; a second
retail regression drives a real Battle until a Grizzly and a GI stand.
Against Hard Russia the one-game smoke ended at 15528 ticks — passive
tanks without micro feed the attacker wave by wave.

The defense slice holds the base: idle tanks and soldiers attack the
nearest intruder inside a 16-cell ring around the yard (one order per
tick, re-derived from each unit's current mission so nothing spams) and
otherwise post on an Area Guard there. With that, the same smoke no
longer loses — the one-game run times out at the 30000-tick backstop
with both sides standing (attack marching is still unimplemented, so a
won position cannot be converted).

## Upstream sync

`superai/base` merges `origin/main` regularly; each pillar lands on its own
`superai/<pillar>` branch off `base`. Anything generally useful (bench
harness parts, command-layer hooks) is a candidate to PR back upstream;
difficulty knobs and strategies stay fork-only.
