# 超级AI (Super AI) — fork development plan

This branch group (`superai/*`) is a fork-only feature line: a command-layer
"super AI" player and a headless win-rate bench. The project's public name is
**超级AI / Super AI**; the in-game rows stay English (`Super AI`,
`Super AI MAX`) because the retail font carries no CJK glyphs. It tracks
upstream `YuriPlanet/vera20k` `main` and keeps its footprint outside
`src/sim/` (four documented exceptions, below). Licensed GPL like the rest of
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

The fourth exception is the DoList admission the second one rides: a
command-driven house issues player-style events, but `Execute_DoList`
(`0x0064C4FB..0x0064C50D`, `HouseState::event_dispatch_eligible`) admits a
house only when it is human or player-controlled. Native never queues
player events for a computer house, so the refusal changed no
native-reachable state — but on this fork it left every Super-AI order
admitted and recorded yet never dispatched: whole matches where six MAX
houses each logged six `DeployMcv` and nothing else, the MCVs never
twitching (the frozen-opening replay and the headless XTerritor regression
test `unlimited_team_houses_open_without_starting_units` reproduce it down
to the entity ids). The fix admits `command_driven` houses alongside human
ones; the launch-flag, the DoList widening and this test are one exception
surface.

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

Three live-play wedges the first MAX matches exposed, all strategy-side.
The order chains' combat universe was the core three types only: every
specialist the roster mix built — heroes included — was invisible to
the guard-post and wave chains, orderless for life where it rolled out,
jamming the factory door (which in turn backed production up behind
it); the chains now carry the whole roster (`combat_roster`). The air
wing's top-up lived behind a both-ground-queues-busy window it lost
every tick to cheaper re-arming ground items, so past the first
commissioned jet the wing never grew; top-ups now ride the same
commission branch on the wing's own aircraft queue — but the base
outranks the wing: while the second yard is still owed, the top-ups
wait (a full wing once ate every wallet peak the remote refinery and
the MCV needed, and stood over a second base that never came). And the expansion's
ore-field scan kept only the richest cell, distance-blind — on a big
community map every house marched its MCV at the same central
mega-field across hostile ground ("the AI charges with its base"); the
scan now scores value per distance walked, and the winner must host the
following structure's foundation — the same placement predicate the
deploy chain uses — so a field with no room beside it never wins and the
walk aim never falls back to the ore cell itself, whose ground refuses
the unfolding (that fallback cost a whole match's second yard). The
foundation must also keep the expansion distance from home on its own:
a field at the survey's minimum can hand back a foundation the ring
scan pulled toward home, and the refinery then lands inside the very
distance the second-base latch demands (that off-by-radius cost another
match's second yard). Known
gap, deliberate for
now: the wave and superweapon targeting read the raw entity set — no
shroud filter — so the AI aim is map-transparent; a vision-aware
targeting pass needs the scouting chain strengthened first, or the AI
goes passive the moment nothing stands revealed.

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
progressively; no banked-cost wait belongs in the branch. The boarding
branch —
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

## Power management

The growth tail alone owned power, and it ran last: the gun limb and the
ladder claimed the empty building slot ahead of the deficit rule, so a
defended base chained one Powered gun after another into a deepening
brownout — guns that stand offline in low power defend nothing — and by
the time the deficit rule finally ran, the blackout had already slowed
every factory through the native power factor (observed live: blackouts
mid-game with plants still owed behind the gun ring). The building
block's entrance now hoists one grid reading and preempts: the margin
prices the production queue's unlanded drain in (`pending_structure_drain`
— drain bites at placement while a queued plant pays nothing back until
it stands), and a projected deficit queues the plant before any gun,
ladder rung or growth limb may spend the slot. The gun limb additionally
holds while the grid is actually red, and `POWER_CAP` rose from eight to
sixteen: a two-yard base's drain (robot control −100, repair bay −200,
the lab-and-radar climb, the gun ring) runs past eight Allied plants'
1600 output, and the old cap pinned late-game grids into a permanent
brownout. Damaged plants never need their own rule — output is already
health-scaled, so the deficit reacts to a wounded grid on its own, and
the repair limb's economy ranking covers the healing.

## The base alarm

Retail answers a raid with `TechnoClass__RespondToBaseAttack`
(`0x00708080`) — a per-victim call that recruits nearby idle units — but
nothing ever recalled the garrison when the army was away: a base whose
wave was out fighting waved the enemy through (observed live: razed
yards, no reaction). The fork reads the native damage admission stamp
the engine already maintains (`HouseStrategyEmergencyState`'s
`last_building_attack_frame`, written by every building damage prelude,
in session ticks) and arms a stance while the stamp is fresh: the focus
latches on the intruder nearest any yard at a 24-cell alarm radius —
stand-off fire sits outside the idle perimeter's 16 — or the
worst-wounded structure when no intruder stands visible; a due idle unit
attacks the intruder outright, and short of a four-defender commit floor
the nearest posted unit leaves its guard post and attack-moves on the
focus, fighting whatever it meets on the way. The stance holds 450 ticks
past its last refresh for the mop-up, then the defenders return to the
idle flow and the posts re-form. Only a fresh stamp holds the next wave
home — the mop-up hold alone must not, or a raided base on a crowded big
map never attacks again (observed live: a besieged base that stopped
attacking for good).

## Expansion production

