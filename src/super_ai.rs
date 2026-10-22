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

/// The army's core — its main tank, the side's basic soldier — fills
/// every queue slot until it reaches this size; only then does the mix
/// diversify. A diversified opening would field specialists before the
/// line that holds the base exists.
const CORE_BEFORE_SPECIALISTS: usize = 6;

/// Out of every ten queue slots past the core, one goes to each
/// specialist role and the situation can raise the demanded role to
/// three: anti-air steps up while hostile aircraft exist, and
/// armor-crackers against a hostile armored mass. The core keeps every
/// remaining slot.
const ROLE_SHARE_BASE: usize = 1;
const ROLE_SHARE_BOOST: usize = 3;

/// Retail's air-capable combat types the mix reacts to — the engine
/// files them under infantry or vehicle categories, not aircraft:
/// Rocketeer, Kirov, Harrier, Black Hawk, Hind, Floating Disk.
const AIR_THREAT_TYPES: [&str; 6] = ["JUMPJET", "ZEP", "ORCA", "SHAD", "HIND", "DISK"];

/// Ticks a superweapon launch attempt waits before trying again. The
/// engine refuses a second storm while one runs and the dominator
/// mid-pulse; a refused launch must not spin this branch the way a
/// dropped unit order would starve the branches below it.
const SW_FIRE_RETRY_TICKS: u32 = 150;

/// Combat-slot wheels: past the core, every Nth issued combat slot goes
/// to the roster mix and the rest back to the core type. A live-count
/// gate measures the war, not the build program — under attrition the
/// standing army never clears the core floor (observed: a whole game
/// with no IFV while the queue rebuilt tanks), so the mix rides issued
/// slots, which only ever grow.
const SPECIALIST_EVERY_VEHICLE: u32 = 4;
const SPECIALIST_EVERY_INFANTRY: u32 = 3;

/// Growth caps: refineries stop here once the miner fleet saturates
/// the standing docks (two miners each); factories once six stand —
/// retail's MultipleFactory=0.8 per extra one has shrinking returns.
/// The first [`FACTORY_BUILDOUT`] factories are part of the standard
/// build-out and queue without any wallet bar: the ladder's last step
/// (the 5000-credit superweapon) leaves the bank drained and from there
/// the army queues absorb the income, so a wallet gate on the fourth
/// factory never coincides with a free building slot (observed: a whole
/// game stuck at three under bars of 3000 and 1500 alike — the item
/// draws its cost as it builds, the native way, instead of waiting for
/// a lump sum). Only factories past the build-out wait for
/// [`FACTORY_CREDITS`], a bar the wallet must clear before another
/// production line is worth feeding.
const REFINERY_CAP: usize = 4;
const FACTORY_BUILDOUT: usize = 4;
const FACTORY_CAP: usize = 6;
const FACTORY_CREDITS: i32 = 1500;
/// Growth power cap. Without one the deficit rule (queue a plant the
/// moment drain overtakes output) claims every growth slot in a
/// power-hungry base and the refinery and factory rules behind it never
/// take a turn (observed: two growth plants and no fourth factory).
const POWER_CAP: usize = 8;

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

/// The battlefield role a buildable combat type fills in the army mix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Shoots air — the answer to hostile aircraft.
    AntiAir,
    /// Cracks armor.
    AntiArmor,
    /// Sweeps infantry.
    AntiInfantry,
    /// Long-range damage against structures and massed targets.
    Siege,
}

