//! The Super AI: a VERA-native decision layer that plays one house through
//! the same [`CommandEnvelope`] stream a human client sends.
//!
//! This is a fork-original feature (not a gamemd port): nothing here traces a
//! native body, and the tiers below describe VERA's own design goals.
//!
//! **Determinism contract.** For an identical simulation view and an identical
//! `tick`, [`SuperAi::orders`] must produce the identical command sequence on
//! every platform and run: the decision layer is a pure function of its
//! arguments plus its own pinned state. The current strategy draws no
//! randomness at all; later milestones that need some must draw from an RNG
//! the instance seeds itself; reading the system clock, global process state
//! or any other ambient source is forbidden. Feeding the returned envelopes
//! to the simulation never mutates the runtime, so the caller can re-derive
//! the same batch at will.
//!
//! **M1 scope.** One deterministic opening: deploy the launch MCV, then walk
//! the side's opening build (power, refinery, barracks, war factory, a second
//! refinery) keeping the building queue busy, place each ready building next
//! to the construction yard, and keep the vehicle queue building harvesters
//! once the war factory stands. From this slice the opening also arms: the
//! vehicle queue keeps miners coming up to a target and then builds the
//! side's main battle tank without pause, the war factory's rally sits on the
//! construction yard so its output gathers in the base, and the barracks
//! queue runs the side's basic soldier continuously. Idle soldiers and tanks
//! defend the base: the nearest intruder inside a ring around the yard draws
//! explicit attack orders, and units with nothing to fight hold an Area
//! Guard post on the yard; a full group gathered there attack-marches on the
//! hostile bases' mass center. Past the opening the construction queue keeps
//! expanding production: a second and third war factory (each extra one
//! multiplies vehicle build time by retail `MultipleFactory=0.8`) with the
//! power plants to feed them, then the tech climb — the radar class and the
//! battle lab, which switches the vehicle queue to the side's tier-2 tank
//! (Prism Tank, Apocalypse, Master Mind). Rallies, guard posts and wave
//! destinations fan out over fixed offsets so groups spread instead of
//! jamming on one cell. Scouting and micro are later milestones; the tiers
//! only change the strength subsidies (opening grant, halved build times),
//! never the decisions themselves.

use crate::sim::command::Command;
use crate::sim::command::CommandEnvelope;
use crate::sim::intern::InternedId;
use crate::sim::runtime::SimRuntime;

/// Ticks between two deploy retries: enough for the MCV's deploy animation to
/// finish, few enough to recover within a minute of game time.
const DEPLOY_RETRY_TICKS: u32 = 90;

/// Deploy retries before the opening gives up (a blocked start waypoint will
/// never clear by repeating the order).
const DEPLOY_ATTEMPTS: u8 = 5;

/// How far from the construction yard the site search scans, in cells.
const SITE_SEARCH_RADIUS: i16 = 24;

/// Miners the vehicle queue builds before it switches to tanks: enough to
/// feed two refineries' docks while the army starts coming out.
const HARVESTER_TARGET: usize = 5;

/// How far from the construction yard an intruder still counts as attacking
/// the base, in cells: the defense picks explicit targets out to this ring;
/// idle units inside it hold an Area Guard post on the yard.
const BASE_DEFENSE_RADIUS: i16 = 16;

/// How many combat units must stand gathered (idle or on the guard post)
/// before a wave launches as one mass: production has to outpace attrition
/// first. Sized for the three-factory expansion — the faster queue gathers
/// the group quickly enough that holding it costs less than the bigger
/// punch gains.
const ATTACK_WAVE_SIZE: usize = 18;

/// How many standing combat units stay home whatever the offensive does: a
/// base emptied of defenders loses to the first counterattack that walks
/// past the marching wave.
const GUARD_RESERVE: usize = 4;

/// Power plants the expansion ladder tops out at: each extra war factory
/// drains, and negative power would slow every build through the native
/// power factor in `time_to_build`.
const POWER_TARGET: usize = 3;

/// Refineries the ladder builds before it switches purely to factories:
/// enough docks to keep five miners turning ore into credits.
const REFINERY_TARGET: usize = 2;

/// War factories the expansion ladder builds up to: every extra one
/// multiplies vehicle build time by retail `[General] MultipleFactory=0.8`
/// (the factory-count loop in `time_to_build`), so the third factory builds
/// tanks at 0.64× the single-factory time — the army output cap the tiers'
/// money and speed subsidies convert into.
const WAR_FACTORY_TARGET: usize = 3;

/// Ticks between wave episodes. A full pulse drains the standing set to the
/// guard reserve; this interval lets production refill it before the next
/// departure and bounds a wave whose dispatches were dropped to one wasted
/// order per unit per interval.
const WAVE_RETRY_TICKS: u32 = 300;

/// Every side's war factory type (retail `UIName`s): the structures a wave
/// razes first to starve the enemy's reinforcements.
const WAR_FACTORY_TYPES: [&str; 3] = ["GAWEAP", "NAWEAP", "YAWEAP"];

/// How long an idle unit that was just given a one-unit order waits before
/// taking another: an order the command layer drops leaves the unit's
/// mission `None`, so without this pause the defense and guard branches
/// would re-order the same first idle unit every tick and starve every
/// branch below them (observed: a Guard post cell blocked by a building
/// froze the whole build ladder for a tank's entire build time).
const IDLE_REORDER_TICKS: u32 = 150;

/// Deterministic fan-out offsets (cells) that keep the AI's destinations
/// from converging on one point: rallies, guard posts and wave destinations
/// each pick an offset by index or entity id, so a group spreads over
/// neighboring cells instead of jamming on a single one. Move, Guard and
/// AttackMove all complete in the destination's vicinity, so an offset that
/// lands on a building still parks the unit beside it rather than stacking.
const SPREAD_OFFSETS: [(i8, i8); 8] = [
    (0, 0),
    (4, 0),
    (0, 4),
    (-4, 0),
    (0, -4),
    (4, 4),
    (-4, 4),
    (-4, -4),
];

/// The ladder the Super AI plays at.
///
/// The tiers describe the target behaviour, not what the current milestone
/// does; see the module docs for the milestone split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SuperAiDifficulty {
    /// Zero bonuses, pure decisions: the benchmark baseline. Whatever the
    /// decision layer achieves at this tier is skill, not subsidy.
    #[default]
    Fair,
    /// Fair play plus credit and harvesting bonuses, in the economy the
    /// simulation already owns. Lands in a later milestone.
    Brutal,
    /// Fast building, parallel factories and continuous unit production on
    /// top of Brutal's economy. The build-speed knob is a later milestone's
    /// minimal simulation-side change; nothing simulation-side changes yet.
    Super,
}