A sub-base fields its own army and miners: the house's queues are
shared, but a unit exits the factory that builds it — without a barracks
and a war factory in the expansion's ring, every soldier and every
miner the expansion is owed rolls out of the home base and walks the
whole map first (observed live: the expansion's miners crossing two
bridges to reach its dock). The cluster rules owe each yard's ring one
barracks and one war factory (the factory inside the house-wide cap),
routed by the placement intent. The barracks rule carries the loop
lesson: a cluster item whose expansion ring refuses every site falls
back home at placement, the count near the expansion stays at zero, and
a rule without a house-wide bound re-queues the same building forever
(observed live: a Yuri base that did nothing but chain barracks, the
loop starving the building slot the factory growth needed). Two stands
is the bound — the ladder's home barracks plus one more wherever the
item landed; a razed one rebuilds.

## The navy

The navy was the last absent domain. Three pieces: the naval survey
latches the one legal shoreline cell for the side's yard
(`GAYARD`/`NAYARD`/`YAYARD`, `Naval=yes` — a WaterSet tile inside the
base's own build area) by scanning capped rings around every standing
yard with the same admission placement runs, paced and budgeted so a
landlocked base abandons the navy after three surveys instead of
wedging a ready shipyard that can never land; the growth tail's lowest
rung queues the yard once a cell stands latched, and the latched cell
places the ready yard — no search at placement time. The hull line
rides the vehicle queue after the miner line: the side's basic hull
(Destroyer, Typhoon, Boomer) to a quota of three, the AA escort (Aegis,
Squid) once the radar stands and its extra prerequisite honestly passes
`can_build`. The
hulls join the idle and standing pools by name — the land rosters never
listed them — so the idle perimeter's Attack answers coast raiders; the
wave's zone gate keeps them out of ground marches on its own, and the
guard-post limb filters them back out, a ship holding no land ring. The
offensive half arrived with the arsenal below: the sortie hunts fleets
and shores, and the amphibious transports ferry the foot army.

## The full arsenal

The unit tables closed their gaps against the rules corpus wholesale.
The engineer pick was a shared `ENGINEER` name, but the three sides'
engineers are three rules sections split by `ForbiddenHouses` — the
bridge and repair commissions silently produced nobody outside an
Allied base until the `Opening` carried its own (`ENGINEER`,
`SENGINEER`, `YENGINEER`). The roster additions ride `can_build`'s
country gate as every country-locked entry already did: the Chrono
Legionnaire behind the Allied lab, the Cuban Terrorist and the Libyan
Demolition Truck on their countries' slots, a Yuri hound. The navy
gained a second hull line (Dolphin, Giant Squid) past the basic quota
and a lab-gated capital (Carrier, Dreadnought) — and the Soviet AA
escort became the Sea Scorpion it always should have been: the Squid
is an anti-hull grappler, not a flak platform, and now rotates in the
hull line instead. The amphibious transports (Landing Craft, Armored
Transport, Hover Transport) commission once behind the naval yard and
run a ferry — beach at home, board idle infantry with the engine's
own Enter click, plain-Move to the enemy's aim (the cargo is the
payload; a fight on the way must not sink the delivery), unload with
the Unload mission, one leg per window. The Allies carry a second
ferry hull the shoreline does not gate: the Night Hawk
(`[VehicleTypes]`, so the vehicle queue fields it) rides `Fly` rows
where the whole map is one connected zone, so an inland Allied base —
no naval yard, no Landing Craft — still runs the ferry line off its
war factory; the two hulls share the single transport latch, and the
naval commission outranks the air one, so a shoreline base keeps the
twelve-seat craft.

The last two gaps closed with it. The stolen-tech trio — Chrono
Commando, Chrono Ivan, Psi-Corp Trooper — rides every side's infantry
table with no prerequisite of its own: retail gates each behind
`RequiresStolen*=yes`, `can_build` checks the house's stolen-tech byte,
and the spy's battle-lab infiltration is what flips it, so the rows sit
dark until a theft lands and army_pick's buildable filter passes them
over without spending a slot until then. What stays out is out for a
reason: the Cosmonaut needs a lobby past the standard tech ceiling, the
Slave Miner's ore line is the refinery's own five slaves already, and
Yuri's second hound is a twin of the one already fielded.

## Prerequisites the AI house skips

The engine grades `CanBuild` differently for a house no human controls:
the `Prerequisite=` chain is skipped outright, while `RequiredHouses`/
`ForbiddenHouses`, the `RequiresStolen*` bytes and the tech-level
ceiling against the lobby cap all still bite. The gap is real — a row
whose retail prerequisite rises above its production factory (a radar
over a war factory or barracks, a battle lab over a naval yard) would
field from a bare factory. The rule the tables follow: every such row
carries the gate itself — the roster's `needs` names the building and
the rotation drops the row until it stands, the naval pick checks the
radar or the battle lab before the escort, the second hull or the
Boomer claims a slot. This round closed the last thirteen roster rows
(TNKD, JUMPJET, GHOST, SNIPE on `GAAIRC`; TTNK, V3, FLAKT, IVAN, DESO,
TERROR on `NARADR`; TELE, VIRUS, YURI on `NAPSIS`) plus the naval
candidates (Aegis and Sea Scorpion behind the radar, Dolphin and Squid
behind the lab, and the Boomer's whole navy line resting until Yuri's
Psychic Sensor stands). Country locks stay with the engine — `can_build`
grades them for AI houses too — and the Allied jet commission already
waited for the radar before this round; nothing there needed a change.

## The second superweapon

