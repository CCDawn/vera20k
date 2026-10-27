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

/// The Allied jet wing's target size (retail Intruders `ORCA` / Korea's
/// Black Eagles `BEAG`, both `Factory=AircraftType` off the Airforce
/// Command). Four keeps the wing a real strike force without starving
/// the ground queues the same wallet feeds.
const AIR_WING_TARGET: usize = 4;

/// Ticks one airstrike sortie waits before the next. The jets return,
/// land and rearm natively after a strike; re-ordering them mid-sortie
/// reroutes the wing for nothing.
const AIRSTRIKE_RETRY_TICKS: u32 = 900;

/// Placement strikes: three `PlaceReadyBuilding` orders consumed while
/// the item still sits ready means the command layer keeps dropping the
/// site it picked — the branch then pauses that type's placement for
/// [`PLACE_PAUSE_TICKS`] so the one-command budget runs past the wedge.
const PLACE_STRIKES: u32 = 3;
const PLACE_PAUSE_TICKS: u32 = 900;

/// Ticks before a war factory's rally correction is re-attempted: a
/// desired ring cell the base's own buildings occupy gets the SetRally
/// refused, and without pacing the re-issue loop starved the whole
/// command chain (observed).
const RALLY_RETRY_TICKS: u32 = 900;

/// Live scouts the opening keeps on the map. Retail never scouts (its
/// dogs sit as base guards), so the fork's chain is its own: two cheap
/// fast eyes (a 200-credit dog at sight 9) cover the map's dark half
/// without taxing the barracks rotation more than a pair of soldiers.
const SCOUT_TARGET: usize = 2;

/// Combat-slot wheels: past the core, every Nth issued combat slot goes
/// to the roster mix and the rest back to the core type. A live-count
/// gate measures the war, not the build program — under attrition the
/// standing army never clears the core floor (observed: a whole game
/// with no IFV while the queue rebuilt tanks), so the mix rides issued
/// slots, which only ever grow.
const SPECIALIST_EVERY_VEHICLE: u32 = 4;
const SPECIALIST_EVERY_INFANTRY: u32 = 3;

/// Defense caps: two pillboxes hold the perimeter once tanks roll, AA
/// rises only against air that actually fields (the limb re-reads it
/// every slot, so peace drops the queue), and two heavy towers stand
/// behind the radar their prerequisite names.
const BASIC_DEFENSE_TARGET: usize = 2;
const AA_DEFENSE_CAP: usize = 2;
const HEAVY_DEFENSE_CAP: usize = 2;

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
    /// Super's treatment plus a wallet floor topped up every tick: the
    /// spectacle tier for watching the strategy spend without an economy
    /// behind it. Fork-only showcase row — strength claims never cite it,
    /// and the income-recovery cancel rule stays asleep under it (a
    /// wallet above every buildable's cost can finish anything queued).
    Unlimited,
}

