# 超级AI (Super AI) — fork development plan

This branch group (`superai/*`) is a fork-only feature line: a command-layer
"super AI" player and a headless win-rate bench. The project's public name is
**超级AI / Super AI**; the in-game rows stay English (`Super AI`,
`Super AI MAX`) because the retail font carries no CJK glyphs. It tracks
upstream `YuriPlanet/vera20k` `main` and keeps its footprint outside
`src/sim/` (three documented exceptions, below). Licensed GPL like the rest of
the tree.

## Positioning

The AI is a **virtual player at the command layer**: it observes the runtime
through the read-only view and issues the same `Command` stream a human
player would. It never mutates simulation state directly. This keeps
determinism, replays and upstream merges intact.

## Difficulty tiers

| Tier     | Strength sources                                             |
|----------|--------------------------------------------------------------|
| Fair     | None. Decisions only. Baseline for win-rate measurement.     |
| Brutal   | Fair + the native Hard AI's opening grant plus a recurring   |
|          | credit bonus (+1000 per 600 ticks in the bench harness).     |
| Super    | Brutal + per-house build times scaled ×0.5 (fast build).     |
| Unlimited| Super's whole treatment plus a per-tick wallet floor topped   |
|          | up to 1,000,000 (`UNLIMITED_WALLET_FLOOR`). Spectacle tier    |
|          | for watching the strategy spend without an economy behind    |
|          | it — strength claims never cite it.                           |