Each side's ladder carried exactly one super rung — the Soviet slot
held the Iron Curtain, Yuri's the Dominator — so the Nuclear Silo and
the Genetic Mutator never entered any queue (observed live: "where is
the nuke"). The `Opening` now carries a second pair — the Silo, the
Mutator, the Chronosphere — queued once the first super has stood
(its own once-per-game latch, so the two monuments never race for the
one building slot), and the fire walk launches whichever special
charges first: the mutator aims at the hostile infantry centroid, the
rest at the generic hostile aim.

## The per-tick census

The ladder's structure counts re-walked the whole entity set — with a
`String` per own building — on every read, thirty-plus reads per
orders pass per AI per tick: a fixed tax that scaled with the map's
entity count and showed up as frame cost on the 512-cell fields. The
first read of a pass now rebuilds one census keyed by type id; the
rest of the pass reads the map. `orders` only reads the sim and
issues commands, so a census latched to the pass's own tick is exact
for every read inside it.

## Walls and the defense extensions

Each side's `Wall=yes` segment — the Allied and Soviet walls, and
Yuri's Citadel Wall (`GAFWLL`, actively registered at BuildingTypes
312 despite the old French-wall comment beside it) — queues one
100-credit cycle at a time and places one cell per cycle around the
first superweapon, then the second, then the yard; the engine's own
auto-fill closes the spans toward standing segments for free. The
walk skips cells the live overlay grid already holds a wall on,
retires a spent ring once and for all (a fallen segment is never
rebuilt), and cancels a stranded ready segment rather than wedging
the one placement slot. The gap generator and the French Grand Cannon
ride the gun limb's queue step behind the core lines — the generator
behind the Allied battle lab (its shroud bubble is fully native), the
cannon behind the radar and a live French-country check, because
`RequiredHouses` would refuse every other Allied house forever and a
refused item parked in the one building slot is the barracks lesson
again. The blackout gate covers both for free: they stand offline
below full power, same as a coil.

## The engineer's war

Past the bridge, the engineers earn their keep three ways. A neutral-
owned tech building is free money — the derrick's entry bonus alone
pays the engineer twice over — so a windowed limb walks the nearest
revealed one (derricks first) with the engine's own capture click, one
engineer per building (see "Neutral tech capture" below), topping the
stock to two through the commission core the bridge chain
shares. The ride raid: an engineer boards the side's land transport
(the IFV, the Flak Track; Yuri fields none), attack-moves at a
latched enemy priority structure, unloads beside it and walks the
capture in — the stage machine marches one command a tick, each leg
re-issued only when its actor stands idle again, every stage on a
hard clock sized for a cross-map sail, every lost actor into the
shared cooldown. The spy retargets enemy superweapons first (the
charge reset), then radars, then power plants — the neutral tech
tour only when no revealed enemy aim stands, since a spy's arrival
on a neutral building pays nothing.

## Sentries, sorties and stances

Every unit keeps its own war. The dogs: the roster fields a hound per
side (`DetectDisguise=yes` on all three) and the wave dispatch skips
the type outright — a hound marching with the tanks is a dead 200
credits at the first vehicle line — while a sentry limb posts two
per standing yard on the ring; the Guard post's own leash, chase and
spy-bite are the engine's, and a recalled or killed sentry frees its
slot to a fresh one by mission state alone. The sortie: the idle
pools held the hulls for life, so an episode arms like a wave — two
idle hulls minimum, the first held home for the defense recall — and
dispatches one hull per tick at the latched aim, the nearest enemy
hull the sailer's own water rows reach, else the nearest shore
structure. The Desolator: the generic home-deploy skips the type (an
unconditional rad field at the base door burns the house's own
feet); the engagement rule arms it instead — three enemy foot units
inside five cells buy the one-time toggle, and the field decays on
its own when the assault is gone.

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

## Base livability fixes: site spacing, the stuck rescue, the radar alias, the wall cooldown

Four live-play wedges, one theme: an AI base that could quietly stop
functioning. (1) The site scans (`find_site_around_capped`,
`find_site_beyond` in `src/super_ai.rs`) now keep a one-cell Chebyshev gap
between foundations and demand a two-cell clear ring around factories,
barracks and naval yards — the old first-legal-cell pick packed bases so
tight that factory exits sealed and finished units held their factories at
Done, stalling the queue; a packed base where no spaced candidate exists
degrades one pass to the original predicate, because a tight base beats a
frozen build chain. (2) A combat unit whose pathing burns out stood idle
forever; a paced scan (`stuck_since`, `STUCK_TIMEOUT`) re-issues one
AttackMove per window for a unit still on its snapshot cell, skipping
units other chains own. (3) The `needs` gates read the Allied radar pair
through an alias — GAAIRC is retail-forbidden to the Americans country,
whose buildable twin is AMRADR, so a literal-only read kept an American
ally's TNKD/JUMPJET/GHOST/SNIPE and its naval AA escort shut for whole
games; the locked `needs` table values are unchanged, only the read. (4)
The wall ring's refusal loop (refuse, pause, re-open the same ring, burn
another 100-credit segment) drains: the ready segment's cancel escape is
reachable even while its placement is paused, and an exhausted escape pass
rests the whole target roster for `WALL_TARGET_COOLDOWN_TICKS` instead of
re-queueing into the same refusals.

## The wall guarantee window and the outpost production wheel