/// The wallet floor the Unlimited tier tops up to every ordinary tick
/// (`sim_tick` and the benches apply the same rule). Comfortably above the
/// priciest retail buildable so every queue the house owns rolls at once.
pub const UNLIMITED_WALLET_FLOOR: i32 = 1_000_000;

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
/// mix targets it at. `needs` names a building that must stand first:
/// the native `CanBuild` skips `Prerequisite=` chains for a house no
/// human controls (the quirk behind the radar side-scoping and the
/// tier-2 lab gate), so an entry whose retail prerequisite carries real
/// weight — the battle lab behind the Kirov, the Siege Chopper and the
/// Floating Disk — gates itself here instead of trusting the cameo,
/// which draws bright long before the lab exists.
struct RosterEntry {
    name: &'static str,
    role: Role,
    /// A structure that must stand before this entry may queue
    /// (retail `Prerequisite=` evidence, per entry).
    needs: Option<&'static str>,
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
    /// The side's base defenses by retail section name — a cheap always-on
    /// pillbox, the anti-air answer, and the late heavy tower. Allied AA is
    /// `NASAM` (the Patriot Missile) and its heavy tower's section is
    /// `ATESLA` ("Allied Prism Cannon", image GAPRIS); the Soviet heavy is
    /// `[TESLA]` — the firing-anim prefix `NATSLA_` names no building.
    /// Yuri's Gatling Cannon doubles as pillbox and AA. Each wants
    /// BARRACKS plus its side's yard and the heavies want POWER,RADAR (the
    /// psychic radar for Yuri's tower), all ladder steps the defense limb
    /// sits behind.
    defense_basic: &'static str,
    defense_aa: &'static str,
    defense_heavy: &'static str,
    /// Aircraft-tab types the air wing fields, preference order (Korea's
    /// Black Eagle `BEAG` first, the shared Intruder `ORCA` behind — the
    /// cameo's country gate skips the one this house may not take). Only
    /// the Allied side has an aircraft factory at retail
    /// (`Factory=AircraftType` on `GAAIRC`/`AMRADR`); Soviet and Yuri air
    /// power (Kirov, Siege Chopper, Floating Disk) rides `VehicleTypes`
    /// and their war factory, so their tables stay empty here.
    aircraft: &'static [&'static str],
    /// The side's scout: a barracks-cheap fast eye the fork keeps
    /// [`SCOUT_TARGET`] of alive and walks into unrevealed territory.
    /// Retail's own dogs are counter-infiltration base guards — the
    /// scouting chain is fork-original — so the pick is purely economics:
    /// the 200-credit dog (speed 8, sight 9) for both army sides, Yuri's
    /// initiate (also sight 9, half the speed) where no dog exists.
    scout: &'static str,
    /// The side's OpenTopped transport (retail `OpenTopped=yes` with
    /// passengers who fire from inside): only the Allied Battle Fortress
    /// qualifies — a five-seat late-game piece behind the battle lab the
    /// loading pair boards with the roster's anti-armor infantry (the
    /// Guardian GI's deployed launcher is the classic loadout). The
    /// other sides keep `None` and the boarding branch sleeps.
    fortress: Option<&'static str>,
    /// Combat vehicles the army mixes in past its core tank, each with
    /// its role; the core keeps the remaining slots. Country-locked
    /// entries stay in the table — CanBuild's `RequiredHouses=` gate
    /// skips them for other houses.
    vehicles: &'static [RosterEntry],
    /// Infantry the barracks mixes in past its core soldier.
    infantry: &'static [RosterEntry],
    /// The side's hero (retail `BuildLimit=1`): commissioned directly at
    /// the first specialist slot once the battle lab stands, instead of
    /// waiting for a whole-role deficit window in the rotation.
    hero: &'static str,
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
        hero: "TANY",
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
        // gates the country lock, the `needs` gate the lab or the
        // control-center prerequisite the AI house's `CanBuild` skips
        // (the prerequisite quirk behind the radar side-scoping).
        vehicles: &[
            RosterEntry {
                name: "FV",
                role: Role::AntiAir,
                needs: None,
            },
            RosterEntry {
                name: "MGTK",
                role: Role::AntiArmor,
                needs: Some("GATECH"),
            },
            RosterEntry {
                name: "TNKD",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "ROBO",
                role: Role::AntiInfantry,
                needs: Some("GAROBO"),
            },
            RosterEntry {
                name: "BFRT",
                role: Role::Siege,
                needs: Some("GATECH"),
            },
        ],
        infantry: &[
            // The hero leads her role: rotation breaks standing-count ties
            // by roster order, so the hero listed first claims the fresh
            // slot (and the rebuild after she falls) while the cheaper
            // entries rotate behind her — listed last she starved again
            // under attrition, every death resetting the tie to the top
            // of the list. Her `BuildLimit=1` darkens her own cameo once
            // she stands (retail Prerequisite=GAPILE,GATECH).
            RosterEntry {
                name: "TANY",
                role: Role::AntiInfantry,
                needs: Some("GATECH"),
            },
            // The Guardian GI's deployed MissileLauncher is also the
            // Allied anti-air answer (AAHeatSeeker2); filed under
            // AntiArmor because its standing role cracks vehicles.
            RosterEntry {
                name: "GGI",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "JUMPJET",
                role: Role::AntiAir,
                needs: None,
            },
            RosterEntry {
                name: "GHOST",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "SNIPE",
                role: Role::AntiInfantry,
                needs: None,
            },
        ],
        defense_basic: "GAPILL",
        defense_aa: "NASAM",
        defense_heavy: "ATESLA",
        aircraft: &["BEAG", "ORCA"],
        scout: "ADOG",
        fortress: Some("BFRT"),
    },
    Opening {
        construction_yard: "NACNST",
        power: "NAPOWR",
        refinery: "NAREFN",
        barracks: "NAHAND",
        war_factory: "NAWEAP",
        radar: "NARADR",
        battle_lab: "NATECH",
        hero: "BORIS",
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
        // Desolator Arabs-only; CanBuild gates them per house. The
        // Kirov `ZEP` and Siege Chopper `SCHP` are `VehicleTypes` (war
        // factory aircraft, not the aircraft tab) behind `NATECH` —
        // the needs gate carries that prerequisite the AI house's
        // `CanBuild` would skip.
        vehicles: &[
            RosterEntry {
                name: "HTK",
                role: Role::AntiAir,
                needs: None,
            },
            RosterEntry {
                name: "DRON",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "TTNK",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "V3",
                role: Role::Siege,
                needs: None,
            },
            RosterEntry {
                name: "SCHP",
                role: Role::Siege,
                needs: Some("NATECH"),
            },
            RosterEntry {
                name: "ZEP",
                role: Role::Siege,
                needs: Some("NATECH"),
            },
        ],
        infantry: &[
            RosterEntry {
                name: "FLAKT",
                role: Role::AntiAir,
                needs: None,
            },
            // The hero leads his role for the same tie-break reason as
            // Tanya's table position (retail Prerequisite=NAHAND,NATECH,
            // `BuildLimit=1`).
            RosterEntry {
                name: "BORIS",
                role: Role::AntiArmor,
                needs: Some("NATECH"),
            },
            RosterEntry {
                name: "SHK",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "IVAN",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "DESO",
                role: Role::AntiInfantry,
                needs: None,
            },
        ],
        defense_basic: "NALASR",
        defense_aa: "NAFLAK",
        defense_heavy: "TESLA",
        aircraft: &[],
        scout: "DOG",
        fortress: None,
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
        hero: "YURIPR",
        harvester: None,
        main_tank: "LTNK",
        tier2_tank: "MIND",
        basic_infantry: "INIT",
        super_weapon_building: "YAPPET",
        super_weapon_type: "PsychicDominatorSpecial",
        super_weapon_targets_self: false,
        // The Gattling Tank's AAGattling fires the AA-only Invisible4.
        // The Magnetron lifts armor, the Chaos Drone gas masses. The
        // Floating Disk `DISK` is a `VehicleTypes` war-factory aircraft
        // behind `YATECH` — the needs gate carries the prerequisite the
        // AI house's `CanBuild` would skip. `YURI` is the Yuri Clone
        // (retail Prerequisite=YABRCK,NAPSIS, cost 800); the hero is
        // `YURIPR` (Yuri Prime, YABRCK,YATECH, `BuildLimit=1`).
        vehicles: &[
            RosterEntry {
                name: "YTNK",
                role: Role::AntiAir,
                needs: None,
            },
            RosterEntry {
                name: "TELE",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "CAOS",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "DISK",
                role: Role::Siege,
                needs: Some("YATECH"),
            },
        ],
        infantry: &[
            RosterEntry {
                name: "BRUTE",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "VIRUS",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "YURI",
                role: Role::AntiArmor,
                needs: None,
            },
            // The hero: long-range mind control picks the enemy's best
            // off, so he files under Siege (retail YABRCK,YATECH,
            // `BuildLimit=1`).
            RosterEntry {
                name: "YURIPR",
                role: Role::Siege,
                needs: Some("YATECH"),
            },
        ],
        defense_basic: "YAGGUN",
        defense_aa: "YAGGUN",
        defense_heavy: "YAPSYT",
        aircraft: &[],
        scout: "INIT",
        fortress: None,
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
    /// Earliest tick the air wing may fly its next sortie; see
    /// [`AIRSTRIKE_RETRY_TICKS`]. Pinned state, never read from the runtime.
    next_airstrike_tick: u32,
    /// Whether a superweapon has ever stood. The ladder's superweapon rung
    /// is once-per-game: a razed 5000-credit storm never rebuilds, because
    /// the observed rebuild held the one building slot to the end of the
    /// game and starved the expansion and the guns behind it. Pinned state,
    /// never read from the runtime.
    sw_ever_stood: bool,
    /// Whether the hero's commission has issued at least once; the
    /// reservation hands the hero the first specialist slot past the lab.
    /// Pinned state, never read from the runtime.
    hero_issued: bool,
    /// Whether the fortress's one-off commission has issued: the loading
    /// pair boards cargo into a standing fortress, so the vehicle rotation
    /// owes the base exactly one. Pinned state, never read from the runtime.
    fortress_issued: bool,
    /// Whether the anti-air insurance vehicle's commission has issued: the
    /// rotation owes the base exactly one of the roster's AntiAir entries,
    /// so the side's mobile AA answer stands before hostile air arrives.
    /// Pinned state, never read from the runtime.
    aa_issued: bool,
    /// The tick a war factory's rally correction may be re-attempted.
    /// Pinned state, never read from the runtime.
    rally_gate: std::collections::HashMap<u64, u32>,
    /// How many `PlaceReadyBuilding` orders this type consumed while
    /// still sitting ready (a placement the command layer keeps
    /// dropping). Pinned state, never read from the runtime.
    place_stuck: std::collections::HashMap<InternedId, u32>,
    /// The tick a wedged type's placement pause lifts: while paused the
    /// ready item is skipped so the orders chain runs past it. Pinned
    /// state, never read from the runtime.
    place_pause_until: std::collections::HashMap<InternedId, u32>,
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
            next_airstrike_tick: 0,
            next_sw_fire_tick: 0,
            sw_ever_stood: false,
            hero_issued: false,
            fortress_issued: false,
            aa_issued: false,
            place_stuck: std::collections::HashMap::new(),
            place_pause_until: std::collections::HashMap::new(),
            rally_gate: std::collections::HashMap::new(),
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
    /// tick; else post idle units on an Area Guard), fly the air wing's
    /// sortie, board the fortress's anti-armor cargo, walk an idle scout
    /// toward the map's nearest dark cell, keep the building queue walking
    /// the ladder, hold the war factories' rallies around the yard, keep
    /// the vehicle queue on miners-then-tanks, keep the barracks queue on
    /// the side's basic soldier. See the module docs for the determinism
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
            // Fork: attempts spent or pacing between them, the MCV waits —
            // it must not starve every branch below. Returning empty here
            // froze the whole game when a second AMCV appeared mid-run and
            // burned the attempts: every tick returned nothing while the
            // wallet piled up unspent and the army died unrebuilt
            // (observed: 24000 frozen ticks from the census). At the
            // opening this fall-through is inert — no yard, no factories,
            // nothing ready: every branch below has nothing to issue.
        }
        let Some(opening) = self.opening(sim) else {
            return Vec::new();
        };
        // The ladder's superweapon rung is once-per-game: remember the
        // first one standing, so a razed storm never rebuilds (see the
        // walk below for the starvation that motivated the latch).
        if self.count_structures_of_type(sim, opening.super_weapon_building) >= 1 {
            self.sw_ever_stood = true;
        }

        // Place one completed building next to the construction yard. A
        // placement the command layer keeps dropping (the ring-scan site
        // failing admission at apply time) leaves the building ready
        // forever, and this branch — the first return in the chain —
        // then re-issues it every tick and starves every branch below
        // it: a whole 24000-tick stretch ran with an empty queue, a fat
        // wallet and no jet while some ready item soaked up every
        // command (observed; the wave branch's `launched` list guards
        // the same dropped-dispatch class). Three strikes on the same
        // still-ready type pauses that type's placement so production
        // continues; the pause lifts and placement retries.
        let ready_stock = crate::sim::production::ready_buildings_for_owner(sim, rules, &owner);
        // A type absent from the ready stock placed or drained: its
        // strike count starts over.
        self.place_stuck
            .retain(|type_id, _| ready_stock.iter().any(|item| item.type_id == *type_id));
        self.place_pause_until.retain(|_, until| tick < *until);
        if let Some(ready) = ready_stock
            .iter()
            .filter(|item| !self.place_pause_until.contains_key(&item.type_id))
            .last()
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
            let stuck = self.place_stuck.entry(ready.type_id).or_insert(0);
            *stuck += 1;
            if *stuck >= PLACE_STRIKES {
                self.place_stuck.remove(&ready.type_id);
                self.place_pause_until
                    .insert(ready.type_id, tick + PLACE_PAUSE_TICKS);
            }
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

        // The air wing's sortie. Idle jets sit parked on their pads — a
        // jet's bombs matter only aimed at the enemy — so when the cooldown
        // clears, every idle aircraft attacks the top enemy priority target
        // as one flight; the cooldown then holds while they fly, return,
        // land and rearm the native way. The wing rides its own aircraft
        // queue (the Airforce Command), never the ground wave's slots.
        if tick >= self.next_airstrike_tick
            && let Some(&(target, _)) = self.enemy_priority_targets(sim).first()
        {
            let flight: Vec<u64> = sim
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == self.house
                        && entity.category() == crate::map::entities::EntityCategory::Aircraft
                        && matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                        )
                })
                .map(|(id, _)| id)
                .collect();
            if !flight.is_empty() {
                self.next_airstrike_tick = tick + AIRSTRIKE_RETRY_TICKS;
                return flight
                    .into_iter()
                    .map(|jet| {
                        CommandEnvelope::new(
                            self.house,
                            at,
                            Command::Attack {
                                attacker_id: jet,
                                target_id: target,
                            },
                        )
                    })
                    .collect();
            }
        }

        // The fortress's boarding call. The Battle Fortress is the one
        // roster entry the wave system never claims: neither it nor the
        // infantry it wants sit in the core-type sets, so both idle at
        // home. The pairing boards the roster's anti-armor infantry (the
        // Guardian GI's deployed launcher is the classic loadout) into the
        // first fortress with a free seat — the same Enter click a player
        // makes, one boarding call per tick, the passenger's order gate
        // pacing the walk.
        if let (Some(fortress_name), Some(cargo_name)) = (
            opening.fortress,
            opening
                .infantry
                .iter()
                .find(|entry| entry.role == Role::AntiArmor)
                .map(|entry| entry.name),
        ) && let Some((fortress_id, fortress_entity)) =
            sim.substrate.entities.iter_sorted().find(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Unit
                    && sim.interner.resolve(entity.type_ref()) == fortress_name
                    && entity
                        .passenger_role
                        .cargo()
                        .is_some_and(|cargo| cargo.count() < cargo.capacity())
            })
        {
            let fort_cell = (
                fortress_entity.position.rx() as i16,
                fortress_entity.position.ry() as i16,
            );
            // The nearest due idle cargo, ties by the substrate's stable
            // order; a passenger already carried rides in limbo and never
            // re-boards.
            let mut boarding: Option<(u32, u64)> = None;
            for (id, entity) in sim.substrate.entities.iter_sorted() {
                let dx = entity.position.rx() as i16 - fort_cell.0;
                let dy = entity.position.ry() as i16 - fort_cell.1;
                if self.order_gate.get(&id).is_none_or(|&due| tick >= due)
                    && entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Infantry
                    && sim.interner.resolve(entity.type_ref()) == cargo_name
                    && !entity.lifecycle.in_limbo()
                    && matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
                {
                    let distance =
                        (i32::from(dx) * i32::from(dx) + i32::from(dy) * i32::from(dy)) as u32;
                    if boarding.is_none_or(|(kept, _)| distance < kept) {
                        boarding = Some((distance, id));
                    }
                }
            }
            if let Some((_, boarder)) = boarding {
                self.gate_idle_order(boarder, tick);
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::clicked_mission(
                        boarder,
                        crate::sim::mission::MissionType::Enter,
                        crate::sim::combat::TargetKind::Entity(fortress_id),
                    ),
                )];
            }
        }

        // The scouting chain's dispatch. Retail never lifts its own shroud
        // (its dogs sit posted as base guards), so the fork walks its own:
        // an idle, ungated scout marches to the nearest cell the house has
        // never revealed. The march reveals on the way (sight joins the
        // house as it moves) and arrival posts it there until the next due
        // window re-points it into what is still dark. The dispatch sits
        // below the combat branches: production and defense own the busy
        // ticks, the dogs take the spare ones.
        let mut idle_scouts = sim.substrate.entities.iter_sorted().filter(|(id, entity)| {
            self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Infantry
                && sim.interner.resolve(entity.type_ref()) == opening.scout
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        });
        if let Some((scout, entity)) = idle_scouts.next()
            && let Some(cell) = self.nearest_unrevealed_cell(
                sim,
                (entity.position.rx() as i16, entity.position.ry() as i16),
            )
        {
            self.gate_idle_order(scout, tick);
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::Move {
                    entity_id: scout,
                    target_rx: cell.0,
                    target_ry: cell.1,
                },
            )];
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
        // A base with no standing refinery has no income, and nothing in
        // the building queue can replace it: an expensive item already
        // drawing the last trickle (the 5000-credit superweapon is the
        // classic) never finishes on its own and nothing cancels it, so
        // the refinery rebuild never gets the slot while the climb's
        // credit throttle keeps the unit queues closed — the whole base
        // froze exactly this way in live play after a counter-raid razed
        // the refineries. The native player hits ABANDON
        // (`HouseClass::Abandon_Production @ 0x004FAA10`) and rebuilds
        // the economy first; the AI likewise cancels every queued
        // structure that is not itself the economy (refinery, power
        // plant) while no refinery stands — but only when the wallet
        // genuinely cannot finish the item (a topped-up wallet, the
        // Unlimited tier's floor, completes anything and must keep it).
        if self.count_structures_of_type(sim, opening.refinery) == 0 {
            let wallet = crate::sim::production::credits_for_owner(sim, &owner);
            let is_economy = |name: &str| name == opening.refinery || name == opening.power;
            if let Some(item) = queue.iter().find(|item| {
                let name = sim.interner.resolve(item.type_id);
                !is_economy(name)
                    && matches!(
                        item.queue_category,
                        crate::sim::production::ProductionCategory::Building
                            | crate::sim::production::ProductionCategory::Defense
                    )
                    && rules.object(name).is_some_and(|ty| wallet < ty.cost)
            }) {
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::CancelProductionByType {
                        type_id: item.type_id,
                        all: false,
                    },
                )];
            }
        }
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
        // Any battle tank standing gates the perimeter guns and the
        // expansion rungs past the opening. The tier-2 tank counts once
        // the lab stands, so the death of the first tanks never freezes
        // the base at its opening size.
        let any_tank = self.count_units_of_type(sim, opening.main_tank)
            + self.count_units_of_type(sim, opening.tier2_tank)
            > 0;
        // A gun still below its cap while the yard stands and tanks roll.
        // While one is wanted the unit queues bank their purchases the way
        // a tech climb does — each item waits for its own full cost — so
        // the wallet refills past the gun's full-cost gate in the defense
        // limb instead of the queues drinking every credit (observed:
        // credits pinned near two for a whole game with zero defenses
        // standing). Mirrors the limb's own conditions exactly: a gun the
        // limb would not arm never banks the army either.
        let guns_wanted = any_tank
            && self
                .structure_center(sim, opening.construction_yard)
                .is_some()
            && (self.count_structures_of_type(sim, opening.defense_basic) < BASIC_DEFENSE_TARGET
                || self.count_structures_of_type(sim, radar_name) >= 1
                    && self.count_structures_of_type(sim, opening.defense_heavy)
                        < HEAVY_DEFENSE_CAP
                || self.enemy_air_count(sim) > 0
                    && self.count_structures_of_type(sim, opening.defense_aa) < AA_DEFENSE_CAP);
        if self
            .structure_center(sim, opening.construction_yard)
            .is_some()
            && !building_busy
        {
            // The defense limb runs before the ladder walk. Behind it the
            // guns are dead code: the walk doubles as the rebuild scan, so
            // a destroyed late rung re-arms every slot-free tick and the
            // guns never once ran (observed: a whole game at zero defenses
            // behind a superweapon rebuild). Anti-air answers air that
            // actually fields, the heavy tower waits behind the radar its
            // prerequisite names, and the pillbox quota holds the perimeter
            // once tanks roll. Placement rides the same yard-centered ring
            // scan every other building uses; Yuri's Gatling Cannon serves
            // both the AA and the pillbox slots.
            if any_tank {
                let defenses = [
                    (
                        self.enemy_air_count(sim) > 0
                            && self.count_structures_of_type(sim, opening.defense_aa)
                                < AA_DEFENSE_CAP,
                        opening.defense_aa,
                    ),
                    (
                        self.count_structures_of_type(sim, radar_name) >= 1
                            && self.count_structures_of_type(sim, opening.defense_heavy)
                                < HEAVY_DEFENSE_CAP,
                        opening.defense_heavy,
                    ),
                    (
                        self.count_structures_of_type(sim, opening.defense_basic)
                            < BASIC_DEFENSE_TARGET,
                        opening.defense_basic,
                    ),
                ];
                for (behind, name) in defenses {
                    if !behind {
                        continue;
                    }
                    // Guns queue only from a wallet that can pay for them
                    // outright: a broke base that queues a 1500-credit
                    // Prism Tower parks it in the one building slot at
                    // zero progress forever (observed: factories stuck at
                    // three while ATESLA held the queue for the rest of
                    // the game), starving the growth tail behind it. Poor
                    // bases expand; rich bases arm.
                    if let Some(ty) = rules.object(name)
                        && crate::sim::production::credits_for_owner(sim, &owner) >= ty.cost
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
            for (index, (name, target)) in ladder.iter().enumerate() {
                if index >= OPENING_STEPS && !any_tank {
                    break;
                }
                // The superweapon is a once-per-game rung: once one has
                // stood, a razed storm does not rebuild — the 5000-credit
                // replacement would hold the one building slot to the end
                // of the game (observed) and starve the expansion and the
                // guns behind it.
                if *name == opening.super_weapon_building && self.sw_ever_stood {
                    continue;
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
                    sim.substrate.entities.get(id).is_some_and(|factory| {
                        factory.rally_cell() != Some(desired)
                            && self.rally_gate.get(&id).is_none_or(|&next| tick >= next)
                    })
                })
        {
            // Fork: pace each factory's correction — a desired cell the
            // base's own later buildings occupy (the crowded ring around
            // the yard) gets its SetRally refused at apply time, and an
            // unpaced re-issue looped here every tick and starved every
            // branch below for the rest of the game (observed: the
            // freeze began as the fourth factory and the guns filled the
            // ring). One attempt per factory per window; the wedge then
            // costs a tick per window instead of all of them.
            self.rally_gate.insert(factory_id, tick + RALLY_RETRY_TICKS);
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

        // The wing's first-jet commission. The air branch sits last in the
        // one-command budget — it only fires while both ground queues hold
        // an item — and there its 1200-credit banked gate loses every
        // wallet race to the cheaper ground items that re-arm the moment
        // they complete: a whole 42000-tick game passed with zero jets
        // while the wing's own factory (the Airforce Command, the same
        // building the ladder's radar step raised) stood ready (observed).
        // Once that factory stands the first jet queues outright — the
        // same existence commission the hero gets — keeping only the
        // tech-climb bank so the lab climb keeps its income, and waiting
        // for its money natively otherwise. Top-ups to the wing target
        // stay in the ordinary low-priority branch below.
        let aircraft_busy = queue.iter().any(|item| {
            item.queue_category == crate::sim::production::ProductionCategory::Aircraft
        });
        let wing: usize = opening
            .aircraft
            .iter()
            .map(|&jet| self.count_aircraft_of_type(sim, jet))
            .sum();
        if !aircraft_busy
            && wing == 0
            && self.owns_structure_of_type(sim, radar_name)
            && let Some(name) = opening.aircraft.iter().copied().find(|&name| {
                // `can_build`, not the cameo: the draw brightness knows
                // nothing of country locks, so the Korea-first table would
                // commission a Black Eagle an Americans house can never
                // produce (the same trap the top-up branch below guards).
                rules.object(name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            })
            && let Some(ty) = rules.object(name)
            && !(tech_climb_busy
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::QueueProduction { type_id },
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
        // The roster's anti-air entry, derived like the fortress's cargo —
        // the side's mobile AA answer, whatever its name.
        let aa_name = opening
            .vehicles
            .iter()
            .find(|entry| entry.role == Role::AntiAir)
            .map(|entry| entry.name);
        let vehicle_want = if economy_first {
            opening.harvester
        } else if self.vehicle_slots >= CORE_BEFORE_SPECIALISTS as u32
            && !self.aa_issued
            && aa_name.is_some_and(|name| {
                rules.object(name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            })
        {
            // The AA insurance: exactly one of the roster's AntiAir entries
            // once the tank core stands, so the side's AA answer exists
            // before hostile air arrives.
            aa_name
        } else if opening.fortress.is_some()
            && !self.fortress_issued
            && self.count_structures_of_type(sim, opening.battle_lab) >= 1
            && opening
                .fortress
                .and_then(|name| rules.object(name))
                .is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
        {
            // The fortress's one-off commission: the loading pair boards
            // cargo into a standing Battle Fortress, and a base that never
            // builds one boards nothing. This and the AA insurance are
            // dedicated existence commissions rather than specialist-slot
            // reservations — a hot war keeps the wheel from reaching its
            // next specialist slot while the barracks rotation drains the
            // bank faster than a 2000-credit item can gather, so a
            // reservation starves and commissions nothing. Once the
            // QueueProduction order is out the factory charges
            // progressively, so no banked-cost wait belongs here.
            // `can_build`, not the cameo — the country-lock lesson again.
            opening.fortress
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
            // While a tech-climb piece or a wanted gun claims the income
            // flow the unit queues spend only banked credits — an item
            // waits until its own cost sits in the bank, so the incoming
            // ore flow stays with the radar, lab or gun instead of
            // splitting three ways and stretching the wait over a tank's
            // build time each. Miners are the income source itself and
            // queue unconditionally.
            && !((tech_climb_busy || guns_wanted)
                && !economy_first
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            if !economy_first {
                self.vehicle_slots += 1;
                if Some(name) == aa_name {
                    self.aa_issued = true;
                }
                if opening.fortress == Some(name) {
                    self.fortress_issued = true;
                }
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
            // The scout reservation claims the first specialist slots the
            // rotation opens: the map's dark half is the whole chain's
            // future (wave targets, defense readings) and costs a pair of
            // soldiers' worth of barracks time. `can_build`, not the
            // cameo — the same country-lock lesson as `army_pick`.
            let scout_want = self.count_combat_of_type(sim, opening.scout) < SCOUT_TARGET
                && rules.object(opening.scout).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                });
            // The hero's commission: once the battle lab stands the hero
            // takes the next specialist slot outright instead of waiting
            // for a whole-role deficit window (observed: Tanya never once
            // fielded while AntiAir and AntiArmor absorbed every slot and
            // the role-mates met the share). `BuildLimit=1` darkens the
            // hero's own cameo once fielded, so after the first commission
            // the reservation yields to the ordinary rotation; a fallen
            // hero re-lights the cameo and the reservation re-arms the
            // replacement.
            if scout_want {
                Some(opening.scout)
            } else if !self.hero_issued
                && self.count_structures_of_type(sim, opening.battle_lab) >= 1
                && rules.object(opening.hero).is_some_and(|ty| {
                    crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
                })
            {
                Some(opening.hero)
            } else {
                // The same mix over the infantry roster, the core soldier
                // keeping the line rolling once every share is met.
                self.army_pick(sim, rules, opening.infantry)
                    .or(Some(opening.basic_infantry))
            }
        } else {
            Some(opening.basic_infantry)
        };
        if !infantry_busy
            && self.owns_structure_of_type(sim, opening.barracks)
            && let Some(name) = infantry_want
            && let Some(ty) = rules.object(name)
            && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
            // The same banked-credits gate: soldiers wait for their own
            // cost while a tech-climb piece or a wanted gun claims the
            // income flow.
            && !((tech_climb_busy || guns_wanted)
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            if name == opening.hero {
                self.hero_issued = true;
            }
            self.infantry_slots += 1;
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::QueueProduction { type_id },
            )];
        }

        // The air wing's top-up queue (busy/wing standings above, at the
        // first-jet commission). Only the Allied side fields aircraft-tab
        // types at retail (`Factory=AircraftType` on GAAIRC/AMRADR — the
        // same building the ladder's radar step already raised), so Soviet
        // and Yuri tables are empty and this branch sleeps. The wing tops
        // up to [`AIR_WING_TARGET`] jets — Korea's Black Eagle first, the
        // shared Intruder behind (the cameo's country gate skips whichever
        // this house may not take) — on its own category queue, so it
        // never competes with the ground queues for their factory slots.
        if !aircraft_busy
            && wing < AIR_WING_TARGET
            && let Some(name) = opening.aircraft.iter().copied().find(|&name| {
                // `can_build`, not the cameo: the draw brightness knows
                // nothing of country locks, so the Korea-first table
                // queued a Black Eagle an Americans house can never
                // produce — the item sat at zero progress and wedged the
                // wing for a whole game (observed). `can_build` sorts the
                // country gate the same way it sorts the radar pair.
                rules.object(name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            })
            && let Some(ty) = rules.object(name)
            && !((tech_climb_busy || guns_wanted)
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
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

    /// How many of the house's aircraft — the engine files pad jets under
    /// `Aircraft`, outside `count_combat_of_type`'s vehicle/infantry pair —
    /// are exactly this retail type name. The wing's standing reads it: a
    /// Unit/Infantry-only count reads zero for a living wing and the
    /// first-jet commission would re-fire forever.
    fn count_aircraft_of_type(
        &self,
        sim: &crate::sim::world::Simulation,
        type_name: &str,
    ) -> usize {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Aircraft
                    && sim.interner.resolve(entity.type_ref()) == type_name
            })
            .count()
    }

    /// The fog grid's nearest cell this house has never revealed, ties
    /// broken by row-major scan order (the substrate order's determinism
    /// discipline). The scouting dispatch walks a dog to this cell; a map
    /// with no dark cell left answers `None` and the chain rests.
    fn nearest_unrevealed_cell(
        &self,
        sim: &crate::sim::world::Simulation,
        from: (i16, i16),
    ) -> Option<(u16, u16)> {
        let fog = &sim.fog;
        let (width, height) = (fog.width(), fog.height());
        let mut best: Option<(u32, (u16, u16))> = None;
        for ry in 0..height {
            for rx in 0..width {
                if fog.is_cell_revealed(self.house, rx, ry) {
                    continue;
                }
                let dx = i32::from(rx) - i32::from(from.0);
                let dy = i32::from(ry) - i32::from(from.1);
                let distance = (dx * dx + dy * dy) as u32;
                if best.is_none_or(|(kept, _)| distance < kept) {
                    best = Some((distance, (rx, ry)));
                }
            }
        }
        best.map(|(_, cell)| cell)
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

    /// The enemy's standing air-capable combat count, summed over
    /// [`AIR_THREAT_TYPES`] — the army mix's and the defense limb's shared
    /// air-pressure reading.
    fn enemy_air_count(&self, sim: &crate::sim::world::Simulation) -> usize {
        AIR_THREAT_TYPES
            .iter()
            .map(|&name| self.count_enemy_units_of_type(sim, name))
            .sum()
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
        let enemy_air = self.enemy_air_count(sim);
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
            // Rotation, not first-fit: the deficit role's buildable entry
            // with the fewest standing units claims the slot (ties keep
            // roster order). First-fit starved every entry listed behind a
            // rival — the heroes never once fielded (observed: a whole
            // game without Tanya while the cheaper Ghost and Sniper held
            // their role's slots). Rotation surfaces a hero at his role's
            // first specialist slot — his `BuildLimit=1` darkens his own
            // cameo once he stands, so he drops out until he falls.
            let mut pick: Option<(&RosterEntry, usize)> = None;
            for entry in roster.iter().filter(|entry| entry.role == role) {
                if let Some(gate) = entry.needs
                    && !self.owns_structure_of_type(sim, gate)
                {
                    continue;
                }
                let standing = self.count_combat_of_type(sim, entry.name);
                if standing >= pick.map_or(usize::MAX, |(_, best)| best) {
                    continue;
                }
                // `can_build`, not the cameo: the draw brightness knows
                // nothing of country locks, so a cameo-sorted pick handed
                // an Americans house the Germans-only Tank Destroyer —
                // the item parked at zero progress forever and wedged the
                // vehicle queue for the rest of the game (observed).
                // `can_build` runs the country gate the same way it sorts
                // the radar pair and the jet table; the `needs` gate above
                // still carries the prerequisites the AI house's quirk
                // would skip.
                let buildable = rules.object(entry.name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                });
                if buildable {
                    pick = Some((entry, standing));
                }
            }
            if let Some((entry, _)) = pick {
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
            SuperAiDifficulty::Unlimited,
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
            SuperAiDifficulty::Unlimited,
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

        // The launch path marks the driven house command-driven (the native
        // AI stands down and the fork's completion publication serves it);
        // a hand-built battle session skips the launch, so the driver marks
        // its own house here — same owner, same tier as `SuperAi::new`.
        scenario.apply_super_ai_tier(ai.house, ai.difficulty);
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

    /// [`drive_until`] plus a per-tick label of what `orders` returned:
    /// the census prints the tally, so a starved run names its command
    /// eater outright instead of one hypothesis per 22-minute rerun.
    fn drive_until_tallied(
        scenario: &mut crate::headless_scenario::HeadlessScenario,
        ai: &mut SuperAi,
        budget: u32,
        done: impl Fn(&crate::headless_scenario::HeadlessScenario) -> bool,
        tally: &mut std::collections::BTreeMap<String, u32>,
    ) -> u32 {
        use crate::sim::world::{FrameEffects, TickLane};
        scenario.apply_super_ai_tier(ai.house, ai.difficulty);
        let mut tick = 0u32;
        while tick < budget && !done(scenario) {
            let orders = ai.orders(&scenario.runtime, tick);
            let label = orders.first().map_or_else(
                || "empty".to_string(),
                |env| {
                    if let crate::sim::command::Command::QueueProduction { type_id } = &env.payload
                    {
                        // Which type the queue branch kept issuing tells
                        // whether admission ever landed: a type issued
                        // thousands of times with none standing was refused
                        // at apply, not starved by the mix.
                        format!(
                            "QueueProduction:{}",
                            scenario.sim().interner.resolve(*type_id)
                        )
                    } else {
                        let debug = format!("{:?}", env.payload);
                        debug
                            .split(['{', '('])
                            .next()
                            .unwrap_or("payload")
                            .trim()
                            .to_string()
                    }
                },
            );
            *tally.entry(label).or_insert(0) += 1;
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
    /// — native Hard difficulty, the Hard-row opening grant, the build-time
    /// scale (Super and Unlimited alike) and the tier mark the driver
    /// rebuild reads back — while an ordinary computer slot stays a native
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
        for tier in [SuperAiDifficulty::Super, SuperAiDifficulty::Unlimited] {
            // A four-start map: fewer start waypoints than houses would seed
            // the extra starts at random, which the lobby never allows.
            let map = "XMP03T4.MAP";
            let mut session = crate::headless_scenario::battle_session(
                map,
                &[
                    (LaunchCountry::Russia, AiDifficulty::Hard),
                    (LaunchCountry::America, AiDifficulty::Normal),
                ],
            );
            session.opponents[0].super_tier = Some(tier);
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
            assert_eq!(
                tier_house.super_tier(),
                Some(tier),
                "the tier mark rides the house for the driver rebuild"
            );
            assert_eq!(tier_house.difficulty(), HouseDifficulty::Hard);
            assert_eq!(
                tier_house.native_build_time_scale(),
                Some(NativeF32Bits::from_bits(0x3F00_0000)),
                "{tier:?} halves build times"
            );
            // The tier grant replaced the native opening-credit loop (skipped
            // for command-driven houses): Hard row x 0.01 x the launch balance.
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
            assert_eq!(plain_house.super_tier(), None);
            assert_eq!(plain_house.difficulty(), HouseDifficulty::Normal);
            assert_eq!(plain_house.native_build_time_scale(), None);
            // Retail MultiplayerAICM's Normal row is 0, so the native loop
            // granted this house nothing on top of its launch balance.

            assert_eq!(plain_house.economy().available_money(), starting_credits);
        }
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

        // The engine files pad aircraft under their own category — the
        // Unit/Infantry pair above reads zero for a living jet, and the
        // census would report the wing empty however many stood.
        let aircraft = |scenario: &crate::headless_scenario::HeadlessScenario, name: &str| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Aircraft
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
            defenses: usize,
            ifv: usize,
            guardian: usize,
            tanya: usize,
            jets: usize,
            amradr: usize,
            gairc: usize,
            /// The construction yard's standing count at the final census
            /// and the last tick one stood: orders() returns empty every
            /// tick with no yard (no opening table resolves), so a yard
            /// that fell pins the freeze tick.
            yard: usize,
            yard_last_tick: u32,
            /// Own undeployed MCVs: the deploy branch likewise returns
            /// empty every tick once its attempts are spent.
            mcv: usize,
            /// End-state producer counts (assigned, not maxed).
            barracks_end: usize,
            factories_end: usize,
            amradr_end: usize,
            /// The house's banked AirportDocks, max seen and final: an
            /// AirportBound jet's admission dies at `pads >= docks`, and a
            /// standing AMRADR whose grand opening never ran banks none.
            docks_max: i32,
            docks_end: i32,
            /// The first standing AMRADR's entity id, to cross-reference
            /// the grand-opening trace.
            amradr_id: Option<u64>,
            /// First tick every first-jet-commission precondition held:
            /// an aircraft factory standing, some jet `can_build`-Yes and
            /// the 1200-credit cost banked. `None` at failure means no
            /// window ever opened; `Some` with zero jets means the orders
            /// chain never reached the commission branch.
            commission_ready: Option<u32>,
            /// The fortress pair's proof: the Battle Fortress count and
            /// the busiest cargo load seen on one (max, not end — the
            /// pairing's claim is that boarding ever happened).
            fortress: usize,
            fortress_seats: usize,
        }
        let seen = std::cell::RefCell::new(Seen::default());
        let mut command_tally = std::collections::BTreeMap::new();
        // Grand_Opening trace for the whole drive: whether the AMRADR's
        // first opening ever ran decides between "docks never banked" and
        // "admission refuses with docks banked".
        crate::sim::building_art::start_grand_opening_trace();
        let tick = drive_until_tallied(
            &mut scenario,
            &mut ai,
            42000,
            |scenario| {
                let mut seen = seen.borrow_mut();
                seen.superweapon = seen.superweapon.max(structures(scenario, "GAWEAT"));
                seen.factories = seen.factories.max(structures(scenario, "GAWEAP"));
                seen.refineries = seen.refineries.max(structures(scenario, "GAREFN"));
                seen.power = seen.power.max(structures(scenario, "GAPOWR"));
                seen.amradr = seen.amradr.max(structures(scenario, "AMRADR"));
                seen.gairc = seen.gairc.max(structures(scenario, "GAAIRC"));
                seen.yard = structures(scenario, "GACNST");
                if seen.yard > 0 {
                    seen.yard_last_tick = scenario.sim().session().tick() as u32;
                }
                // End-state (not max) producer counts: admission refusals
                // correlate with which producers still stood at the end.
                seen.barracks_end = structures(scenario, "GAPILE");
                seen.factories_end = structures(scenario, "GAWEAP");
                seen.amradr_end = structures(scenario, "AMRADR");
                let docks = scenario
                    .sim()
                    .houses
                    .get(&human)
                    .map_or(0, |house| house.tracking().airport_docks());
                seen.docks_max = seen.docks_max.max(docks);
                seen.docks_end = docks;
                if seen.amradr_id.is_none() && seen.amradr_end > 0 {
                    seen.amradr_id = scenario
                        .sim()
                        .substrate
                        .entities
                        .iter_sorted()
                        .find(|(_, entity)| {
                            entity.owner() == human
                                && entity.category()
                                    == crate::map::entities::EntityCategory::Structure
                                && scenario.sim().interner.resolve(entity.type_ref()) == "AMRADR"
                        })
                        .map(|(id, _)| id);
                }
                seen.mcv = seen.mcv.max(units(scenario, "AMCV"));
                // The three Allied defense classes in one perimeter reading —
                // the pillbox quota alone satisfies it, AA/heavy only add.
                seen.defenses = seen.defenses.max(
                    structures(scenario, "GAPILL")
                        + structures(scenario, "NASAM")
                        + structures(scenario, "ATESLA"),
                );
                seen.ifv = seen.ifv.max(units(scenario, "FV"));
                seen.guardian = seen.guardian.max(units(scenario, "GGI"));
                // The hero surfaces through the rotation once the battle lab
                // stands; the jet wing rides its own aircraft queue behind the
                // radar (the Airforce Command is both).
                seen.tanya = seen.tanya.max(units(scenario, "TANY"));
                seen.jets = seen
                    .jets
                    .max(aircraft(scenario, "ORCA") + aircraft(scenario, "BEAG"));
                for (_, entity) in scenario.sim().substrate.entities.iter_sorted() {
                    if entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Unit
                        && scenario.sim().interner.resolve(entity.type_ref()) == "BFRT"
                    {
                        seen.fortress = seen.fortress.max(1);
                        if let Some(cargo) = entity.passenger_role.cargo() {
                            seen.fortress_seats = seen.fortress_seats.max(cargo.count() as usize);
                        }
                    }
                }
                if seen.commission_ready.is_none() {
                    let rules = &scenario.runtime.resources.rules;
                    let jet_ok = ["ORCA", "BEAG"].iter().any(|&name| {
                        rules.object(name).is_some_and(|ty| {
                            crate::sim::production::can_build(
                                scenario.sim(),
                                rules,
                                human,
                                ty,
                                false,
                                false,
                            ) == crate::sim::production::CanBuild::Yes
                        })
                    });
                    let factory =
                        structures(scenario, "AMRADR") + structures(scenario, "GAAIRC") > 0;
                    let credits = crate::sim::production::credits_for_owner(
                        scenario.sim(),
                        scenario.sim().interner.resolve(human),
                    );
                    if jet_ok && factory && credits >= 1200 {
                        seen.commission_ready = Some(scenario.sim().session().tick() as u32);
                    }
                }
                seen.superweapon >= 1
                    && seen.factories >= 4
                    && seen.defenses >= 2
                    && seen.ifv >= 1
                    && seen.guardian >= 1
                    && seen.tanya >= 1
                    && seen.jets >= 1
            },
            &mut command_tally,
        );
        let opening_trace = crate::sim::building_art::take_grand_opening_trace();
        let seen = seen.into_inner();
        // One live admission probe with the end-state world standing: the
        // direct enqueue bypasses the orders chain, so its verdict separates
        // "the chain never asked" from "the sim refuses outright".
        let probe_orca = {
            let runtime = &mut scenario.runtime;
            let owner = runtime.simulation.interner.resolve(human).to_string();
            crate::sim::production::enqueue_by_type(
                &mut runtime.simulation,
                &runtime.resources.rules,
                &owner,
                "ORCA",
                crate::sim::world::FrameEffects::default(),
                false,
            )
        };
        let amradr_opened = opening_trace
            .iter()
            .any(|&(id, _)| seen.amradr_id == Some(id));
        // The scouting chain's end read: what fraction of the house's fog
        // grid stands revealed. The dogs' commission and dispatch are the
        // fork's own (retail posts its dogs as base guards), so the census
        // owns their proof; the floor stays low on purpose — the figure to
        // watch is the printed one.
        let revealed_pct = {
            let sim = scenario.sim();
            let fog = &sim.fog;
            let (w, h) = (u32::from(fog.width()), u32::from(fog.height()));
            let mut lit = 0u32;
            for ry in 0..h {
                for rx in 0..w {
                    if fog.is_cell_revealed(human, rx as u16, ry as u16) {
                        lit += 1;
                    }
                }
            }
            if w * h == 0 { 0 } else { lit * 100 / (w * h) }
        };
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
        // The ready stock names what placement has finished but not yet
        // placed — a stuck entry here is the placement wedge itself.
        let ready_names = |scenario: &crate::headless_scenario::HeadlessScenario| {
            crate::sim::production::ready_buildings_for_owner(
                scenario.sim(),
                &scenario.runtime.resources.rules,
                scenario.sim().interner.resolve(human),
            )
            .iter()
            .map(|item| scenario.sim().interner.resolve(item.type_id).to_string())
            .collect::<Vec<_>>()
            .join(",")
        };
        // One combined assertion with the full census: a slow retail test
        // must explain its whole end state in one panic, not one fact per
        // rerun.
        assert!(
            seen.superweapon >= 1
                && seen.factories >= 4
                && seen.defenses >= 2
                && seen.ifv >= 1
                && seen.guardian >= 1
                && seen.tanya >= 1
                && seen.jets >= 1
                && revealed_pct >= 25
                && seen.fortress >= 1
                && seen.fortress_seats >= 1,
            "growth slice incomplete at tick {tick}: GAWEAT={} GAWEAP={} refineries={} \
             power={} defenses={} FV={} GGI={} TANY={} jets={} credits={} miners={} MTNK={} \
             SREF={} E1={} queue=[{}] AMRADR={} GAAIRC={} commission_ready={:?} \
             ready=[{}] yard={} yard_last_tick={} mcv={} \
             barracks_end={} factories_end={} amradr_end={} docks_max={} docks_end={} \
             probe_orca={} trace_len={} amradr_opened={} revealed_pct={} \
             fortress={} fortress_seats={} tally={:?}",
            seen.superweapon,
            seen.factories,
            seen.refineries,
            seen.power,
            seen.defenses,
            seen.ifv,
            seen.guardian,
            seen.tanya,
            seen.jets,
            crate::sim::production::credits_for_owner(
                scenario.sim(),
                scenario.sim().interner.resolve(human),
            ),
            units(&scenario, "CMIN"),
            units(&scenario, "MTNK"),
            units(&scenario, "SREF"),
            units(&scenario, "E1"),
            queue_names(&scenario),
            seen.amradr,
            seen.gairc,
            seen.commission_ready,
            ready_names(&scenario),
            seen.yard,
            seen.yard_last_tick,
            seen.mcv,
            seen.barracks_end,
            seen.factories_end,
            seen.amradr_end,
            seen.docks_max,
            seen.docks_end,
            probe_orca,
            opening_trace.len(),
            amradr_opened,
            revealed_pct,
            seen.fortress,
            seen.fortress_seats,
            command_tally,
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