/// One combat type the army mixes in past its core, with the role the
/// mix targets it at.
struct RosterEntry {
    name: &'static str,
    role: Role,
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
    /// The side's superweapon structure and the special it grants
    /// (retail `SuperWeapon=` on the building): Weather Controller,
    /// Iron Curtain, Psychic Dominator. Every retail one is
    /// `DisableableFromShell=yes`, so the lobby's superweapons toggle
    /// owns the whole chain — CanBuild refuses the building while it is
    /// off and the engine revokes a standing grant.
    super_weapon_building: &'static str,
    super_weapon_type: &'static str,
    /// Whether the special lands on friends rather than enemies: the
    /// Iron Curtain makes the vehicles under it invulnerable while
    /// organic infantry there take damage instead, so it aims at the
    /// standing armor while the storm and the dominator strike the
    /// enemy base.
    super_weapon_targets_self: bool,
    /// Combat vehicles the army mixes in past its core tank, each with
    /// its role; the core keeps the remaining slots. Country-locked
    /// entries stay in the table — CanBuild's `RequiredHouses=` gate
    /// skips them for other houses.
    vehicles: &'static [RosterEntry],
    /// Infantry the barracks mixes in past its core soldier.
    infantry: &'static [RosterEntry],
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
        super_weapon_building: "GAWEAT",
        super_weapon_type: "LightningStormSpecial",
        super_weapon_targets_self: false,
        // Anti-air picks cite the weapon's retail projectile `AA=yes`:
        // the IFV's HoverMissile fires AAHeatSeeker2, the Rocketeer's
        // 20mm Invisible3. The Tank Destroyer is Germans-only and the
        // Robot Tank waits for its Robot Control Center — CanBuild
        // gates both until the house qualifies.
        vehicles: &[
            RosterEntry {
                name: "FV",
                role: Role::AntiAir,
            },
            RosterEntry {
                name: "MGTK",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "TNKD",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "ROBO",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "BFRT",
                role: Role::Siege,
            },
        ],
        infantry: &[
            // The Guardian GI's deployed MissileLauncher is also the
            // Allied anti-air answer (AAHeatSeeker2); filed under
            // AntiArmor because its standing role cracks vehicles.
            RosterEntry {
                name: "GGI",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "JUMPJET",
                role: Role::AntiAir,
            },
            RosterEntry {
                name: "GHOST",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "SNIPE",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "TANY",
                role: Role::AntiInfantry,
            },
        ],
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
        super_weapon_building: "NAIRON",
        super_weapon_type: "IronCurtainSpecial",
        super_weapon_targets_self: true,
        // Flak answers air on both lines: the Flak Track's
        // FlakTrackAAGun and the Flak Trooper's FlakGuyAAGun fire the
        // AA-only FlakProj. The Tesla Tank is Russians-only and the
        // Desolator Arabs-only; CanBuild gates them per house.
        vehicles: &[
            RosterEntry {
                name: "HTK",
                role: Role::AntiAir,
            },
            RosterEntry {
                name: "DRON",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "TTNK",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "V3",
                role: Role::Siege,
            },
        ],
        infantry: &[
            RosterEntry {
                name: "FLAKT",
                role: Role::AntiAir,
            },
            RosterEntry {
                name: "SHK",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "IVAN",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "DESO",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "BORIS",
                role: Role::AntiArmor,
            },
        ],
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
        super_weapon_building: "YAPPET",
        super_weapon_type: "PsychicDominatorSpecial",
        super_weapon_targets_self: false,
        // The Gattling Tank's AAGattling fires the AA-only Invisible4.
        // The Magnetron lifts armor, the Chaos Drone gas masses; the
        // Floating Disk stays out until the naval/air chains land.
        vehicles: &[
            RosterEntry {
                name: "YTNK",
                role: Role::AntiAir,
            },
            RosterEntry {
                name: "TELE",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "CAOS",
                role: Role::AntiInfantry,
            },
        ],
        infantry: &[
            RosterEntry {
                name: "BRUTE",
                role: Role::AntiArmor,
            },
            RosterEntry {
                name: "VIRUS",
                role: Role::AntiInfantry,
            },
            RosterEntry {
                name: "YURI",
                role: Role::AntiArmor,
            },
        ],
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
    /// Earliest tick the next superweapon launch may be attempted. Pinned
    /// state, never read from the runtime; see [`SW_FIRE_RETRY_TICKS`].
    next_sw_fire_tick: u32,
    /// Combat production slots issued since deploy — vehicles and infantry
    /// each. Pinned state driving the specialist wheels; see
    /// [`SPECIALIST_EVERY_VEHICLE`].
    vehicle_slots: u32,
    infantry_slots: u32,
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
            next_sw_fire_tick: 0,
            vehicle_slots: 0,
            infantry_slots: 0,
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

