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
| Brutal | Fair + the native Hard AI's opening grant plus a recurring   |
|        | credit bonus (+1000 per 600 ticks in the bench harness).     |
| Super  | Brutal + per-house build times scaled ×0.5 (fast build).     |

The tier treatment is applied by the harness (`apply_super_ai_tier` in
`headless_scenario.rs` plus the bench's drip), not by the strategy: Brutal and
Super re-use `native_ai_opening_grant` — the same kernel behind retail
`[General] MultiplayerAICM=400,0,0`'s Hard row — so the grant itself is a
native calculation, and the recurring drip goes through `Economy::add_credits`,
the entry native credit actions already use.

The build-speed multiplier is simulation-side by nature (INI mods change the
same native parameters). Super tier therefore allows exactly one minimal
`src/sim/` change on this fork: a per-house `native_build_time_scale` field
folded into the country multiplier of `production/factory.rs`
`time_to_build`. `None` is bit-identical to native. Nothing else in `src/sim/`
may depend on super-AI code.

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
otherwise post on an Area Guard there. With pure defense the one-game
smoke stops losing — a 30000-tick timeout with both sides standing.

The offensive slice then arms the march: once the army counts twelve
combat units, its standing units (idle or on their guard post) attack-
move on the hostile bases' mass center, keeping a four-unit guard
reserve home. The mechanism works and changes games, but at the Fair
tier every form of marching loses faster than pure defense — three
seeded games end 21946–26910 ticks against Hard Russia, versus the pure-
defense timeouts: no economic edge and no micro means every frontal
trade is negative. The march is the delivery vehicle for the Brutal and
Super tiers; the Fair baseline keeps it on record.

The tier slice lands the strength model itself. `apply_super_ai_tier`
applies the opening treatment before the first tick (Brutal and Super
take the native Hard grant; Super additionally installs the ×0.5
build-time scale), and the bench drips +1000 credits per 600 ticks into
Brutal/Super. A retail regression drives the same opening under Fair
and Super and asserts Super finishes it strictly earlier — the scale
and grant measurably reach `time_to_build`. The three seeded games
against Hard Russia end 0-3 (22098–29727 ticks) with the drip active
and are bit-identical to the drip-less run: with five miners feeding a
single war factory, money is not the binding constraint on this map, so
extra credits idle and no decision moves. The binding constraint is
parallel production — one war factory caps army output regardless of
tier — which is the next macro lever.

## Upstream sync

`superai/base` merges `origin/main` regularly; each pillar lands on its own
`superai/<pillar>` branch off `base`. Anything generally useful (bench
harness parts, command-layer hooks) is a candidate to PR back upstream;
difficulty knobs and strategies stay fork-only.