Two starvations the two-pass read of a live game caught. (1) The wall
walk sat at the building chain's tail behind the ladder and the gun
rebuilds and drew ~3 turns per 10000 ticks against ~170 segments of
ring theory, with `guns_settled` false through any live war closing
the queue door outright — walls were silently off exactly when they
mattered. The limb now sits ahead of the gun queue and the ladder, and
a guarantee window paces it: at quota the old always-on right of way
holds, otherwise `WALL_WINDOW_TICKS` (600, rearmed on the issued
order) buys the ring one segment per window — a ~56-tick build per
span, ~9% of the building slot, at most one build time off the
ladder's own rung. (2) Layer 2.6 raised the expansion's own war
factory, but the engine's one production line per category still rode
a single carrier (`find_factory` prefers the flagged primary), so
every rollout left home. The primary wheel (`TogglePrimaryFactory`,
unused until now) alternates the carrier every
`PRIMARY_TOGGLE_EVERY_VEHICLES` issued slots by reading the live
primary flags — no direction bit; the first factory raised carries the
lifecycle's flag, each toggle hands the line to the other factory and
the flags carry the alternation on their own. The rally limb anchors
each factory on the nearest standing yard instead of the first yard's
ring (a pure `rally_anchor` picks per factory, Chebyshev), and the ore
survey adds a one-point enemy-direction tie-break
(`enemy_direction_bias`): a field within 5/4 of home's span to the
enemy centroid gains the tie, a field behind home's back loses it —
ore direction is not enemy direction, and an outpost that mines
backward arms a base that never sees the war, but the bias only flips
ties so the richness term keeps its voice.

## The queue retry gate and the six-tenths bank

Two dead-weight drains the replay census caught: the engine refuses a
building `PRODUCE` against a busy yard without surfacing the refusal, so
the building limbs re-filed the identical dead command every tick
(~12 000 a game, 91-99% of ticks spinning) and every building queue site
now paces its type through `BUILD_QUEUE_RETRY_TICKS` (150); and the
tech-climb bank's full-price bar shared one wallet between the climb and
the army — across an Allied superweapon's ~2000-tick savings window the
unit output fell to 52% and three houses reached zero — so
`tech_bank_short` queues the unit lines once six tenths of the item's
cost sits in the bank.

## The siege flyer wings

The war-factory flyers never flew. The Kirov and the Floating Disk are
`[VehicleTypes]` rows with `MovementZone=Fly` — the vehicle queue builds
them like any tank, but the specialist wheel's siege slot is one turn per
`SPECIALIST_EVERY_VEHICLE` shared with every other Siege row, so a hot war
fielded launchers and choppers forever and the flyer never lifted. The
opening tables now carry the flyer wings: `siege_flyer` with a quota —
three Kirovs (`ZEP`) for the Soviets, three Floating Disks (`DISK`) for
Yuri, the Allies `None` (their heavy air rides the aircraft tab, whose
wing grew to six jets to carry the same weight) — and a second line
beside the primary: the Siege Chopper (`SCHP`) fields two behind the
Kirovs. The commission tops each line up to its target once the battle
lab stands — live plus in-production counted, the six-tenths bank the
one-off commission flew behind, the per-type retry gate pacing the
re-file — and replenishment is the same check: a lost Kirov's count drops
under the quota and the next window buys the replacement, a wing the
whole siege plan rests on that never rests at one per game.

Two flight limbs read the movement zone beside the class: the air wing's
sortie fields the flyer at the enemy's priority structure (the old filter
read `EntityCategory::Aircraft` alone, and a Unit-category flyer is never
in that class), and the guard-post limb spares it the yard ring — a Kirov
holding a sentry post wastes its siege sortie on a job the guns already
do. The shared `is_fly_type` answer parses `MovementZone=Fly` off the
rules row, which also covers the Night Hawk — and keeps the whole wing
out of the ground waves, where the sortie limb owns it.

## The AA answer: mobile quota, tiered towers, bunkers

Three answers to the air war, all count-driven and read every pass. The
mobile quota fields the roster's AntiAir vehicles to `AA_UNIT_FLOOR` (2)
once the tank core stands — the pair exists before the first hostile
aircraft does — one more per two hostile aircraft to `AA_UNIT_CAP` (5):
live hulls plus in-production counted, per-type retry gate pacing the
re-file, full price (300-600 credit hulls are too cheap to bank for).
The specialist wheel's base share rose with it (`ROLE_SHARE_BASE` 1→2),
so a standing AA pair rides every mix. The tower ring tiers with the air
war per standing yard — `aa_per_yard_quota` reads 1 in peace, 2 once
hostile air fields, 3 against a real air force (`AA_HEAVY_AIR_THREATS`)
— with the house-wide bound at three times `AA_DEFENSE_CAP`; the old
`enemy_air_count > 0` gate is gone (peace keeps its baseline ring). The
heavy tower doubles per yard (`DEFENSE_PER_YARD_HEAVY` 2), its house-wide
bound four times the cap plus the late-sink tier. And each side's bunker
joins the gun ring: one per yard, `BUNKER_DEFENSE_CAP` (3) to a house —
the Soviet Battle Bunker (`NABNKR`, five soldiers behind `CanBeOccupied`)
and Yuri's Tank Bunker (`NATBNK`, one vehicle behind `Bunker=yes`; the
Allies field none). The `defense_bunker` table entry carries the retail
prerequisites' evidence in the section comments; the quota row sleeps
where the entry is `None`.

## The garrison limb: fill to capacity