The tier treatment is applied by the harness (`apply_super_ai_tier` in
`headless_scenario.rs` plus the bench's drip), not by the strategy: Brutal and
Super re-use `native_ai_opening_grant` — the same kernel behind retail
`[General] MultiplayerAICM=400,0,0`'s Hard row — so the grant itself is a
native calculation, and the recurring drip and the Unlimited floor both go
through `Economy::add_credits`, the entry native credit actions already use.
The launch's `sim_tick` applies the same rules as the bench every ordinary
tick, so the tier behaves identically in-game and headless. The tier a house
launched with rides the house itself (`HouseState::super_tier`), so the driver
rebuild — fresh launch and saved-game restore alike — recovers it exactly;
saves written before the mark fall back to the build-scale heuristic.

The Unlimited floor deliberately cannot wedge the strategy: the
income-recovery cancel rule (below) fires only when the wallet genuinely
cannot finish the queued item, and a topped-up wallet finishes anything.

The build-speed multiplier is simulation-side by nature (INI mods change the
same native parameters). Super tier therefore allows exactly one minimal
`src/sim/` change on this fork: a per-house `native_build_time_scale` field
folded into the country multiplier of `production/factory.rs`
`time_to_build`. `None` is bit-identical to native. The second exception is
`HouseState::command_driven` (below): a per-house fork flag that stands the
native AI's decision loops down for a house the command layer drives.
Nothing else in `src/sim/` may depend on super-AI code.

The third exception carries no super-AI knowledge at all — it is a crash
guard the AI-vs-AI matches exposed. Native's Fly admission
(`world/fly_process.rs`) places an out-of-diamond step as it is for an
owner that may not leave the map, leaning on the engine's wrapping fixed
point to keep the off-map walk representable; VERA's `SimFixed` panics once
the walk leaves its domain. Three guards now hold the line: the landing
drift ends when its destination's cell is off-playfield, the paid step
refuses out-of-diamond candidates for owners that may not operate outside
the playfield, and every class is refused a step past the position's
fixed-point aliasing range (`put_world_xy`'s own documented domain).
FlyBy Aircraft and owners that may leave the map keep native behavior
inside that range. The observed walker was a spy plane whose mission
dispatch stopped mid-flight — it then overflew its destination, left the
map, and (mission still Approach with a live target, so the off-map
removal predicate answers no) walked on toward the corner; the stalled
mission dispatch is an open engine thread the guards make harmless.

## In-game selection and the command-driven house

The Super AI is selectable before the match. The skirmish lobby offers two
fork-only opponent rows — **Super AI** (persisted as its own slot code) and
**Super AI MAX** (Unlimited: the same treatment plus the wallet floor) — each
packing the native Hard difficulty plus a `super_tier` marker on the launch
slot.
`populate_launch_houses` turns a marked slot into a **command-driven
house**: it keeps the tier treatment above (Hard row grant, Super build
scale) and sets `command_driven`, and the native AI's decision loops
(base building, team creation, factory AI, MCV deploy, AI activation,
locomotor lifts) stand down for that house — the native AI and the
command layer must not both drive one house. The app's match loop then
rebuilds one `SuperAi` per command-driven non-passive non-human house on
every runtime install (fresh match and saved game, tier read back from
`HouseState::super_tier`) and merges its orders
into the tick's command stream on the Ordinary lane, including the
Brutal/Super credit drip and the Unlimited floor the bench harness
applies. An ordinary computer
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
refinery (the idle-MCV baseline terminated at 12904 ticks). The opening
deploy clears its own ground first: `is_clear_to_build` counts standing
infantry and vehicles as occupants (only the deploying MCV is lifted), and
a community map's packed start never clears while every unit stands idle
waiting for the yard (observed: six houses, five attempts each, no yard,
the whole match silent) — so idle own units inside the foundation are
walked out one Move at a time, a start the ground itself refuses walks the
MCV to the nearest legal foundation, and only a foundation that would land
spends one of the paced attempts.

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

The situation slice gives the strategy the three things its first pass
lacked. **Superweapons follow the lobby setting** — the build ladder's
superweapon step (Weather Controller, Iron Curtain, Psychic Dominator,
right behind the battle lab) needs no toggle of its own: every retail
superweapon is `DisableableFromShell=yes`, so `can_build` refuses the
building while the lobby toggle is off, its cameo stays dark and the
step falls through to the expansion; with it on, a charged instance
fires (charge gate, active and ready — the fire button's own
admission), the Iron Curtain centering the first standing own vehicle
(its invulnerability buffs armor; organic infantry under it take
damage) and the storm/dominator striking the hostile war factories if
any stand, else any structure, else the hostile mass center; a refused
launch (a storm already in flight, a dominator mid-pulse) retries after
150 ticks instead of spinning the branch. **The army mixes its whole
roster in**, chosen by the situation: each side's vehicle and infantry
rosters carry retail-evidenced roles (anti-air picks cite the weapon's
`AA=yes` projectile; the Tank Destroyer is Germans-only, the Tesla Tank
Russians-only), the queue fills the core tank/soldier for the first
six issued combat slots, then a wheel sends every fourth vehicle slot
(every third infantry slot) to the role furthest behind its demanded
share — the mix rides issued slots, not standing vehicles: under
attrition the army dies faster than the queue replaces it, and a
live-count gate stayed on core tanks forever (observed: a whole game
with no IFV while the line hovered under six) — and hostile aircraft
(`AIR_THREAT_TYPES`: Rocketeer, Kirov, Harrier, Black Hawk, Hind,
Floating Disk) lift anti-air's demand to three, an enemy that
out-tanks the house lifts armor-crackers the same way — and a role
whose entries this house cannot field (country-locked types, a hero
past its `BuildLimit`) passes its turn to the next role behind, so the
queue never rests on a cameo that stays dark; when every share is met
the core keeps the queues rolling — the wave pipeline refills instead
of resting at a capped-out mix, and the miner target scales with the
standing docks so the refinery rule never starves itself. The
specialists the wheel fields hold the base for now — wave dispatch
names only the opening's core types, so specialists marching with the
attack waves is a follow-up. **The base
keeps expanding** past the fixed ladder (the earlier freeze: the ladder broke
on a live main-tank count of zero, so once the army switched to tier-2
tanks the base stopped growing forever): a growth tail queues whichever
limb runs furthest behind — power only on an actual deficit (the
margin rule claimed every growth slot under war damage: five plants
while refineries sat at two), war factories (the fourth is owed by
the ladder's last rung — as a growth entry it never once won a slot
from the power-deficit rule (observed), and the rung carries no wallet
bar for the same reason the build-out always had none: the ladder's
superweapon drains the bank and the army queues absorb the income
after; the item draws its cost as it builds the native way —
while the fifth and
sixth wait for a wallet over 1500 credits, capped at six by retail's
`MultipleFactory=0.8` shrinking returns), refineries while the miner
fleet can feed another dock (two miners each, capped at four) — and the expansion gate now counts the tier-2 tank
too. The radar climb is also side-scoped: the native `can_build`
answers Yes without prerequisites for a house no human controls, so
the earlier whole-`PrerequisiteRadar`-class scan let a Soviet or Yuri
house queue the Allied Airforce Command (its `Owner=` lists every
side); the candidates now start from the side's own radar. Three
retail regressions cover the slice: a Fair Battle driven until the
Weather Controller, four war factories, an IFV and a Guardian GI all
stand; a Russian house driven until its Soviet radar stands with zero
Allied radars in the base; and a superweapons-off Battle driven past
the battle lab with no superweapon ever built or granted.

The base-guns slice arms the perimeter with retail defense structures:
a defense limb between the ladder walk and the growth tail queues the
side's three guns — the basic pillbox up to a quota of two, anti-air
while the enemy actually fields aircraft (the same
`AIR_THREAT_TYPES` the roster wheel reads), and the heavy tower once
the side's own radar stands, capped at two each. The radar gate is
load-bearing: the native `can_build` answers Yes without prerequisites
for a house no human controls (the quirk that forced the radar
side-scoping), so the AI checks the radar's standing count itself
instead of trusting the cameo the Prism Tower and Tesla Coil would
light up early — their retail `Prerequisite=` chains name the radar.
Names come from retail INI, not guesswork: Allied Pillbox `GAPILL`,
Patriot `NASAM`, Prism Tower `ATESLA`; Soviet Sentry `NALASR`, Flak
Cannon `NAFLAK`, Tesla Coil `[TESLA]` (`NATSLA` is only its firing-anim
prefix); Yuri's Gatling Cannon `YAGGUN` serves both the pillbox and the
AA slots, the Psychic Tower `YAPSYT` waits behind `NAPSIS`. Guns ride
the same yard-centered ring scan as every other structure and the same
cameo admission, so a gun this house cannot field falls through to the
next limb — and each gun queues only from a wallet that can pay for it
outright: a broke base that queues a 1500-credit tower parks it in the
one building slot at zero progress forever (observed: factories stuck at
three while the Prism Tower held the queue for the rest of the game),
starving the growth tail behind it. Poor bases expand; rich bases arm.
But the full-cost gate alone can wait forever in a war-poor game: each
unit item banks only its own cost, and the hundred-credit soldier
ladder keeps the wallet pinned under the gun's price between ore dumps
(observed: whole games, zero gun orders). While fewer than two guns
stand, the unit queues hold for the next gun the limb itself would
claim — a savings hold that pools the flow past the gun's full cost on
the idle ticks between building items. The economy's miners and the
one-shot commissions are exempt: income first, existence second, the
floor third. The fair_growth regression now requires two perimeter guns
standing in its end census.

The income-recovery rule fixes a total freeze observed in live play:
after the first marches traded away, a counter-raid razed the
refineries — and from that moment nothing was ever built again. The
mechanism is a three-way wedge: the single structure queue held an
expensive climb piece (the 5000-credit Weather Controller) that the now
-zero income could never finish and nothing ever cancelled, so the
refinery rebuild never got the slot; and with the climb piece still in
the queue the climb's credit throttle held the barracks and war-factory
queues closed too. The rule: while no refinery stands, cancel every
queued structure that is not itself the economy (refinery, power
plant) — the native ABANDON (`HouseClass::Abandon_Production @
0x004FAA10`, issued as `Command::CancelProductionByType`). The freed
slot goes straight back to the ladder's refinery step (a standing-count
ladder re-enters razed steps by itself), and with the climb piece gone
the throttle reopens and the unit queues resume from banked credits
while the refinery rebuilds. The check runs ahead of the ladder walk on
purpose: the wedged state it fixes is exactly the one where the queue
is busy and the ladder never runs.

The heroes-and-air slice completes the roster the situation slice left
starved. **Rotation replaces first-fit** in the mix wheel's pick: the
deficit role's buildable entry with the fewest standing units claims the
slot. First-fit starved every entry listed behind a rival — a whole game
passed without Tanya while the cheaper Ghost and Sniper held her role's
slots — and plain rotation still starved her under attrition (ties break
by roster order, and every death reset the tie to the cheap entries at
the top), so each hero now leads his role in the table: the hero claims
the fresh slot and the rebuild after he falls, his retail `BuildLimit=1`
darkening his own cameo once he stands, and the cheaper entries rotate
behind him. Every entry whose retail `Prerequisite=` carries real weight
now carries it as a standing building gate (`needs`): the AI house's
`CanBuild` skips prerequisite chains (the quirk behind the radar
side-scoping), so the Mirage Tank, Battle Fortress, Tanya, Boris, Yuri
Prime, the Siege Chopper, the Kirov and the Floating Disk wait for their
battle labs here instead of fielding unearned tier-3 units, and the
Robot Tank waits for a Robot Control Center the ladder never builds (its
retail prerequisite names one; fielding robots without it produces inert
tanks). **Air power splits by its retail tab**: the Kirov `ZEP`, Siege
Chopper `SCHP` and Floating Disk `DISK` are `VehicleTypes` —
war-factory aircraft that ride the vehicle roster with a lab gate —
while only the Allied side owns an aircraft-tab factory
(`Factory=AircraftType` on `GAAIRC`/`AMRADR`, the same building the
ladder's radar step raises). The Allied wing tops up to four jets on its
own Aircraft-category queue (Korea's Black Eagle `BEAG` first, the
shared Intruder `ORCA` behind), sorted by the native `can_build` gate —
the cameo's draw brightness knows nothing of country locks, and a
cameo-sorted pick queued a Black Eagle an Americans house can never
produce, wedging the wing for a whole game (observed). Idle jets fly a
coordinated sortie against the top enemy priority target on a 900-tick
cooldown — they return, land and rearm natively while it holds. The
fair_growth census now requires a standing Tanya and at least one jet
alongside the earlier growth facts. Residuals: the specialist ground
units still do not march with attack waves (core-type dispatch only, as
before), and sorties' in-flight behavior under the command layer's
Attack order rides the native aircraft mission machinery untested until
the next live session.

The slot-priority slice answers the fair_growth census the engine fix
exposed (the end state was alive but unfinished: zero defenses, two
factories, no IFV, no Tanya, no jets, credits pinned near two). Three
rules move. **Guns before the ladder**: the defense limb now runs
ahead of the ladder walk, not behind it — the walk doubles as the
rebuild scan (a destroyed late rung re-arms every slot-free tick), so
in its old position the guns were dead code that never once ran. The
limb's own wallet gate already keeps a poor base expanding instead of
arming; the move only fixes who sees the slot first. **The superweapon
rung is once-per-game**: `sw_ever_stood` latches the first storm
standing and a razed one never rebuilds — the observed GAWEAT rebuild
loop held the one serialized structure slot to the end of the game and
starved both the guns and the expansion behind it. **Gun banking**:
while any gun below its cap is wanted (the limb's own conditions,
mirrored), the three unit queues bank their purchases the way a tech
climb does — each item waits for its own full cost — so the wallet
refills past the gun's full-cost gate instead of the queues drinking
every credit as it arrives. The hero commission completes it: once the
battle lab stands the hero takes the next infantry specialist slot
outright (`hero_issued` latches the first commission) instead of
waiting for a whole-role deficit window the rotation may never open.
The mix's own buildable test moves from the cameo to the native
`can_build` gate: the draw brightness knows nothing of country locks,
so a cameo-sorted pick handed an Americans house the Germans-only Tank
Destroyer — the item parked at zero progress forever and wedged the
vehicle queue for the rest of the game (observed).
The war-factory rally correction gains per-factory pacing: a desired
ring cell the base's own later buildings occupy (the crowded yard
ring once four factories and the guns stand) gets its SetRally
refused at apply time, and the unpaced re-issue loop — one of the
only early returns in the chain — then ate every command tick for
the rest of the game (observed: the freeze began as the ring filled;
every jet-commission precondition held and the branch below never
ran). One attempt per factory per window caps the wedge.
The deploy branch loses its unconditional empty return: with a
deployable MCV standing and the deploy attempts spent (or pacing
between retries) it returned nothing every tick, so a second AMCV
that appeared mid-run froze the whole command chain — 24000 ticks of
empty queue, unspent wallet and an unbuilt army while every
jet-commission precondition held (observed; pinned by the census
instrumentation: yard alive to the last tick, AMCV standing, freeze
from tick 17464). The MCV now waits and the chain runs on.
The ready-building placement branch gains a dropped-dispatch guard
of the wave branch's `launched`-list class: a `PlaceReadyBuilding`
order the command layer keeps dropping leaves its building ready
forever, and as the first return in the one-command chain it
re-issued every tick and starved every branch below — a whole
24000-tick stretch ran with an empty queue, a fat wallet and no
production at all (observed; the census instrumentation pinned it:
every jet-commission precondition held from tick 17464 yet the
branch never ran). Three strikes on the same still-ready type now
pause that type's placement for a while; the pause lifts and the
placement retries into a possibly-changed base.
The first jet gets the same existence commission ahead of the vehicle
branch: the air branch sits last in the one-command budget and its
banked gate lost every wallet race to the cheaper ground items (a
whole 42000-tick census passed at zero jets), so once the radar the
ladder raised stands as the Airforce Command the first jet queues
outright — keeping only the tech-climb bank — and top-ups stay in the
ordinary branch.
The zero-jet reading that motivated that commission was itself two
stacked counting blind spots, not an admission fault. Pad aircraft
file under their own `Aircraft` entity category — outside both the
census's Unit/Infantry pair and `count_combat_of_type`'s — so a living
wing read zero everywhere: the census could not see jets however many
stood, and the commission/top-up branches re-issued the first jet
forever. The per-type command histogram that looked like mass
admission refusal (`QueueProduction:ORCA` ≈ 17 700 against an empty
queue) was that re-issue running into the native pad limit: four
AMRADR docks banked at its grand opening, `PadAircraft=ORCA,BEAG`,
`pads >= docks` refuses the fifth — the same refusal an extra click
earns a human player. Both counters now read the wing through
`count_aircraft_of_type` (Aircraft category; the Rocketeer stays under
Infantry where the engine files it), the census counts jets the same
way, and fair_growth goes green with the wing standing, ending early
(~870 s of the 42000-tick budget). The census keeps the diagnostic
layer built on the way — per-type command tally, banked docks, a live
post-drive enqueue probe and the grand-opening trace — for the next
regression that needs them.

## Scouting

Retail never lifts its own shroud: the aimd corpus posts dogs as base
and refinery guards and its roaming "patrols" circle the base's own
buildings, so the scouting chain is fork-original. The opening table
carries each side's scout — the 200-credit dog (speed 8, sight 9) for
both army sides, Yuri's initiate (also sight 9) where no dog exists —
kept two alive through a reservation on the barracks rotation's
specialist slots (`can_build`-gated, the same country-lock lesson as
`army_pick`). An idle, ungated scout marches to the fog grid's nearest
never-revealed cell (ties broken by row-major scan order); the march
reveals as it goes, arrival posts it there, and the next due window
re-points it into what is still dark. The dispatch sits below the
combat branches — production owns the busy ticks, the dogs take the
spare ones — and the fair-growth census carries the proof: a
revealed-fraction floor on the end state.

## The fortress loading pair

The Battle Fortress is the Allied late-game OpenTopped transport — five
seats, its passengers firing from inside — and the one roster entry the
wave system never claims: neither it nor the infantry it wants sit in
the core-type sets, so both idle at home where the pairing finds them.
The vehicle rotation owes the base exactly one fortress, and the
commission is a dedicated existence branch, not a specialist-slot
reservation: a reservation starves exactly when it matters — a hot war
keeps the wheel from reaching its next specialist slot while the
barracks rotation drains the bank faster than a 2000-credit item can
gather — so the fortress takes the first free vehicle turn once the
battle lab stands (`can_build`-gated, the country-lock lesson again),
and once the QueueProduction order is out the factory charges
progressively; no banked-cost wait belongs in the branch. The same
treatment fields the AA insurance: one of the roster's AntiAir entries
once the tank core stands, so the side's mobile AA answer exists before
hostile air arrives. The boarding branch —
below the combat branches, above the scouts — sends the roster's
anti-armor infantry (the Guardian GI's deployed launcher, the classic
loadout; derived from the roster, not hardcoded) into the first
fortress with a free seat, as the same Enter click a player makes
(`Command::clicked_mission` with `MissionType::Enter`), one boarding
call per tick with the passenger's order gate pacing the walk. The
fair-growth census carries the proof: a fortress stood and a seat was
filled.

## Base expansion

Retail never expanded: its MCV teams regroup and deploy in place, and
its refineries ring one base-plan anchor. The fork grows outward in two
phases, both riding native machinery. When the home economy reaches its
design steady state, a one-shot survey walks the overlay grid (row-major,
strict `<`, richest cell wins) and latches an ore-field site past the
home ring-scan's reach; the growth tail's first limb then queues a
refinery there, and the ready-placement branch routes that one refinery
around the latched site instead of the yard. Everything after placement
is native — the refinery spawns its own FreeUnit miner beside it, miners
scan ore from their own cell and dock at the nearest free bay, and miner
production scales with the refinery count automatically. Placement
legality is cell-local (no base-radius rule anywhere in the build-site
predicates), so the far site needs only a clear foundation.

The second phase walks a base out: once the survey latches a far ore
field — the home economy at its design steady state — the vehicle
rotation owes the house one expansion
MCV — an existence commission like the fortress's, with the type
derived from the yard's own `UndeploysInto` (AMCV/SMCV/PCV); the
remote refinery and the MCV then advance in parallel on the building
and vehicle queues (observed the serial order starve: the MCV was
still charging its 3000 credits at the census budget's end in a
war-poor late game). Retail
gates every side's MCV behind its service depot (`Prerequisite=`:
AMCV on GADEPT, SMCV on NADEPT, PCV on the Grinder — YADEPT sits at
TechLevel 15, past any lobby), so the depot is a ladder rung of its
own, built with the late base; `can_build` refuses the MCV until it
stands. The routed remote refinery also carries a home fallback: a
far ore field whose rings hold no legal foundation must never wedge
the one building slot. The
opening deploy branch stands down once any yard stands, so the escort
branch owns the later MCV alone. Its walk aims at a legal yard
foundation beside the second survey's field (the first field excluded),
never the surveyed ore cell itself — tiberium-covered ground refuses
the unfolding, and an MCV loitering near the field spent its whole
attempt budget on misses (observed). `find_site_around` supplies the
foundation's origin and the walk targets its center cell — the deploy
tests the origin one cell up-left of the MCV — with exact arrival
required before a paced `DeployMcv` draws on a per-MCV attempt budget;
the opening's game-lifetime budget never sees a later MCV and must not
swallow its deploys. A foundation whose deploys the attempts cannot
buy abandons the plan; the aim latches on its first scan — a re-scanned
target drifts as the ore spreads and is never reached (observed: the
whole walk spent re-pointing between shifting foundations) — and the
deploy draws once the MCV stands on the latched cell or beside it, a
one-cell miss handing the finish to `try_to_deploy`'s own fallback
drive. The second yard keeps the home anchor for
placement (the substrate's stable order prefers the older yard), while
the native base centroid follows all owned buildings. The fair-growth
census carries the proof: a refinery stood at the expansion distance,
and two yards stood at once.

## Command-driven production publication

Upstream `44f14000a` routed automatic mobile delivery through the local
sidebar's strip visits (`publish_production_changes` serves only the
current house's visited factory links). A command-driven house owns no
strip, so its completed mobiles held their factories at Done forever and
wedged the whole house (observed on the merged tree: a fair tier froze at
one power plant, two refineries and one war factory with 17 700 credits
while `Vehicle:CMIN` and `Infantry:E1` heads sat complete for 10 000
ticks). The fork hook `publish_command_driven_production`
(`src/sim/production/production_queue.rs`, called from
`admit_frame_commands` next to the strip publication) re-issues the
pre-strip-scan unconditional publication — from Factory state, through the
same raw PLACE record — scoped to houses whose `HouseState::command_driven`
is set. Upstream houses keep the strip path untouched.

## Upstream sync

`superai/base` merges `origin/main` regularly; each pillar lands on its own
`superai/<pillar>` branch off `base`. Anything generally useful (bench
harness parts, command-layer hooks) is a candidate to PR back upstream;
difficulty knobs and strategies stay fork-only.
