# Super AI — fork development plan

This branch group (`superai/*`) is a fork-only feature line: a command-layer
"super AI" player and a headless win-rate bench. It tracks upstream
`YuriPlanet/vera20k` `main` and keeps its footprint outside `src/sim/` (two
documented exceptions, below). Licensed GPL like the rest of the tree.

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
`time_to_build`. `None` is bit-identical to native. The second exception is
`HouseState::command_driven` (below): a per-house fork flag that stands the
native AI's decision loops down for a house the command layer drives.
Nothing else in `src/sim/` may depend on super-AI code.

## In-game selection and the command-driven house

The Super AI is selectable before the match. The skirmish lobby offers a
**Super AI** opponent row (persisted as its own slot code) that packs the
native Hard difficulty plus a `super_tier` marker on the launch slot.
`populate_launch_houses` turns a marked slot into a **command-driven
house**: it keeps the tier treatment above (Hard row grant, Super build
scale) and sets `command_driven`, and the native AI's decision loops
(base building, team creation, factory AI, MCV deploy, AI activation,
locomotor lifts) stand down for that house — the native AI and the
command layer must not both drive one house. The app's match loop then
rebuilds one `SuperAi` per command-driven non-passive non-human house on
every runtime install (fresh match and saved game) and merges its orders
into the tick's command stream on the Ordinary lane, including the
Brutal/Super credit drip the bench harness applies. An ordinary computer
slot is untouched: native AI, native grants, no drip.

Targeting is alliance-aware (both directions, in-game diplomacy can be
one-way): a Super AI on team A does not treat its teammates as intruders
or wave targets.

## M1 pillars (order of work)

1. **Macro** — build order, power management, economy/harvester escort,
   expansion, tech timing; production queues never idle. (Opening, mass
   production, the three-factory expansion and the tech climb have
   landed.)
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
tier — which the next slice addresses.

The expansion slice converts that production cap into pressure. The
build ladder now counts instead of ticking names off: past the opening
it adds a second and third war factory (each extra one multiplies
vehicle build time by retail `MultipleFactory=0.8`, so three build
tanks at 0.64× the single-factory time) with a power plant ahead of
each, gated on the first main tank leaving the factory — expanding
production must not starve the pipeline it expands. A destroyed
building drops its count and re-enters the ladder, so the walk doubles
as rebuild, and every factory's rally gets corrected, not just the
first one found. The wave size grows to match the faster queue (18
combat units before the first march), and the march target prefers the
hostile war factories' mass center — razing production starves
reinforcements — over the raw structure centroid. A retail regression
drives a Battle until a second GAWEAP stands. The seeded smokes
against Hard Russia show the trend, still without a first win: 0-3
(avg 25665) at one factory → 0-2-1 (24053) with three factories →
0-2-1 (27594) with 18-unit waves → 0-1-2 (27024, two 30000-tick
timeouts) with factory-focused waves and the tank gate. Survival went
from three losses to near-parity; closing games needs the next slice's
wave sustain and targeting depth. Brutal under the same setup loses 0-3
(avg 22207): the credits alone do not save games — the ×0.5 build
scale is what converts into survival.

The wave-pulse slice fixes the offensive's shape. The old branch sent
one standing unit per tick whenever more than the reserve stood — the
army left in single file and arrived piecemeal into a standing defense.
Now `wave_launching` (pinned state, part of the determinism contract)
gates a pulse: when `ATTACK_WAVE_SIZE` units stand gathered the whole
group departs over consecutive ticks — a mass, not a conga line — until
only the guard reserve remains home, and production reforms the next
wave while the current one fights. The seeded smoke moves the same
direction as ever without a first win: 0-1-2 (avg 28977, the single
loss postponed 21073→26932). Both sides now routinely hold to the
30000-tick backstop; breaking that stalemate is scouting/count and
micro territory, not more wave shaping.

The tech-climb slice adds the tier the strategy never had. Right behind
the second war factory the build ladder climbs — the radar class, then
the battle lab — before the expansion finishes with its third factory,
and once the lab stands the vehicle queue switches from the side's main
tank to its tier-2 tank (Prism, Apocalypse, Master Mind — the retail
`Prerequisite=…,<lab>` chain). The switch reads the lab's existence,
not the cameo's brightness: `cameo_draw_state` mirrors `StripClass::Draw`,
which checks factory readiness and build limit but no prerequisites —
`Recalculate` owns refusal — so a bright cameo appears long before the
lab exists and switching on it builds a tank the house never earned
(and stalls the expansion gate, which waits for the first main tank).
The radar step is the retail radar class, not a fixed type: the ladder
asks the native `can_build` gate which member of
`[General] PrerequisiteRadar=GAAIRC,NARADR,AMRADR,NAPSIS` this house
may build, in the list's order. That matters because GAAIRC itself
carries `ForbiddenHouses=Americans` — an Americans house builds
`AMRADR`, its identical twin under the American id; queueing the
forbidden GAAIRC instead (a stall the queue admits but per-tick
production keeps refusing, so the item sits at zero progress and zero
cost) deadlocks the whole climb. While a tech-climb piece (the radar or
the lab) is under construction the unit queues spend only banked
credits — an item waits until its own cost sits in the bank, so the
incoming ore flow stays with the climb instead of splitting three ways
across tanks and soldiers (miners are exempt: deferring the income
source behind a cushion would starve everything; so are routine
expansion buildings — throttling the defending army for every power
plant costs more games than the building rush saves). A retail
regression drives a Battle until the radar, the lab and a Prism stand.
The seeded smoke splits by tier: Brutal converts the climb into the
strategy's first win — 1-2 over three games, average 16595 ticks (was
0-3, average 22207) — while Fair pays for it in tempo: 0-3, average
18015 (was 0-1-2, average 28977). The unsubsidized radar-and-lab
investment thins the standing army that holds Hard's pressure; when to
climb on no subsidies is scouting/counter and micro territory, not more
wave shaping.

The traffic slice de-converges the flows that jammed the base. Rally
cells and guard posts fan out over an 8-cell offset pattern
(`SPREAD_OFFSETS`): each war factory rallies its output to its own cell
around the yard and guard posts spread by unit id instead of stacking
on the yard itself. A departing wave's dispatches cycle the enemy's
standing structures (war factories first) by unit id — the mass fans
out over the target base, and each destination is a live building's
own cell, always reachable-adjacent. Two hardening rules came out of
the retail regressions: every offset destination is clamped onto the
map (a wrapped off-map coordinate is silently dropped by the command
layer), and a wave episode never re-orders a unit it already
dispatched — a dropped order used to pin the wave branch on one unit
every tick and starve every branch below it, freezing all production.
The same drop-and-reissue starvation later surfaced in the defense and
guard branches (a guard post cell that a placed building occupies makes
the order permanently undroppable-in-the-other-direction: the unit
stays mission-`None`, and the branch re-ordered it every tick for a
tank's whole build time), so every single-unit order now passes a
per-unit reissue gate (`IDLE_REORDER_TICKS`): an order the command
layer drops costs one tick, not the branch's whole attention.

## Upstream sync

`superai/base` merges `origin/main` regularly; each pillar lands on its own
`superai/<pillar>` branch off `base`. Anything generally useful (bench
harness parts, command-layer hooks) is a candidate to PR back upstream;
difficulty knobs and strategies stay fork-only.