/// One side's opening build, by retail type name. The construction yard each
/// MCV deploys into names the side (`GACNST`/`NACNST`/`YACNST`).
struct Opening {
    construction_yard: &'static str,
    power: &'static str,
    refinery: &'static str,
    barracks: &'static str,
    war_factory: &'static str,
    /// The side's radar-class building — the battle lab's `RADAR`
    /// prerequisite (Airforce Command, Soviet Radar, Psychic Radar).
    radar: &'static str,
    /// The side's battle lab, the tech gate for the tier-2 tank.
    battle_lab: &'static str,
    /// The side's miner, if the war factory can build it (Yuri's slave miner
    /// is not a plain vehicle and stays out of the opening).
    harvester: Option<&'static str>,
    /// The side's first battle tank (Grizzly, Rhino, Lasher).
    main_tank: &'static str,
    /// The side's tier-2 battle tank behind the battle lab (Prism Tank,
    /// Apocalypse, Master Mind).
    tier2_tank: &'static str,
    /// The side's basic soldier (GI, Conscript, Initiate).
    basic_infantry: &'static str,
}

const OPENINGS: [Opening; 3] = [
    Opening {
        construction_yard: "GACNST",
        power: "GAPOWR",
        refinery: "GAREFN",
        barracks: "GAPILE",
        war_factory: "GAWEAP",
        radar: "GAAIRC",
        battle_lab: "GATECH",
        harvester: Some("CMIN"),
        main_tank: "MTNK",
        tier2_tank: "SREF",
        basic_infantry: "E1",
    },
    Opening {
        construction_yard: "NACNST",
        power: "NAPOWR",
        refinery: "NAREFN",
        barracks: "NAHAND",
        war_factory: "NAWEAP",
        radar: "NARADR",
        battle_lab: "NATECH",
        harvester: Some("HARV"),
        main_tank: "HTNK",
        tier2_tank: "APOC",
        basic_infantry: "E2",
    },
    Opening {
        construction_yard: "YACNST",
        power: "YAPOWR",
        refinery: "YAREFN",
        barracks: "YABRCK",
        war_factory: "YAWEAP",
        // Yuri's radar class is NAPSIS (Psychic Sensor, `Radar=yes`); the
        // similarly-named YAPSYT is the Psychic Tower defense building and
        // satisfies no prerequisite.
        radar: "NAPSIS",
        battle_lab: "YATECH",
        harvester: None,
        main_tank: "LTNK",
        tier2_tank: "MIND",
        basic_infantry: "INIT",
    },
];

/// One Super AI player bound to the house it commands.
pub struct SuperAi {
    /// The house whose commands this player issues; the same identity a
    /// [`CommandEnvelope`]'s `owner` carries.
    house: InternedId,
    difficulty: SuperAiDifficulty,
    /// The tick the next MCV deploy may be issued at.
    next_deploy_tick: u32,
    /// Deploy orders issued so far this game.
    deploy_attempts: u8,
    /// Whether a gathered wave is currently launching: one standing unit
    /// leaves per tick until only the guard reserve remains home, so the
    /// group departs together and arrives as a mass instead of trickling
    /// single file into the enemy's defense. Pinned state, never read from
    /// the runtime — the determinism contract allows exactly this.
    wave_launching: bool,
    /// Units already dispatched in the current wave episode. A dispatch the
    /// command layer drops leaves its unit in the standing set; without this
    /// list the wave branch would re-order the same unit every tick and
    /// starve every branch below it (observed: two units whose offset
    /// destination was impassable froze all production).
    launched: Vec<u64>,
    /// Earliest tick the next wave episode may trigger. Paces pulses apart
    /// and bounds the damage a wave of entirely dropped dispatches can do to
    /// one order per unit per interval.
    next_wave_tick: u32,
    /// Earliest tick each idle unit may take another single-unit order; see
    /// [`IDLE_REORDER_TICKS`]. Pinned state, never read from the runtime.
    order_gate: std::collections::HashMap<u64, u32>,
}

impl SuperAi {
    /// Bind the AI to `house` at `difficulty`.
    pub fn new(house: InternedId, difficulty: SuperAiDifficulty) -> Self {
        Self {
            house,
            difficulty,
            next_deploy_tick: 0,
            deploy_attempts: 0,
            wave_launching: false,
            launched: Vec::new(),
            next_wave_tick: 0,
            order_gate: std::collections::HashMap::new(),
        }
    }

    /// The house this player commands.
    pub const fn house(&self) -> InternedId {
        self.house
    }

    /// The tier this player plays at.
    pub const fn difficulty(&self) -> SuperAiDifficulty {
        self.difficulty
    }