        // A charged superweapon fires. The lobby toggle already gates the
        // whole chain upstream: with superweapons off, `can_build` refuses
        // the `DisableableFromShell=yes` building (and the engine revokes a
        // standing grant), so no instance is ever owned or charged. The
        // charge gate (`charge_start_tick` set, active, ready) mirrors the
        // fire button's own admission; `LaunchSuperWeapon` can still be
        // refused (a storm already in flight, a dominator mid-pulse), so a
        // launch retries after [`SW_FIRE_RETRY_TICKS`] instead of
        // monopolizing this tick's single command.
        if tick >= self.next_sw_fire_tick
            && let Some(sw_type) = sim.interner.get(opening.super_weapon_type)
            && let Some(instance) = sim
                .super_weapons()
                .get(&self.house)
                .and_then(|house_sws| house_sws.get(&sw_type))
            && instance.charge_start_tick() != -1
            && instance.is_active()
            && instance.is_ready()
            && let Some((rx, ry)) = self.superweapon_target(sim, opening)
        {
            self.next_sw_fire_tick = tick + SW_FIRE_RETRY_TICKS;
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::LaunchSuperWeapon {
                    sw_type_id: sw_type,
                    target_rx: rx,
                    target_ry: ry,
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
        // The climb's radar step resolves within the side's own radar
        // class. GAAIRC is the Allied default and AMRADR its
        // Americans-buildable twin (retail `ForbiddenHouses=Americans` on
        // GAAIRC); Soviet and Yuri keep their own radar. The side must come
        // first: the native CanBuild answers Yes without prerequisites for
        // a house no human controls, so scanning the whole retail
        // `PrerequisiteRadar=GAAIRC,NARADR,AMRADR,NAPSIS` class lets an AI
        // Soviet or Yuri house accept the Allied Airforce Command (its
        // `Owner=` lists every side). The gate still sorts the Allied pair
        // — the forbidden-houses check runs for AI houses too.
        let mut radar_candidates = [opening.radar, opening.radar];
        if opening.radar == "GAAIRC" {
            radar_candidates[1] = "AMRADR";
        }
        let radar_name = radar_candidates
            .into_iter()
            .find(|&name| {
                rules.object(name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            })
            .unwrap_or(opening.radar);
        // Whether a tech-climb piece (the radar, the battle lab or the
        // superweapon) is the building under construction. Those carry the
        // tier the whole strategy climbs for, so they alone may claim the
        // income flow; a routine expansion building must not throttle the
        // army that defends the base while it builds.
        let tech_climb_busy = queue.iter().any(|item| {
            matches!(
                item.queue_category,
                crate::sim::production::ProductionCategory::Building
                    | crate::sim::production::ProductionCategory::Defense
            ) && {
                let name = sim.interner.resolve(item.type_id);
                name == radar_name
                    || name == opening.battle_lab
                    || name == opening.super_weapon_building
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
                // The superweapon step follows the lobby toggle for free:
                // every retail one is `DisableableFromShell=yes`, so its
                // cameo stays dark while superweapons are off (CanBuild
                // refuses the building) and the step falls through to the
                // expansion without occupying a slot.
                (opening.super_weapon_building, 1),
                (opening.power, POWER_TARGET),
                (opening.war_factory, WAR_FACTORY_TARGET),
            ];
            // The first five steps are the opening; the expansion beyond
            // them waits for a battle tank to leave the factory —
            // expanding production must not starve the pipeline it expands.
            // The tier-2 tank counts once the lab stands, so the death of
            // the first tanks never freezes the base at its opening size.
            const OPENING_STEPS: usize = 5;
            let any_tank = self.count_units_of_type(sim, opening.main_tank)
                + self.count_units_of_type(sim, opening.tier2_tank)
                > 0;
            for (index, (name, target)) in ladder.iter().enumerate() {
                if index >= OPENING_STEPS && !any_tank {
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

            // The growth tail: the base keeps expanding past the fixed
            // ladder instead of reaching a scripted size and stopping.
            // Whichever limb runs furthest behind claims the next slot —
            // power only on an actual deficit (a base that browns out
            // builds nothing, but a margin rule claims every growth slot
            // under war damage: observed five plants while refineries sat
            // at two), then war factories (the fourth as part of the
            // standard build-out with no wallet bar — the ladder's
            // superweapon drains the bank and the army queues absorb the
            // income after, so a wallet gate never coincides with a free
            // slot; observed a whole game stuck at three under bars of
            // 3000 and 1500 alike — while the fifth and sixth wait for a
            // deep wallet), then refineries while the miner fleet can
            // feed another dock (two miners each). Every candidate
            // still passes its own cameo, so a limb the house cannot build
            // falls through to the next instead of wedging the queue.
            if any_tank {
                let (output, drain) =
                    crate::sim::production::power_balance_for_owner(sim, rules, &owner);
                let refineries = self.count_structures_of_type(sim, opening.refinery);
                let factories = self.count_structures_of_type(sim, opening.war_factory);
                let miners = opening
                    .harvester
                    .map(|miner| self.count_units_of_type(sim, miner))
                    .unwrap_or(usize::MAX);
                let growth = [
                    (
                        output - drain < 0
                            && self.count_structures_of_type(sim, opening.power) < POWER_CAP,
                        opening.power,
                    ),
                    (
                        factories < FACTORY_BUILDOUT
                            || (factories < FACTORY_CAP
                                && crate::sim::production::credits_for_owner(sim, &owner)
                                    > FACTORY_CREDITS),
                        opening.war_factory,
                    ),
                    (
                        refineries < REFINERY_CAP && miners >= refineries * 2,
                        opening.refinery,
                    ),
                ];
                for (behind, name) in growth {
                    if !behind {
                        continue;
                    }
                    if let Some(ty) = rules.object(name)
                        && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                        && let Some(type_id) = sim.interner.get(name)
                    {
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::QueueProduction { type_id },
                        )];
                    }
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
        // (miners up to the target), then the core tank until
        // [`CORE_BEFORE_SPECIALISTS`] combat slots have been issued, then a
        // wheel that sends every [`SPECIALIST_EVERY_VEHICLE`]th slot to the
        // roster mix that reads the battlefield and the rest back to the
        // core. The mix rides issued slots, not standing vehicles: under
        // fire the army dies faster than the queue replaces it, and a
        // live-count gate stays on core tanks forever (observed: a whole
        // game with no IFV while the line hovered under six). The tier-2
        // tank takes over the core the moment its cameo lights up (the
        // battle lab standing); until then the side's first tank rolls. A
        // queue entered while short on credits simply waits for the money,
        // as a native production event does.
        let vehicle_busy = queue
            .iter()
            .any(|item| item.queue_category == crate::sim::production::ProductionCategory::Vehicle);
        let economy_first = opening.harvester.is_some_and(|miner| {
            // Two miners per standing dock, floored at the opening target:
            // the growth tail's refinery rule (below) builds the next dock
            // while the fleet saturates the standing ones, and the fleet
            // must then grow to match or the rule starves itself.
            let refineries = self.count_structures_of_type(sim, opening.refinery);
            self.count_units_of_type(sim, miner) < (refineries * 2).max(HARVESTER_TARGET)
        });
        // The tier-2 tank takes over once the battle lab stands. The lab
        // standing is the switch's own condition: `cameo_draw_state` mirrors
        // only the sidebar's draw brightness (`StripClass::Draw`, no
        // prerequisite query — `Recalculate` owns refusal), so it stays
        // `enabled` long before the lab exists and would switch the queue to
        // a tank the house cannot honestly field.
        let tier2_ready = self.count_structures_of_type(sim, opening.battle_lab) >= 1
            && sim.interner.get(opening.tier2_tank).is_some();
        let core_tank = if tier2_ready {
            opening.tier2_tank
        } else {
            opening.main_tank
        };
        let vehicle_want = if economy_first {
            opening.harvester
        } else if self.vehicle_slots < CORE_BEFORE_SPECIALISTS as u32 {
            Some(core_tank)
        } else if self.vehicle_slots % SPECIALIST_EVERY_VEHICLE == SPECIALIST_EVERY_VEHICLE - 1 {
            // The wheel's specialist slot: the roster role furthest behind
            // its demanded share — lifted against hostile air or an
            // out-tanking enemy — claims it, and an entry this house cannot
            // field never does. With every share met the core keeps the
            // line rolling: the wave pipeline refills instead of resting at
            // a capped-out mix.
            self.army_pick(sim, rules, opening.vehicles)
                .or(Some(core_tank))
        } else {
            Some(core_tank)
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
            if !economy_first {
                self.vehicle_slots += 1;
            }
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::QueueProduction { type_id },
            )];
        }

        // The barracks stands: keep its queue on the side's basic soldier
        // until the core fills, then the same battlefield mix over the
        // infantry roster.
        let infantry_busy = queue.iter().any(|item| {
            item.queue_category == crate::sim::production::ProductionCategory::Infantry
        });
        // The same wheel as the vehicles: core soldiers for the first
        // [`CORE_BEFORE_SPECIALISTS`] infantry slots, then every
        // [`SPECIALIST_EVERY_INFANTRY`]th slot to the mix.
        let infantry_want = if self.infantry_slots < CORE_BEFORE_SPECIALISTS as u32 {
            Some(opening.basic_infantry)
        } else if self.infantry_slots % SPECIALIST_EVERY_INFANTRY == SPECIALIST_EVERY_INFANTRY - 1 {
            // The same mix over the infantry roster, the core soldier
            // keeping the line rolling once every share is met.
            self.army_pick(sim, rules, opening.infantry)
                .or(Some(opening.basic_infantry))
        } else {
            Some(opening.basic_infantry)
        };
        if !infantry_busy
            && self.owns_structure_of_type(sim, opening.barracks)
            && let Some(name) = infantry_want
            && let Some(ty) = rules.object(name)
            && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
            // The same banked-credits gate: soldiers wait for their own
            // cost while a tech-climb piece claims the income flow.
            && !(tech_climb_busy
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            self.infantry_slots += 1;
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

    /// How many of the house's combat units — vehicle or infantry; the
    /// engine files foot soldiers and the Rocketeer under `Infantry`, tanks
    /// under `Unit` — are exactly this retail type name. The mix's fielded
    /// count reads both: a soldiers-only count reads zero for an infantry
    /// roster and the mix would never see its shares met.
    fn count_combat_of_type(&self, sim: &crate::sim::world::Simulation, type_name: &str) -> usize {
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
                    && sim.interner.resolve(entity.type_ref()) == type_name
            })
            .count()
    }

    /// How many combat units of exactly this retail type name stand under
    /// hostile flags — the mix's air-pressure reading counts the enemy's
    /// aircraft one type at a time.
    fn count_enemy_units_of_type(
        &self,
        sim: &crate::sim::world::Simulation,
        type_name: &str,
    ) -> usize {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                self.is_enemy(sim, entity.owner())
                    && matches!(
                        entity.category(),
                        crate::map::entities::EntityCategory::Unit
                            | crate::map::entities::EntityCategory::Infantry
                    )
                    && sim.interner.resolve(entity.type_ref()) == type_name
            })
            .count()
    }

    /// The hostile vehicle count — the armor-pressure reading's enemy side.
    fn count_enemy_vehicles(&self, sim: &crate::sim::world::Simulation) -> usize {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                self.is_enemy(sim, entity.owner())
                    && entity.category() == crate::map::entities::EntityCategory::Unit
            })
            .count()
    }