The engine's `CanBeOccupied` machinery stood unused by the AI. The
garrison limb (Layer 3, above the guard poster) fills the nearest
revealed neutral occupiable civilians — two at a time, health above
`ConditionRed`, passive-owner checked like every neutral scan — and the
side's own occupiable bunkers to the type's full `MaxNumberOccupants`,
read off the rules row at order time (civilians 10, the Battle Bunker 5):
占满, never a token pair. The infantry are the retail `Occupier=yes` rows
(GI, Conscript, Initiate — the engineers, the spy, the hero and the
hounds never qualify), idle-class, free of the boarding, raid, capture
and wave chains; the nearest soldier takes the nearest open slot through
the engine's own admission (`can_enter_transport`, the garrison click's
pair check) and the capture click — the same order a player's garrison
click sends. The `(building, soldier)` inbound pairs count against the
capacity until he lands (passengers ride in limbo and the occupant count
carries him), dies or the target changes hands, so a building is never
over-ordered and a full one rests. The deficit feeds production: unfilled
slots across the targets latch a demand the infantry wheel queues ahead
of its mix — the side's basic soldier (the same three rows), at most
`GARRISON_DEMAND_IN_PRODUCTION` (2) in the pipe, full price. Yuri's Tank
Bunker takes its one ride the same way: an empty `Bunker=yes` hole draws
the nearest idle armed turret vehicle on a ground locomotor (never the
miners, the MCVs, the ferry hulls, the fortress, the naval line, the
flyers or the AA roster), boarded with the fortress Enter click, one ride
per bunker through the pinned bound pair.

Three guards keep the limb from wedging itself. A walker whose click was
dropped or refused stands idle once his order gate lapses — the walk runs
the Capture or Enter mission, never an idle one — and the pair frees his
slot instead of holding the building shut for the rest of the game. Every
pick is zone-gated (a soldier or a tank across a breach never lands), and
a civilian roof the side's soldier cannot walk to from the yard never
becomes a target, so it latches no demand the barracks could never fill.
And the garrisoned pieces stay parked: a roof's occupant or any carrier's
passenger (riding in limbo) and a tank bunker's installed driver are out
of the idle and standing pools and the stuck rescue, so neither the guard
poster, the perimeter attack, the alarm, the waves nor the rescue pulls
them back out — a bunker emptied by the poster would draw the next tank
and churn forever.

## The wave system: retail compositions