    /// The commands this player issues for the frame after `tick`.
    ///
    /// One command per tick, in a fixed priority: deploy the opening MCV,
    /// place a ready building, defend the base (attack the nearest intruder;
    /// else launch a gathered wave on the enemy's structures, one unit per
    /// tick; else post idle units on an Area Guard), keep the building queue
    /// walking the ladder, hold the war factories' rallies around the yard,
    /// keep the vehicle queue on miners-then-tanks, keep the barracks queue
    /// on the side's basic soldier. See the module docs for the determinism
    /// contract.
    pub fn orders(&mut self, runtime: &SimRuntime, tick: u32) -> Vec<CommandEnvelope> {
        let sim = &runtime.simulation;
        let rules = &runtime.resources.rules;
        let at = sim.session().tick() + 1;
        let owner = sim.interner.resolve(self.house).to_string();

        // Deploy: the launch MCV deploys into the side's construction yard.
        if let Some(mcv) = self.deployable_mcv(sim, rules) {
            if tick >= self.next_deploy_tick && self.deploy_attempts < DEPLOY_ATTEMPTS {
                self.next_deploy_tick = tick + DEPLOY_RETRY_TICKS;
                self.deploy_attempts += 1;
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::DeployMcv { entity_id: mcv },
                )];
            }
            return Vec::new();
        }
        let Some(opening) = self.opening(sim) else {
            return Vec::new();
        };

        // Place one completed building next to the construction yard.
        let ready = crate::sim::production::ready_buildings_for_owner(sim, rules, &owner).pop();
        if let Some(ready) = ready
            && let Some(ty) = rules.object(sim.interner.resolve(ready.type_id))
            && let Some(center) = self.structure_center(sim, opening.construction_yard)
            && let Some((rx, ry)) = find_site_around(
                sim,
                rules,
                &runtime.resources.overlay_registry,
                ty,
                center,
                self.house,
            )
        {
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::PlaceReadyBuilding {
                    type_id: ready.type_id,
                    rx,
                    ry,
                },
            )];
        }

        // Home defense and the offensive. Idle combat units first engage the
        // nearest intruder inside the base perimeter. Otherwise the offense
        // moves in waves: once `ATTACK_WAVE_SIZE` units stand gathered (idle
        // or on their guard post), the wave launches — one standing unit per
        // tick attack-moves on the enemy center, so the group departs
        // together and arrives as a mass — until only the guard reserve
        // remains home; production then rebuilds the next wave while the
        // current one fights. While a wave is still gathering, newcomers
        // hold an Area Guard post on the yard. Orders issue one unit per
        // tick; an engaged, marching or posted unit leaves the standing set,
        // so no spam follows, and a wave that dies lets the next one form.
        if let Some(center) = self.structure_center(sim, opening.construction_yard) {
            let idle = self.idle_combat_units(sim, opening);
            if let Some(attacker) = self.due_idle_unit(&idle, tick)
                && let Some(target) = self.nearest_intruder(sim, center)
            {
                self.gate_idle_order(attacker, tick);
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::Attack {
                        attacker_id: attacker,
                        target_id: target,
                    },
                )];
            }
            let standing = self.standing_combat_units(sim, opening);
            let enemy_targets = self.enemy_priority_targets(sim);
            let enemy_center = if enemy_targets.is_empty() {
                self.enemy_center(sim)
            } else {
                None
            };
            if !self.wave_launching
                && tick >= self.next_wave_tick
                && standing.len() >= ATTACK_WAVE_SIZE
                && (!enemy_targets.is_empty() || enemy_center.is_some())
            {
                self.wave_launching = true;
                self.launched.clear();
            }
            if self.wave_launching {
                // Dispatch one not-yet-launched standing unit per tick. The
                // `launched` list bounds the episode: a dispatch the command
                // layer drops (its unit stays standing) is never re-ordered
                // within the episode, so the branch cannot spin on it and
                // starve the branches below.
                let next = standing
                    .iter()
                    .find(|&&unit| !self.launched.contains(&unit))
                    .copied();
                match next {
                    Some(unit) if standing.len() > GUARD_RESERVE => {
                        self.launched.push(unit);
                        // Each unit attack-moves on one enemy structure's own
                        // cell, cycled by unit id — the wave fans out over the
                        // target base instead of converging on one cell, and a
                        // live building's cell is always reachable-adjacent,
                        // unlike a centroid plus offset, which can land on
                        // impassable ground. With no structure left the wave
                        // falls back to the hostile mobiles' centroid.
                        let destination = if enemy_targets.is_empty() {
                            enemy_center.map(|center| {
                                let spread = SPREAD_OFFSETS[(unit as usize) % SPREAD_OFFSETS.len()];
                                on_map_cell(
                                    sim,
                                    (
                                        center.0 + i16::from(spread.0),
                                        center.1 + i16::from(spread.1),
                                    ),
                                    center,
                                )
                            })
                        } else {
                            enemy_targets
                                .get(unit as usize % enemy_targets.len())
                                .map(|&(_, cell)| (cell.0 as u16, cell.1 as u16))
                        };
                        if let Some((rx, ry)) = destination {
                            return vec![CommandEnvelope::new(
                                self.house,
                                at,
                                Command::AttackMove {
                                    entity_id: unit,
                                    target: None,
                                    destination: Some(crate::sim::combat::TargetKind::Cell(rx, ry)),
                                },
                            )];
                        }
                        self.wave_launching = false;
                        self.next_wave_tick = tick + WAVE_RETRY_TICKS;
                    }
                    _ => {
                        // The reserve is home (a full pulse) or every standing
                        // unit was already dispatched (dropped orders): end
                        // the episode and let production fill the next one.
                        self.wave_launching = false;
                        self.next_wave_tick = tick + WAVE_RETRY_TICKS;
                    }
                }
            }
            if let Some(unit) = self.due_idle_unit(&idle, tick) {
                self.gate_idle_order(unit, tick);
                // Spread the guard posts on a ring around the yard by unit
                // id: defenders hold neighboring cells instead of stacking
                // on the yard itself. Clamped onto the map for the same
                // reason as the wave destination.
                let spread = SPREAD_OFFSETS[(unit as usize) % SPREAD_OFFSETS.len()];
                let post = on_map_cell(
                    sim,
                    (
                        center.0 + i16::from(spread.0),
                        center.1 + i16::from(spread.1),
                    ),
                    center,
                );
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::Guard {
                        entity_id: unit,
                        target: Some(crate::sim::combat::TargetKind::Cell(post.0, post.1)),
                    },
                )];
            }
        }

        // Walk the build ladder while the building queue idles: the opening's
        // fixed five, then the expansion — each further war factory (retail
        // MultipleFactory=0.8 per extra one) with a power plant ahead of it,
        // refineries topping out at the docks five miners keep busy. A
        // structure the enemy destroys drops the count and re-enters the
        // ladder, so the walk doubles as rebuild.
        let queue = crate::sim::production::queue_view_for_owner(sim, rules, &owner);
        let building_busy = queue.iter().any(|item| {
            matches!(
                item.queue_category,
                crate::sim::production::ProductionCategory::Building
                    | crate::sim::production::ProductionCategory::Defense
            )
        });
        // The climb's radar step is the retail radar class, not one fixed
        // type: `[General] PrerequisiteRadar=GAAIRC,NARADR,AMRADR,NAPSIS`,
        // and GAAIRC itself carries `ForbiddenHouses=Americans` — an
        // Americans house builds AMRADR, its twin under the American id.
        // Ask the native gate which member this house may build, in the
        // list's own order; the opening's radar is the unreachable-mod
        // fallback.
        const RADAR_CLASS: [&str; 4] = ["GAAIRC", "NARADR", "AMRADR", "NAPSIS"];
        let radar_name = RADAR_CLASS
            .into_iter()
            .find(|&name| {
                rules.object(name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            })
            .unwrap_or(opening.radar);
        // Whether a tech-climb piece (the radar or the battle lab) is the
        // building under construction. Those two carry the tier the whole
        // strategy climbs for, so they alone may claim the income flow; a
        // routine expansion building must not throttle the army that
        // defends the base while it builds.
        let tech_climb_busy = queue.iter().any(|item| {
            matches!(
                item.queue_category,
                crate::sim::production::ProductionCategory::Building
                    | crate::sim::production::ProductionCategory::Defense
            ) && {
                let name = sim.interner.resolve(item.type_id);
                name == radar_name || name == opening.battle_lab
            }
        });
        if self
            .structure_center(sim, opening.construction_yard)
            .is_some()
            && !building_busy
        {
            let ladder = [
                (opening.power, 1),
                (opening.refinery, 1),
                (opening.barracks, 1),
                (opening.war_factory, 1),
                (opening.refinery, REFINERY_TARGET),
                (opening.power, 2),
                (opening.war_factory, 2),
                // Tech before the expansion finishes: the radar class is the
                // battle lab's `RADAR` prerequisite, and the lab unlocks the
                // tier-2 tank the vehicle queue switches to (retail
                // `Prerequisite=GAWEAP,GATECH` and its side's twins). Two
                // factories fund the climb; the third completes the expansion
                // once the tier is secure.
                (radar_name, 1),
                (opening.battle_lab, 1),
                (opening.power, POWER_TARGET),
                (opening.war_factory, WAR_FACTORY_TARGET),
            ];
            // The first five steps are the opening; the expansion beyond
            // them waits for the first main tank to leave the factory —
            // expanding production must not starve the pipeline it expands.
            const OPENING_STEPS: usize = 5;
            for (index, (name, target)) in ladder.iter().enumerate() {
                if index >= OPENING_STEPS && self.count_units_of_type(sim, opening.main_tank) == 0 {
                    break;
                }
                if self.count_structures_of_type(sim, name) >= *target {
                    continue;
                }
                if let Some(ty) = rules.object(*name)
                    && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
                    // Read-only lookup: every retail type name is interned at
                    // rules load; a miss would only skip this candidate.
                    && let Some(type_id) = sim.interner.get(*name)
                {
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::QueueProduction { type_id },
                    )];
                }
            }
        }

        // Keep every war factory's rally around the construction yard so its
        // output gathers in the base instead of jamming the factory exit —
        // each factory on its own offset cell (its index in the substrate's
        // stable order), so three factories don't funnel every tank onto one
        // point. The command archives the cell per factory (`SetRallyPoint
        // 0x00443860`), so correcting whichever factory's archived cell
        // differs — one per tick — suffices for all of them.
        if let Some(center) = self.structure_center(sim, opening.construction_yard)
            && let Some((factory_id, desired)) = self
                .own_structures(sim)
                .into_iter()
                .filter(|(_, name)| name == opening.war_factory)
                .enumerate()
                .map(|(index, (id, _))| {
                    let spread = SPREAD_OFFSETS[index % SPREAD_OFFSETS.len()];
                    (
                        id,
                        on_map_cell(
                            sim,
                            (
                                center.0 + i16::from(spread.0),
                                center.1 + i16::from(spread.1),
                            ),
                            center,
                        ),
                    )
                })
                .find(|&(id, desired)| {
                    sim.substrate
                        .entities
                        .get(id)
                        .is_some_and(|factory| factory.rally_cell() != Some(desired))
                })
        {
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::SetRally {
                    rx: desired.0,
                    ry: desired.1,
                    producer_ids: vec![factory_id],
                },
            )];
        }

        // The war factory stands: keep its queue busy — the economy first
        // (miners up to the target), then tanks without pause. The tier-2
        // tank takes over the moment its cameo lights up (the battle lab
        // standing); until then the side's first tank rolls. A queue entered
        // while short on credits simply waits for the money, as a native
        // production event does.
        let vehicle_busy = queue
            .iter()
            .any(|item| item.queue_category == crate::sim::production::ProductionCategory::Vehicle);
        let economy_first = opening
            .harvester
            .is_some_and(|miner| self.count_units_of_type(sim, miner) < HARVESTER_TARGET);
        // The tier-2 tank takes over once the battle lab stands. The lab
        // standing is the switch's own condition: `cameo_draw_state` mirrors
        // only the sidebar's draw brightness (`StripClass::Draw`, no
        // prerequisite query — `Recalculate` owns refusal), so it stays
        // `enabled` long before the lab exists and would switch the queue to
        // a tank the house cannot honestly field.
        let tier2_ready = self.count_structures_of_type(sim, opening.battle_lab) >= 1
            && sim.interner.get(opening.tier2_tank).is_some();
        let vehicle_want = if economy_first {
            opening.harvester
        } else if tier2_ready {
            Some(opening.tier2_tank)
        } else {
            Some(opening.main_tank)
        };
        if !vehicle_busy
            && self.owns_structure_of_type(sim, opening.war_factory)
            && let Some(name) = vehicle_want
            && let Some(ty) = rules.object(name)
            && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
            // While a tech-climb piece is under construction the unit
            // queues spend only banked credits — an item waits until its
            // own cost sits in the bank, so the incoming ore flow stays
            // with the radar or lab instead of splitting three ways and
            // stretching the climb over a tank's build time each. Miners
            // are the income source itself and queue unconditionally.
            && !(tech_climb_busy
                && !economy_first
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::QueueProduction { type_id },
            )];
        }

        // The barracks stands: keep its queue on the side's basic soldier.
        let infantry_busy = queue.iter().any(|item| {
            item.queue_category == crate::sim::production::ProductionCategory::Infantry
        });
        if !infantry_busy
            && self.owns_structure_of_type(sim, opening.barracks)
            && let Some(ty) = rules.object(opening.basic_infantry)
            && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
            // The same banked-credits gate: soldiers wait for their own
            // cost while a tech-climb piece claims the income flow.
            && !(tech_climb_busy
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(opening.basic_infantry)
        {
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::QueueProduction { type_id },
            )];
        }
        Vec::new()
    }

    /// The side's opening table: read from the construction yard the launch
    /// MCV deploys into while that MCV still stands, and from a standing
    /// construction yard afterwards.
    fn opening(&self, sim: &crate::sim::world::Simulation) -> Option<&'static Opening> {
        let yard = self
            .own_structures(sim)
            .into_iter()
            .map(|(_, type_name)| type_name)
            .find(|type_name| OPENINGS.iter().any(|o| o.construction_yard == type_name))?;
        OPENINGS.iter().find(|o| o.construction_yard == yard)
    }

    /// The still-mobile MCV whose deploy target names one of the opening
    /// construction yards, if the house owns one.
    fn deployable_mcv(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
    ) -> Option<u64> {
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.owner() != self.house
                || entity.category() != crate::map::entities::EntityCategory::Unit
            {
                continue;
            }
            let deploys_into_a_yard = rules
                .object(sim.interner.resolve(entity.type_ref()))
                .and_then(|ty| ty.deploys_into.as_deref())
                .is_some_and(|deploys_into| {
                    OPENINGS.iter().any(|o| o.construction_yard == deploys_into)
                });
            if deploys_into_a_yard {
                return Some(id);
            }
        }
        None
    }

    /// The house's structures as `(entity id, type name)` pairs, in the
    /// substrate's stable order.
    fn own_structures(&self, sim: &crate::sim::world::Simulation) -> Vec<(u64, String)> {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Structure
            })
            .filter_map(|(id, entity)| {
                Some((id, sim.interner.resolve(entity.type_ref()).to_string()))
            })
            .collect()
    }

    /// Whether the house owns a structure of exactly this retail type name.
    fn owns_structure_of_type(&self, sim: &crate::sim::world::Simulation, type_name: &str) -> bool {
        self.own_structures(sim)
            .iter()
            .any(|(_, name)| name == type_name)
    }

    /// How many of the house's structures are exactly this retail type name:
    /// the build ladder's expansion targets count (and re-count what the
    /// enemy destroys).
    fn count_structures_of_type(
        &self,
        sim: &crate::sim::world::Simulation,
        type_name: &str,
    ) -> usize {
        self.own_structures(sim)
            .iter()
            .filter(|(_, name)| name == type_name)
            .count()
    }

    /// How many of the house's units are exactly this retail type name.
    fn count_units_of_type(&self, sim: &crate::sim::world::Simulation, type_name: &str) -> usize {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Unit
                    && sim.interner.resolve(entity.type_ref()) == type_name
            })
            .count()
    }

    /// The house's idle combat units for the opening's army types, in the
    /// substrate's stable order: arrived tanks and soldiers whose current
    /// mission is None or Sleep (a unit walking to the rally, engaging a
    /// target or holding a post is not idle and takes no new order).
    fn idle_combat_units(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
    ) -> Vec<u64> {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && matches!(
                        entity.category(),
                        crate::map::entities::EntityCategory::Unit
                            | crate::map::entities::EntityCategory::Infantry
                    )
                    && {
                        let name = sim.interner.resolve(entity.type_ref());
                        name == opening.main_tank
                            || name == opening.tier2_tank
                            || name == opening.basic_infantry
                    }
                    && matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                    )
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// The first idle unit clear to take a single-unit order. One whose
    /// previous order the command layer dropped stays mission-`None`; the
    /// [`IDLE_REORDER_TICKS`] gate makes it wait instead of monopolizing the
    /// branch that ordered it.
    fn due_idle_unit(&self, idle: &[u64], tick: u32) -> Option<u64> {
        idle.iter()
            .copied()
            .find(|&unit| self.order_gate.get(&unit).is_none_or(|&at| tick >= at))
    }

    /// Mark `unit` as just ordered and forget gates that have expired.
    fn gate_idle_order(&mut self, unit: u64, tick: u32) {
        self.order_gate.insert(unit, tick + IDLE_REORDER_TICKS);
        self.order_gate.retain(|_, &mut at| at > tick);
    }

    /// The house's combat units free to take a march order, in the
    /// substrate's stable order: idle units plus the ones holding their
    /// guard post — an attack-move simply replaces that post. Units already
    /// marching or engaging a target are not re-pointed.
    fn standing_combat_units(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
    ) -> Vec<u64> {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && matches!(
                        entity.category(),
                        crate::map::entities::EntityCategory::Unit
                            | crate::map::entities::EntityCategory::Infantry
                    )
                    && {
                        let name = sim.interner.resolve(entity.type_ref());
                        name == opening.main_tank
                            || name == opening.tier2_tank
                            || name == opening.basic_infantry
                    }
                    && matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Whether `owner` is an enemy of this AI's house: not itself, not a
    /// passive observer, and not an ally — team play must not turn the AI's
    /// defense and waves on its own teammates. Alliances are checked in both
    /// directions: in-game diplomacy can be one-way.
    fn is_enemy(&self, sim: &crate::sim::world::Simulation, owner: InternedId) -> bool {
        if owner == self.house {
            return false;
        }
        let Some(house) = sim.houses.get(&owner) else {
            return false;
        };
        if house.multiplay_passive() {
            return false;
        }
        let me = sim.interner.resolve(self.house);
        let them = sim.interner.resolve(owner);
        let alliances = sim.house_alliances();
        !crate::map::houses::is_allied_with(alliances, me, them)
            && !crate::map::houses::is_allied_with(alliances, them, me)
    }

    /// The nearest hostile entity inside `BASE_DEFENSE_RADIUS` cells of
    /// `center`, ties broken by the substrate's stable order. Passive houses
    /// (observers, civilians) and allies do not count as intruders.
    fn nearest_intruder(
        &self,
        sim: &crate::sim::world::Simulation,
        center: (i16, i16),
    ) -> Option<u64> {
        let limit = i32::from(BASE_DEFENSE_RADIUS) * i32::from(BASE_DEFENSE_RADIUS);
        let mut best: Option<(i32, u64)> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if !self.is_enemy(sim, entity.owner()) {
                continue;
            }
            let dx = i32::from(entity.position.rx() as i16 - center.0);
            let dy = i32::from(entity.position.ry() as i16 - center.1);
            let distance_squared = dx * dx + dy * dy;
            if distance_squared > limit {
                continue;
            }
            if best.is_none_or(|(best_distance, _)| distance_squared < best_distance) {
                best = Some((distance_squared, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// The hostile structures a wave marches on, in the substrate's stable
    /// order with each one's cell: the enemy's war factories first — a wave
    /// that razes production starves the reinforcements and snowballs — or
    /// any hostile structure when no factory stands. Allies and passive
    /// houses never count as hostile. Wave dispatches cycle this list by unit
    /// id, so the group fans out over the target base; each destination is a
    /// live building's own cell, which is always reachable-adjacent.
    fn enemy_priority_targets(
        &self,
        sim: &crate::sim::world::Simulation,
    ) -> Vec<(u64, (i16, i16))> {
        let mut factories = Vec::new();
        let mut structures = Vec::new();
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.category() != crate::map::entities::EntityCategory::Structure
                || !self.is_enemy(sim, entity.owner())
            {
                continue;
            }
            let cell = (entity.position.rx() as i16, entity.position.ry() as i16);
            let name = sim.interner.resolve(entity.type_ref());
            if WAR_FACTORY_TYPES.contains(&name) {
                factories.push((id, cell));
            }
            structures.push((id, cell));
        }
        if factories.is_empty() {
            structures
        } else {
            factories
        }
    }

    /// The hostile mass center for the endgame fallback, when no hostile
    /// building stands: the centroid of their mobile forces. Sums over the
    /// entity set, so the result does not depend on iteration order.
    fn enemy_center(&self, sim: &crate::sim::world::Simulation) -> Option<(i16, i16)> {
        let centroid = |want_structure: bool| -> Option<(i16, i16)> {
            let mut sum_x = 0i64;
            let mut sum_y = 0i64;
            let mut count = 0u64;
            for (_, entity) in sim.substrate.entities.iter_sorted() {
                let is_structure =
                    entity.category() == crate::map::entities::EntityCategory::Structure;
                if is_structure != want_structure || !self.is_enemy(sim, entity.owner()) {
                    continue;
                }
                sum_x += i64::from(entity.position.rx());
                sum_y += i64::from(entity.position.ry());
                count += 1;
            }
            (count > 0).then(|| ((sum_x / count as i64) as i16, (sum_y / count as i64) as i16))
        };
        centroid(false)
    }

    /// The anchor cell the site search scans around: the construction yard's,
    /// or the first own structure's when the yard itself is the ready piece.
    fn structure_center(
        &self,
        sim: &crate::sim::world::Simulation,
        prefer_type: &str,
    ) -> Option<(i16, i16)> {
        let structures = self.own_structures(sim);
        let pick = structures
            .iter()
            .find(|(_, name)| name == prefer_type)
            .or_else(|| structures.first())?;
        let entity = sim.substrate.entities.get(pick.0)?;
        Some((entity.position.rx() as i16, entity.position.ry() as i16))
    }
}

/// Clamp an offset destination into the map. An offset target off the map
/// would wrap through `as u16` to the far corner, and the command layer
/// silently drops such orders — the unit never leaves its standing set and
/// the issuing branch would re-order it forever, starving every lower
/// branch. `fallback` (always an on-map cell, e.g. the un-offset center)
/// covers a terrain-less simulation.
fn on_map_cell(
    sim: &crate::sim::world::Simulation,
    cell: (i16, i16),
    fallback: (i16, i16),
) -> (u16, u16) {
    let Some(terrain) = sim.resolved_terrain.as_ref() else {
        return (fallback.0 as u16, fallback.1 as u16);
    };
    let max_x = (terrain.width() as i16).saturating_sub(1);
    let max_y = (terrain.height() as i16).saturating_sub(1);
    (cell.0.clamp(0, max_x) as u16, cell.1.clamp(0, max_y) as u16)
}

/// The first legal placement origin for `ty`, scanning outward from `center`
/// ring by ring in a fixed row-major order.
fn find_site_around(
    sim: &crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    registry: &crate::rules::overlay_types::OverlayTypeRegistry,
    ty: &crate::rules::object_type::ObjectType,
    center: (i16, i16),
    house: InternedId,
) -> Option<(u16, u16)> {
    for radius in 1..=SITE_SEARCH_RADIUS {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs() != radius && dy.abs() != radius {
                    continue;
                }
                let origin = (center.0 + dx, center.1 + dy);
                if origin.0 <= 0 || origin.1 <= 0 {
                    continue;
                }
                if crate::sim::build_site::can_place_building_at(
                    sim,
                    rules,
                    Some(registry),
                    ty,
                    origin,
                    Some(house),
                    None,
                    None,
                ) {
                    return Some((origin.0 as u16, origin.1 as u16));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{SuperAi, SuperAiDifficulty};
    use crate::sim::intern::InternedId;
    use crate::sim::runtime::SimRuntime;

    /// A bound runtime with one interned house name, as the entry point sees
    /// it, and that house's identity.
    fn runtime() -> (SimRuntime, InternedId) {
        let mut sim = crate::sim::world::Simulation::new();
        let house = sim.intern("SuperAiTest");
        (SimRuntime::from_simulation(sim), house)
    }

    #[test]
    fn every_tier_emits_no_commands_without_an_opening() {
        let (runtime, house) = runtime();
        for difficulty in [
            SuperAiDifficulty::Fair,
            SuperAiDifficulty::Brutal,
            SuperAiDifficulty::Super,
        ] {
            let mut ai = SuperAi::new(house, difficulty);
            for tick in [0u32, 1, 7, u32::MAX] {
                assert!(
                    ai.orders(&runtime, tick).is_empty(),
                    "{difficulty:?} must emit nothing at tick {tick} on an empty world"
                );
            }
        }
    }

    #[test]
    fn same_view_and_tick_produce_the_identical_sequence() {
        let (runtime, house) = runtime();
        let mut a = SuperAi::new(house, SuperAiDifficulty::Fair);
        let mut b = SuperAi::new(house, SuperAiDifficulty::Fair);
        for tick in [0u32, 42] {
            assert_eq!(a.orders(&runtime, tick), b.orders(&runtime, tick));
        }
    }

    #[test]
    fn difficulty_defaults_to_fair_and_round_trips() {
        assert_eq!(SuperAiDifficulty::default(), SuperAiDifficulty::Fair);
        for difficulty in [
            SuperAiDifficulty::Fair,
            SuperAiDifficulty::Brutal,
            SuperAiDifficulty::Super,
        ] {
            let json = serde_json::to_string(&difficulty).expect("serialize difficulty");
            assert_eq!(
                serde_json::from_str::<SuperAiDifficulty>(&json).expect("deserialize difficulty"),
                difficulty
            );
        }
    }

    #[test]
    fn construction_keeps_house_and_tier() {
        let (runtime, house) = runtime();
        let mut ai = SuperAi::new(house, SuperAiDifficulty::Super);
        assert_eq!(ai.house(), house);
        assert_eq!(ai.difficulty(), SuperAiDifficulty::Super);
        assert!(ai.orders(&runtime, 0).is_empty());
    }

    /// A retail XMP03T4 Battle as the Super AI plays it in the bench: the
    /// local house is America (Allied opening) against one Easy Russia, both
    /// sides starting from their own MCV, and that local house's identity.
    fn retail_battle() -> (
        crate::headless_scenario::HeadlessScenario,
        crate::sim::intern::InternedId,
    ) {
        use crate::skirmish_launch::{AiDifficulty, LaunchCountry};

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        // A four-start map: fewer start waypoints than houses would seed the
        // extra starts at random, which the lobby never allows.
        let map = "XMP03T4.MAP";
        let mut session = crate::headless_scenario::battle_session(
            map,
            &[(LaunchCountry::Russia, AiDifficulty::Easy)],
        );
        session.local_slot_mut().country = LaunchCountry::America;
        // No roaming starting units: the local house opens with its own MCV.
        session.options.unit_count = 0;
        let scenario =
            crate::headless_scenario::load_session(&ra2, map, 0x00C0_FFEE, session).unwrap();
        let human = scenario
            .sim()
            .houses
            .iter()
            .find(|(_, house)| house.is_controlled_by_human(true))
            .map(|(&id, _)| id)
            .expect("the local house");
        (scenario, human)
    }

    /// Drive the Super AI's orders until `done` accepts the view or the tick
    /// budget runs out; the callers assert on the end view.
    fn drive_until(
        scenario: &mut crate::headless_scenario::HeadlessScenario,
        ai: &mut SuperAi,
        budget: u32,
        done: impl Fn(&crate::headless_scenario::HeadlessScenario) -> bool,
    ) -> u32 {
        use crate::sim::world::{FrameEffects, TickLane};

        let mut tick = 0u32;
        while tick < budget && !done(scenario) {
            let orders = ai.orders(&scenario.runtime, tick);
            scenario
                .runtime
                .advance_frame(
                    &orders,
                    crate::headless_scenario::SIM_TICK_MS,
                    TickLane::Ordinary,
                    FrameEffects::default(),
                )
                .expect("simulation frame failed");
            tick += 1;
        }
        tick
    }

    /// The M1 opening on a retail Battle: the Super AI drives the local
    /// house from its launch MCV and must own a construction yard, a power
    /// plant and a refinery well inside the tick budget.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_opening_deploys_and_builds_the_first_structures() {
        let (mut scenario, human) = retail_battle();
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let owns = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .any(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && scenario.sim().interner.resolve(entity.type_ref()) == name
                })
        };

        let tick = drive_until(&mut scenario, &mut ai, 6000, |scenario| {
            owns(scenario, "GACNST") && owns(scenario, "GAPOWR") && owns(scenario, "GAREFN")
        });
        assert!(owns(&scenario, "GACNST"), "the MCV deployed (tick {tick})");
        assert!(
            owns(&scenario, "GAPOWR"),
            "the power plant stands (tick {tick})"
        );
        assert!(
            owns(&scenario, "GAREFN"),
            "the refinery stands (tick {tick})"
        );
    }

    /// The Super tier's one simulation-side exception halves build times:
    /// on the same map and seed the opening's first three buildings must
    /// stand sooner under Super than under Fair.
    #[test]
    #[ignore = "slow; retail; loads two matches and drives thousands of frames"]
    fn super_tier_builds_the_opening_faster_than_fair() {
        fn opening_tick(tier: SuperAiDifficulty) -> u32 {
            let (mut scenario, human) = retail_battle();
            scenario.apply_super_ai_tier(human, tier);
            let mut ai = SuperAi::new(human, tier);
            let owns = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
                scenario
                    .sim()
                    .substrate
                    .entities
                    .iter_sorted()
                    .any(|(_, entity)| {
                        entity.owner() == human
                            && entity.category() == crate::map::entities::EntityCategory::Structure
                            && scenario.sim().interner.resolve(entity.type_ref()) == name
                    })
            };
            drive_until(&mut scenario, &mut ai, 6000, |scenario| {
                owns(scenario, "GACNST") && owns(scenario, "GAPOWR") && owns(scenario, "GAREFN")
            })
        }
        let fair = opening_tick(SuperAiDifficulty::Fair);
        let super_tier = opening_tick(SuperAiDifficulty::Super);
        assert!(
            super_tier < fair,
            "the halved build times must finish the opening sooner (super {super_tier} vs fair {fair})"
        );
    }

    /// The mass-production slice: past the opening the war factory keeps
    /// building (miners to target, then Grizzly tanks with the rally on the
    /// yard) and the barracks runs GIs, so an army exists well inside the
    /// budget.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_opening_arms_the_war_factory() {
        let (mut scenario, human) = retail_battle();
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let units_of_type = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && matches!(
                            entity.category(),
                            crate::map::entities::EntityCategory::Unit
                                | crate::map::entities::EntityCategory::Infantry
                        )
                        && scenario.sim().interner.resolve(entity.type_ref()) == name
                })
                .count()
        };

        let tick = drive_until(&mut scenario, &mut ai, 12000, |scenario| {
            units_of_type(scenario, "MTNK") >= 1 && units_of_type(scenario, "E1") >= 1
        });
        assert!(
            units_of_type(&scenario, "MTNK") >= 1,
            "a Grizzly came out (tick {tick})"
        );
        assert!(
            units_of_type(&scenario, "E1") >= 1,
            "a GI came out (tick {tick})"
        );
    }

    /// The offensive slice: once the army has grown to a full group, its
    /// standing units march — a tank is on a Move mission beyond the base
    /// perimeter (not the short walk from the factory to the rally) with the
    /// enemy base still standing, well inside the budget.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_offensive_marches_a_gathered_group() {
        let (mut scenario, human) = retail_battle();
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let combat = |scenario: &crate::headless_scenario::HeadlessScenario| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && matches!(
                            entity.category(),
                            crate::map::entities::EntityCategory::Unit
                                | crate::map::entities::EntityCategory::Infantry
                        )
                        && matches!(
                            scenario.sim().interner.resolve(entity.type_ref()),
                            "MTNK" | "E1"
                        )
                })
                .count()
        };
        let yard = |scenario: &crate::headless_scenario::HeadlessScenario| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .find(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && scenario.sim().interner.resolve(entity.type_ref()) == "GACNST"
                })
                .map(|(_, entity)| (entity.position.rx() as i16, entity.position.ry() as i16))
        };
        let marching = |scenario: &crate::headless_scenario::HeadlessScenario| {
            let Some(yard) = yard(scenario) else {
                return false;
            };
            let limit =
                i32::from(super::BASE_DEFENSE_RADIUS) * i32::from(super::BASE_DEFENSE_RADIUS);
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .any(|(_, entity)| {
                    if entity.owner() != human
                        || entity.category() != crate::map::entities::EntityCategory::Unit
                        || scenario.sim().interner.resolve(entity.type_ref()) != "MTNK"
                        || entity.passive_acquire_mission()
                            != crate::sim::mission::MissionType::Move
                    {
                        return false;
                    }
                    let dx = i32::from(entity.position.rx() as i16 - yard.0);
                    let dy = i32::from(entity.position.ry() as i16 - yard.1);
                    dx * dx + dy * dy > limit
                })
        };

        // The tech climb trades early army for late power: past the first
        // tank the ladder buys power plants and factories before the army
        // deepens, so the first 18-strong wave forms later than the pre-tech
        // timeline. The budget covers the climb, not a speed record.
        let tick = drive_until(&mut scenario, &mut ai, 26000, |scenario| {
            combat(scenario) >= 8 && marching(scenario)
        });
        assert!(
            combat(&scenario) >= 8 && marching(&scenario),
            "a full group formed and marched (tick {tick}, combat {})",
            combat(&scenario)
        );
    }

    /// The expansion slice: past the opening the build ladder keeps going —
    /// a second war factory (with its power plant) must stand well inside
    /// the budget, the production capacity the tiers' subsidies convert
    /// into army output.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_expansion_builds_a_second_war_factory() {
        let (mut scenario, human) = retail_battle();
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let structures_of_type = |scenario: &crate::headless_scenario::HeadlessScenario,
                                  name: &str| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && scenario.sim().interner.resolve(entity.type_ref()) == name
                })
                .count()
        };

        let tick = drive_until(&mut scenario, &mut ai, 16000, |scenario| {
            structures_of_type(scenario, "GAWEAP") >= 2
        });
        assert!(
            structures_of_type(&scenario, "GAWEAP") >= 2,
            "a second war factory stands (tick {tick})"
        );
    }

    /// The tech slice: past the expansion the ladder climbs — the radar
    /// class and the battle lab stand, and the vehicle queue switches to the
    /// side's tier-2 tank (a Prism Tank rolls for America) well inside the
    /// budget.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_tech_climb_builds_the_lab_and_a_tier2_tank() {
        let (mut scenario, human) = retail_battle();
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let owns_structure = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .any(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && scenario.sim().interner.resolve(entity.type_ref()) == name
                })
        };
        let units_of_type = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && matches!(
                            entity.category(),
                            crate::map::entities::EntityCategory::Unit
                                | crate::map::entities::EntityCategory::Infantry
                        )
                        && scenario.sim().interner.resolve(entity.type_ref()) == name
                })
                .count()
        };

        // The climb sits right behind the second factory, so the budget
        // spans the expansion's first half plus the climb itself, not the
        // lab's own build time.
        let tick = drive_until(&mut scenario, &mut ai, 26000, |scenario| {
            // Retail country quirk: `ForbiddenHouses=Americans` on GAAIRC
            // means an Americans house builds AMRADR, its identical twin.
            let radar_stands =
                owns_structure(scenario, "GAAIRC") || owns_structure(scenario, "AMRADR");
            radar_stands
                && owns_structure(scenario, "GATECH")
                && units_of_type(scenario, "SREF") >= 1
        });
        let radar_stands =
            owns_structure(&scenario, "GAAIRC") || owns_structure(&scenario, "AMRADR");
        assert!(radar_stands, "the Airforce Command stands (tick {tick})");
        assert!(
            owns_structure(&scenario, "GATECH"),
            "the battle lab stands (tick {tick})"
        );
        assert!(
            units_of_type(&scenario, "SREF") >= 1,
            "a Prism Tank rolled out (tick {tick})"
        );
    }

    /// The fork launch path (`populate_launch_houses`) turns a `super_tier`
    /// opponent slot into a command-driven house carrying the tier treatment
    /// — native Hard difficulty, the Hard-row opening grant, and the Super
    /// build-time scale — while an ordinary computer slot stays a native
    /// computer (`docs/superai.md`).
    #[test]
    #[ignore = "slow; retail; loads a match from the retail install"]
    fn launch_super_tier_slot_command_drives_the_opponent_house() {
        use crate::sim::house_state::HouseDifficulty;
        use crate::skirmish_launch::{AiDifficulty, LaunchCountry};
        use crate::util::native_x87::NativeF32Bits;

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        // A four-start map: fewer start waypoints than houses would seed the
        // extra starts at random, which the lobby never allows.
        let map = "XMP03T4.MAP";
        let mut session = crate::headless_scenario::battle_session(
            map,
            &[
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::America, AiDifficulty::Normal),
            ],
        );
        session.opponents[0].super_tier = Some(SuperAiDifficulty::Super);
        // No roaming starting units: the tier grant multiplies the launch
        // balance with nothing deducted or credited back in between.
        session.options.unit_count = 0;
        let starting_credits = session.options.starting_credits;
        let scenario =
            crate::headless_scenario::load_session(&ra2, map, 0x00C0_FFEE, session).unwrap();

        let house_by_name = |name: &str| {
            let id = scenario
                .sim()
                .interner
                .get(name)
                .unwrap_or_else(|| panic!("{name} is a launch house name"));
            scenario
                .sim()
                .houses
                .get(&id)
                .unwrap_or_else(|| panic!("{name} house"))
        };
        let tier_house = house_by_name("Computer1");
        let plain_house = house_by_name("Computer2");

        assert!(tier_house.command_driven(), "the tier slot command-drives");
        assert_eq!(tier_house.difficulty(), HouseDifficulty::Hard);
        assert_eq!(
            tier_house.native_build_time_scale(),
            Some(NativeF32Bits::from_bits(0x3F00_0000)),
            "the Super tier halves build times"
        );
        // The tier grant replaced the native opening-credit loop (skipped for
        // command-driven houses): Hard row x 0.01 x the launch balance.
        let hard_coefficient = scenario
            .runtime
            .resources
            .rules
            .general
            .multiplayer_ai_cm
            .get(HouseDifficulty::Hard.table_index())
            .copied()
            .unwrap_or(0);
        assert_eq!(
            tier_house.economy().available_money(),
            starting_credits
                + crate::sim::scenario_bootstrap::native_ai_opening_grant(
                    hard_coefficient,
                    starting_credits
                ),
        );

        assert!(
            !plain_house.command_driven(),
            "an ordinary computer slot stays native-AI driven"
        );
        assert_eq!(plain_house.difficulty(), HouseDifficulty::Normal);
        assert_eq!(plain_house.native_build_time_scale(), None);
        // Retail MultiplayerAICM's Normal row is 0, so the native loop granted
        // this house nothing on top of its launch balance.

        assert_eq!(plain_house.economy().available_money(), starting_credits);
    }
}