    /// The house's own vehicle count — the armor-pressure reading's home
    /// side.
    fn count_own_vehicles(&self, sim: &crate::sim::world::Simulation) -> usize {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Unit
            })
            .count()
    }

    /// The specialist a wheel slot goes to once the core has its
    /// [`CORE_BEFORE_SPECIALISTS`] slots: the roster role furthest behind
    /// its demanded share. Hostile aircraft lift anti-air's demand to
    /// [`ROLE_SHARE_BOOST`] — the mix fields answer guns instead of
    /// losing the base to air it cannot touch — and an enemy that
    /// out-tanks this house lifts armor-crackers the same way. A role
    /// whose entries this house cannot field (country-locked types such
    /// as the Germans' Tank Destroyer, a hero past its `BuildLimit`)
    /// passes its turn to the next role behind, so the queue never rests
    /// on a cameo that stays dark. `None` — every role meets its share —
    /// hands the slot back to the caller's core.
    fn army_pick(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        roster: &[RosterEntry],
    ) -> Option<&'static str> {
        let enemy_air: usize = AIR_THREAT_TYPES
            .iter()
            .map(|&name| self.count_enemy_units_of_type(sim, name))
            .sum();
        let anti_air_demand = if enemy_air > 0 {
            ROLE_SHARE_BOOST
        } else {
            ROLE_SHARE_BASE
        };
        let anti_armor_demand = if self.count_enemy_vehicles(sim) > self.count_own_vehicles(sim) {
            ROLE_SHARE_BOOST
        } else {
            ROLE_SHARE_BASE
        };
        let demands = [
            (Role::AntiAir, anti_air_demand),
            (Role::AntiArmor, anti_armor_demand),
            (Role::AntiInfantry, ROLE_SHARE_BASE),
            (Role::Siege, ROLE_SHARE_BASE),
        ];
        for (role, demand) in demands {
            let fielded: usize = roster
                .iter()
                .filter(|entry| entry.role == role)
                .map(|entry| self.count_combat_of_type(sim, entry.name))
                .sum();
            if fielded >= demand {
                continue;
            }
            if let Some(entry) = roster.iter().find(|entry| {
                entry.role == role
                    && rules.object(entry.name).is_some_and(|ty| {
                        crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
                    })
            }) {
                return Some(entry.name);
            }
        }
        None
    }

    /// Where a charged superweapon lands. The Iron Curtain is the one
    /// friendly special — its invulnerability goes on this house's own
    /// armor (organic infantry under the curtain take damage instead), so
    /// it centers the first standing vehicle. The storm and the dominator
    /// strike the enemy where it hurts most: their war factories if any
    /// stand, else any structure, else the hostile mass center.
    fn superweapon_target(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
    ) -> Option<(u16, u16)> {
        if opening.super_weapon_targets_self {
            let vehicle = sim.substrate.entities.iter_sorted().find(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Unit
            })?;
            return Some((vehicle.1.position.rx(), vehicle.1.position.ry()));
        }
        let cell = self
            .enemy_priority_targets(sim)
            .first()
            .map(|&(_, cell)| cell)
            .or_else(|| self.enemy_center(sim))?;
        Some((cell.0 as u16, cell.1 as u16))
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

    /// A retail Battle like [`retail_battle`] with the local house's country
    /// and the lobby's superweapon toggle chosen by the caller; the local
    /// house's identity.
    fn retail_battle_as(
        country: crate::skirmish_launch::LaunchCountry,
        super_weapons: bool,
    ) -> (
        crate::headless_scenario::HeadlessScenario,
        crate::sim::intern::InternedId,
    ) {
        use crate::skirmish_launch::AiDifficulty;

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        // A four-start map: fewer start waypoints than houses would seed the
        // extra starts at random, which the lobby never allows.
        let map = "XMP03T4.MAP";
        let mut session = crate::headless_scenario::battle_session(
            map,
            &[(
                crate::skirmish_launch::LaunchCountry::Russia,
                AiDifficulty::Easy,
            )],
        );
        session.local_slot_mut().country = country;
        session.options.super_weapons = super_weapons;
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

    /// The growth slice: past the fixed ladder the base keeps expanding and
    /// the army mixes its roster in, and with superweapons enabled the
    /// opening's weather controller stands — the chain the lobby toggle
    /// owns, exercised end to end.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_growth_mixes_the_roster_and_climbs_to_the_superweapon() {
        let (mut scenario, human) =
            retail_battle_as(crate::skirmish_launch::LaunchCountry::America, true);
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let structures = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
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
        let units = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
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

        // End-state counts are war reports, not build logs: by 42000 ticks
        // the enemy has razed factories (observed: a ladder-complete base
        // read GAWEAP=1 mid-rebuild), so the assertion asks what stood at
        // each tick's best moment, not what survived to the last one.
        #[derive(Default)]
        struct Seen {
            superweapon: usize,
            factories: usize,
            refineries: usize,
            power: usize,
            ifv: usize,
            guardian: usize,
        }
        let seen = std::cell::RefCell::new(Seen::default());
        let tick = drive_until(&mut scenario, &mut ai, 42000, |scenario| {
            let mut seen = seen.borrow_mut();
            seen.superweapon = seen.superweapon.max(structures(scenario, "GAWEAT"));
            seen.factories = seen.factories.max(structures(scenario, "GAWEAP"));
            seen.refineries = seen.refineries.max(structures(scenario, "GAREFN"));
            seen.power = seen.power.max(structures(scenario, "GAPOWR"));
            seen.ifv = seen.ifv.max(units(scenario, "FV"));
            seen.guardian = seen.guardian.max(units(scenario, "GGI"));
            seen.superweapon >= 1 && seen.factories >= 4 && seen.ifv >= 1 && seen.guardian >= 1
        });
        let seen = seen.into_inner();
        // The census names the vehicle line's every member and the queue's
        // current items: which gate held the mix back is a question about
        // counts (miners, core tanks, army size) and the queue's contents,
        // and one slow rerun must answer it outright.
        let queue_names = |scenario: &crate::headless_scenario::HeadlessScenario| {
            crate::sim::production::queue_view_for_owner(
                scenario.sim(),
                &scenario.runtime.resources.rules,
                scenario.sim().interner.resolve(human),
            )
            .iter()
            .map(|item| {
                format!(
                    "{:?}:{}",
                    item.queue_category,
                    scenario.sim().interner.resolve(item.type_id)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
        };
        // One combined assertion with the full census: a slow retail test
        // must explain its whole end state in one panic, not one fact per
        // rerun.
        assert!(
            seen.superweapon >= 1 && seen.factories >= 4 && seen.ifv >= 1 && seen.guardian >= 1,
            "growth slice incomplete at tick {tick}: GAWEAT={} GAWEAP={} refineries={} \
             power={} FV={} GGI={} credits={} miners={} MTNK={} SREF={} E1={} queue=[{}]",
            seen.superweapon,
            seen.factories,
            seen.refineries,
            seen.power,
            seen.ifv,
            seen.guardian,
            crate::sim::production::credits_for_owner(
                scenario.sim(),
                scenario.sim().interner.resolve(human),
            ),
            units(&scenario, "CMIN"),
            units(&scenario, "MTNK"),
            units(&scenario, "SREF"),
            units(&scenario, "E1"),
            queue_names(&scenario),
        );
    }

    /// The Soviet house climbs on its own radar class only. The native
    /// CanBuild answers Yes without prerequisites for a house no human
    /// controls, so the earlier whole-class scan let a Soviet house queue
    /// the Allied Airforce Command (its `Owner=` lists every side); the
    /// side-scoped candidates keep GAAIRC and its American twin out of a
    /// Russian base.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn soviet_climb_builds_the_soviet_radar_class_only() {
        let (mut scenario, human) =
            retail_battle_as(crate::skirmish_launch::LaunchCountry::Russia, true);
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let structures = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
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

        // The Soviet opening is pricier than the Allied one (a power plant
        // costs 600 against 300, the radar 2000 against 1000), so the Fair
        // climb needs the wider budget; the census in the panic separates a
        // slow climb from a stalled one.
        let tick = drive_until(&mut scenario, &mut ai, 40000, |scenario| {
            structures(scenario, "NARADR") >= 1
        });
        assert!(
            structures(&scenario, "NARADR") >= 1,
            "no Soviet radar at tick {tick}: power={} refineries={} barracks={} \
             factory={} NARADR={} NATECH={}",
            structures(&scenario, "NAPOWR"),
            structures(&scenario, "NAREFN"),
            structures(&scenario, "NAHAND"),
            structures(&scenario, "NAWEAP"),
            structures(&scenario, "NARADR"),
            structures(&scenario, "NATECH"),
        );
        assert_eq!(
            structures(&scenario, "GAAIRC"),
            0,
            "no Allied Airforce Command in a Soviet base"
        );
        assert_eq!(structures(&scenario, "AMRADR"), 0, "nor its American twin");
    }

    /// With the lobby's superweapon toggle off the chain never starts: the
    /// Weather Controller's cameo stays dark (CanBuild refuses the
    /// `DisableableFromShell=yes` building while the toggle is off), so no
    /// instance is granted, charged or fired while the rest of the climb
    /// walks past the step.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn superweapons_off_skips_the_superweapon_entirely() {
        let (mut scenario, human) =
            retail_battle_as(crate::skirmish_launch::LaunchCountry::America, false);
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        let structures = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
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

        // The battle lab standing proves the ladder walked past the
        // superweapon step (it sits between the lab and the expansion)
        // instead of stalling on a dark cameo.
        let tick = drive_until(&mut scenario, &mut ai, 28000, |scenario| {
            structures(scenario, "GATECH") >= 1
        });
        assert!(
            structures(&scenario, "GATECH") >= 1,
            "the climb passed the battle lab (tick {tick})"
        );
        assert_eq!(
            structures(&scenario, "GAWEAT"),
            0,
            "the superweapon building never stood"
        );
        // The engine constructs every super's entry for a house at scenario
        // start (`SuperClass` constructor, inactive); the toggle-off pass
        // merely never grants one (`Deactivate`: "the entry stays"). And a
        // country's own support powers stay out of scope: America's
        // paratroopers (`SuperWeapon=AmericanParaDropSpecial` on the country
        // section) are granted with the country, not by a
        // `DisableableFromShell` building. So the proof of "off" is the
        // side's superweapon special — the storm — never activating, not an
        // empty or fully-inactive map.
        let storm = scenario
            .sim()
            .interner
            .get("LightningStormSpecial")
            .and_then(|sw_type| {
                scenario
                    .sim()
                    .super_weapons()
                    .get(&human)
                    .and_then(|house_sws| house_sws.get(&sw_type))
            });
        assert!(
            storm.is_none_or(|sw| !sw.is_active()),
            "the storm special is never granted while the toggle is off"
        );
    }
}