The offense marches the aimd team scripts now. The `Opening` carries each
side's waves in three phases — before the battle lab, with the lab, under
the late sink — and each phase lists its sibling teams (retail's
equal-weight teams for the same trigger), taken in turn by successive
episodes; a sibling whose lead the house cannot field yet (the V3 before
its radar) yields its turn to the phase's first team. Soviets: 3 Rhinos +
2 Flak Tracks with 8 Conscripts; behind the lab 5 Rhinos + 2 Flak Tracks
or the V3 bombardment (2 V3s + 2 Flak Tracks), either with 10; under the
sink 4 Apocalypses + 2 Flak Tracks with 20 Conscripts or the mixed squad
of 2 Flak Troopers, 10 Conscripts and 3 Tesla Troopers, a four-drone
screen trailing if it stands. Allies: 3 Grizzlies with a 2 GI + 2
Guardian pair, then 4 Grizzlies + Mirage + Prism with 3 + 3 and two jets
paired on, then the ultra 6 Grizzlies + 3 Prisms + 4 IFVs with 5 + 5 and
two jets again (whichever jet the country fields — Korea's Black Eagle,
everyone else's Intruder — drawn only from a wing standing at
`AIR_WING_TARGET`). Yuri: 3 Lashers + 1 Gattling with 4 Initiates, then
5 + 2 with 5, then 6 + 3 + 2 Master Minds with 10 and a Clone. One wave is
alive at a time (retail `Max=1`), the next decision
`WAVE_CADENCE_TICKS` (2000 — retail TeamDelays) after the last launch:
the plan latches, the members gather at their posts while the production
feed queues the missing core types ahead of the wheels (a wave short of
its Rhinos does not spend the factory on specialists), the form clock
launches a thinner wave when the queues cannot fill it, and the launch
marches the members one per tick at the gather cell —
`WAVE_GATHER_DISTANCE` (20, retail AISafeDistance) short of the enemy
base toward home, the retail regroup. Arrival turns the stage to the
strike: the latched sequence runs the script's quarry order — factories,
then defenses (`WAVE_DEFENSE_TYPES`), then every other structure, siege
units bombarding from standoff — and the empty sequence is the hunt,
attack-moves at the hostile mass center's spread ring. The stage clocks
(form 1500, march 1800, strike 3000 per head) bound every wedge the way
the old dispatch bounds bounded a dropped order; the alarm still holds a
forming wave home, and the flyer wings stay out of the ground march — the
escort the plan pairs on joins at the strike with the Attack click. The
expansion MCV, the miner line and the naval line keep their turns ahead
of the wave feed in the vehicle chain; the primary wheel, the rallies and
the stuck rescue are untouched (the rescue parks a marching wave's
members — the regroup is the march's own wait).

## Counter triggers

The aimd counter triggers, read on the stuck scan's cadence and latched
into two flags the limbs consult. Six hostile aircraft arm the AA defense
team: the mobile-AA quota's floor lifts to four hulls
(`AA_COUNTER_VEHICLE_FLOOR`, the cap still holding) and the AntiAir
role's share to six (`AA_COUNTER_ROLE_SHARE`) — the retail
six-Flak-Trooper answer, fielded through whichever rows the side's roster
names (Flak Trooper and Flak Track for the Soviets, Gattling Tank for
Yuri, Rocketeer and IFV for the Allies). Two hostile Floating Disks or
Kirovs arm the anti-floater guard: the AA vehicles post over the nearest
own power plant or refinery instead of the yard ring (the floaters raid
the economy, so the flak stands over it) and sit the waves out. And a
revealed enemy Nuclear Silo or Weather Controller leads the next wave's
strike sequence — the priority strike the retail counter table orders.

## Neutral tech capture: one engineer per building

The tech chain walks the engine's `[AI] NeutralTechBuildings=` set
(`TECH_BUILDING_TYPES`, retail `CAAIRP,CATHOSP,CAOILD,CAOUTP,CAMACH,CAPOWR`)
in its own preference order (`CAPTURE_PRIORITY_TYPES`): the oil derrick
first — its `ProduceCashStartup` pays the engineer back on entry — then
the airport, the machine shop, the hospital and the rest, nearest the home
yard within a class. Candidates are revealed, alive and held by a passive
(`MultiplayPassive`) house, never already ours. Each building draws one
engineer: the `capture_bound` pair (engineer to building) is the
building's latch, so a building some live walker already heads for is not
a candidate for the next one — the old survey re-picked the nearest
derrick every window and sent the second engineer after the first. A
walker lost short of a building that still stands neutral rests it for
`CAPTURE_RETRY_TICKS` (1800, four capture windows) before the next
engineer tries, so a derrick under a hostile gun cannot eat the stock one
walker per window; a success flips the owner and the building leaves the
survey on its own. A bound engineer whose click the command layer dropped
stands idle again and is re-pointed at his own building first (entity
order, never the bound map's — the determinism contract). The rest is the
existing machinery: the capture click (`Command::CaptureBuilding`), the
`ENGINEER_CAPTURE_WINDOW_TICKS` (450) pacing, the commission core the
bridge chain shares, capped at `CAPTURE_ENGINEER_STOCK` (2) standing plus
queued behind the bridge chain's wallet bar.

## Survival without a yard: the pinned opening

The "green freeze": two owners in one replay went 100% silent from tick
~15215 and ~15334 while their units and factories still stood. The cause
was the chain's first gate — `orders()` resolved the side's `Opening`
through a standing construction yard every tick and returned nothing
without one, so a base whose last yard fell lost every limb at once. The
resolution is now pinned on first sight (`pinned_opening`), and so is the
home yard's cell (`pinned_home`) while a yard stands: after the last yard
falls, every non-building limb — the wave machine, the rescue, the
garrison, the sortie, the spy, the hero, the raiders, the guard poster —
keeps speaking through the pinned table and the pinned center. The wave
machine is decoupled from the yard the same way: the combat block's gate
is the home center with the pin as its fallback, not the yard's own
structure center. The building limbs need no gate of their own: the
ladder's first read is the yard's structure center, the placement branch
has nothing ready, the unit queues starve out on their own.

## The wave launch: the alarm's grace, the validated gather, the two-thirds regroup

Four reasons one replay logged 8275 ticks with zero launches while the
army stood complete. (1) A fresh base alarm held a forming wave home with
no timeout at all; under constant alarms it never launched. The alarm may
now delay the launch, never cancel it: past `WAVE_FORM_TIMEOUT_TICKS`
(1500) plus `WAVE_ALARM_GRACE_TICKS` (900) the thinner wave launches under
the alarm with what stands. (2) The demand feed (`wave_shortage`) answered
the first short type only, and when the house could not build it the
whole feed parked for the form clock; the walk now covers every short type
in turn and answers the first buildable one (`can_build`, the country-lock
lesson), one command per tick as before. (3) The gather cell was pure
arithmetic — the aim pulled back `WAVE_GATHER_DISTANCE` with no
validation, so it latched water, cliffs and one-cell passes, and every
member marched at that one cell. `valid_gather_cell` steps from the aim
toward home until a cell both connects to home in the ground zone and
passes the openness floor (`cell_openness`, `OPEN_NEIGHBOR_FLOOR` — at
least three of the eight neighbors connected and free of structures and
terrain objects, the choke-avoid test), and the march dispatch spreads the
members over `SPREAD_OFFSETS` around it the way the hunt and the rescue
already do. (4) The regroup required every ground member inside the
arrival ring, so one zone-skipped straggler burned the full 1800-tick
march clock on most episodes; `WAVE_REGROUP_NUMERATOR`/`DENOMINATOR` (2/3)
of the ground members arrived turns the march, the clock still the bound.
The rescue's exemption narrows to match: a marching member is parked only
while he still holds a live destination (`navigation.nav_com()`,
`motion_query::is_moving`) or stands inside the arrival ring; an idle
member far from the gather leaves `wave_marched` so the dispatch loop
re-sends him instead of standing exempt for the whole clock.

## The traffic limb: congestion, the engine's regroup, the rescue rework

The 100-unit pileup. Engine facts, verified read-only: every construction
yard deploy moves the house's base centre and orders every ground unit —
super AI houses included — to one regroup cell beside it
(`world_spawn.rs` → `MoveGroundForcesToBase`); a core path failure walks
the unit home; head-on blockers stop both streams; `Command::Scatter` is
refused for a unit holding a destination and moves only parked units to
the nearest free cell. The AI can read a unit's destination
(`navigation.nav_com()`) and motion (`motion_query::is_moving`), unused
until now.

The congestion limb (`congestion_orders`, every `CONGESTION_SCAN_TICKS`,
ahead of every dispatch in the combat block) snapshots every own ground
unit's cell and detects a cluster: `CONGESTION_CLUSTER_MIN` (8) own ground
units with no displacement over the window standing within
`CONGESTION_CLUSTER_RADIUS` (5) of one of them. On a cluster the parked
members (no live destination — posted units included) scatter, and the
members still holding a destination step to the nearest open cell on
their own side — strictly closer to home than where they stand, so the
stream backs out of the corridor instead of pushing through it; all in
one pass (multiple envelopes per tick, the sortie limb's precedent). The
cluster's center latches into `congestion_center`, and while it stands
detected the wave march dispatch, the rescue and the alarm recall hold
their dispatch into it; a later scan showing it cleared releases them.
The engine's regroup gets its counter: the newest yard pins as
`engine_home` (the engine's own base centre), the guard poster biases its
home ring to it so the AI's streams and the engine's agree instead of
crossing (the other yard keeps the expansion ring), and a yard count that
rose this pass — the regroup having fired — re-posts every unit still
aiming at the regroup cell from past `ENGINE_REGROUP_FAR_CELLS` (10) on
its own side (`engine_regroup_reposts`, capped at eight).

The rescue reworked: self-rescheduling (`rescue_due`) instead of
`tick % 150`; up to `RESCUE_BATCH` (3) rescues per scan in one pass; a
candidate whose aim or futility check fails keeps its snapshot and the
next candidate takes the turn — the old limb popped its pick before the
checks and lost the window on the first failure. The exemptions grow by
the garrison walkers (`garrison_inbound`, `bunker_bound`), the capture
walkers (`capture_bound`) and the units on their own Guard post; the scout
exemption narrows to the ids the scouting chain actually dispatched
(`scout_chain`) — Yuri's scout is the Initiate, and the type test exempted
every Initiate in the army. The `raiders` set was push-only; a raider idle
again with his gate lapsed is released, as the doc always promised. Guard
posts gain the openness test (`pick_guard_post` prefers an open connected
cell, the anti-floater anchor prefers an open economy structure), a posted
unit inside a detected cluster scatters with it, and the engineer's
tech-capture survey and dispatch gain the zone-connectivity check (a
derrick across a breach is no candidate) and a futility backoff on the
re-send (`FutilityKind::Capture`).

## Counting only what stands

The replay's queue storms had one root: `own_structures` and the census
behind `count_structures_of_type` counted buildings still in the factory —
queued, building, or ready and unplaced, every one in limbo. An airfield
queued at 4619 lit the jet commission 271 ticks before it landed and the
wing re-queued 267 times; a fortress Enter clicked one tick after the
fortress was queued; rally points went out 600 ticks before the first
factory. Both reads now filter `in_limbo()`, and the audit of the callers:
the jet gate's radar read, the rallies' factory list, the fortress and
ferry commissions, the `expansion_owed` yard count, the ladder's every
rung, the per-base gun quotas and `structure_center` (a limbo building sat
at (0,0) and could anchor a placement scan there) all read placed
buildings only — which is what each of them meant. The jet limb also gains
the building-queue retry gate it lacked (`build_queue_retry_open`), so a
refused airframe PRODUCE re-files at `BUILD_QUEUE_RETRY_TICKS` pace.

Three more replay bugs: the expansion-center distance took the absolute
value of the max instead of the y difference, so a yard north of its
field was mis-judged and one owner's wheel toggled between two home
factories (fixed, both sites); the Battle Fortress was boarded all game
and never once unloaded — fifteen Enter clicks, zero `UnloadPassengers`
anywhere — so the unload leg discharges a loaded fortress that reached the
enemy's side of the map (the ferry's discharge test) or stood idle loaded
past `FORTRESS_UNLOAD_IDLE_TICKS` (300); and one owner's expansion MCV
walked ~100 cells into enemy ground and never deployed, so the ore survey
is bounded by `EXPANSION_MAX_SITE_DISTANCE` (70) and a walking MCV that
stands still past `MCV_STUCK_TICKS` with its aim still far drops its
latched site for a paced, nearer re-survey.

## The outpost's production line (P1–P7)

前哨必须能出兵. Engine truth: only the primary-flagged factory produces,
a nonprimary only when no flag exists. (P1) The outpost anchor is the
expansion yard once two stand, else the remote refinery's own cell once
it stands at the expansion distance — Layer 2.6 and the placement router
feed from it, so the forward base raises its lines before a second yard
ever lands. (P2) The reserved factory slot: the home growth array builds
out to `FACTORY_BUILDOUT` and caps at `FACTORY_CAP`; Layer 2.6 holds
`FACTORY_CAP + 1`, so home growth can never consume the outpost's factory
first. (P3) No home fallback for outpost production buildings: an
outpost-intent item whose ring refuses every site widens the ring once
(`EXPANSION_SITE_RADIUS * 2`), and a still-refusing neighborhood counts a
strike toward the pause; a paused outpost item is cancel-only through the
wall's own escape, never a wedge and never a factory stood at home. (P4)
The first-appearance latch: a changed main/outpost pair (`primary_pair`)
hands the primary flag to the outpost that tick, not after four vehicles;
afterwards the alternation runs on the slot odometer OR
`PRIMARY_TOGGLE_EVERY_TICKS` (1500) since the last toggle, and the mark
resets with the pair. (P5) The demand arm (`outpost_carrier_demand`): the
carrier holds at the outpost while its own cluster ring stands under half
the guard reserve or the live wave's gather cell sits nearer the outpost
than home; the blind alternation stays the fallback. (P6) The rally anchor
set gains the outpost anchor, and the rally cell is picked on the
factory's exit side — the first rotated spread offset at least three
cells off the anchor — so it never sits on the crowded yard ring the
engine refuses and the 900-tick gate then parked. (P7) Layer 2.6 gates on
the expansion standing, not `any_tank`: a forward base with every tank
dead must still raise the lines that rebuild them.

## The superweapon suite (S-A–S-F)

超时空/铁幕 et al., each with a strategy. Engine facts: a special fires
through `Command::LaunchSuperWeapon` only when its instance reads
`charge_start_tick != -1 && is_active && is_ready` (an uncharged fire is
silently dropped); every house owns dormant instances of every type (a
captured building grants the live one); the Chronosphere is two clicks —
`ChronoSphereSpecial` stores the source 3x3 and reads not-ready until the
warp, `ChronoWarpSpecial` zips source to target offset-preserving,
passengers ride safely, no return timer, `ChronoDelay=60` frames before
the ride is controllable; the Iron Curtain is a 3x3 for 750 frames and
kills organic foot in the block; the Force Shield is radius 4 for 500.

The fork fired only the sphere's source click and never the warp, so the
sphere parked `is_ready=false` and never recharged. `superweapon_orders`
now walks the house's whole instance set behind the retry gate — the
side's own two supers first, then the captured grants — and hands each
charged one to its strategy: (S-A) the Iron Curtain column — the first own
3x3 holding `IRON_CURTAIN_COLUMN_MIN` (5) of the side's heavy tanks and no
own infantry is curtained, and the column attack-moves at the nearest
enemy gun cluster (the war factory the fallback) for the curtain's life
(`iron_column`, the follow-through limb); no such pad falls back to the
old front-most tank. (S-B) The chrono engineer steal (`chrono_steal_orders`,
the engineer raid's stage shape). The cycle arms only while the house holds
a live Chronosphere grant (`SuperWeaponInstance::is_active`, the sphere
standing — never the registry slot, which every house carries for every
special from the first tick) and an enemy conyard stands revealed, and the
arming half runs once per `ENGINEER_CAPTURE_WINDOW_TICKS` (`chrono_arm_due`,
the raid's `raid_arm_due` shape), never per tick. A ride — the side's own
fortress first, its engineer ride behind it, by the side table's names;
commissioned only behind a standing war factory, the queue dedup and the
per-type retry gate — loads up to three engineers (one commissioned per
window through the shared stock cap and the same retry gate) one Enter per
window, parks on its pad, and the tick the sphere stands ready the suite
returns both clicks from the same pass: the sphere on the pad, the warp on
a passable cell two to three cells beside the enemy conyard on home's side
(`chrono_steal_dest`); `CHRONO_RIDE_DELAY_TICKS` later the ride unloads,
the first engineer takes the conyard, the spares the next-ranked
structures. The ride is never curtained before the warp. (The build11
regression: the arming gate read the registry slot, so it passed for every
AI house the tick an enemy yard deployed, and the unpaced, factory-less
ride commission — a literal Battle Fortress, which the native CanBuild
admits without side or prerequisite for a house no human controls — filed
one QueueProduction every tick ahead of the build ladder; the one-command
budget never reached the ladder and the AIs built nothing.)
`enemy_priority_targets` gains the conyard-first entry every raid now
reads. (S-C) The chrono prism drop: a 3x3 holding `CHRONO_PRISM_DROP_MIN`
(3) idle Prism-class tanks is the pad, the enemy power cluster's passable
3x3 (the engine's own `Find_Nearby_Passable_Cell`) the destination, and the
warped tanks attack-move at the nearest power then the refinery line for
one clock (`prism_drop`). (S-D) Nuke and storm: the enemy structure cell
maximizing the summed retail value inside `SW_SCORE_RADIUS` — conyard, war
factory, tech center 100, power 60, base defense 35, else 4, the `[General]
AIIonCannon*` table — replaces the first-priority-target click. (S-E) The
dominator takes the enemy 3x3 with the most vehicle cost clear of own
units within `DOMINATOR_OWN_CLEARANCE` (7, retail `CellSpread`); the
mutator the densest enemy foot mass. (S-F) The paradrop (the Americans'
AMRADR grant, or anyone's after capturing CAAIRP — which now ranks first
in the tech-capture survey) lands on the enemy's nearest refinery line;
the force shield fires on the own conyard while the alarm is armed and a
structure inside `FORCE_SHIELD_RADIUS` bleeds.

## Tactic extensions

The engineer transport raid loads up to three engineers (the ride's seats
permitting) and, when the enemy conyard stands, the Soviet suicide
payload in the seats left (`SUICIDE_RAIDER_TYPES`: Crazy Ivan, the Terror
Drone, the demolition truck) — the raid aims conyard-first, the first
engineer takes it, the spares the war factory and the lab, and the payload
attacks it on foot (Ivan's Attack is the bomb attach). `EngineerRaid`
carries `engineers` and `payload` vectors now; a lost ride or last
engineer still tears the cycle down, a lost spare merely leaves the crew.
The fortress's cargo is per-type quotas (`boarding_cargo`: two Guardian
GIs, two GIs, one Ghost — the retail loadout) counted against what rides
and what stands reserved; the basic soldier never once boarded before. The
hero boards the ferry (`SHAD`/`LCRF`) when he carries `C4=` and a priority
target is zone-cut from home — the ferry is his only road across, and his
own limb plants the charge once he lands. The Desolator's ore denial: an
idle Desolator walks at the enemy's nearest harvester cell through the
raider window (`ore_denial`) and toggles his radiation stance within two
cells of it — the field burns the ore run under the enemy's miners.

## Upstream sync

`superai/base` merges `origin/main` regularly; each pillar lands on its own
`superai/<pillar>` branch off `base`. Anything generally useful (bench
harness parts, command-layer hooks) is a candidate to PR back upstream;
difficulty knobs and strategies stay fork-only.
