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

/// Minimum distance, in cells, from the home yard for an expansion site:
/// anything closer sits inside the home ring-scan's reach, so only the far
/// ore fields — past [`SITE_SEARCH_RADIUS`] — justify walking a base out
/// to them.
const EXPANSION_MIN_DISTANCE: i16 = 30;

/// Deploy attempts the expansion MCV gets before the plan is abandoned —
/// per MCV, unlike the opening's game-lifetime budget, which never sees a
/// later MCV and must not swallow its deploys.
const EXPANSION_DEPLOY_ATTEMPTS: u32 = 5;

/// How far from the expansion yard the sub-base's site search scans, in
/// cells: the engine's own build-area admission (`Adjacent+1` ring, default
/// `Adjacent=3`) already bounds every legal foundation to the yard's
/// neighborhood, so a wider scan would only walk origins the placement
/// order can never land — the scan stays tight and validates each
/// candidate with the engine's real predicate.
const EXPANSION_SITE_RADIUS: i16 = 5;

/// Structures within this many cells of a yard count as that base's
/// cluster: the expansion limbs read the sub-base's power and refinery
/// standing against this ring, and the per-base defense quotas count each
/// yard's guns inside it.
const BASE_CLUSTER_RADIUS: i16 = 8;

/// A standing yard this close (Chebyshev) to the latched far ore field
/// admits the remote-refinery routing around that field: a foundation the
/// scan picks beside a yard-less field sits in no friendly build area and
/// the placement order can never land (observed as OutOfBuildArea
/// rejections soaking the one building slot), so until a yard walked out
/// there the ready refinery takes the home rings or the expansion yard's.
const EXPANSION_YARD_PROXIMITY: i16 = 8;

/// Ore cells within this distance of any standing refinery — any owner's,
/// including other houses' — read as that field's contested half: the ore
/// survey prefers the uncontested fields, so neighboring bases stop
/// latching the same ground.
const ORE_FIELD_EXCLUSION_RADIUS: i16 = 10;

/// How many of the fan-out offsets anchor idle defenders around the
/// expansion yard: half the home set, enough posts for the sub-base's
/// guard reserve without thinning the home perimeter's own spread.
const EXPANSION_GUARD_OFFSETS: usize = 8;

/// Miners the vehicle queue builds before it switches to tanks: enough to
/// feed two refineries' docks while the army starts coming out.
const HARVESTER_TARGET: usize = 5;

/// How far from the construction yard an intruder still counts as attacking
/// the base, in cells: the defense picks explicit targets out to this ring;
/// idle units inside it hold an Area Guard post on the yard.
const BASE_DEFENSE_RADIUS: i16 = 16;

/// The base alarm's recency window (Layer 3.5): a building hit this
/// recently — by the house's native damage admission stamp — arms the
/// active-defense limb.
const BASE_ALARM_TICKS: u32 = 90;

/// How long the defense stance holds after its last refresh, in ticks: the
/// defenders keep the focus through the mop-up instead of disbanding the
/// instant the last hit lands.
const DEFENSE_HOLD_TICKS: u32 = 450;

/// The defense stance's commit floor: units are pulled off their guard
/// posts onto the focus until this many defenders stand on it. The
/// [`GUARD_RESERVE`] alone cannot hold a determined push.
const DEFENSE_COMMIT_MIN: usize = 4;

/// The alarm limb's intruder search radius, cells — wider than the idle
/// perimeter's [`BASE_DEFENSE_RADIUS`] so stand-off artillery or a raiding
/// flight sitting outside the perimeter still draws the recall.
const ALARM_RADIUS: i16 = 24;

/// The naval layer (Layer 2.7): combat ships the side fields once its
/// naval yard stands — the basic hull line up to this quota, then the AA
/// escort when its extra prerequisite honestly passes `can_build`.
const NAVAL_SHIP_QUOTA: usize = 3;

/// The second hull line's quota (Layer 2.7): past the basic hull quota the
/// extra hull (the Dolphin, the Giant Squid) rotates up to this many — its
/// own line, not a squeeze on the basic one.
const NAVAL_EXTRA_QUOTA: usize = 3;

/// The capital ship's quota (Layer 2.7): one, behind the battle lab — the
/// Aircraft Carrier and the Dreadnought are the lab's naval expression,
/// and one strike-carrier turns the quota fleet into a siege arm.
const NAVAL_CAPITAL_QUOTA: usize = 1;

/// How far from a yard the naval survey scans for a legal shoreline cell:
/// the naval yard's foundation needs a WaterSet tile inside the base's own
/// build area, so the survey rides the same admission the placement runs.
const NAVAL_SITE_RADIUS: i16 = 14;

/// Pacing between shoreline surveys, in ticks — the ring scan probes a
/// placement per cell, so a landlocked base must not rescan every growth
/// slot.
const NAVAL_PROBE_TICKS: u32 = 900;

/// The survey budget: this many failed probes and the navy is dead for the
/// game (no legal shoreline near any yard).
const NAVAL_PROBE_MAX: u8 = 3;

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

/// Ticks between wave episodes. A full pulse drains the standing set to the
/// guard reserve; this interval lets production refill it before the next
/// departure and bounds a wave whose dispatches were dropped to one wasted
/// order per unit per interval.
const WAVE_RETRY_TICKS: u32 = 300;

/// Ticks between ore-field re-surveys while the expansion MCV waits: a
/// survey without a deployable winner scans every candidate's
/// neighborhood, so it runs paced, never per tick.
const SURVEY_RETRY_TICKS: u32 = 300;

/// Ticks between two broken-bridge surveys. The survey reads the map's
/// bridge-repair huts and the hostile structures the breach cut off, so it
/// runs paced, never per tick.
const BRIDGE_SURVEY_TICKS: u32 = 200;

/// Ticks before the repair limb may re-issue its capture order or its
/// engineer commission: a dropped [`Command::CaptureBuilding`], or an
/// engineer the queue has not produced yet, must not soak the one-command
/// budget every tick the way an unpaced re-issue once starved the branches
/// below.
const BRIDGE_RETRY_TICKS: u32 = 150;

/// The wallet bar the repair engineer's commission waits for: the commission
/// rides the infantry queue like any unit, so it is only worth a slot while
/// the base's growth is comfortably funded (the air wing's top-up guard, in
/// miniature — a broke base expands, a rich one repairs).
const BRIDGE_ENGINEER_CREDITS: i32 = 2000;

/// Ticks between the building-repair limb's surveys. A survey walks the
/// house's whole structure set (health, repair byte, priority rank), so it
/// runs paced, never per tick; one repair activation at a time latches
/// between surveys anyway.
const REPAIR_SURVEY_TICKS: u32 = 300;

/// The wallet bar a repair activation waits for: repair steps pay as they
/// heal, so a broke base would park a half-paid repair that the engine's
/// own step aborts the moment the wallet dips — the air wing's top-up
/// guard, in miniature. A broke base expands; a funded one repairs.
const STRUCTURE_REPAIR_CREDITS: i32 = 1500;

/// A structure whose health fraction drops under 3/5 earns a repair
/// activation (integer-safe comparison: `health * 5 < strength * 3`).
const REPAIR_FRACTION_NUMERATOR: i32 = 3;
const REPAIR_FRACTION_DENOMINATOR: i32 = 5;

/// The stock bridge-repair hut (retail `BridgeRepairHut=yes`; stock CABHUT
/// is the only one, `object_type.rs`). Resolved through the interner at
/// survey time like every other rules name: a map whose rules lack the type
/// simply skips the limb.
const BRIDGE_HUT_TYPE: &str = "CABHUT";

// The bridge repair's capture infantry is the opening's own `engineer`
// field, not a shared name: the three sides' engineers are three rules
// sections split by `ForbiddenHouses` ([ENGINEER] Allies, [SENGINEER]
// Soviets, [YENGINEER] Yuri), so a shared "ENGINEER" commissioned nobody
// outside an Allied base.

/// Blocker shoves one opening MCV gets while it stands on the same cell:
/// past the cap the site counts as failed and the stuck window below owns
/// the MCV (observed: 582 shove orders across 145 units while one MCV stood
/// at one cell from tick 4000 to 20772, never deploying).
const SHOVE_ATTEMPTS: u32 = 5;

/// Ticks an opening MCV may stand stuck on one cell, refused its deploy
/// spot, before the site is abandoned: a reachable legal foundation is
/// walked to once, or the MCV is latched dead for the deploy branch —
/// anything but shoving forever.
const MCV_STUCK_TICKS: u32 = 600;

/// Cells an opening MCV must travel from its latched site before the
/// per-foundation budgets (deploy attempts, shoves, the stuck window)
/// re-arm: a cell of jiggle must not renew them (the old budget reset
/// re-armed on any movement at all, which is how the shove loop spun).
const MCV_RESITE_DISTANCE: u16 = 4;

/// Every side's war factory type (retail `UIName`s): the structures a wave
/// razes first to starve the enemy's reinforcements.
const WAR_FACTORY_TYPES: [&str; 3] = ["GAWEAP", "NAWEAP", "YAWEAP"];

/// Every side's refinery section name: the ore survey's exclusivity term
/// reads standing structures of any of these — any owner's — so
/// neighboring bases stop latching the same field.
const REFINERY_TYPES: [&str; 3] = ["GAREFN", "NAREFN", "YAREFN"];

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
/// Rocketeer, Kirov, Harrier, Black Hawk, Hind, Floating Disk — plus the
/// Black Eagle (Layer 3: the jet is the one air threat the list lacked, so
/// a Korean wing never lifted the anti-air answer).
const AIR_THREAT_TYPES: [&str; 7] = ["JUMPJET", "ZEP", "ORCA", "SHAD", "HIND", "DISK", "BEAG"];

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
/// The standing-gun floor the fair-growth census reads: while fewer than
/// this many guns stand the unit queues hold their purchases for the next
/// gun the defense limb claims (the savings hold, `gun_hold` below). Bare —
/// a perimeter, not an armament program.
const DEFENSE_FLOOR: usize = 2;

/// Per-base defense quotas (Layer 2): the house-wide caps above split per
/// standing yard — each base carries its own pillboxes and AA, the heavy
/// tower one per yard — so the expansion yard is not a naked foundation
/// while the home ring holds every gun. The house-wide bound stays at
/// twice the old caps, so the split can only move guns around, never
/// inflate the armament program as a whole.
const DEFENSE_PER_YARD_BASIC: usize = 2;
const DEFENSE_PER_YARD_AA: usize = 2;
const DEFENSE_PER_YARD_HEAVY: usize = 1;

/// The wall ring's wallet floor: a wall segment is 100 credits, but the ring
/// spends dozens of them one production cycle at a time, so the limb stays
/// shut until the wallet is deep enough that the perimeter never competes
/// with the army queues or a real gun for the same income.
const WALL_CREDIT_FLOOR: i32 = 1500;

/// The late-game defense sink: past this wallet, with every economy cap
/// met and the power grid in the black, each yard's heavy quota gains one
/// tier — wealth converts to defense depth, and to nothing else (no new
/// economy opens past the caps).
const LATE_SINK_CREDITS: i32 = 20_000;

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
/// production line is worth feeding. The ladder's last rung carries the
/// build-out: as a growth entry the fourth factory never won a slot
/// from the power-deficit rule (observed), so the rung owes it with
/// ladder priority.
const REFINERY_CAP: usize = 4;
const FACTORY_BUILDOUT: usize = 4;
const FACTORY_CAP: usize = 6;
const FACTORY_CREDITS: i32 = 1500;
/// Growth power cap. Without one the deficit rule (queue a plant the
/// moment drain overtakes output) claims every growth slot in a
/// power-hungry base and the refinery and factory rules behind it never
/// take a turn (observed: two growth plants and no fourth factory). The
/// Layer 2.5 preemption raised the real cap from eight: a two-yard base's
/// drain (robot control −100, repair bay −200, the lab-and-radar climb,
/// the gun ring) runs past eight Allied plants' 1600 output well before
/// the game ends, and the old cap pinned late-game grids into a permanent
/// brownout (observed live: blackouts at eight plants standing).
const POWER_CAP: usize = 16;

/// The power margin (Layer 2): while the grid's headroom (`output - drain`)
/// sits below the draw of one more war factory plus refinery — a fixed
/// estimate, the retail plants' drain is a few dozen points apiece — a
/// plant queues ahead of the next factory line, but only while the
/// factory fleet is still inside its standard build-out. Past the
/// build-out the deficit rule alone owns power: a war-damaged grid must
/// not claim every growth slot the way the old unbounded margin once did
/// (observed: five plants while refineries sat at two).
const POWER_HEADROOM_TARGET: i32 = 250;

/// Deterministic fan-out offsets (cells) that keep the AI's destinations
/// from converging on one point: rallies, guard posts and wave destinations
/// each pick an offset by index or entity id, so a group spreads over
/// neighboring cells instead of jamming on a single one. Move, Guard and
/// AttackMove all complete in the destination's vicinity, so an offset that
/// lands on a building still parks the unit beside it rather than stacking.
/// Two rings: the combat roster now fields far more than eight units, and
/// one ring funneled every ninth unit back onto an occupied post.
const SPREAD_OFFSETS: [(i8, i8); 16] = [
    (0, 0),
    (4, 0),
    (0, 4),
    (-4, 0),
    (0, -4),
    (4, 4),
    (-4, 4),
    (-4, -4),
    (4, -4),
    (8, 0),
    (-8, 0),
    (0, 8),
    (0, -8),
    (8, 4),
    (-8, -4),
    (8, -4),
];

// ---- Layer 3: combat & unit management ----
//
// The combat layer's own knobs: the futility backoff the repeated-order
// sites share, the hero's and raiders' pacing, the boarding retry budget,
// the siege standoff range, and the retail type tables the specialist
// branches read.

/// Futility strikes before an order kind goes into backoff for an entity:
/// three paced re-orders of the same kind with the entity (or its aim) still
/// on the same cell mean the order class is stuck — the post is blocked, the
/// dark cell unreachable, the boarding refused — and repeating it is waste.
const FUTILITY_STRIKES: u32 = 3;

/// Ticks an order kind stays suppressed for an entity after
/// [`FUTILITY_STRIKES`]: long enough to outlast the churn windows it bounds
/// (the 150-tick guard repost, the 300-tick wave episode), short enough that
/// a cleared wedge retries within a minute of game time.
const FUTILITY_BACKOFF_TICKS: u32 = 1200;

/// Boarding: Enter attempts a (unit, fortress) pair gets while the Enter
/// stays refused (the unit never lands) before the unit returns to the wave
/// pool instead of walking into the refusal forever.
const BOARD_MAX_ATTEMPTS: u32 = 2;

/// Ticks between raider windows (the Terror Drone's harvester hunt, Crazy
/// Ivan's bomb run, the Floating Disk's power raid). One raid order per
/// window; the reservation list keeps the raider out of the generic wave
/// while the raid is live.
const RAIDER_RETRY_TICKS: u32 = 300;

/// Ticks between deploy-mechanic windows: the scan for a holding deployable
/// is cheap but pointless per tick, and the toggle is once-per-unit anyway.
const DEPLOY_MECH_TICKS: u32 = 150;

/// The Soviet Desolator — the roster's rad-field piece (`DESO`,
/// `Deployer=yes`, the anti-foot line's radiation answer). Its deploy is an
/// engagement switch: the engine keeps a deployed soldier guarding in
/// place while its weapon radiates the cell, so the AI's whole order is
/// the toggle. The generic home-deploy of the deploy mechanic skips the
/// type — an unconditional field at the base door would burn the house's
/// own feet — and the engagement below arms it instead.
const DESO_TYPE: &str = "DESO";

/// Enemy infantry within this many cells of an idle Desolator that arm its
/// radiation stance: the field is the anti-assault wall, not a posture.
const DESO_DEPLOY_RADIUS: i16 = 5;

/// The count inside [`DESO_DEPLOY_RADIUS`] that arms the stance: a lone
/// scout does not buy a toggle; an assault does.
const DESO_DEPLOY_INFANTRY: usize = 3;

/// Ticks between naval-sortie windows: the episode (a latched target, one
/// hull dispatched per tick until only the home guard remains) may arm at
/// most this often, so a fleet that fought its last battle waits a minute
/// of game time before the next one instead of re-reading the shore every
/// tick.
const NAVAL_SORTIE_WINDOW_TICKS: u32 = 600;

/// Idle combat hulls the sortie needs before it sails: a single ship is a
/// gift to the enemy's fleet and the wave's mass lesson applies at sea too,
/// so the branch waits for a pair — and keeps one more home as the defense
/// stock on top.
const NAVAL_SORTIE_FLEET_MIN: usize = 2;

/// Dogs per standing yard that hold the sentry ring (`Command::Guard`
/// posts whose leash, chase and spy-bite are the engine's own): the hound
/// is retail's counter-infiltration base guard, so this is a cap on the
/// kennel, not a commission — the extras stay in the idle pools for the
/// perimeter attack and the alarm recall.
const SENTRY_DOGS_PER_YARD: usize = 2;

/// Ticks between amphibious-ferry windows (the transport's dock, sail and
/// unload legs, [`Self::ferry_orders`]): the branch is a pacing problem —
/// one leg per window, the order gate pacing each passenger's boarding
/// walk — so a refused unload or a boarded walking candidate costs a
/// window, never the chain below.
const FERRY_WINDOW_TICKS: u32 = 300;

/// Chebyshev distance from the home yard past which a loaded ferry
/// unloads and an empty one turns back: near home it beaches and loads.
const FERRY_DISCHARGE_DISTANCE: i16 = 28;

/// How far from the beached transport an idle boarding candidate may
/// stand: the Enter walk stays short and the loading quick.
const FERRY_BOARD_RADIUS: i16 = 12;

/// Ticks between engineer-capture windows (the neutral-tech scan, the idle
/// dispatch and the topped-up commission, [`Self::engineer_capture_orders`]):
/// the scan walks every structure and a refused capture costs a window, so
/// the branch is paced like the ferry's, never per tick.
const ENGINEER_CAPTURE_WINDOW_TICKS: u32 = 450;

/// The capture chain's own preference order over [`TECH_BUILDING_TYPES`]:
/// the oil derrick leads — its `ProduceCashStartup` pays the engineer back
/// the moment he walks in, and its tick income compounds — then the stock
/// list's order. Distance picks within a class, never across it.
const CAPTURE_PRIORITY_TYPES: [&str; 6] =
    ["CAOILD", "CAAIRP", "CATHOSP", "CAOUTP", "CAMACH", "CAPOWR"];

/// Total engineers (standing plus queued) the capture chain commissions up
/// to: the bridge repair keeps its own one-at-a-time rule and outranks this
/// limb, so the capture chain only ever spends the surplus above it.
const CAPTURE_ENGINEER_STOCK: usize = 2;

/// Ticks a finished (or failed) engineer ride keeps the raid limb asleep:
/// an engineer-and-vehicle pair spent every cooldown would strip the AA
/// roster line the waves need, so the raid fires once per breach of the
/// wallet bar, never continuously.
const RAID_COOLDOWN_TICKS: u32 = 6000;

/// The wallet above which one engineer-ride raid may arm: comfortably past
/// the pair's combined cost, so the commission never starves the growth
/// ladder or the defense line that pays for itself first.
const RAID_WALLET_CREDITS: i32 = 3000;

/// Hard per-stage budget for the raid state machine: a leg that has not
/// landed in this long (a refused Enter, an unload the ground refuses, a
/// capture the arrival rules void) tears the raid down and arms the
/// cooldown, so a wedged leg cannot pin the state forever. Sized for the
/// sail: a loaded ride crossing a 512-cell map at vehicle speed runs
/// past a tight bound, and tearing that leg down mid-sea strands the
/// engineer in the hold — the wedge this budget exists to break costs
/// two idle units for forty seconds, the cheaper failure.
const RAID_STAGE_TIMEOUT_TICKS: u32 = 1800;

/// Chebyshev distance from the raid target within which the loaded ride
/// unloads: close enough that the walk-in stays short, far enough to stay
/// outside the target's own foundation.
const RAID_ARRIVAL_DISTANCE: i32 = 6;

/// Enemy superweapon structures the spy infiltrates first: the arrival
/// resets the charging special's clock (the recon facts' infiltration
/// table), so the piece that can level the base outranks the rest.
const ENEMY_SUPERWEAPON_TYPES: [&str; 6] =
    ["GAWEAT", "NAIRON", "YAPPET", "GACSPH", "NAMISL", "YAGNTC"];

/// Enemy radar structures the spy infiltrates next: the arrival knocks the
/// radar offline, blinding their response while the waves march.
const ENEMY_RADAR_TYPES: [&str; 4] = ["GAAIRC", "AMRADR", "NARADR", "NAPSIS"];

/// How many dark cells nearest the scout get the zone-connection check: the
/// scan effort stays capped even on a map whose every near candidate is
/// across a breach.
const SCOUT_ZONE_CANDIDATES: usize = 16;

/// How many scouts may be on the march at once when the side's scout is its
/// core soldier (Yuri's Initiate): every scout walking is one fewer home
/// defender, so the cap holds the defense line (the dedicated two-dog scout
/// of the other sides needs no cap — it is the whole quota).
const SCOUT_MAX_ACTIVE: usize = 2;

/// Siege standoff: the cells short of the target a siege-role wave unit
/// holds at, spread across the range by the dispatch, instead of walking
/// into the target's defenses.
const SIEGE_STANDOFF_MIN: i32 = 10;
const SIEGE_STANDOFF_MAX: i32 = 12;

/// How many top enemy priority targets the air flight fans across: a flight
/// of four drops its bombs over four structures instead of stacking on the
/// substrate's first.
const AIR_FAN_TARGETS: usize = 4;

/// The enemy economy units the Terror Drone hunts, by retail section name.
const ENEMY_MINER_TYPES: [&str; 3] = ["HARV", "CMIN", "SMIN"];

/// Enemy power plants the Floating Disk raids first (the blinding cut).
const ENEMY_POWER_TYPES: [&str; 3] = ["GAPOWR", "NAPOWR", "YAPOWR"];

/// Enemy refineries the Floating Disk raids when no plant stands.
const ENEMY_REFINERY_TYPES: [&str; 3] = ["GAREFN", "NAREFN", "YAREFN"];

/// Neutral tech buildings the spy runs at (the stock YR
/// `NeutralTechBuildings` list: airfield, hospital, oil derrick, outpost,
/// machine shop, service pad).
const TECH_BUILDING_TYPES: [&str; 6] =
    ["CAAIRP", "CATHOSP", "CAOILD", "CAOUTP", "CAMACH", "CAPOWR"];

/// The Allied infiltrator: commissioned once, behind the battle lab, and
/// walked at the nearest revealed neutral tech building. Countries whose
/// cameo refuses the type simply never commission one (`can_build` gate).
const SPY_TYPE: &str = "SPY";

/// The retail MCVs — the vehicles the Iron Curtain must never center on
/// (the substrate's first vehicle was all too often the expansion MCV).
const MCV_TYPES: [&str; 3] = ["AMCV", "SMCV", "PCV"];

/// The raider specialists by retail name: the Terror Drone, Crazy Ivan and
/// the Floating Disk.
const RAIDER_DRONE: &str = "DRON";
const RAIDER_IVAN: &str = "IVAN";
const RAIDER_DISK: &str = "DISK";

/// The order kinds the futility backoff tracks. One kind per entity at a
/// time: a different kind (or a moved aim) restarts the streak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FutilityKind {
    /// The home guard-post re-issue (the 150-tick churn).
    GuardPost,
    /// The wave episode's attack-move dispatch.
    Wave,
    /// The scout's march at one dark cell (the aim is the tracked cell).
    Scout,
    /// The fortress boarding Enter.
    Board,
    /// The raiders' hunt orders.
    Raid,
    /// The hero's sabotage run.
    HeroC4,
    /// The spy's infiltration run.
    Spy,
}

/// One entity's futility-backoff state: the last order kind and aim cell it
/// was recorded against, the strike streak, and the tick the current
/// suppression lifts (`0` — never suppressed).
#[derive(Clone, Copy, Debug)]
struct FutilityState {
    kind: FutilityKind,
    cell: (u16, u16),
    strikes: u32,
    suppress_until: u32,
}

/// The engineer-ride raid's one live cycle (the engineer-in-an-IFV/Flak-
/// Track quick strike), its stage machine marched one leg per window in
/// [`Self::engineer_raid_orders`]. Every variant names both actors it
/// still needs — a leg whose actor died tears the raid down — and the
/// owner pins the leg's start tick beside it for the hard stage timeout.
#[derive(Clone, Copy, Debug)]
enum EngineerRaid {
    /// Both commissioned or found; the Enter walk is pending.
    Boarding { engineer: u64, ride: u64 },
    /// The engineer rides (in the hold, in limbo); the vehicle attack-moves
    /// at the latched enemy target.
    Sailing {
        engineer: u64,
        ride: u64,
        target: u64,
    },
    /// Arrived at the target: the unload lands, and the engineer
    /// re-appears on foot after the hold's own discharge delay.
    Unloading {
        engineer: u64,
        ride: u64,
        target: u64,
    },
    /// The engineer stands beside the target again; the capture click is
    /// his to walk in and be consumed by.
    Capturing { engineer: u64, target: u64 },
}

impl EngineerRaid {
    /// Every entity id the current stage leans on, for the wave pool's
    /// exclusion and the liveness checks.
    fn actors(&self) -> Vec<u64> {
        match *self {
            EngineerRaid::Boarding { engineer, ride }
            | EngineerRaid::Sailing { engineer, ride, .. }
            | EngineerRaid::Unloading { engineer, ride, .. } => vec![engineer, ride],
            EngineerRaid::Capturing { engineer, .. } => vec![engineer],
        }
    }
}

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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    /// The service depot the side's MCV is gated behind (retail
    /// `Prerequisite=`: AMCV on GADEPT, SMCV on NADEPT, PCV on the
    /// Grinder — YADEPT sits at TechLevel 15, past any lobby, so Yuri's
    /// real path is the Grinder alternate). The expansion builds exactly
    /// one while its MCV commission is armed.
    service_depot: &'static str,
    /// The side's first battle tank (Grizzly, Rhino, Lasher).
    main_tank: &'static str,
    /// The side's tier-2 battle tank behind the battle lab (Prism Tank,
    /// Apocalypse, Master Mind).
    tier2_tank: &'static str,
    /// The side's basic soldier (GI, Conscript, Initiate).
    basic_infantry: &'static str,
    /// The side's bridge-repair engineer. The three sides' engineers are
    /// three rules sections split by `ForbiddenHouses` — [ENGINEER] refuses
    /// the Soviet and Yuri countries, [SENGINEER] the Allies and Yuri,
    /// [YENGINEER] the Allies and the Soviets — so every engineer pick
    /// (the hut dispatch and the commission alike) reads this field; a
    /// shared "ENGINEER" name commissioned nobody outside an Allied base.
    engineer: &'static str,
    /// The side's land transport an engineer rides into a raid (retail
    /// `Passengers=` war-factory vehicles): the Allied IFV [FV] and the
    /// Soviet Flak Track [HTK], both already on the roster's anti-air line
    /// (what the vehicle turns into under an engineer's IFVMode is the
    /// delivery's problem, not the table's). `None` where the side fields
    /// no land transport — Yuri's only hulls are the amphibious Hover
    /// Transport (the ferry's) and the sea Boomer — so the raid branch
    /// sleeps.
    engineer_ride: Option<&'static str>,
    /// The side's superweapon structure and the special it grants
    /// (retail `SuperWeapon=` on the building): Weather Controller,
    /// Iron Curtain, Psychic Dominator. Every retail one is
    /// `DisableableFromShell=yes`, so the lobby's superweapons toggle
    /// owns the whole chain — CanBuild refuses the building while it is
    /// off and the engine revokes a standing grant.
    super_weapon_building: &'static str,
    super_weapon_type: &'static str,
    /// The side's second superweapon structure and the special it grants
    /// (retail `SuperWeapon=` on the building): the Nuclear Silo, the
    /// Genetic Mutator and the Chronosphere — each side's other retail
    /// super behind the lab. The same `DisableableFromShell=yes` lobby
    /// toggle owns its chain too, and the ladder's rung for it opens only
    /// once the first super has stood (the same once-per-game latch).
    second_super_weapon_building: &'static str,
    second_super_weapon_type: &'static str,
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
    /// The side's wall segment (retail `Wall=yes` BuildingType — the Defense
    /// queue's perimeter ring, one cell per production cycle): the Allied
    /// Concrete Wall behind its barracks, the Soviet Wall behind its Hand,
    /// Yuri's Citadel Wall behind its pedestal ([GAFWLL] — its section
    /// header reads `[GAFWLL];temp wall for yuri[YAWALL]`, so the `[YAWALL]`
    /// half is commented out and GAFWLL is the live section).
    wall: Option<&'static str>,
    /// The side's gap generator: [GAGAP], the shroud bubble behind the battle
    /// lab (retail `Prerequisite=GATECH,GACNST`, `Powered=yes`, -100 power).
    /// Allied only — the side table keeps the Soviet and Yuri entries `None`
    /// so no house without an Allied lab ever queues one.
    gap_generator: Option<&'static str>,
    /// The side's country-locked heavy turret: [GTGCAN], the French Grand
    /// Cannon behind the radar (retail `RequiredHouses=French`, -100 power).
    /// The lock lives in the field staying on the Allied table AND in the
    /// defense limb's own country re-check before the queue step: CanBuild
    /// refuses a `RequiredHouses` type forever, and a refused item parked in
    /// the one building slot is the engineer-commission lesson again.
    country_cannon: Option<&'static str>,
    /// The side's naval yard (`Naval=yes`, so it founds on a WaterSet tile):
    /// one stands when the naval survey finds a legal shoreline cell inside
    /// the base's build area. Prerequisites are the ladder's own conyard,
    /// power and refinery, so the yard queues as a growth rung.
    naval_yard: &'static str,
    /// The side's basic combat ship — prerequisite the yard alone, so it is
    /// the line the naval quota fills first: the Allied Destroyer, the
    /// Soviet Typhoon, Yuri's Boomer (the side's only combat hull).
    naval_ship: &'static str,
    /// The side's naval AA escort behind its extra prerequisite (the Aegis
    /// Cruiser's and the Sea Scorpion's radar), when one exists.
    naval_aa: Option<&'static str>,
    /// The side's second combat hull, rotating in past the basic hull's
    /// [`NAVAL_SHIP_QUOTA`] up to [`NAVAL_EXTRA_QUOTA`]: the Dolphin behind
    /// its lab, the Giant Squid behind its own. `None` where the basic hull
    /// line is the whole navy.
    naval_hull_extra: Option<&'static str>,
    /// The side's capital ship: one, behind the battle lab — the Aircraft
    /// Carrier and the Dreadnought, both retail `Spawns=` strike-craft
    /// carriers whose lab prerequisite the AI house's `CanBuild` would
    /// skip. `None` where the basic hull is already the capital piece (the
    /// Boomer spawns its own cruise missiles aboard).
    naval_capital: Option<&'static str>,
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
    /// The side's amphibious transport (retail `Naval=yes` hover hull with
    /// `Passengers=12`: the Landing Craft, the Armored Transport, the Hover
    /// Transport) — the ferry branch's sea hull (its air fallback lives in
    /// [`Opening::air_ferry`]). All three are
    /// `MovementZone=Amphibious`, so they beach beside the yard to take
    /// infantry aboard and unload on the far shore; each builds at the
    /// side's naval yard behind its TechLevel. `None` keeps the branch
    /// asleep.
    amphibious_transport: Option<&'static str>,
    /// The side's air ferry — the same ferry line where no naval yard ever
    /// stands (retail `Passengers=` hull with `MovementZone=Fly`): the
    /// Allied Night Hawk [SHAD], a war-factory build
    /// (`Prerequisite=GAWEAP`, filed under `[VehicleTypes]`, so the vehicle
    /// queue produces it like the hover hulls). The fly rows treat the map
    /// as one connected zone, which makes it the inland Allies' only
    /// ferry; a map with a shoreline keeps the Landing Craft first (the
    /// naval commission outranks the air one and the two share the single
    /// transport latch). `None` where the side fields no aircraft hull —
    /// the Soviet and Yuri hover transports keep their naval-yard road.
    air_ferry: Option<&'static str>,
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
        service_depot: "GADEPT",
        main_tank: "MTNK",
        tier2_tank: "SREF",
        basic_infantry: "E1",
        // The Allied section: ForbiddenHouses refuses the Soviet and Yuri
        // countries ([SENGINEER]/[YENGINEER] are their sides' twins).
        engineer: "ENGINEER",
        // The IFV: the raid ride (an engineer aboard is a repair vehicle by
        // IFVMode — irrelevant to the delivery) and the roster's own AA lead.
        engineer_ride: Some("FV"),
        super_weapon_building: "GAWEAT",
        super_weapon_type: "LightningStormSpecial",
        second_super_weapon_building: "GACSPH",
        second_super_weapon_type: "ChronoSphereSpecial",
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
            // The attack dog: retail's counter-infiltration hound
            // (`DetectDisguise=yes`, 200 credits, TechLevel 2) — the sentry
            // ring's guard: posted on a yard ring its own Guard mission
            // bites the spy and the raiding infantry that step to the door,
            // so the entry needs no prerequisite and no branch of its own.
            RosterEntry {
                name: "ADOG",
                role: Role::AntiInfantry,
                needs: None,
            },
            // The Chrono Legionnaire: the NeutronRifle erases its target
            // from the timeline — a slow, expensive vehicle answer, so it
            // files under AntiArmor behind its tech center (retail
            // Prerequisite=GAPILE,TECH, cost 1500, TechLevel 10).
            RosterEntry {
                name: "CLEG",
                role: Role::AntiArmor,
                needs: Some("GATECH"),
            },
            // The stolen-tech trio — all three sides' rosters carry all
            // three sections (retail Owner= names every country, so any
            // house that steals the matching tech fields any of them).
            // Each is `RequiresStolen*=yes` behind `Prerequisite=BARRACKS`
            // alone: a spy's battle-lab infiltration flips the house's
            // stolen-tech byte and can_build opens the cameo, so `needs`
            // stays None — the gate is the theft itself, and until it
            // lands army_pick's buildable filter passes these rows over
            // without spending a slot. The rotation fields them like any
            // specialist once the cameo lights. The Chrono Commando: the
            // hero's raid shape — a teleporting MP5 with C4 — AntiInfantry
            // like Tanya's row (retail TechLevel 9, cost 2000).
            RosterEntry {
                name: "CCOMAND",
                role: Role::AntiInfantry,
                needs: None,
            },
            // Chrono Ivan: the bomb planter cracks vehicles and
            // structures, so his row files AntiArmor like Boris's (retail
            // TechLevel 9, cost 1750).
            RosterEntry {
                name: "CIVAN",
                role: Role::AntiArmor,
                needs: None,
            },
            // Psi-Corp Trooper: mind control picks the enemy's best off,
            // the Yuri Prime row's own rationale — Siege (retail
            // TechLevel 9, cost 1000).
            RosterEntry {
                name: "PTROOP",
                role: Role::Siege,
                needs: None,
            },
        ],
        defense_basic: "GAPILL",
        defense_aa: "NASAM",
        defense_heavy: "ATESLA",
        wall: Some("GAWALL"),
        gap_generator: Some("GAGAP"),
        country_cannon: Some("GTGCAN"),
        naval_yard: "GAYARD",
        naval_ship: "DEST",
        naval_aa: Some("AEGIS"),
        // The second hull rotates past the destroyer quota (the Dolphin,
        // retail Prerequisite=GAYARD,GATECH); the carrier is the lab's
        // capital piece (retail Prerequisite=GAYARD,TECH, Spawns=HORNET).
        naval_hull_extra: Some("DLPH"),
        naval_capital: Some("CARRIER"),
        aircraft: &["BEAG", "ORCA"],
        scout: "ADOG",
        fortress: Some("BFRT"),
        // The Landing Craft: retail Prerequisite=GAYARD, TechLevel 4, a
        // `Naval=yes` hover hull with `Passengers=12` and
        // `MovementZone=Amphibious` — it beaches to load and unload.
        amphibious_transport: Some("LCRF"),
        // The Night Hawk: retail Prerequisite=GAWEAP, TechLevel 7, 1000
        // credits, `Passengers=5` and `MovementZone=Fly` — the sea-free
        // ferry. On a shoreline map the naval yard commissions the Landing
        // Craft first; inland, where no GAYARD ever stands, this hull is
        // the ferry line the fly rows carry anywhere.
        air_ferry: Some("SHAD"),
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
        service_depot: "NADEPT",
        main_tank: "HTNK",
        tier2_tank: "APOC",
        basic_infantry: "E2",
        // The Soviet section: ForbiddenHouses refuses the Allies and Yuri
        // (retail [SENGINEER], image ENGINEER).
        engineer: "SENGINEER",
        // The Flak Track: the raid ride and the roster's own AA lead.
        engineer_ride: Some("HTK"),
        super_weapon_building: "NAIRON",
        super_weapon_type: "IronCurtainSpecial",
        second_super_weapon_building: "NAMISL",
        second_super_weapon_type: "NukeSpecial",
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
            // The Libyan Demolition Truck: a bomb on wheels (`Demobomb`
            // primary and death weapon, `Explodes=yes`) — AntiArmor, never
            // Siege, since the wave's standoff post would keep a suicide
            // truck out of its own blast. `RequiredHouses=Africans` gates
            // it to Libya through `can_build`; the `needs` radar carries
            // the retail NAWEAP,RADAR prerequisite the AI house's
            // `CanBuild` would skip (cost 1500, TechLevel 10).
            RosterEntry {
                name: "DTRUCK",
                role: Role::AntiArmor,
                needs: Some("NARADR"),
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
            // The attack dog: also this side's scout (the `scout` field
            // names it — the scouting chain keeps its own pair alive), so
            // the roster row fields hounds past that pair: the extras hold
            // the sentry ring and bite the spy, `DetectDisguise=yes`,
            // 200 credits, TechLevel 2, no prerequisite.
            RosterEntry {
                name: "DOG",
                role: Role::AntiInfantry,
                needs: None,
            },
            // The Cuban Terrorist: a 200-credit suicide rush (TerrorBomb
            // on attack and on death) — cheap enough to ride the wave as
            // living ordnance. `RequiredHouses=Confederation` gates him
            // to Cuba through `can_build` (retail TechLevel 5).
            RosterEntry {
                name: "TERROR",
                role: Role::AntiInfantry,
                needs: None,
            },
            // The stolen-tech trio — the Allied table's note applies word
            // for word: all three sections ride every side's roster, the
            // spy's theft opens can_build, and until then the buildable
            // filter passes them over. Chrono Ivan bombs armor
            // (AntiArmor), the Psi-Corp Trooper's mind control sieges
            // (the Yuri Prime row's rationale), the Chrono Commando raids
            // infantry (Tanya's).
            RosterEntry {
                name: "CCOMAND",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "CIVAN",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "PTROOP",
                role: Role::Siege,
                needs: None,
            },
        ],
        defense_basic: "NALASR",
        defense_aa: "NAFLAK",
        defense_heavy: "TESLA",
        // The Soviet Wall; the gap generator and the Grand Cannon are
        // Allied-only retail pieces, so their entries stay empty.
        wall: Some("NAWALL"),
        gap_generator: None,
        country_cannon: None,
        naval_yard: "NAYARD",
        naval_ship: "SUB",
        // The Sea Scorpion is the AA escort (its FlakWeapon fires AA; the
        // old pick, the Giant Squid, is an anti-hull grappler, not AA).
        // The Squid rotates in the hull line behind its own lab instead,
        // and the Dreadnought is the lab's capital piece (retail
        // Prerequisite=NAYARD,NATECH on both, Spawns=DMISL).
        naval_aa: Some("HYD"),
        naval_hull_extra: Some("SQD"),
        naval_capital: Some("DRED"),
        aircraft: &[],
        scout: "DOG",
        fortress: None,
        // The Armored Transport: retail Prerequisite=NAYARD, TechLevel 2,
        // `Passengers=12`, `MovementZone=Amphibious`.
        amphibious_transport: Some("SAPC"),
        // No air hull: the Soviets field no passenger aircraft, so the
        // ferry line stays on the naval yard's hover transport alone.
        air_ferry: None,
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
        service_depot: "YAGRND",
        main_tank: "LTNK",
        tier2_tank: "MIND",
        basic_infantry: "INIT",
        // The Yuri section: ForbiddenHouses refuses the Allies and the
        // Soviets (retail [YENGINEER], image ENGINEER).
        engineer: "YENGINEER",
        // No land transport: Yuri's only `Passengers=` hull is the amphibious
        // Hover Transport the ferry owns — the raid branch stays asleep.
        engineer_ride: None,
        super_weapon_building: "YAPPET",
        super_weapon_type: "PsychicDominatorSpecial",
        second_super_weapon_building: "YAGNTC",
        second_super_weapon_type: "GeneticConverterSpecial",
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
            // The attack dog: Yuri's anti-infantry and anti-spy hound
            // (`DetectDisguise=yes`, 200 credits, TechLevel 2). Retail
            // ships two Yuri-country twins ([YADOG] and [YDOG], both
            // `RequiredHouses=YuriCountry`, mirroring the sides' own dog
            // sections); the table takes [YDOG]. Neither twin's
            // `Prerequisite` (GAPILE/NAHAND) gates a house that owns its
            // barracks.
            RosterEntry {
                name: "YDOG",
                role: Role::AntiInfantry,
                needs: None,
            },
            // The stolen-tech trio — the Allied table's note applies word
            // for word: all three sections ride every side's roster, the
            // spy's theft opens can_build, and until then the buildable
            // filter passes them over. The Psi-Corp Trooper's mind
            // control sieges beside the hero (the same rationale),
            // Chrono Ivan bombs armor, the Chrono Commando raids
            // infantry.
            RosterEntry {
                name: "CCOMAND",
                role: Role::AntiInfantry,
                needs: None,
            },
            RosterEntry {
                name: "CIVAN",
                role: Role::AntiArmor,
                needs: None,
            },
            RosterEntry {
                name: "PTROOP",
                role: Role::Siege,
                needs: None,
            },
        ],
        defense_basic: "YAGGUN",
        defense_aa: "YAGGUN",
        defense_heavy: "YAPSYT",
        // Yuri's wall is the Citadel Wall ([GAFWLL], retail `Wall=yes`
        // behind YABRCK, TechLevel 2, 100 credits); the gap generator and
        // the Grand Cannon are Allied pieces.
        wall: Some("GAFWLL"),
        gap_generator: None,
        country_cannon: None,
        naval_yard: "YAYARD",
        naval_ship: "BSUB",
        // The Boomer is both hull and capital (its own cruise missiles
        // spawn aboard, retail Spawns=CMISL), so the second-hull and
        // capital pair stays empty and the quota line is the whole navy.
        naval_aa: None,
        naval_hull_extra: None,
        naval_capital: None,
        aircraft: &[],
        scout: "INIT",
        fortress: None,
        // The Hover Transport: retail Prerequisite=YAYARD, TechLevel 2,
        // `Passengers=12`, `MovementZone=Amphibious`.
        amphibious_transport: Some("YHVR"),
        // No air hull: Yuri fields no passenger aircraft, so the ferry
        // line stays on the naval yard's hover transport alone.
        air_ferry: None,
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
    /// The MCV's cell the deploy attempts were spent at: a walked MCV
    /// re-arms [`Self::deploy_attempts`] (the budget is per foundation,
    /// not per game).
    deploy_origin: Option<(u16, u16)>,
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
    /// The base-alarm stance's focus cell and the tick it lifts; see
    /// [`Self::base_defense_orders`]. Pinned state, never read from the
    /// runtime.
    defense_focus: Option<((u16, u16), u32)>,
    /// Units committed to the active base-alarm stance, off their guard
    /// posts until the stance lifts. Pinned state, never read from the
    /// runtime.
    defenders: Vec<u64>,
    /// The naval survey's latched shoreline cell for the naval yard: the
    /// one legal WaterSet placement inside the base's build area, so the
    /// ready yard never has to search for it at placement time. Pinned
    /// state, never read from the runtime.
    naval_site: Option<(u16, u16)>,
    /// The tick the next shoreline survey may run; see
    /// [`NAVAL_PROBE_TICKS`]. Pinned state, never read from the runtime.
    naval_probe_due: u32,
    /// Shoreline surveys spent without a find; see [`NAVAL_PROBE_MAX`].
    /// Pinned state, never read from the runtime.
    naval_probes: u8,
    /// Whether the navy is dead for this game (no legal shoreline near any
    /// yard). Pinned state, never read from the runtime.
    naval_abandoned: bool,
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
    /// Whether the second superweapon has ever stood: the once-per-game
    /// latch for the silo/mutator/chronosphere rung, set the same way
    /// `sw_ever_stood` is. Pinned state, never read from the runtime.
    sw2_ever_stood: bool,
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
    /// Whether the ferry's one-off commission has issued: the two ferry
    /// hulls — the naval yard's hover transport and the war factory's air
    /// hull — share this one latch, so a base commissions one ferry line,
    /// never two. Pinned state, never read from the runtime.
    transport_issued: bool,
    /// The remote ore-field cell latched for the expansion refinery once
    /// the home economy reaches its design steady state; kept after the
    /// refinery stands so the second survey can exclude its field.
    /// Pinned state, never read from the runtime.
    expansion_site: Option<(u16, u16)>,
    /// Whether a refinery has ever stood at the expansion distance: arms
    /// the second-MCV commission. Pinned state, set from observation the
    /// same way `sw_ever_stood` is.
    remote_refinery_stood: bool,
    /// Whether the expansion MCV's commission has issued. Pinned state,
    /// never read from the runtime.
    expansion_yard_issued: bool,
    /// The second ore field the expansion MCV walks to. Pinned state,
    /// never read from the runtime.
    expansion2_site: Option<(u16, u16)>,
    /// The latched center cell of the second yard's foundation — the
    /// expansion MCV's walk aim. A re-scanned aim drifts as the ore
    /// spreads and is never reached; the latch walks one cell. Pinned
    /// state, never read from the runtime.
    expansion_deploy_target: Option<(u16, u16)>,
    /// The tick an ore-field re-survey may run (both surveys: the first
    /// expansion's refinery site and the second yard's field). A map whose
    /// every field neighborhood refuses a foundation must not re-scan (a
    /// grid pass plus a placement probe per candidate) every tick while
    /// the house waits. Pinned state, never read from the runtime.
    survey_due: u32,
    /// Deploy attempts per expansion MCV, by entity id. Pinned state,
    /// never read from the runtime.
    expansion_deploy_attempts: std::collections::HashMap<u64, u32>,
    /// The tick an expansion MCV's deploy retry lifts. Pinned state,
    /// never read from the runtime.
    next_expansion_deploy_tick: u32,
    /// Whether the expansion plan is dead for this game (no second field,
    /// or deploys refused past the attempt cap). Pinned state, never read
    /// from the runtime.
    expansion_abandoned: bool,
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
    /// The wall ring's finished targets, by entity id: a structure whose
    /// perimeter was fully walked (segments placed, cells refused, or the
    /// ring already wall-to-wall) is one and done — a fallen wall segment
    /// is never rebuilt. Pinned state, never read from the runtime.
    walled: std::collections::HashSet<u64>,
    /// The cells the wall placement has already issued into — placed or
    /// refused alike — so a refused cell is never knocked on twice ahead of
    /// a fresher one. Pinned state, never read from the runtime.
    wall_cells: std::collections::HashSet<(u16, u16)>,
    /// Combat production slots issued since deploy — vehicles and infantry
    /// each. Pinned state driving the specialist wheels; see
    /// [`SPECIALIST_EVERY_VEHICLE`].
    vehicle_slots: u32,
    infantry_slots: u32,
    /// The tick the next broken-bridge survey may run; see
    /// [`BRIDGE_SURVEY_TICKS`]. Pinned state, never read from the runtime.
    bridge_survey_due: u32,
    /// The latched bridge-repair hut worth an engineer. Pinned state, never
    /// read from the runtime.
    bridge_hut: Option<u64>,
    /// The engineer walking to the latched hut. Pinned state, never read
    /// from the runtime.
    bridge_engineer: Option<u64>,
    /// The tick the repair limb's next dispatch or commission may issue;
    /// see [`BRIDGE_RETRY_TICKS`]. Pinned state, never read from the runtime.
    bridge_retry_due: u32,
    /// Engineers already dispatched at a neutral tech building, by id, with
    /// the building each walks at: consumed (success) or lost engineers and
    /// no-longer-neutral targets drop out; a bound engineer is this chain's
    /// own and draws no second order. Pinned state, never read from the
    /// runtime.
    capture_bound: std::collections::HashMap<u64, u64>,
    /// The tick the next engineer-capture window may run at; see
    /// [`ENGINEER_CAPTURE_WINDOW_TICKS`]. Pinned state, never read from the
    /// runtime.
    capture_due: u32,
    /// The live engineer-ride raid cycle, one at a time; see
    /// [`EngineerRaid`]. Pinned state, never read from the runtime.
    raid_stage: Option<EngineerRaid>,
    /// The tick the current raid stage started at, for the hard
    /// [`RAID_STAGE_TIMEOUT_TICKS`] budget. Pinned state, never read from
    /// the runtime.
    raid_stage_since: u32,
    /// The tick the next raid cycle may arm at; see [`RAID_COOLDOWN_TICKS`].
    /// Pinned state, never read from the runtime.
    raid_cooldown_until: u32,
    /// The tick the next raid arming window may run at; see
    /// [`ENGINEER_CAPTURE_WINDOW_TICKS`]. Pinned state, never read from the
    /// runtime.
    raid_arm_due: u32,
    /// Blocker shoves spent per opening MCV, keyed by entity id, each
    /// budget keyed to the latched cell it was spent at; see
    /// [`SHOVE_ATTEMPTS`]. Pinned state, never read from the runtime.
    shove_attempts: std::collections::HashMap<u64, u32>,
    /// The cell each MCV's shove budget is keyed to; see
    /// [`Self::shove_attempts`]. Pinned state, never read from the runtime.
    shove_origin: std::collections::HashMap<u64, (u16, u16)>,
    /// The cell and tick each opening MCV was last seen standing still at;
    /// see [`MCV_STUCK_TICKS`]. Pinned state, never read from the runtime.
    mcv_stuck_since: std::collections::HashMap<u64, ((u16, u16), u32)>,
    /// Opening MCVs whose site was abandoned (no reachable foundation and
    /// the shove budget spent): the deploy branch draws them no more
    /// orders. Pinned state, never read from the runtime.
    mcv_abandoned: std::collections::HashMap<u64, ()>,
    /// Where the growth plan wants the building it just queued placed: the
    /// type plus whether it routes to the expansion yard's rings instead of
    /// the home yard's. Set at every building queue site (defense guns for
    /// the under-quota base, the expansion cluster's power and refinery),
    /// read by the placement branch and overwritten at the next queue — a
    /// canceled intent's type is never ready, so a stale entry cannot
    /// misroute a different item. Pinned state, never read from the
    /// runtime.
    placement_intent: Option<(InternedId, bool)>,
    /// The structure the repair limb switched repair on for: the command is
    /// a toggle (a re-issue would switch the repair back off), so the
    /// activation latches until the building's repair byte reads off again
    /// — repaired whole, unpaid, dead or sold. Pinned state, never read
    /// from the runtime.
    repairing_structure: Option<u64>,
    /// Earliest tick the repair limb's next survey may run; see
    /// [`REPAIR_SURVEY_TICKS`]. Pinned state, never read from the runtime.
    repair_due: u32,
    /// The tick the current [`Self::orders`] pass runs for, latched at the
    /// entry so the structure census knows when its counts are stale.
    /// Pinned state, never read from the runtime.
    now_tick: u32,
    /// The tick the structure census was last rebuilt; `u32::MAX` forces
    /// the first rebuild. Pinned state, never read from the runtime.
    census_tick: u32,
    /// The per-tick structure census — type id to standing count, rebuilt
    /// at most once per orders pass instead of one full entity walk per
    /// ladder read. Pinned state, never read from the runtime.
    census_counts: std::collections::HashMap<InternedId, u32>,
    // ---- Layer 3: combat & unit management state ----
    /// The futility backoff, per entity: see [`FutilityState`]. The shared
    /// helper behind the repeated-order sites this layer owns (guard
    /// re-posts, wave dispatch, scout dispatch, boarding, raids, the hero's
    /// sabotage run, the spy's run). Pinned state, never read from the
    /// runtime.
    futility: std::collections::HashMap<u64, FutilityState>,
    /// Infantry reserved for the fortress's cargo: excluded from the wave
    /// pool while their boarding is pending, released after
    /// [`BOARD_MAX_ATTEMPTS`] refused Enters or once they ride in limbo.
    /// Pinned state, never read from the runtime.
    boarders: Vec<u64>,
    /// Enter attempts per (unit, fortress) pair: see [`BOARD_MAX_ATTEMPTS`].
    /// Pinned state, never read from the runtime.
    board_attempts: std::collections::HashMap<(u64, u64), u32>,
    /// Raiders with a live raid (Terror Drone, Crazy Ivan, Floating Disk):
    /// excluded from the generic wave pool until their order completes.
    /// Pinned state, never read from the runtime.
    raiders: Vec<u64>,
    /// The tick the next raider window may issue at; see
    /// [`RAIDER_RETRY_TICKS`]. Pinned state, never read from the runtime.
    raid_due: u32,
    /// The tick the next amphibious-ferry window may run at; see
    /// [`FERRY_WINDOW_TICKS`]. Pinned state, never read from the runtime.
    ferry_due: u32,
    /// Units the deploy mechanic already toggled: the sim deploy is a
    /// toggle, so a re-issue would undeploy. Pinned state, never read from
    /// the runtime.
    deploy_issued: std::collections::HashSet<u64>,
    /// The tick the next deploy-mechanic window may scan at; see
    /// [`DEPLOY_MECH_TICKS`]. Pinned state, never read from the runtime.
    deploy_due: u32,
    /// Dark cells the scouting chain has struck out: cells some scout was
    /// paced at repeatedly without them ever revealing. Pinned state, never
    /// read from the runtime.
    scout_blacklist: std::collections::HashSet<(u16, u16)>,
    /// Whether the spy's single commission has issued; the quota is one per
    /// game. Pinned state, never read from the runtime.
    spy_issued: bool,
    /// Whether a naval-sortie episode is launching: the target latched, the
    /// hulls dispatched one per tick until only the home guard remains —
    /// the wave episode's shape, at sea. Pinned state, never read from the
    /// runtime.
    naval_sortie_launching: bool,
    /// The sortie episode's latched target (an enemy hull, else an enemy
    /// shore structure): every hull of the episode attacks the same one, so
    /// the group arrives as a mass. Cleared when the episode closes or the
    /// target dies. Pinned state, never read from the runtime.
    naval_sortie_target: Option<u64>,
    /// Hulls already dispatched in the current sortie episode. A dispatch
    /// the command layer drops leaves its hull mission-idle; without this
    /// list the episode would re-order the same hull every tick — the wave
    /// branch's `launched` list, naval edition. Cleared with the episode.
    /// Pinned state, never read from the runtime.
    naval_sortie_launched: Vec<u64>,
    /// Earliest tick the next naval-sortie episode may arm at; see
    /// [`NAVAL_SORTIE_WINDOW_TICKS`]. Pinned state, never read from the
    /// runtime.
    naval_sortie_due: u32,
}

impl SuperAi {
    /// Bind the AI to `house` at `difficulty`.
    pub fn new(house: InternedId, difficulty: SuperAiDifficulty) -> Self {
        Self {
            house,
            difficulty,
            next_deploy_tick: 0,
            deploy_attempts: 0,
            deploy_origin: None,
            wave_launching: false,
            launched: Vec::new(),
            next_wave_tick: 0,
            defense_focus: None,
            defenders: Vec::new(),
            naval_site: None,
            naval_probe_due: 0,
            naval_probes: 0,
            naval_abandoned: false,
            order_gate: std::collections::HashMap::new(),
            next_airstrike_tick: 0,
            next_sw_fire_tick: 0,
            sw_ever_stood: false,
            sw2_ever_stood: false,
            hero_issued: false,
            fortress_issued: false,
            aa_issued: false,
            transport_issued: false,
            expansion_site: None,
            remote_refinery_stood: false,
            expansion_yard_issued: false,
            expansion2_site: None,
            expansion_deploy_target: None,
            survey_due: 0,
            expansion_deploy_attempts: std::collections::HashMap::new(),
            next_expansion_deploy_tick: 0,
            expansion_abandoned: false,
            place_stuck: std::collections::HashMap::new(),
            place_pause_until: std::collections::HashMap::new(),
            walled: std::collections::HashSet::new(),
            wall_cells: std::collections::HashSet::new(),
            rally_gate: std::collections::HashMap::new(),
            vehicle_slots: 0,
            infantry_slots: 0,
            bridge_survey_due: 0,
            bridge_hut: None,
            bridge_engineer: None,
            bridge_retry_due: 0,
            capture_bound: std::collections::HashMap::new(),
            capture_due: 0,
            raid_stage: None,
            raid_stage_since: 0,
            raid_cooldown_until: 0,
            raid_arm_due: 0,
            shove_attempts: std::collections::HashMap::new(),
            shove_origin: std::collections::HashMap::new(),
            mcv_stuck_since: std::collections::HashMap::new(),
            mcv_abandoned: std::collections::HashMap::new(),
            placement_intent: None,
            repairing_structure: None,
            repair_due: 0,
            now_tick: 0,
            census_tick: u32::MAX,
            census_counts: std::collections::HashMap::new(),
            futility: std::collections::HashMap::new(),
            boarders: Vec::new(),
            board_attempts: std::collections::HashMap::new(),
            raiders: Vec::new(),
            raid_due: 0,
            ferry_due: 0,
            deploy_issued: std::collections::HashSet::new(),
            deploy_due: 0,
            scout_blacklist: std::collections::HashSet::new(),
            spy_issued: false,
            naval_sortie_launching: false,
            naval_sortie_target: None,
            naval_sortie_launched: Vec::new(),
            naval_sortie_due: 0,
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
    /// tick; else post idle units on an Area Guard), toggle a Desolator
    /// into its radiation stance on an enemy foot assault, post the dogs'
    /// sentry ring, sail the navy's sortie at the nearest enemy hull or
    /// shore, fly the air wing's
    /// sortie, board the fortress's anti-armor cargo, walk the expansion
    /// MCV to the second ore field and deploy it, walk an idle scout
    /// toward the map's nearest dark cell, keep the building queue walking
    /// the ladder, raise the wall ring once the core gun quotas hold, hold
    /// the war factories' rallies around the yard, keep
    /// the vehicle queue on miners-then-tanks, keep the barracks queue on
    /// the side's basic soldier. See the module docs for the determinism
    /// contract.
    pub fn orders(&mut self, runtime: &SimRuntime, tick: u32) -> Vec<CommandEnvelope> {
        self.now_tick = tick;
        let sim = &runtime.simulation;
        let rules = &runtime.resources.rules;
        let at = sim.session().tick() + 1;
        let owner = sim.interner.resolve(self.house).to_string();

        // Deploy: the launch MCV deploys into the side's construction yard.
        // try_to_deploy tests the origin one cell up-left of the MCV with
        // the MCV itself lifted, and `is_clear_to_build` counts every
        // other standing infantry and vehicle as an occupant — a community
        // map's packed 9x9 start never clears on its own while every unit
        // stands idle waiting for the yard (observed: six houses, five
        // attempts each, no yard ever, the whole match silent). So the
        // opening spends its ticks on clearing instead of attempts: own
        // idle units inside the foundation are walked out one Move at a
        // time, a start the ground itself refuses walks the MCV to the
        // nearest legal foundation (the escort's pattern in miniature),
        // and only a foundation that would land draws the paced DeployMcv.
        // The yard type comes from the MCV's own `DeploysInto` — the same
        // rule `deployable_mcv` matched — because `opening()` resolves
        // through a *standing* yard: gating this branch on it would be the
        // chicken-and-egg that froze a house whose native deploy never
        // landed (observed: 42000 ticks of empty orders, the MCV standing
        // until the enemy found it).
        if let Some(mcv) = self.deployable_mcv(sim, rules)
            && !self.mcv_abandoned.contains_key(&mcv)
            && let Some(entity) = sim.substrate.entities.get(mcv)
            && let Some(yard_name) = rules
                .object(sim.interner.resolve(entity.type_ref()))
                .and_then(|ty| ty.deploys_into.as_deref())
            && let Some(yard_ty) = rules.object(yard_name)
            && matches!(
                entity.passive_acquire_mission(),
                crate::sim::mission::MissionType::None
                    | crate::sim::mission::MissionType::Sleep
                    | crate::sim::mission::MissionType::Guard
                    | crate::sim::mission::MissionType::AreaGuard
            )
        {
            let (mx, my) = (entity.position.rx() as i16, entity.position.ry() as i16);
            let lifted = crate::sim::build_site::LiftedUnit::read(sim, mcv);
            let clear = crate::sim::build_site::can_place_building_at(
                sim,
                rules,
                Some(&runtime.resources.overlay_registry),
                yard_ty,
                (mx - 1, my - 1),
                None,
                None,
                Some(&lifted),
            );
            let blocker = (!clear)
                .then(|| {
                    yard_ty.foundation.cell_offsets().iter().find_map(|&(dx, dy)| {
                        sim.cell_objects(
                            ((mx - 1 + dx) as u16, (my - 1 + dy) as u16),
                            crate::sim::movement::locomotor::MovementLayer::Ground,
                        )
                        .find_map(|member| match member {
                            crate::sim::occupancy::CellObjectMember::Entity(id) => sim
                                .substrate
                                .entities
                                .get(id)
                                .filter(|e| {
                                    e.owner() == self.house
                                        && e.category()
                                            == crate::map::entities::EntityCategory::Unit
                                        && matches!(
                                            e.passive_acquire_mission(),
                                            crate::sim::mission::MissionType::None
                                                | crate::sim::mission::MissionType::Sleep
                                                | crate::sim::mission::MissionType::Guard
                                                | crate::sim::mission::MissionType::AreaGuard
                                        )
                                })
                                .map(|_| id),
                            _ => None,
                        })
                    })
                })
                .flatten();
            if !clear
                && let Some(unit) = blocker
                && self.order_gate.get(&unit).is_none_or(|&due| tick >= due)
            {
                // The shove budget is keyed to the MCV's own latched cell:
                // churn among the blockers must not renew it (observed: the
                // shove loop re-armed forever while the MCV stood still, 582
                // orders across 145 shoved units in one game).
                let here = (entity.position.rx(), entity.position.ry());
                if self.shove_origin.get(&mcv) != Some(&here) {
                    self.shove_origin.insert(mcv, here);
                    self.shove_attempts.insert(mcv, 0);
                }
                let attempts = self.shove_attempts.entry(mcv).or_insert(0);
                if *attempts < SHOVE_ATTEMPTS {
                    *attempts += 1;
                    self.gate_idle_order(unit, tick);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::Move {
                            entity_id: unit,
                            target_rx: (mx + 6).max(1) as u16,
                            target_ry: my.max(1) as u16,
                        },
                    )];
                }
                // The budget is spent at this cell — and the shove target
                // itself may be the problem, a blocker the zone grid cannot
                // connect to (mx+6,my) nulls its walk and jams back. Fall
                // through to the stuck window below; no more shoves here.
            }
            if !clear && blocker.is_none() {
                // The ground itself refuses this start: walk the MCV to
                // the nearest foundation the placement test admits (its
                // rings skip every origin the standing MCV itself would
                // block) and deploy from its center on arrival. A walking
                // MCV is not idle and draws no order until it stops. The
                // walk is a deliberate re-site, so the per-foundation
                // budgets re-arm here — a foundation the ring one cell
                // over never crosses the resite distance.
                if let Some((ox, oy)) = find_site_around(
                    sim,
                    rules,
                    &runtime.resources.overlay_registry,
                    yard_ty,
                    (mx, my),
                    self.house,
                ) {
                    self.deploy_attempts = 0;
                    self.shove_attempts.remove(&mcv);
                    self.shove_origin.remove(&mcv);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::Move {
                            entity_id: mcv,
                            target_rx: ox + 1,
                            target_ry: oy + 1,
                        },
                    )];
                }
            }
            if clear
                && tick >= self.next_deploy_tick
                && self.deploy_attempts < DEPLOY_ATTEMPTS
            {
                self.next_deploy_tick = tick + DEPLOY_RETRY_TICKS;
                self.deploy_attempts += 1;
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::DeployMcv { entity_id: mcv },
                )];
            }
            // The stuck window: an MCV that has stood on one cell for
            // [`MCV_STUCK_TICKS`] and is still refused its deploy spot is
            // not going to deploy here. A reachable legal foundation is
            // walked to once (the zone gate keeps the walk from aiming
            // across a breach); none at all abandons the site — this MCV
            // draws no more orders from the branch, instead of shoving
            // forever. A real re-site (below) re-arms everything.
            if !clear {
                let here = (entity.position.rx(), entity.position.ry());
                let stuck = self.mcv_stuck_since.entry(mcv).or_insert((here, tick));
                if stuck.0 != here {
                    *stuck = (here, tick);
                } else if tick >= stuck.1 + MCV_STUCK_TICKS {
                    let resite = find_site_around(
                        sim,
                        rules,
                        &runtime.resources.overlay_registry,
                        yard_ty,
                        (mx, my),
                        self.house,
                    )
                    .filter(|&(ox, oy)| {
                        zones_connected(
                            sim,
                            crate::rules::locomotor_type::MovementZone::Normal,
                            here,
                            (ox + 1, oy + 1),
                        )
                    });
                    if let Some((ox, oy)) = resite {
                        self.mcv_stuck_since.insert(mcv, ((ox + 1, oy + 1), tick));
                        self.deploy_attempts = 0;
                        self.shove_attempts.remove(&mcv);
                        self.shove_origin.remove(&mcv);
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::Move {
                                entity_id: mcv,
                                target_rx: ox + 1,
                                target_ry: oy + 1,
                            },
                        )];
                    }
                    self.mcv_abandoned.insert(mcv, ());
                }
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
        // A walked MCV re-arms the per-foundation budgets: the deploy
        // attempts, the shove budget and the stuck window are spent per
        // site, and transient refusals while the clearing Moves were still
        // walking must not spend the whole game's five. The re-arm is keyed
        // to a real re-site — [`MCV_RESITE_DISTANCE`] from the latched one —
        // not to any movement at all: a jiggle of one cell must not renew
        // the budgets the way it renewed the shove loop (observed).
        if let Some(mcv) = self.deployable_mcv(sim, rules)
            && let Some(entity) = sim.substrate.entities.get(mcv)
        {
            let here = (entity.position.rx(), entity.position.ry());
            let resited = match self.deploy_origin {
                Some((ox, oy)) => {
                    ox.abs_diff(here.0).max(oy.abs_diff(here.1)) >= MCV_RESITE_DISTANCE
                }
                None => true,
            };
            if resited {
                self.deploy_origin = Some(here);
                self.deploy_attempts = 0;
                self.shove_attempts.remove(&mcv);
                self.shove_origin.remove(&mcv);
                self.mcv_stuck_since.remove(&mcv);
                self.mcv_abandoned.remove(&mcv);
            }
        }
        // Everything below the deploy speaks through the side's opening,
        // which resolves through the standing yard — the branch above is
        // what stands it.
        let Some(opening) = self.opening(sim) else {
            return Vec::new();
        };
        // The ladder's superweapon rungs are once-per-game each: remember
        // the supers standing, so a razed storm never rebuilds (see the
        // walk below for the starvation that motivated the latch) and the
        // second super's rung opens on the first one having stood.
        if self.count_structures_of_type(sim, opening.super_weapon_building) >= 1 {
            self.sw_ever_stood = true;
        }
        if self.count_structures_of_type(sim, opening.second_super_weapon_building) >= 1 {
            self.sw2_ever_stood = true;
        }

        // The expansion survey and its proof latch: pure observations,
        // hoisted outside the building walk's `any_tank` gate — a window
        // with every tank dead must not blind the base to a remote
        // refinery that stood and fell while the growth block never ran
        // (observed: the census saw the refinery; the latch never did,
        // and the whole expansion froze behind it).
        let home_center = self.structure_center(sim, opening.construction_yard);
        // The standing yards (Layer 2): the first in the substrate's stable
        // order is the home base every legacy ring scan centers on; once a
        // second yard stands, the one furthest from home — or nearest the
        // expansion MCV's latched field, when that walk is still armed — is
        // the expansion base the growth limbs route to.
        let yard_centers = self.own_yard_centers(sim, opening);
        let expansion_center = if yard_centers.len() >= 2 {
            match self.expansion2_site {
                Some(site) => yard_centers.iter().copied().min_by_key(|cell| {
                    (cell.0 - site.0 as i16).abs().max(cell.1 - site.1 as i16).abs()
                }),
                None => {
                    let home = home_center.unwrap_or(yard_centers[0]);
                    yard_centers
                        .iter()
                        .copied()
                        .max_by_key(|cell| (cell.0 - home.0).abs().max(cell.1 - home.1).abs())
                }
            }
        } else {
            None
        };
        if self.expansion_site.is_none()
            && !self.remote_refinery_stood
            && tick >= self.survey_due
            && let Some(home) = home_center
            && self.count_structures_of_type(sim, opening.refinery) >= REFINERY_TARGET
            && opening.harvester.is_none_or(|miner| {
                self.count_units_of_type(sim, miner) >= HARVESTER_TARGET.max(REFINERY_TARGET * 2)
            })
        {
            // Ore drifts with ore_growth, but a latched field regrows as
            // fast as it depletes — the pick stays good enough for one
            // refinery's lifetime. The refinery itself is the validating
            // structure: the field must host its foundation, or the
            // placement the ladder later attempts has nothing to land on.
            self.expansion_site = self.best_ore_field(
                sim,
                rules,
                &runtime.resources.overlay_registry,
                home,
                None,
                Some(opening.refinery),
            );
            if self.expansion_site.is_none() {
                self.survey_due = tick + SURVEY_RETRY_TICKS;
            }
        }
        // A refinery standing at the expansion distance completes the
        // first expansion: its FreeUnit miner spawns beside it and the
        // native scan-and-dock machinery runs the remote economy with no
        // further orders from this branch.
        if self.expansion_site.is_some()
            && !self.remote_refinery_stood
            && let Some(home) = home_center
            && self.own_structures(sim).iter().any(|(id, name)| {
                name == opening.refinery
                    && sim.substrate.entities.get(*id).is_some_and(|entity| {
                        (entity.position.rx() as i16 - home.0)
                            .abs()
                            .max((entity.position.ry() as i16 - home.1).abs())
                            >= EXPANSION_MIN_DISTANCE
                    })
            })
        {
            self.remote_refinery_stood = true;
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
            // Routing, in order: the first expansion's refinery may still
            // route around its latched far ore field — but only once a yard
            // stands near that field, because the engine admits a placement
            // only inside a friendly structure's build area and a
            // yard-less far site wedged the ready item on OutOfBuildArea
            // rejections. Every other expansion-intent item (the growth
            // plan's mark, `placement_intent`) places in the expansion
            // yard's tight, build-area-validated rings; everything else —
            // and any expansion item whose ring scan fails — falls back to
            // the home rings, so a ready building can never wedge the one
            // placement slot (the old fallback's discipline, kept).
            && let Some((rx, ry)) = {
                let type_name = sim.interner.resolve(ready.type_id);
                // The wall ring's placement (Layer 2.8): a ready segment
                // goes to the ring cell [`Self::wall_next_cell`] picks —
                // the first untried, wall-free cell on the current target's
                // perimeter — and the cell is marked tried the moment the
                // order issues, so a placement the engine refuses moves to
                // the next cell next tick instead of knocking on the same
                // one. The strike counter below still counts the refused
                // attempts and pauses the type, so a fully blocked ring
                // cannot soak the one-command budget. `None` after the
                // escape pass means every ring is genuinely done (the
                // auto-fill completed the last one under us): cancel the
                // 100-credit segment rather than let the ready item hold
                // the Defense factory forever.
                if opening
                    .wall
                    .is_some_and(|wall| sim.interner.get(wall) == Some(ready.type_id))
                {
                    match self.wall_next_cell(
                        sim,
                        rules,
                        opening,
                        &runtime.resources.overlay_registry,
                        true,
                    ) {
                        Some(cell) => {
                            self.wall_cells.insert(cell);
                            Some(cell)
                        }
                        None => {
                            return vec![CommandEnvelope::new(
                                self.house,
                                at,
                                Command::CancelProductionByType {
                                    type_id: ready.type_id,
                                    all: false,
                                },
                            )];
                        }
                    }
                } else {
                let far_site = if type_name == opening.refinery
                    && self.expansion_site.is_some()
                    && !self.remote_refinery_stood
                    && self.yard_near_site(
                        sim,
                        opening,
                        self.expansion_site,
                        EXPANSION_YARD_PROXIMITY,
                    )
                {
                    self.expansion_site
                        .and_then(|(sx, sy)| {
                            home_center.map(|home| {
                                find_site_beyond(
                                    sim,
                                    rules,
                                    &runtime.resources.overlay_registry,
                                    ty,
                                    (sx as i16, sy as i16),
                                    self.house,
                                    home,
                                    EXPANSION_MIN_DISTANCE,
                                )
                            })
                        })
                        .flatten()
                } else {
                    None
                };
                far_site
                    .or_else(|| {
                        // The naval yard's latched shoreline cell (Layer
                        // 2.7): the survey already paid for the ring scan,
                        // so the ready shipyard lands exactly there.
                        // Terrain does not move, and anything that drifted
                        // onto the cell is scattered by the placement's
                        // own flush.
                        if sim.interner.get(opening.naval_yard) == Some(ready.type_id) {
                            self.naval_site
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                    // The expansion routing: the first expansion's refinery
                    // (before its remote latch arms, with the second yard
                    // already standing) and anything the growth plan marked
                    // for the expansion base. The remote foundation must
                    // itself keep the expansion distance from home — the
                    // ring scan starts at the field and can pull a
                    // foundation toward home, and a refinery inside the
                    // distance never arms the second-base chain behind it.
                    let expansion_route = expansion_center.filter(|_| {
                        (type_name == opening.refinery
                            && self.expansion_site.is_some()
                            && !self.remote_refinery_stood)
                            || self
                                .placement_intent
                                .is_some_and(|(intent_type, expansion)| {
                                    intent_type == ready.type_id && expansion
                                })
                    });
                    match expansion_route {
                        Some(center) => {
                            find_site_around_capped(
                                sim,
                                rules,
                                &runtime.resources.overlay_registry,
                                ty,
                                center,
                                self.house,
                                Some(owner.as_str()),
                                EXPANSION_SITE_RADIUS,
                            )
                            .or_else(|| {
                                home_center.and_then(|home| {
                                    find_site_around(
                                        sim,
                                        rules,
                                        &runtime.resources.overlay_registry,
                                        ty,
                                        home,
                                        self.house,
                                    )
                                })
                            })
                        }
                        None => home_center.and_then(|home| {
                            find_site_around(
                                sim,
                                rules,
                                &runtime.resources.overlay_registry,
                                ty,
                                home,
                                self.house,
                            )
                        }),
                    }
                })
                }
            }
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
        // monopolizing this tick's single command. Both supers — the
        // ladder's first and second — sit in the walk: whichever is
        // charged first fires.
        if tick >= self.next_sw_fire_tick {
            for sw_type_name in [opening.super_weapon_type, opening.second_super_weapon_type] {
                if let Some(sw_type) = sim.interner.get(sw_type_name)
                    && let Some(instance) = sim
                        .super_weapons()
                        .get(&self.house)
                        .and_then(|house_sws| house_sws.get(&sw_type))
                    && instance.charge_start_tick() != -1
                    && instance.is_active()
                    && instance.is_ready()
                    && let Some((rx, ry)) = self.superweapon_target(sim, opening, sw_type_name)
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
            }
        }

        // The broken-bridge limb. A breach the zone grid already knows about
        // cuts the base off from every hostile structure beyond it, and the
        // old ungated waves walked into it one dead Move at a time (observed:
        // 582 unreachable orders at one bridgehead across 16000 ticks). The
        // limb's halves both live in [`Self::bridge_repair_orders`], paced so
        // they never soak the one-command budget: a survey latches a hut
        // worth repairing, and the dispatch sends an idle engineer with the
        // engine's own capture click — the repair runs before the capture
        // checks and rebuilds the span synchronously, republishing the zones
        // and reopening the wave targets the gates below hold shut.
        if let Some(command) = self.bridge_repair_orders(sim, rules, opening, tick) {
            return vec![CommandEnvelope::new(self.house, at, command)];
        }

        // The building-repair limb (Layer 2). A damaged structure sits at a
        // fraction of its strength until something repairs it, and nothing
        // in the chain below ever does: the survey latches the worthiest
        // damaged structure and the toggle turns its paid repair on — one
        // activation at a time, economy and factories first, paced so it
        // never soaks the one-command budget. See
        // [`Self::structure_repair_orders`].
        if let Some(command) = self.structure_repair_orders(sim, rules, opening, &owner, tick) {
            return vec![CommandEnvelope::new(self.house, at, command)];
        }

        // Home defense and the offensive. The base alarm (Layer 3.5) comes
        // first: a building hit inside [`BASE_ALARM_TICKS`] arms an
        // active-defense stance that dispatches idle units onto the
        // intruder and, short of [`DEFENSE_COMMIT_MIN`] defenders, pulls
        // the nearest posted units off their guard posts and onto the
        // focus — the old limb answered only with whatever idle unit
        // happened to stand inside [`BASE_DEFENSE_RADIUS`], so a base
        // whose army was away fighting waved the enemy through
        // (observed live: razed yards, no reaction). While the stance
        // stands no new wave departs; the wave already marching stays
        // out. Otherwise idle combat units engage the nearest intruder
        // inside any yard's perimeter. Otherwise the offense moves in
        // waves: once `ATTACK_WAVE_SIZE` units stand gathered (idle or on
        // their guard post), the wave launches — one standing unit per
        // tick attack-moves on the enemy center, so the group departs
        // together and arrives as a mass — until only the guard reserve
        // remains home; production then rebuilds the next wave while the
        // current one fights. While a wave is still gathering, newcomers
        // hold an Area Guard post on the yard. Orders issue one unit per
        // tick; an engaged, marching or posted unit leaves the standing set,
        // so no spam follows, and a wave that dies lets the next one form.
        if let Some(center) = self.structure_center(sim, opening.construction_yard) {
            let idle = self.idle_combat_units(sim, opening);
            let alarm_armed = self.base_alarm_armed(sim, tick);
            if alarm_armed || self.defense_focus.is_some() {
                if let Some(command) =
                    self.base_defense_orders(sim, rules, opening, &idle, tick, alarm_armed)
                {
                    return vec![CommandEnvelope::new(self.house, at, command)];
                }
            }
            if let Some(attacker) = self.due_idle_unit(&idle, tick)
                && let Some(target) = self
                    .own_yard_centers(sim, opening)
                    .into_iter()
                    .find_map(|yard| self.nearest_intruder(sim, yard, BASE_DEFENSE_RADIUS))
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
            // A fresh alarm (a building hit inside the window) holds the
            // next wave home; the stance's mop-up hold alone must not — on
            // a crowded big map the focus stays latched nearly forever and
            // the offense never sailed again (observed live: a besieged
            // Yuri base that stopped attacking for good). The defenders
            // already committed keep fighting either way.
            if !self.wave_launching
                && !alarm_armed
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
                // starve the branches below. A destination is also gated on
                // the zone grid before any order rides out: from the unit's
                // own cell the wave tries the enemy structures in the unit's
                // rotation order (then, without structures, the fallback
                // offsets in the same rotation) and takes the first the
                // ground rows connect. A target across a collapsed bridge is
                // passed over — the sim would null the walk at the breach,
                // the unit would idle there, and the next episode would
                // re-order the same unreachable cell every retry (observed:
                // 582 dead Moves at one broken bridge) — and an episode
                // whose every un-launched unit is cut off simply ends here
                // instead of marching to the water's edge.
                let mut dispatch: Option<(u64, (u16, u16))> = None;
                if standing.len() > GUARD_RESERVE {
                    // The candidates collect first: the `launched` filter's
                    // borrow must end before the loop body's futility
                    // records (pinned state mutation inside the loop).
                    for unit in standing
                        .iter()
                        .copied()
                        .filter(|unit| !self.launched.contains(unit))
                        .collect::<Vec<_>>()
                    {
                        let Some(entity) = sim.substrate.entities.get(unit) else {
                            continue;
                        };
                        // Layer 3: the hero never rides the generic wave (his
                        // own branch owns him), reserved boarders and raiders
                        // are off the pool while their orders run, the raid
                        // cycle's engineer and ride likewise (their stage
                        // machine owns them), and the fortress stays home
                        // while its seats are open and boarding is in
                        // progress.
                        let type_name = sim.interner.resolve(entity.type_ref());
                        if type_name == opening.hero
                            || self.boarders.contains(&unit)
                            || self.raiders.contains(&unit)
                            || self.raid_involves(unit)
                            // Layer 3: the dogs stay home — the sentry ring
                            // is their post and the spy their war; a hound
                            // that marched with the tanks is a dead 200
                            // credits at the first vehicle line.
                            || Self::is_dog(type_name)
                        {
                            continue;
                        }
                        if Some(type_name) == opening.fortress
                            && !self.boarders.is_empty()
                            && entity
                                .passenger_role
                                .cargo()
                                .is_some_and(|cargo| cargo.count() < cargo.capacity())
                        {
                            continue;
                        }
                        let from = (entity.position.rx(), entity.position.ry());
                        let movement_zone = rules
                            .object(type_name)
                            .map(|ty| ty.movement_zone)
                            .unwrap_or_default();
                        let raw = if enemy_targets.is_empty() {
                            enemy_center.and_then(|aim| {
                                (0..SPREAD_OFFSETS.len())
                                    .map(|index| {
                                        (unit as usize + index) % SPREAD_OFFSETS.len()
                                    })
                                    .find_map(|index| {
                                        let spread = SPREAD_OFFSETS[index];
                                        let cell = on_map_cell(
                                            sim,
                                            (
                                                aim.0 + i16::from(spread.0),
                                                aim.1 + i16::from(spread.1),
                                            ),
                                            aim,
                                        );
                                        zones_connected(sim, movement_zone, from, cell)
                                            .then_some(cell)
                                    })
                            })
                        } else {
                            (0..enemy_targets.len())
                                .map(|attempt| {
                                    enemy_targets[(unit as usize + attempt) % enemy_targets.len()]
                                })
                                .find_map(|(_, cell)| {
                                    let cell = (cell.0 as u16, cell.1 as u16);
                                    zones_connected(sim, movement_zone, from, cell).then_some(cell)
                                })
                        };
                        // Layer 3: siege-role units bombard from standoff —
                        // the destination pulls back to a cell short of the
                        // aim along the home line instead of the aim cell
                        // itself (the old walk-in died one V3 at a time in
                        // the target's defense ring).
                        let siege = opening
                            .vehicles
                            .iter()
                            .any(|entry| entry.role == Role::Siege && entry.name == type_name);
                        let destination = match raw {
                            Some(cell) if siege => {
                                siege_standoff_cell(sim, center, cell, from, movement_zone)
                            }
                            other => other,
                        };
                        let Some(cell) = destination else {
                            continue;
                        };
                        // Layer 3: the futility backoff — a unit whose wave
                        // dispatch keeps landing on the same standing cell
                        // (a dropped order, an aim the ground refuses) stops
                        // spending the episode's order on it for a while.
                        if self.futile(unit, FutilityKind::Wave, from, tick) {
                            continue;
                        }
                        dispatch = Some((unit, cell));
                        break;
                    }
                }
                if let Some((unit, (rx, ry))) = dispatch {
                    self.launched.push(unit);
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
                // The reserve is home, every standing unit was already
                // dispatched (dropped orders), or no un-launched unit has a
                // destination the zone grid still connects: end the episode
                // and let production fill the next one — the units held back
                // are not spent, so a repaired span re-opens them.
                self.wave_launching = false;
                self.next_wave_tick = tick + WAVE_RETRY_TICKS;
            }
            // Layer 3: the hero's own small branch — the wave dispatch no
            // longer takes him (filtered above), so what remains is his own
            // pacing. A `C4=` carrier (Tanya) sabotage-runs the first
            // reachable enemy structure in the wave's priority order (its
            // factories lead); the rifle heroes (Boris, Yuri Prime — their
            // weapons auto-fire) simply hold a home-defense post like any
            // defender. The engine's MiG flare support for Boris rides its
            // own thread and stays out of scope.
            let hero_pick = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
                self.order_gate.get(id).is_none_or(|&due| tick >= due)
                    && entity.owner() == self.house
                    && !entity.lifecycle.in_limbo()
                    && sim.interner.resolve(entity.type_ref()) == opening.hero
                    && matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
            });
            if let Some((hero, hero_entity)) = hero_pick {
                let from = (hero_entity.position.rx(), hero_entity.position.ry());
                let c4_carrier = rules.object(opening.hero).is_some_and(|ty| ty.c4);
                let c4_target = if c4_carrier {
                    enemy_targets.iter().copied().find(|&(_, cell)| {
                        zones_connected(
                            sim,
                            crate::rules::locomotor_type::MovementZone::Normal,
                            from,
                            (cell.0.max(0) as u16, cell.1.max(0) as u16),
                        )
                    })
                } else {
                    None
                };
                if let Some((target, _)) = c4_target {
                    if !self.futile(hero, FutilityKind::HeroC4, from, tick) {
                        self.gate_idle_order(hero, tick);
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::PlantC4 {
                                attacker_id: hero,
                                target_building_id: target,
                            },
                        )];
                    }
                } else if matches!(
                    hero_entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None | crate::sim::mission::MissionType::Sleep
                ) && !self.futile(hero, FutilityKind::GuardPost, from, tick)
                    && let Some((px, py)) =
                        connected_ring_cell(sim, rules, center, from, hero, opening.hero)
                {
                    self.gate_idle_order(hero, tick);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::Guard {
                            entity_id: hero,
                            target: Some(crate::sim::combat::TargetKind::Cell(px, py)),
                        },
                    )];
                }
            }

            // Layer 3: the raiders. Three paced specialists leave the
            // generic wave pool while their raid is live (the reservation
            // list above the wave filter) and run their own orders: the
            // Terror Drone hunts the enemy's economy (the nearest harvester's
            // cell), Crazy Ivan bombs a reachable enemy structure (his
            // Attack IS the bomb-attach), and the Floating Disk raids power
            // plants and refineries instead of marching with the tanks it
            // outflies.
            if tick >= self.raid_due {
                let mut raid: Option<(u64, Command)> = None;
                for (id, entity) in sim.substrate.entities.iter_sorted() {
                    if entity.owner() != self.house
                        || entity.lifecycle.in_limbo()
                        || !matches!(
                            entity.category(),
                            crate::map::entities::EntityCategory::Unit
                                | crate::map::entities::EntityCategory::Infantry
                        )
                        || !matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                                | crate::sim::mission::MissionType::Guard
                                | crate::sim::mission::MissionType::AreaGuard
                        )
                        || self.order_gate.get(&id).is_some_and(|&due| tick < due)
                        || self.raiders.contains(&id)
                    {
                        continue;
                    }
                    let type_name = sim.interner.resolve(entity.type_ref());
                    let from = (entity.position.rx(), entity.position.ry());
                    if type_name == RAIDER_DRONE
                        && let Some((cell, _)) = self.nearest_enemy_harvester(sim, from)
                        && zones_connected(
                            sim,
                            crate::rules::locomotor_type::MovementZone::Normal,
                            from,
                            cell,
                        )
                        && !self.futile(id, FutilityKind::Raid, from, tick)
                    {
                        raid = Some((id, Command::AttackMove {
                            entity_id: id,
                            target: None,
                            destination: Some(crate::sim::combat::TargetKind::Cell(
                                cell.0, cell.1,
                            )),
                        }));
                        break;
                    }
                    if type_name == RAIDER_IVAN
                        && let Some((target, _)) = enemy_targets.iter().copied().find(
                            |&(_, cell)| {
                                zones_connected(
                                    sim,
                                    crate::rules::locomotor_type::MovementZone::Normal,
                                    from,
                                    (cell.0.max(0) as u16, cell.1.max(0) as u16),
                                )
                            },
                        )
                        && !self.futile(id, FutilityKind::Raid, from, tick)
                    {
                        raid = Some((id, Command::Attack {
                            attacker_id: id,
                            target_id: target,
                        }));
                        break;
                    }
                    if type_name == RAIDER_DISK
                        && let Some((_, cell)) = self.disc_raid_target(sim)
                        && !self.futile(id, FutilityKind::Raid, from, tick)
                    {
                        raid = Some((id, Command::AttackMove {
                            entity_id: id,
                            target: None,
                            destination: Some(crate::sim::combat::TargetKind::Cell(
                                cell.0.max(0) as u16,
                                cell.1.max(0) as u16,
                            )),
                        }));
                        break;
                    }
                }
                if let Some((id, command)) = raid {
                    self.raiders.push(id);
                    self.gate_idle_order(id, tick);
                    self.raid_due = tick + RAIDER_RETRY_TICKS;
                    return vec![CommandEnvelope::new(self.house, at, command)];
                }
            }

            // Layer 3: the deploy mechanic. Deploy-fire infantry (Guardian
            // GI, Desolator) and the Siege Chopper hold their Deploy toggle
            // all game otherwise — the order exists, the AI never sent it.
            // A unit holding an idle-class post gets its toggle once per
            // game (the sim deploy is a toggle: a re-issue would undeploy),
            // the infantry near home, the simple-deployer chopper out in
            // the field where its siege mode matters.
            if tick >= self.deploy_due {
                self.deploy_due = tick + DEPLOY_MECH_TICKS;
                self.deploy_issued.retain(|&id| {
                    sim.substrate.entities.get(id).is_some_and(|entity| {
                        entity.owner() == self.house && entity.lifecycle.object_alive()
                    })
                });
                let pick = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
                    entity.owner() == self.house
                        && entity.lifecycle.object_alive()
                        && !entity.lifecycle.in_limbo()
                        && matches!(
                            entity.category(),
                            crate::map::entities::EntityCategory::Unit
                                | crate::map::entities::EntityCategory::Infantry
                        )
                        && !self.deploy_issued.contains(id)
                        && !self.boarders.contains(id)
                        && !self.raiders.contains(id)
                        && sim.interner.resolve(entity.type_ref()) != opening.hero
                        // The Desolator sits out this generic pick: its rad
                        // field burns every foot unit that crosses — own
                        // posted infantry included — so the stance arms on
                        // the engagement below, not on standing near home.
                        && sim.interner.resolve(entity.type_ref()) != DESO_TYPE
                        && matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                                | crate::sim::mission::MissionType::Guard
                                | crate::sim::mission::MissionType::AreaGuard
                        )
                        && !entity.is_deployed()
                        && {
                            let type_name = sim.interner.resolve(entity.type_ref());
                            let distance = (i32::from(entity.position.rx() as i16)
                                - i32::from(center.0))
                                .abs()
                                .max(
                                    (i32::from(entity.position.ry() as i16)
                                        - i32::from(center.1))
                                    .abs(),
                                );
                            match entity.category() {
                                crate::map::entities::EntityCategory::Infantry => {
                                    distance <= i32::from(BASE_DEFENSE_RADIUS)
                                        && rules.object(type_name).is_some_and(|ty| {
                                            ty.deployer && ty.undeploy_delay <= 0
                                        })
                                }
                                _ => {
                                    distance > i32::from(BASE_DEFENSE_RADIUS)
                                        && rules
                                            .object(type_name)
                                            .is_some_and(|ty| ty.is_simple_deployer)
                                }
                            }
                        }
                });
                if let Some((id, entity)) = pick {
                    let infantry =
                        entity.category() == crate::map::entities::EntityCategory::Infantry;
                    self.deploy_issued.insert(id);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        if infantry {
                            Command::ToggleInfantryDeploy { entity_id: id }
                        } else {
                            Command::DeployMcv { entity_id: id }
                        },
                    )];
                }
                // Layer 3: the Desolator's radiation stance. Deployed, the
                // soldier holds a Guard in place while the weapon radiates
                // its cell — the engine keeps the stance up on its own and
                // a later Move lifts it, so the AI's whole order is the
                // toggle, issued once per Desolator (the id joins
                // `deploy_issued`: the sim deploy is a toggle, a re-issue
                // would undeploy). The engagement is three enemy foot
                // units inside five cells — an assault buys the field, a
                // scout does not. Undeploy is nobody's order: the field
                // decays without one.
                let deso = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
                    entity.owner() == self.house
                        && entity.lifecycle.object_alive()
                        && !entity.lifecycle.in_limbo()
                        && entity.category() == crate::map::entities::EntityCategory::Infantry
                        && sim.interner.resolve(entity.type_ref()) == DESO_TYPE
                        && !self.deploy_issued.contains(id)
                        && !self.boarders.contains(id)
                        && !self.raiders.contains(id)
                        && !entity.is_deployed()
                        && matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                                | crate::sim::mission::MissionType::Guard
                                | crate::sim::mission::MissionType::AreaGuard
                        )
                        && {
                            let cell = (
                                entity.position.rx() as i16,
                                entity.position.ry() as i16,
                            );
                            self.hostile_infantry_near(sim, cell, DESO_DEPLOY_RADIUS)
                                >= DESO_DEPLOY_INFANTRY
                        }
                });
                if let Some((id, _)) = deso {
                    self.deploy_issued.insert(id);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::ToggleInfantryDeploy { entity_id: id },
                    )];
                }
            }

            // Layer 3: the dogs' sentry ring. The roster's hounds are
            // counter-infiltration base guards (retail `DetectDisguise=yes`
            // on all three sides' sections): posted on a yard ring, the
            // dog's own Guard mission bites the spy and the raiding
            // infantry that step to the door, and the engine's leash walks
            // it back after every chase — so the AI's whole job is handing
            // out the posts. Two dogs per standing yard hold the ring; the
            // "on post" count reads the live Guard missions, so a sentry
            // the alarm recalled or the enemy killed frees its slot and a
            // fresh one takes it. The others stay in the idle pools — the
            // perimeter attack and the alarm recall reach them — and the
            // wave dispatch skips the type outright. The order gate paces
            // the post the way it paces the generic poster's, so a dropped
            // order waits its window instead of re-issuing per tick.
            {
                let sentry_cap =
                    SENTRY_DOGS_PER_YARD * self.own_yard_centers(sim, opening).len().max(1);
                let dogs_on_post = sim
                    .substrate
                    .entities
                    .iter_sorted()
                    .filter(|(_, entity)| {
                        entity.owner() == self.house
                            && entity.category() == crate::map::entities::EntityCategory::Infantry
                            && entity.lifecycle.object_alive()
                            && !entity.lifecycle.in_limbo()
                            && Self::is_dog(sim.interner.resolve(entity.type_ref()))
                            && matches!(
                                entity.passive_acquire_mission(),
                                crate::sim::mission::MissionType::Guard
                                    | crate::sim::mission::MissionType::AreaGuard
                            )
                    })
                    .count();
                if dogs_on_post < sentry_cap {
                    let pick = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
                        self.order_gate.get(id).is_none_or(|&due| tick >= due)
                            && entity.owner() == self.house
                            && entity.category() == crate::map::entities::EntityCategory::Infantry
                            && entity.lifecycle.object_alive()
                            && !entity.lifecycle.in_limbo()
                            && Self::is_dog(sim.interner.resolve(entity.type_ref()))
                            && !self.boarders.contains(id)
                            && !self.raiders.contains(id)
                            && !self.raid_involves(*id)
                            && matches!(
                                entity.passive_acquire_mission(),
                                crate::sim::mission::MissionType::None
                                    | crate::sim::mission::MissionType::Sleep
                            )
                    });
                    if let Some((dog, entity)) = pick {
                        let from = (entity.position.rx(), entity.position.ry());
                        let type_name = sim.interner.resolve(entity.type_ref());
                        if let Some((px, py)) =
                            connected_ring_cell(sim, rules, center, from, dog, type_name)
                            && !self.futile(dog, FutilityKind::GuardPost, from, tick)
                        {
                            self.gate_idle_order(dog, tick);
                            return vec![CommandEnvelope::new(
                                self.house,
                                at,
                                Command::Guard {
                                    entity_id: dog,
                                    target: Some(crate::sim::combat::TargetKind::Cell(px, py)),
                                },
                            )];
                        }
                    }
                }
            }

            // Layer-3 futility gate folded into the Layer-2 post split: a
            // unit whose identical post order keeps landing unmoved stops
            // spending its reorder windows on it for a while. Naval hulls
            // never post — their defense is the idle perimeter's Attack,
            // not a land ring.
            let poster = self
                .due_idle_unit(&idle, tick)
                .filter(|&unit| !self.futility_suppressed(unit, FutilityKind::GuardPost, tick))
                .filter(|&unit| {
                    sim.substrate.entities.get(unit).is_none_or(|entity| {
                        !Self::is_naval_type(opening, sim.interner.resolve(entity.type_ref()))
                    })
                });
            if let Some(unit) = poster {
                // Spread the guard posts on a ring around the yard by unit
                // id: defenders hold neighboring cells instead of stacking
                // on the yard itself. Clamped onto the map for the same
                // reason as the wave destination, and zone-gated in the
                // unit's own rotation order like the wave: a post across a
                // breach is not a post, and no connected offset at all
                // spends only the unit's idle window (the order gate below
                // paces the retry) instead of marching it into the water.
                // With a second yard standing, half the idle defenders
                // (odd unit ids) anchor the expansion yard's own post set
                // instead — a sub-base with no posts leaves its guns
                // uncovered — and a unit with no connected post on its
                // assigned ring falls back to the other base's, so the
                // order gate is spent on a held post either way.
                let home_post = pick_guard_post(sim, rules, unit, center, &SPREAD_OFFSETS);
                let post = if let Some(expansion) = expansion_center {
                    let expansion_post = pick_guard_post(
                        sim,
                        rules,
                        unit,
                        expansion,
                        &SPREAD_OFFSETS[..EXPANSION_GUARD_OFFSETS],
                    );
                    if unit % 2 == 1 {
                        expansion_post.or(home_post)
                    } else {
                        home_post.or(expansion_post)
                    }
                } else {
                    home_post
                };
                // The retry paces whatever the outcome: a dropped post order
                // must wait its window, not re-pick per tick (the futility
                // strikes count windows, not ticks).
                self.gate_idle_order(unit, tick);
                if let (Some((px, py)), Some(entity)) = (post, sim.substrate.entities.get(unit))
                    && !self.futile(
                        unit,
                        FutilityKind::GuardPost,
                        (entity.position.rx(), entity.position.ry()),
                        tick,
                    )
                {
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::Guard {
                            entity_id: unit,
                            target: Some(crate::sim::combat::TargetKind::Cell(px, py)),
                        },
                    )];
                }
            }
        }

        // The navy's sortie — Layer 3. The idle and standing pools collect
        // the combat hulls for home defense, but the wave's zone gate keeps
        // every ship out of the ground marches and no branch ever ordered
        // one forward: a fleet stood at its yard for life. The sortie arms
        // like a wave episode — at least [`NAVAL_SORTIE_FLEET_MIN`] idle
        // hulls, never while the base alarm holds the army home — and
        // dispatches one hull per tick at the latched target until only the
        // home guard remains (the fleet's first idle hull; the defense
        // recall can still pull it). The target is the nearest enemy hull
        // the water rows connect to the first sailer (the ships' own
        // movement zone answers "is this a water fight" — no rules lookup),
        // else the nearest enemy structure those rows still reach (the
        // shore bombardment), else nothing arms and the fleet stays
        // anchored. A dispatched hull leaves the idle classes and drops out
        // of the episode on its own; a dropped dispatch sits in
        // `naval_sortie_launched` until the episode closes, so the branch
        // cannot spin on it.
        {
            let idle_fleet: Vec<u64> = sim
                .substrate
                .entities
                .iter_sorted()
                .filter(|(id, entity)| {
                    entity.owner() == self.house
                        && entity.lifecycle.object_alive()
                        && !entity.lifecycle.in_limbo()
                        && entity.category() == crate::map::entities::EntityCategory::Unit
                        && Self::is_naval_type(opening, sim.interner.resolve(entity.type_ref()))
                        && !self.defenders.contains(id)
                        && !self.naval_sortie_launched.contains(id)
                        && self.order_gate.get(id).is_none_or(|&due| tick >= due)
                        && matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                                | crate::sim::mission::MissionType::Guard
                                | crate::sim::mission::MissionType::AreaGuard
                        )
                })
                .map(|(id, _)| id)
                .collect();
            if !self.naval_sortie_launching
                && tick >= self.naval_sortie_due
                && !self.base_alarm_armed(sim, tick)
                && idle_fleet.len() >= NAVAL_SORTIE_FLEET_MIN
            {
                // Arm the episode: the latch reads the sortie target from
                // the first hull that will sail (the home guard is the
                // fleet's first idle hull; everything behind it sails). No
                // target — no episode; the fleet keeps the anchorage.
                if let Some(target) = self.naval_sortie_target(sim, rules, &idle_fleet[1..]) {
                    self.naval_sortie_launching = true;
                    self.naval_sortie_launched.clear();
                    self.naval_sortie_target = Some(target);
                } else {
                    self.naval_sortie_due = tick + NAVAL_SORTIE_WINDOW_TICKS;
                }
            }
            if self.naval_sortie_launching {
                // The first idle hull is the home guard; the second sails
                // at the latch — while it still lives on the substrate.
                let sail = self
                    .naval_sortie_target
                    .filter(|&target| {
                        sim.substrate.entities.get(target).is_some_and(|entity| {
                            entity.lifecycle.object_alive() && !entity.lifecycle.in_limbo()
                        })
                    })
                    .filter(|_| idle_fleet.len() > 1)
                    .map(|target| (target, idle_fleet[1]));
                if let Some((target, sailor)) = sail {
                    self.naval_sortie_launched.push(sailor);
                    self.gate_idle_order(sailor, tick);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::Attack {
                            attacker_id: sailor,
                            target_id: target,
                        },
                    )];
                }
                // The episode is over: every sailer dispatched (dropped
                // orders included), the fleet down to the home guard, or
                // the target fell under way — the hulls already at sea
                // fight their own Attacks out, production rebuilds the
                // line, and the next window re-reads the water.
                self.naval_sortie_launching = false;
                self.naval_sortie_due = tick + NAVAL_SORTIE_WINDOW_TICKS;
                self.naval_sortie_target = None;
                self.naval_sortie_launched.clear();
            }
        }

        // The air wing's sortie — Layer 3. The old flight filter read only
        // None|Sleep missions, but the airfield reload releases a rearmed
        // jet with Assign_Mission(Guard) and a computer house's armed jets
        // idle-pick AreaGuard — so each jet sortied about once and then sat
        // full-ammo forever (ORCA/BEAG are CanPassiveAquire=no: nothing
        // self-fires them). The flight now reads the same idle classes the
        // ground pools accept, orders only jets that actually hold ordnance
        // (a dry count of exactly zero waits at its pad for the reload),
        // and fans the flight across the first few enemy priority targets
        // by jet id instead of stacking every bomb on the substrate's
        // first. The wing commission's dock gate (the radar class standing)
        // already keeps replacements out of an AirportBound crash loop.
        if tick >= self.next_airstrike_tick {
            let targets: Vec<u64> = self
                .enemy_priority_targets(sim)
                .iter()
                .take(AIR_FAN_TARGETS)
                .map(|&(id, _)| id)
                .collect();
            if !targets.is_empty() {
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
                                    | crate::sim::mission::MissionType::Guard
                                    | crate::sim::mission::MissionType::AreaGuard
                            )
                            && entity.ammo().is_none_or(|ammo| ammo.current() != 0)
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
                                    target_id: targets[(jet as usize) % targets.len()],
                                },
                            )
                        })
                        .collect();
                }
            }
        }

        // The fortress's boarding call — Layer 3. The cargo is reserved (a
        // boarding candidate is off the wave pool through the `boarders`
        // list), the load mixes (anti-armor first, then anti-infantry, up
        // to the seats free), and a refused Enter gets at most
        // [`BOARD_MAX_ATTEMPTS`] tries per (unit, fortress) pair before the
        // unit returns to the wave pool — the old loop re-picked the same
        // first anti-armor type every 150 ticks forever, both against the
        // wave dispatch that kept eating its candidates and against the
        // fortress's own refusal.
        {
            // Reservation hygiene: dead or boarded boarders (passengers
            // ride in limbo) leave the set.
            self.boarders.retain(|&id| {
                sim.substrate.entities.get(id).is_some_and(|entity| {
                    entity.owner() == self.house
                        && entity.lifecycle.object_alive()
                        && !entity.lifecycle.in_limbo()
                })
            });
            self.board_attempts
                .retain(|&(unit, _), _| self.boarders.contains(&unit));
            // A boarder whose Enters were refused twice and who stands idle
            // again (unlanded) returns to the pool.
            let mut pool_back: Vec<u64> = Vec::new();
            for (&(unit, _), &attempts) in self.board_attempts.iter() {
                if attempts >= BOARD_MAX_ATTEMPTS
                    && self.boarders.contains(&unit)
                    && sim.substrate.entities.get(unit).is_some_and(|entity| {
                        matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                                | crate::sim::mission::MissionType::Guard
                                | crate::sim::mission::MissionType::AreaGuard
                        )
                    })
                {
                    pool_back.push(unit);
                }
            }
            for unit in pool_back {
                self.boarders.retain(|&kept| kept != unit);
                self.board_attempts
                    .retain(|&(kept, _), _| kept != unit);
            }
            let cargo_names = Self::boarding_cargo(opening);
            if let Some(fortress_name) = opening.fortress
                && !cargo_names.is_empty()
                && let Some((fortress_id, fortress_entity)) =
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
                // The next boarder: the cargo mix's best candidate — anti-
                // armor outranks anti-infantry, then nearest wins — among
                // the idle, ungated infantry on foot. The hero never boards,
                // and a pair past its attempt budget waits for the pool-back
                // pass above.
                let mut boarding: Option<(usize, u32, u64)> = None;
                for (id, entity) in sim.substrate.entities.iter_sorted() {
                    let type_name = sim.interner.resolve(entity.type_ref());
                    let Some(rank) = cargo_names.iter().position(|name| *name == type_name)
                    else {
                        continue;
                    };
                    if entity.owner() != self.house
                        || entity.category() != crate::map::entities::EntityCategory::Infantry
                        || entity.lifecycle.in_limbo()
                        || type_name == opening.hero
                        || self.order_gate.get(&id).is_some_and(|&due| tick < due)
                        || self
                            .board_attempts
                            .get(&(id, fortress_id))
                            .copied()
                            .unwrap_or(0)
                            >= BOARD_MAX_ATTEMPTS
                        || !matches!(
                            entity.passive_acquire_mission(),
                            crate::sim::mission::MissionType::None
                                | crate::sim::mission::MissionType::Sleep
                                | crate::sim::mission::MissionType::Guard
                                | crate::sim::mission::MissionType::AreaGuard
                        )
                    {
                        continue;
                    }
                    let dx = entity.position.rx() as i16 - fort_cell.0;
                    let dy = entity.position.ry() as i16 - fort_cell.1;
                    let distance =
                        (i32::from(dx) * i32::from(dx) + i32::from(dy) * i32::from(dy)) as u32;
                    if boarding.is_none_or(|(kept_rank, kept_distance, _)| {
                        (rank, distance) < (kept_rank, kept_distance)
                    }) {
                        boarding = Some((rank, distance, id));
                    }
                }
                if let Some((_, _, boarder)) = boarding {
                    let boarder_from = sim
                        .substrate
                        .entities
                        .get(boarder)
                        .map(|entity| (entity.position.rx(), entity.position.ry()));
                    if let Some(from) = boarder_from
                        && !self.futile(boarder, FutilityKind::Board, from, tick)
                    {
                        self.boarders.push(boarder);
                        *self
                            .board_attempts
                            .entry((boarder, fortress_id))
                            .or_insert(0) += 1;
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
            }
        }

        // The expansion MCV's walk: drive it to a buildable foundation
        // beside the second ore field and deploy it there — the ore cell
        // itself never hosts the unfolding (observed: five attempts spent
        // at the field's heart, then abandonment). The opening deploy
        // branch above stands down once any yard stands, so this branch
        // owns the later MCV alone: an idle one off its foundation cell
        // re-receives its Move (the order gate paces the walk), one
        // standing on it draws a paced
        // DeployMcv on a per-MCV attempt budget; a site whose deploys the
        // attempts cannot buy abandons the plan rather than soaking the
        // ticks below. A walking MCV is not idle and draws no order — a
        // dropped Move leaves it idle and the next gate window re-points
        // it, so nothing spins.
        if !self.expansion_abandoned
            && self.expansion_yard_issued
            && self.count_structures_of_type(sim, opening.construction_yard) < 2
            && let Some(home) = self.structure_center(sim, opening.construction_yard)
            && let Some(mcv_name) = rules
                .object(opening.construction_yard)
                .and_then(|ty| ty.undeploys_into.as_deref())
        {
            if self.expansion2_site.is_none() && tick >= self.survey_due {
                // The yard itself validates the survey: the winner's
                // neighborhood must host its foundation, so the walk that
                // follows always has ground to land on.
                self.expansion2_site = self.best_ore_field(
                    sim,
                    rules,
                    &runtime.resources.overlay_registry,
                    home,
                    self.expansion_site,
                    Some(opening.construction_yard),
                );
                if self.expansion2_site.is_none() {
                    self.survey_due = tick + SURVEY_RETRY_TICKS;
                }
            }
            let walker = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
                self.order_gate.get(id).is_none_or(|&due| tick >= due)
                    && entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Unit
                    && sim.interner.resolve(entity.type_ref()) == mcv_name
                    && !entity.lifecycle.in_limbo()
                    && matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
            });
            if let Some((mcv, entity)) = walker
                && let Some((sx, sy)) = self.expansion2_site
            {
                // The walk's aim is a legal yard foundation beside the
                // field, never the surveyed ore cell: tiberium-covered
                // ground refuses the unfolding. find_site_around returns
                // the foundation's origin; the deploy tests the origin one
                // cell up-left of the MCV, so the MCV walks to the
                // foundation's center. The aim latches on the first pass:
                // a re-scan drifts as the ore spreads, and a walking
                // target is never reached (observed: the whole walk spent
                // re-pointing between shifting foundations, not one
                // deploy drawn). A field the survey validated but whose
                // foundation has since stopped standing legal (the ore
                // spread moved, or a neighbor built) drops the pick for a
                // paced re-survey — the old fallback aimed the ore cell
                // itself, and the MCV died pacing ground that refuses the
                // unfolding (observed: one refused deploy, no second yard).
                if self.expansion_deploy_target.is_none() {
                    self.expansion_deploy_target = rules
                        .object(opening.construction_yard)
                        .and_then(|yard| {
                            find_site_beyond(
                                sim,
                                rules,
                                &runtime.resources.overlay_registry,
                                yard,
                                (sx as i16, sy as i16),
                                self.house,
                                home,
                                EXPANSION_MIN_DISTANCE,
                            )
                        })
                        .map(|(ox, oy)| (ox + 1, oy + 1))
                        // The walk aim must sit in ground the zone grid
                        // connects to home: a legal foundation across a
                        // breach is a death walk the sim nulls at the water
                        // (the same class the wave gate holds back). A
                        // filtered-out aim drops into the re-survey below.
                        .filter(|&(ax, ay)| {
                            zones_connected(
                                sim,
                                crate::rules::locomotor_type::MovementZone::Normal,
                                (home.0.max(0) as u16, home.1.max(0) as u16),
                                (ax, ay),
                            )
                        });
                }
                let Some((tx, tyy)) = self.expansion_deploy_target else {
                    self.expansion2_site = None;
                    self.survey_due = tick + SURVEY_RETRY_TICKS;
                    return Vec::new();
                };
                let dx = tx as i16 - entity.position.rx() as i16;
                let dy = tyy as i16 - entity.position.ry() as i16;
                // On the cell or beside it: exact arrival deploys at the
                // validated center, and a one-cell miss hands the finish
                // to try_to_deploy's own fallback drive — it re-points the
                // MCV at the first site whose foundation stands legal. A
                // wider loiter spent every attempt on ore (observed).
                if dx.abs().max(dy.abs()) <= 1 {
                    if tick >= self.next_expansion_deploy_tick {
                        let attempts = self.expansion_deploy_attempts.entry(mcv).or_insert(0);
                        if *attempts < EXPANSION_DEPLOY_ATTEMPTS {
                            self.next_expansion_deploy_tick = tick + DEPLOY_RETRY_TICKS;
                            *attempts += 1;
                            if *attempts >= EXPANSION_DEPLOY_ATTEMPTS {
                                self.expansion_abandoned = true;
                            }
                            return vec![CommandEnvelope::new(
                                self.house,
                                at,
                                Command::DeployMcv { entity_id: mcv },
                            )];
                        }
                        self.expansion_abandoned = true;
                    }
                } else {
                    self.gate_idle_order(mcv, tick);
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::Move {
                            entity_id: mcv,
                            target_rx: tx,
                            target_ry: tyy,
                        },
                    )];
                }
            }
        }

        // The scouting chain's dispatch — Layer 3. The march at the
        // Euclidean-nearest dark cell used to stall forever: a cell the
        // ground rows cannot connect to (a cliff pocket, the far shore of a
        // breach) re-ordered the same dead Move every idle window. The pick
        // now zone-gates the nearest handful of candidates in the scout's
        // rotation, the futility backoff strikes out a cell that was paced
        // at repeatedly without ever revealing (it joins the blacklist and
        // the next-nearest takes the walk), and a side whose scout is its
        // core soldier (Yuri's Initiate) keeps at most two scouts on the
        // march so the home defense keeps its line.
        let scout_is_core = opening.scout == opening.basic_infantry;
        let scouts_out = if scout_is_core {
            sim.substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == self.house
                        && entity.category() == crate::map::entities::EntityCategory::Infantry
                        && sim.interner.resolve(entity.type_ref()) == opening.scout
                        && !entity.lifecycle.in_limbo()
                        && entity.passive_acquire_mission()
                            == crate::sim::mission::MissionType::Move
                })
                .count()
        } else {
            0
        };
        if scouts_out < SCOUT_MAX_ACTIVE {
            let mut idle_scouts = sim.substrate.entities.iter_sorted().filter(|(id, entity)| {
                self.order_gate.get(id).is_none_or(|&due| tick >= due)
                    && entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Infantry
                    && sim.interner.resolve(entity.type_ref()) == opening.scout
                    && !entity.lifecycle.in_limbo()
                    && matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
            });
            if let Some((scout, entity)) = idle_scouts.next() {
                let from = (entity.position.rx() as i16, entity.position.ry() as i16);
                if let Some(cell) =
                    self.nearest_unrevealed_cell(sim, from, &self.scout_blacklist)
                {
                    // The futility aim is the dark cell itself: one scout
                    // paced at the same unreachable cell three windows
                    // retires the cell from the rotation, and the next
                    // idle window walks the next-nearest candidate.
                    if self.futile(scout, FutilityKind::Scout, cell, tick) {
                        self.scout_blacklist.insert(cell);
                    } else {
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
                }
            }
        }

        // The spy's infiltration run — Layer 3. The one commissioned
        // infiltrator walks at the nearest revealed enemy structure the
        // infiltration table pays on — the enemy's superweapon (the charge
        // reset) first, then its radar, its power plants, any standing
        // structure ([`spy_infiltration_rank`], nearest within a class) —
        // and only where no enemy aim stands revealed does the old neutral
        // tech tour run (the stock YR list's buildings the arrival mostly
        // ignores). With nothing revealed to run at, the spy stays home
        // like any other garrison piece. The futility backoff bounds the
        // re-issue loop the same way the scout's march is bounded.
        if let Some((spy, spy_entity)) = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
            self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Infantry
                && sim.interner.resolve(entity.type_ref()) == SPY_TYPE
                && !entity.lifecycle.in_limbo()
                && !self.boarders.contains(id)
                && !self.raiders.contains(id)
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        }) {
            let from = (spy_entity.position.rx() as i16, spy_entity.position.ry() as i16);
            let target = self
                .nearest_enemy_infiltration_target(sim, from)
                .or_else(|| self.nearest_revealed_tech(sim, from));
            if let Some(target) = target
                && !self.futile(
                    spy,
                    FutilityKind::Spy,
                    (from.0.max(0) as u16, from.1.max(0) as u16),
                    tick,
                )
            {
                self.gate_idle_order(spy, tick);
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::CaptureBuilding {
                        engineer_id: spy,
                        target_building_id: target,
                    },
                )];
            }
        }

        // The ferry — Layer 3. The side's transport hull takes idle
        // infantry aboard with the engine's own Enter click (the fortress
        // boarding's mechanism — a transport is one of its Enter targets),
        // rides its movement row toward the enemy's mass center and
        // unloads with the Unload mission. The hull is whichever of the
        // table's two ferries the base fielded: the hover transport
        // beaches beside its naval yard on a shoreline map, the Night Hawk
        // (`MovementZone=Fly`, so the whole map is one connected zone)
        // works the inland Allies' line where no yard ever stood. Both
        // legs speak commands a human client also sends (`MegaMission`
        // Enter and [`Command::UnloadPassengers`]), so the branch is a
        // pacing problem: one leg per [`FERRY_WINDOW_TICKS`] window, below
        // every combat, scouting and raiding limb.
        if tick >= self.ferry_due
            && (opening.amphibious_transport.is_some() || opening.air_ferry.is_some())
        {
            self.ferry_due = tick + FERRY_WINDOW_TICKS;
            if let Some(command) = self.ferry_orders(sim, rules, opening, home_center, tick) {
                return vec![CommandEnvelope::new(self.house, at, command)];
            }
        }

        // The engineer's neutral-tech capture — Layer 3. A revealed tech
        // building still held by a neutral house is free money (the oil
        // derrick's startup cash first), and an engineer the side can spare
        // walks in with the engine's own capture click; the window paces
        // the scan, the dispatch and the capped commission alike. Below the
        // bridge limb on purpose: that chain keeps its own engineer and its
        // own dispatch, this one spends the surplus.
        if let Some(command) =
            self.engineer_capture_orders(sim, rules, opening, home_center, tick)
        {
            return vec![CommandEnvelope::new(self.house, at, command)];
        }

        // The engineer-ride raid — Layer 3. An engineer in the side's land
        // transport rides the enemy priority structures and walks in where
        // he lands: the pair commissions behind the raid's own wallet bar
        // and cooldown, and the armed stage machine spends one command per
        // tick. Yuri's side fields no land transport and the branch sleeps.
        if let Some(command) = self.engineer_raid_orders(sim, rules, opening, tick) {
            return vec![CommandEnvelope::new(self.house, at, command)];
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
                    || name == opening.second_super_weapon_building
            }
        });
        // Any battle tank standing gates the perimeter guns and the
        // expansion rungs past the opening. The tier-2 tank counts once
        // the lab stands, so the death of the first tanks never freezes
        // the base at its opening size.
        let any_tank = self.count_units_of_type(sim, opening.main_tank)
            + self.count_units_of_type(sim, opening.tier2_tank)
            > 0;
        // A gun still below its quota while the yard stands and tanks roll.
        // The limb's own ordering, hoisted once: the savings hold and the
        // limb itself must claim the same gun or the hold banks the wallet
        // for a queue that never fires. Anti-air answers air that actually
        // fields, the heavy tower waits behind the radar its prerequisite
        // names, and the pillbox quota holds the perimeter once tanks roll.
        // Per-base quotas (Layer 2): the caps split across the standing
        // yards, each base carrying its own guns — the expansion yard used
        // to stand bare while the home ring held every one. A gun's base
        // reads as the yard whose [`BASE_CLUSTER_RADIUS`] ring it stands
        // in; with no yard at all the legacy single-base behavior holds
        // (the structure center anchors a virtual base, as the old
        // house-wide caps effectively did). The heavy quota gains one tier
        // per yard only under the late-game sink: a rich, capped base
        // converts wealth to defense depth, and to nothing else.
        let bases: Vec<(i16, i16)> = if yard_centers.is_empty() {
            self.structure_center(sim, opening.construction_yard)
                .into_iter()
                .collect()
        } else {
            yard_centers.clone()
        };
        let late_sink = crate::sim::production::credits_for_owner(sim, &owner) > LATE_SINK_CREDITS
            && self.count_structures_of_type(sim, opening.war_factory) >= FACTORY_BUILDOUT
            && self.count_structures_of_type(sim, opening.refinery) >= REFINERY_CAP
            && bases.len() >= 2;
        // The Grand Cannon's country gate (the entry below): GTGCAN is
        // `RequiredHouses=French`, so every other Allied country (America,
        // Britain, Germany, Korea) must be refused before the queue step
        // ever fires. The engine's own CanBuild answers No forever for a
        // house the `RequiredHouses` lock refuses, and a type the house can
        // never own must not keep reaching for the one building slot — the
        // engineer-commission lesson. The side table already keeps the
        // Soviet and Yuri entries `None`.
        let french_house = sim
            .houses
            .get(&self.house)
            .is_some_and(|house| sim.interner.resolve(house.house_type_id()) == "French");
        let heavy_per_yard = DEFENSE_PER_YARD_HEAVY + usize::from(late_sink);
        let defenses = [
            (
                opening.defense_aa,
                DEFENSE_PER_YARD_AA,
                AA_DEFENSE_CAP * 2,
                self.enemy_air_count(sim) > 0,
            ),
            (
                opening.defense_heavy,
                heavy_per_yard,
                HEAVY_DEFENSE_CAP * 2 + usize::from(late_sink) * bases.len(),
                self.count_structures_of_type(sim, radar_name) >= 1,
            ),
            (
                opening.defense_basic,
                DEFENSE_PER_YARD_BASIC,
                BASIC_DEFENSE_TARGET * 2,
                true,
            ),
            // The defense extension (Layer 2.9): the gap generator and the
            // Grand Cannon ride the same gun queue step as the three core
            // lines, so the blackout gate above covers them for free — both
            // are retail `Powered=yes` with `Power=-100` and only queue
            // while the grid reads `output >= drain`, with the power
            // preemption standing a plant first when the pending drain
            // would run the grid red. Their entries sit after the core
            // lines, so a base arms itself before it decorates. The sides
            // without a piece hold the basic pillbox's name below — never
            // read, because the gate's `&&` chain short-circuits on the
            // `None` field and the gun filter reads the count only behind a
            // true gate.
            (
                opening.gap_generator.unwrap_or(opening.defense_basic),
                1,
                2,
                opening.gap_generator.is_some()
                    && self.count_structures_of_type(sim, opening.battle_lab) >= 1,
            ),
            (
                opening.country_cannon.unwrap_or(opening.defense_basic),
                1,
                2,
                opening.country_cannon.is_some()
                    && french_house
                    && self.count_structures_of_type(sim, radar_name) >= 1,
            ),
        ];
        // The first gated type with a base under its quota claims the next
        // gun, that base's rings the placement intent's target; an empty
        // list — every base at every quota — rests the limb.
        let gun_wants: Vec<(&'static str, bool)> = defenses
            .iter()
            .filter_map(|&(name, per_yard, global_bound, gate)| {
                if !gate || self.count_structures_of_type(sim, name) >= global_bound {
                    return None;
                }
                bases
                    .iter()
                    .position(|&cell| {
                        self.count_structures_of_type_near(sim, name, cell, BASE_CLUSTER_RADIUS)
                            < per_yard
                    })
                    .map(|index| (name, index >= 1))
            })
            .collect();
        let guns_wanted = any_tank
            && self
                .structure_center(sim, opening.construction_yard)
                .is_some()
            && !gun_wants.is_empty();
        // The floor's savings hold: the limb's full-cost gate can wait
        // forever in a war-poor game — each unit item banks only its own
        // cost, so the hundred-credit soldier ladder keeps the wallet
        // pinned under the gun's price between ore dumps (observed: whole
        // games, zero gun orders). While fewer than [`DEFENSE_FLOOR`] guns
        // stand the unit queues hold for the next gun the limb itself
        // would claim, so the flow pools past its full cost on the idle
        // ticks between building items. The economy's miners and the
        // one-shot commissions are exempt — income first, existence
        // second, the floor third.
        let gun_hold = guns_wanted
            && self.count_structures_of_type(sim, opening.defense_basic)
                + self.count_structures_of_type(sim, opening.defense_aa)
                + self.count_structures_of_type(sim, opening.defense_heavy)
                < DEFENSE_FLOOR
            && gun_wants
                .iter()
                .find_map(|&(name, _)| {
                    rules.object(name).and_then(|ty| {
                        crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                            .then_some(ty.cost)
                    })
                })
                .is_some_and(|cost| {
                    crate::sim::production::credits_for_owner(sim, &owner) < cost
                });
        if self
            .structure_center(sim, opening.construction_yard)
            .is_some()
            && !building_busy
        {
            // The power preemption (Layer 2.5). The grid's margin prices
            // the queue's unlanded drain in, and a grid that would run red
            // owes a power plant before anything else in this block: the
            // guns and the ladder below used to claim the empty slot
            // first, so a defended base chained one Powered gun after
            // another into a deepening brownout — guns that stand offline
            // in low power defend nothing — and by the time the growth
            // tail's deficit rule finally ran, the blackout had already
            // slowed every factory through the native power factor
            // (observed live: blackouts mid-game with plants still owed
            // behind the gun ring). The wallet gate keeps an unpayable
            // plant from parking in the one building slot forever.
            let (output, drain) =
                crate::sim::production::power_balance_for_owner(sim, rules, &owner);
            let pending = self.pending_structure_drain(sim, rules, &owner);
            if output - drain - pending < 0
                && self.count_structures_of_type(sim, opening.power) < POWER_CAP
                && let Some(ty) = rules.object(opening.power)
                && crate::sim::production::credits_for_owner(sim, &owner) >= ty.cost
                && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
                && let Some(type_id) = sim.interner.get(opening.power)
            {
                self.placement_intent = None;
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::QueueProduction { type_id },
                )];
            }
            // A grid already in the red adds no Powered gun: the coil and
            // the AA gun stand offline below full power, so their credits
            // buy nothing until a plant lands. The pillbox (`Power=0`)
            // waits with them for simplicity — one plant restores the lot.
            let grid_black = output >= drain;
            // The defense limb runs before the ladder walk. Behind it the
            // guns are dead code: the walk doubles as the rebuild scan, so
            // a destroyed late rung re-arms every slot-free tick and the
            // guns never once ran (observed: a whole game at zero defenses
            // behind a superweapon rebuild). Placement rides the same
            // yard-centered ring scan every other building uses; Yuri's
            // Gatling Cannon serves both the AA and the pillbox slots.
            // The wall limb reads this after the gun loop: an empty list
            // means every base's core gun lines sit at their quotas (or
            // their gates are shut).
            let guns_settled = gun_wants.is_empty();
            if any_tank && grid_black {
                for (name, expansion) in gun_wants {
                    // Guns queue only from a wallet that can pay for them
                    // outright: a broke base that queues a 1500-credit
                    // Prism Tower parks it in the one building slot at
                    // zero progress forever (observed: factories stuck at
                    // three while ATESLA held the queue for the rest of
                    // the game), starving the growth tail behind it. Poor
                    // bases expand; rich bases arm. The placement intent
                    // marks the under-quota base, so the gun lands on the
                    // base that asked for it instead of the home rings.
                    if let Some(ty) = rules.object(name)
                        && crate::sim::production::credits_for_owner(sim, &owner) >= ty.cost
                        && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                        && let Some(type_id) = sim.interner.get(name)
                    {
                        self.placement_intent = Some((type_id, expansion));
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
                // The second superweapon step mirrors the first — the same
                // toggle owns its cameo, and the walk's guard opens it only
                // once the first super has stood (its own once-per-game
                // latch below), so the two monuments never race for the one
                // building slot.
                (opening.second_super_weapon_building, 1),
                // The service depot: every side's MCV is retail-gated
                // behind it (`Prerequisite=` on AMCV/SMCV/PCV), so it is a
                // ladder rung of its own — built with the late base and
                // rebuilt when razed — rather than an expansion-armed
                // growth entry (the growth tail's reachability under war
                // made that starve: observed a whole game with the
                // expansion armed and the depot never queued). YADEPT sits
                // at TechLevel 15, past any lobby, so Yuri's rung is the
                // Grinder alternate.
                (opening.service_depot, 1),
                (opening.power, POWER_TARGET),
                (opening.war_factory, FACTORY_BUILDOUT),
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
                // The second super's rung mirrors the first's once-per-game
                // latch and adds the sequencing gate: it queues only once
                // the first super has stood — the latch or the building
                // still on the field — so a razed second super never
                // rebuilds either.
                if *name == opening.second_super_weapon_building
                    && (self.sw2_ever_stood
                        || !(self.sw_ever_stood
                            || self.count_structures_of_type(sim, opening.super_weapon_building)
                                >= 1))
                {
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
                    // The fixed ladder places at home — clear any expansion
                    // intent a previous plan item left behind.
                    self.placement_intent = None;
                    return vec![CommandEnvelope::new(
                        self.house,
                        at,
                        Command::QueueProduction { type_id },
                    )];
                }
            }

            // The wall ring (Layer 2.8). A perimeter of the side's wall
            // segments goes up one production cycle at a time around the
            // first superweapon, then the second, then the construction
            // yard — the pieces an early raider values most. The gates keep
            // the ring out of the way: the core gun lines are at their
            // quotas (guns_settled), the wallet clears
            // [`WALL_CREDIT_FLOOR`], the building queues are idle (the
            // block this sits in), no wall is queued or ready, and some
            // target still has an untried ring cell. The walk
            // ([`Self::wall_next_cell`]) also retires exhausted targets
            // into `walled` as it goes, so a ring the auto-fill or a refusal
            // closed can never queue another segment for. Yuri walls through
            // the same limb: its Citadel Wall is a retail `Wall=yes`
            // Defense-queue building like the others.
            if guns_settled
                && let Some(wall_name) = opening.wall
                && let Some(wall_ty) = rules.object(wall_name)
                && crate::sim::production::cameo_draw_state(sim, rules, self.house, wall_ty)
                    .enabled
                && crate::sim::production::credits_for_owner(sim, &owner) >= WALL_CREDIT_FLOOR
                && let Some(wall_type_id) = sim.interner.get(wall_name)
                && !queue.iter().any(|item| item.type_id == wall_type_id)
                && !ready_stock.iter().any(|item| item.type_id == wall_type_id)
                && self
                    .wall_next_cell(
                        sim,
                        rules,
                        opening,
                        &runtime.resources.overlay_registry,
                        false,
                    )
                    .is_some()
            {
                self.placement_intent = None;
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::QueueProduction {
                        type_id: wall_type_id,
                    },
                )];
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
                // The grid readings are the block's hoisted ones — the
                // preemption's `output`, `drain` and `pending` above — so
                // the deficit and margin rules below price the queue's
                // unlanded drain in too.
                let refineries = self.count_structures_of_type(sim, opening.refinery);
                let factories = self.count_structures_of_type(sim, opening.war_factory);
                let miners = opening
                    .harvester
                    .map(|miner| self.count_units_of_type(sim, miner))
                    .unwrap_or(usize::MAX);
                // The expansion growth (Layer 2): a second yard is a
                // sub-base, not a monument. Before the house-wide growth
                // array, the cluster around the expansion yard claims what
                // it lacks — one power plant while its ring browns, one
                // refinery while its ring holds no dock and the house-wide
                // economy is one past its own cap — each routed to the
                // expansion yard's rings by the placement intent. A ring
                // scan that finds no legal site falls back home in the
                // placement branch, and the strike/pause machinery paces
                // the retry, so a refusing neighborhood can wedge neither
                // the queue nor the chain.
                if let Some(expansion) = expansion_center {
                    let cluster_power =
                        self.count_structures_of_type_near(sim, opening.power, expansion, BASE_CLUSTER_RADIUS);
                    let cluster_refineries = self.count_structures_of_type_near(
                        sim,
                        opening.refinery,
                        expansion,
                        BASE_CLUSTER_RADIUS,
                    );
                    if cluster_power < 1
                        && self.count_structures_of_type(sim, opening.power) < POWER_CAP
                        && let Some(ty) = rules.object(opening.power)
                        && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                        && let Some(type_id) = sim.interner.get(opening.power)
                    {
                        self.placement_intent = Some((type_id, true));
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::QueueProduction { type_id },
                        )];
                    }
                    if cluster_refineries < 1
                        && refineries < REFINERY_CAP + 1
                        && let Some(ty) = rules.object(opening.refinery)
                        && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                        && let Some(type_id) = sim.interner.get(opening.refinery)
                    {
                        self.placement_intent = Some((type_id, true));
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::QueueProduction { type_id },
                        )];
                    }
                    // The expansion's own production lines (Layer 2.6): a
                    // sub-base fields its own army and miners. The house's
                    // queues are shared, but a unit exits the factory that
                    // builds it — without a barracks and a war factory in
                    // the expansion's ring, every soldier and every miner
                    // it is owed rolls out of the home base and walks the
                    // whole map first (observed live: the expansion's
                    // miners crossing two bridges to reach its dock). The
                    // extra lines also pay the native MultipleFactory
                    // speedup house-wide. Each is one per cluster, the
                    // factory still inside the house-wide cap.
                    let cluster_barracks = self.count_structures_of_type_near(
                        sim,
                        opening.barracks,
                        expansion,
                        BASE_CLUSTER_RADIUS,
                    );
                    let cluster_factories = self.count_structures_of_type_near(
                        sim,
                        opening.war_factory,
                        expansion,
                        BASE_CLUSTER_RADIUS,
                    );
                    // The house-wide bound is the loop breaker: a cluster
                    // item whose expansion ring refuses every site falls
                    // back home at placement, the cluster count near the
                    // expansion stays at zero, and without a cap the rule
                    // re-queues the same building forever (observed live:
                    // a Yuri base that did nothing but chain barracks).
                    // Two stands: the ladder's home barracks plus one more
                    // wherever this rule's item landed; a razed one rebuilds.
                    if cluster_barracks < 1
                        && self.count_structures_of_type(sim, opening.barracks) < 2
                        && let Some(ty) = rules.object(opening.barracks)
                        && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                        && let Some(type_id) = sim.interner.get(opening.barracks)
                    {
                        self.placement_intent = Some((type_id, true));
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::QueueProduction { type_id },
                        )];
                    }
                    if cluster_factories < 1
                        && factories < FACTORY_CAP
                        && let Some(ty) = rules.object(opening.war_factory)
                        && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                            .enabled
                        && let Some(type_id) = sim.interner.get(opening.war_factory)
                    {
                        self.placement_intent = Some((type_id, true));
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::QueueProduction { type_id },
                        )];
                    }
                }
                // The naval survey (Layer 2.7): the naval yard founds on a
                // WaterSet tile inside the base's own build area, so before
                // its rung may queue one the survey must latch a legal
                // shoreline cell — the same admission the placement itself
                // runs, found by scanning rings around every standing yard.
                // A ready yard with no site to land at would wedge the one
                // placement slot for the rest of the game, the ATESLA
                // lesson again. The scan probes a placement per cell, so it
                // is paced and budgeted: a landlocked base (no water in any
                // yard's reach) abandons the navy after
                // [`NAVAL_PROBE_MAX`] surveys instead of rescanning every
                // growth slot.
                if !self.naval_abandoned
                    && self.naval_site.is_none()
                    && self.count_structures_of_type(sim, opening.naval_yard) == 0
                    && tick >= self.naval_probe_due
                {
                    self.naval_probe_due = tick + NAVAL_PROBE_TICKS;
                    self.naval_probes += 1;
                    if self.naval_probes >= NAVAL_PROBE_MAX {
                        self.naval_abandoned = true;
                    }
                    if let Some(yard_ty) = rules.object(opening.naval_yard)
                        && let Some(site) =
                            self.own_yard_centers(sim, opening).iter().find_map(|&yard| {
                                find_site_around_capped(
                                    sim,
                                    rules,
                                    &runtime.resources.overlay_registry,
                                    yard_ty,
                                    yard,
                                    self.house,
                                    Some(owner.as_str()),
                                    NAVAL_SITE_RADIUS,
                                )
                            })
                    {
                        self.naval_site = Some(site);
                        self.naval_probes = 0;
                        self.naval_abandoned = false;
                    }
                }
                let growth = [
                    (
                        // The far-entry cap (Layer 2 G's companion): the
                        // old entry queued one refinery after another
                        // until the remote latch armed, and the routing
                        // now lands them beside the expansion yard — a
                        // borderline-distance yard could keep the latch
                        // quiet while the docks multiplied. The expansion
                        // economy's own cap bounds it instead.
                        self.expansion_site.is_some()
                            && !self.remote_refinery_stood
                            && refineries < REFINERY_CAP + 1,
                        opening.refinery,
                    ),
                    (
                        // The MCV's prerequisite depot: retail gates every
                        // side's MCV behind its service depot (AMCV on
                        // GADEPT, SMCV on NADEPT, PCV on the Grinder —
                        // YADEPT sits at TechLevel 15, past any lobby), so
                        // the armed expansion builds exactly one while the
                        // second yard is still owed; once it stands the
                        // count check keeps the limb quiet.
                        !self.expansion_abandoned
                            && self.expansion_site.is_some()
                            && self.count_structures_of_type(sim, opening.construction_yard) < 2
                            && self.count_structures_of_type(sim, opening.service_depot) == 0,
                        opening.service_depot,
                    ),
                    (
                        output - drain - pending < 0
                            && self.count_structures_of_type(sim, opening.power) < POWER_CAP,
                        opening.power,
                    ),
                    (
                        // The power margin (Layer 2): one more factory
                        // plus refinery must not brown out the grid it
                        // joins — a plant queues while the headroom sits
                        // under [`POWER_HEADROOM_TARGET`], bounded by the
                        // standard build-out so a war-damaged late game
                        // keeps the deficit rule alone in charge.
                        output - drain - pending >= 0
                            && output - drain - pending < POWER_HEADROOM_TARGET
                            && factories < FACTORY_BUILDOUT
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
                    (
                        // The naval yard (Layer 2.7): the lowest growth
                        // priority — the land base feeds and arms itself
                        // first, then the shoreline the survey latched
                        // gets its shipyard. Razed, the count re-arms the
                        // rung and the survey's cell places the rebuild.
                        !self.naval_abandoned
                            && self.naval_site.is_some()
                            && self.count_structures_of_type(sim, opening.naval_yard) == 0,
                        opening.naval_yard,
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
                        // House-wide growth places at home — the intent's
                        // expansion marks belong to the block above.
                        self.placement_intent = None;
                        return vec![CommandEnvelope::new(
                            self.house,
                            at,
                            Command::QueueProduction { type_id },
                        )];
                    }
                }
                // The roster's tech-center rung (Layer 2): a roster entry
                // whose `needs` gate names a structure the fixed ladder
                // never builds — the Robot Control Center behind the Robot
                // Tank, the only such entry in the retail tables — is a
                // commission the mix can never fire while the gate stands
                // unsatisfied (observed: a Robot Tank cameo lit since the
                // lab and never once produced, the control center absent
                // from every build list). The rung queues the center once
                // the battle lab stands, gated on roster membership so a
                // side whose tables name no such entry never queues one,
                // and the count check keeps it a rebuild too.
                let tech_center = opening
                    .vehicles
                    .iter()
                    .chain(opening.infantry.iter())
                    .filter_map(|entry| entry.needs)
                    .find(|&name| name != opening.battle_lab);
                if let Some(center) = tech_center
                    && self.count_structures_of_type(sim, center) == 0
                    && self.count_structures_of_type(sim, opening.battle_lab) >= 1
                    && let Some(ty) = rules.object(center)
                    && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty)
                        .enabled
                    && let Some(type_id) = sim.interner.get(center)
                {
                    self.placement_intent = None;
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

        // The wing's own commission, first jet and top-ups alike. The air
        // branch used to sit last in the one-command budget — it only fired
        // while both ground queues held an item — and there its
        // 1200-credit banked gate lost every wallet race to the cheaper
        // ground items that re-arm the moment they complete: a whole
        // 42000-tick game passed with zero jets while the wing's own
        // factory (the Airforce Command, the same building the ladder's
        // radar step raised) stood ready (observed). The first jet queues
        // outright — the same existence commission the hero gets — keeping
        // only the tech-climb bank so the lab climb keeps its income. The
        // top-ups ride the same branch at their own pace, but the base
        // outranks the wing: while the second yard is still owed (fewer
        // than two stand and the plan is not dead) the top-up drain —
        // jets at 1200 apiece — eats every wallet peak the remote
        // refinery and the MCV need, and a full wing stood over a second
        // base that never came (observed: credits=206, four jets, one
        // yard). The defense floor's savings hold still outranks a luxury
        // wing the same way. The wing rides its own aircraft queue, so it
        // never competes with the ground queues for factory slots.
        let aircraft_busy = queue.iter().any(|item| {
            item.queue_category == crate::sim::production::ProductionCategory::Aircraft
        });
        let wing: usize = opening
            .aircraft
            .iter()
            .map(|&jet| self.count_aircraft_of_type(sim, jet))
            .sum();
        let expansion_owed = !self.expansion_abandoned
            && self.count_structures_of_type(sim, opening.construction_yard) < 2;
        if !aircraft_busy
            && wing < AIR_WING_TARGET
            && (wing == 0 || (!gun_hold && !expansion_owed))
            && self.owns_structure_of_type(sim, radar_name)
            && let Some(name) = opening.aircraft.iter().copied().find(|&name| {
                // `can_build`, not the cameo: the draw brightness knows
                // nothing of country locks, so the Korea-first table would
                // commission a Black Eagle an Americans house can never
                // produce (the same trap the ground mix guards against).
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
        // The one-off existence commissions — AA insurance, fortress,
        // expansion MCV — claim their turn before the miner line: a
        // commission queued behind `economy_first` starves exactly when it
        // matters (war-poor late game, every vehicle turn spent on a
        // replacement miner), the same death the fortress's specialist-
        // slot reservation died of. Each fires once per game, so the miner
        // line loses one turn to each, never more.
        let vehicle_want = if self.vehicle_slots >= CORE_BEFORE_SPECIALISTS as u32
            && !self.aa_issued
            && aa_name.is_some_and(|name| {
                rules.object(name).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            }) {
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
        } else if !self.transport_issued
            && let Some(ferry_name) = opening
                .amphibious_transport
                .filter(|_| self.owns_structure_of_type(sim, opening.naval_yard))
                .or_else(|| {
                    opening
                        .air_ferry
                        .filter(|_| self.owns_structure_of_type(sim, opening.war_factory))
                })
            && rules.object(ferry_name).is_some_and(|ty| {
                crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                    == crate::sim::production::CanBuild::Yes
            })
        {
            // The ferry's one-off commission, the fortress's treatment: a
            // reserved slot starves under war, a dedicated turn does not.
            // Two hulls share the one latch — one ferry line, never two.
            // The hover hull is a naval-yard build (retail
            // Prerequisite=GAYARD/NAYARD/YAYARD), so the commission waits
            // for the shoreline like the naval line behind it; where no
            // yard ever stands the side's air hull takes the line — the
            // Night Hawk is a war-factory build (retail
            // Prerequisite=GAWEAP, filed under `[VehicleTypes]`, so the
            // vehicle queue produces it too) and its `MovementZone=Fly`
            // rows connect the whole map, which is the inland Allies' only
            // ferry. `can_build`, not the cameo — the country-lock lesson
            // again.
            Some(ferry_name)
        } else if !self.expansion_abandoned
            && self.expansion_site.is_some()
            && !self.expansion_yard_issued
            && self.count_structures_of_type(sim, opening.construction_yard) == 1
            && let Some(mcv) = rules
                .object(opening.construction_yard)
                .and_then(|ty| ty.undeploys_into.as_deref())
            && rules.object(mcv).is_some_and(|ty| {
                crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                    == crate::sim::production::CanBuild::Yes
            })
        {
            // The second yard: the same existence-commission treatment as
            // the fortress — a reserved slot starves under war, a dedicated
            // turn does not. The MCV type derives from the yard's own
            // `UndeploysInto` (AMCV/SMCV/PCV), so no roster entry is
            // needed, and the escort branch walks it out before it
            // deploys. `can_build`, not the cameo, per the country-lock
            // lesson.
            Some(mcv)
        } else if economy_first {
            opening.harvester
        } else if self.owns_structure_of_type(sim, opening.naval_yard)
            && let Some(name) = self.naval_pick(sim, rules, opening)
        {
            // The naval line (Layer 2.7): a standing shipyard turns the
            // vehicle queue's turn toward the sea — the basic hull line
            // first, the AA escort once its prerequisite honestly passes —
            // before the tank core resumes. The quota rests the limb on
            // its own; the banked-credits gate below prices the hulls.
            Some(name)
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
            // The floor's savings hold stops the skimming outright; the
            // banked-credits gate below it still orders the affordable.
            && !(gun_hold && !economy_first)
            && !((tech_climb_busy || guns_wanted)
                && !economy_first
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            // The one-shot commissions mark on the order itself, whatever
            // window it fired in: the marks used to live behind the
            // `!economy_first` arm, so a commission ordered while the
            // miner line ran re-armed every turn and ordered again
            // (observed: three expansion MCVs, none ever escorted).
            if Some(name) == aa_name {
                self.aa_issued = true;
            }
            if opening.fortress == Some(name) {
                self.fortress_issued = true;
            }
            if opening.amphibious_transport == Some(name) || opening.air_ferry == Some(name) {
                self.transport_issued = true;
            }
            if !self.expansion_yard_issued
                && self.expansion_site.is_some()
                && rules
                    .object(opening.construction_yard)
                    .and_then(|ty| ty.undeploys_into.as_deref())
                    == Some(name)
            {
                self.expansion_yard_issued = true;
            }
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
            } else if !self.spy_issued
                && self.count_structures_of_type(sim, opening.battle_lab) >= 1
                && rules.object(SPY_TYPE).is_some_and(|ty| {
                    crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                        == crate::sim::production::CanBuild::Yes
                })
            {
                // Layer 3: the spy — a quota of one, behind the battle lab,
                // `can_build`-gated so a country that refuses the infiltrator
                // simply never spends the slot (the same country-lock lesson
                // as the jet table).
                Some(SPY_TYPE)
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
            // income flow — and the floor's savings hold stops the
            // soldier's cheap ladder from skimming the pool the gun is
            // banking.
            && !gun_hold
            && !((tech_climb_busy || guns_wanted)
                && crate::sim::production::credits_for_owner(sim, &owner) < ty.cost)
            && let Some(type_id) = sim.interner.get(name)
        {
            if name == opening.hero {
                self.hero_issued = true;
            }
            if name == SPY_TYPE {
                self.spy_issued = true;
            }
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
    /// construction yards, if the house owns one — and no yard of its own
    /// stands yet: this branch starts the base, and once a yard stands a
    /// later MCV (the expansion commission's) belongs to the expansion
    /// branch's walk-and-deploy, not to an in-place deploy beside the
    /// existing yard.
    fn deployable_mcv(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
    ) -> Option<u64> {
        let yard_stands = sim.substrate.entities.iter_sorted().any(|(_, entity)| {
            entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Structure
                && OPENINGS
                    .iter()
                    .any(|o| o.construction_yard == sim.interner.resolve(entity.type_ref()))
        });
        if yard_stands {
            return None;
        }
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

    /// The ore field worth the expansion, scored as value per distance
    /// walked: a field must be proportionally richer to drag the MCV
    /// further. The raw richest-cell scan had no distance term at all —
    /// on a big community map every house marched its MCV at the same
    /// central mega-field across hostile ground (observed live: "the AI
    /// charges with its base"). Cells at least
    /// [`EXPANSION_MIN_DISTANCE`] from `from` — and, when `exclude`
    /// holds, at least that far from it too, so the second survey cannot
    /// re-pick the first expansion's field. One pass over the overlay
    /// grid collects the candidates; the earliest cell wins score ties;
    /// the survey runs once per expansion, paced by the caller.
    ///
    /// With `validate` naming a structure the field's neighborhood must
    /// host, the winner is the best-scoring field whose surroundings
    /// yield a legal foundation for it — the placement predicate the
    /// deploy and placement chains themselves use, so the survey never
    /// picks a field the follow-up orders cannot build on. The first
    /// distance-weighted pick pointed at a field with no room beside it:
    /// the walk aim fell back to the ore cell itself, whose ground
    /// refuses the unfolding, and the second yard never stood (observed:
    /// `fair_growth` stalled at one yard, one refused deploy, the MCV
    /// pacing ore it could never build on).
    fn best_ore_field(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        overlay_registry: &crate::rules::overlay_types::OverlayTypeRegistry,
        from: (i16, i16),
        exclude: Option<(u16, u16)>,
        validate: Option<&'static str>,
    ) -> Option<(u16, u16)> {
        let grid = sim.overlay_grid.as_ref()?;
        let mut candidates: Vec<((u16, u16), i32)> = Vec::new();
        for ry in 0..grid.height() {
            for rx in 0..grid.width() {
                let (cx, cy) = (rx as i16, ry as i16);
                let distance = (cx - from.0).abs().max((cy - from.1).abs());
                if distance < EXPANSION_MIN_DISTANCE {
                    continue;
                }
                if exclude.is_some_and(|(ex, ey)| {
                    (cx - ex as i16).abs().max((cy - ey as i16).abs()) < EXPANSION_MIN_DISTANCE
                }) {
                    continue;
                }
                let Some(view) = crate::sim::tiberium::tiberium_cell_view(
                    grid,
                    overlay_registry,
                    &rules.tiberium_types,
                    (rx, ry),
                ) else {
                    continue;
                };
                // Value per cell of distance, as fixed-point thousandths:
                // a field twice as far must be twice as rich to win.
                let score = i32::from(view.nominal_value) * 1000
                    / (1000 + i32::from(distance) * 1000 / i32::from(EXPANSION_MIN_DISTANCE));
                candidates.push(((rx, ry), score));
            }
        }
        // Highest score first; the stable sort keeps the earliest
        // (row-major) cell on ties, the single-pass scan's tie order.
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        // The exclusivity term (Layer 2): a cell within
        // [`ORE_FIELD_EXCLUSION_RADIUS`] of any standing refinery — any
        // owner's, this house's included — reads as that field's contested
        // half. Neighboring bases used to latch the same richest central
        // field and mine each other's docks; the survey now prefers the
        // uncontested candidates outright and only falls back to a
        // contested one when no free field qualifies (a small map's
        // reality), so the pick never dead-ends.
        let refinery_cells: Vec<(u16, u16)> = sim
            .substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.category() == crate::map::entities::EntityCategory::Structure
                    && !entity.lifecycle.in_limbo()
                    && REFINERY_TYPES.contains(&sim.interner.resolve(entity.type_ref()))
            })
            .map(|(_, entity)| (entity.position.rx(), entity.position.ry()))
            .collect();
        let contested = |cell: &(u16, u16)| {
            refinery_cells.iter().any(|(rx, ry)| {
                (cell.0 as i16 - *rx as i16)
                    .abs()
                    .max((cell.1 as i16 - *ry as i16).abs())
                    <= ORE_FIELD_EXCLUSION_RADIUS
            })
        };
        let (free, taken): (Vec<&((u16, u16), i32)>, Vec<&((u16, u16), i32)>) =
            candidates.iter().partition(|(cell, _)| !contested(cell));
        let pick = match validate.and_then(|name| rules.object(name)) {
            Some(ty) => free
                .iter()
                .copied()
                .chain(taken.iter().copied())
                .find(|((rx, ry), _)| {
                    find_site_beyond(
                        sim,
                        rules,
                        overlay_registry,
                        ty,
                        (*rx as i16, *ry as i16),
                        self.house,
                        from,
                        EXPANSION_MIN_DISTANCE,
                    )
                    .is_some()
                        // The field itself must sit in ground the zone grid
                        // connects to home: a richer field across a breach
                        // starves behind water no miner crosses.
                        && zones_connected(
                            sim,
                            crate::rules::locomotor_type::MovementZone::Normal,
                            (from.0.max(0) as u16, from.1.max(0) as u16),
                            (*rx, *ry),
                        )
                }),
            None => free.first().or_else(|| taken.first()).copied(),
        };
        pick.map(|(cell, _)| *cell)
    }

    /// The broken-bridge limb's per-tick step; see the call site in
    /// [`Self::orders`] for the shape. `None` — nothing to issue this tick.
    ///
    /// A paced survey ([`BRIDGE_SURVEY_TICKS`]) latches one hut worth an
    /// engineer: a hostile structure must stand cut off from home (if the
    /// zone grid still connects everything, the breach costs nothing), the
    /// hut must want repair ([`crate::sim::world::bridge_orchestrator::
    /// bridge_hut_can_repair`]), and the hut must itself sit on the home
    /// side of the breach — an engineer that cannot walk there repairs
    /// nothing. The dispatch half then sends an idle engineer with the
    /// engine's own capture click; the engine walks it in, runs the repair
    /// before the capture checks and consumes the engineer, which clears
    /// the tracker below and lets a future breach re-arm the limb.
    fn bridge_repair_orders(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        tick: u32,
    ) -> Option<Command> {
        // Forget a consumed or lost engineer. The engine consumes the
        // engineer on hut entry, so a successful repair clears the tracker
        // here; a killed one stops the re-issues until the next survey.
        if let Some(engineer) = self.bridge_engineer
            && !sim.substrate.entities.get(engineer).is_some_and(|entity| {
                entity.owner() == self.house && entity.lifecycle.object_alive()
            })
        {
            self.bridge_engineer = None;
        }

        if tick >= self.bridge_survey_due {
            self.bridge_survey_due = tick + BRIDGE_SURVEY_TICKS;
            self.bridge_hut = None;
            let Some(home) = self.structure_center(sim, opening.construction_yard) else {
                return None;
            };
            let home_cell = (home.0.max(0) as u16, home.1.max(0) as u16);
            let cut_off = |cell: (u16, u16)| {
                !zones_connected(
                    sim,
                    crate::rules::locomotor_type::MovementZone::Normal,
                    home_cell,
                    cell,
                )
            };
            // Worthiness: some hostile structure is unreachable from home.
            let enemy_cut_off = sim.substrate.entities.iter_sorted().any(|(_, entity)| {
                entity.category() == crate::map::entities::EntityCategory::Structure
                    && !entity.lifecycle.in_limbo()
                    && self.is_enemy(sim, entity.owner())
                    && cut_off((entity.position.rx(), entity.position.ry()))
            });
            // The hut may be the map's neutral prop, so the scan is not
            // owner-filtered; the type resolves through the interner like
            // every other rules name (a rules set without it skips the limb).
            if enemy_cut_off {
                let hut = sim.substrate.entities.iter_sorted().find(|(_, entity)| {
                    entity.category() == crate::map::entities::EntityCategory::Structure
                        && sim.interner.resolve(entity.type_ref()) == BRIDGE_HUT_TYPE
                        && entity.lifecycle.object_alive()
                        && crate::sim::world::bridge_orchestrator::bridge_hut_can_repair(
                            sim,
                            (entity.position.rx(), entity.position.ry()),
                        )
                        && !cut_off((entity.position.rx(), entity.position.ry()))
                });
                self.bridge_hut = hut.map(|(id, _)| id);
            }
        }

        // The dispatch half. The latched hut must still want repair — a span
        // already rebuilt (by this engineer or another house's) stops here.
        let hut = self.bridge_hut?;
        let Some(hut_entity) = sim.substrate.entities.get(hut) else {
            self.bridge_hut = None;
            return None;
        };
        let hut_cell = (hut_entity.position.rx(), hut_entity.position.ry());
        if !crate::sim::world::bridge_orchestrator::bridge_hut_can_repair(sim, hut_cell) {
            self.bridge_hut = None;
            return None;
        }
        // A walking engineer is in flight and not re-ordered; one whose order
        // the command layer dropped (idle again) is re-pointed after the
        // retry window, never per tick.
        if tick < self.bridge_retry_due
            || self.bridge_engineer.is_some_and(|engineer| {
                sim.substrate.entities.get(engineer).is_some_and(|entity| {
                    !matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
                })
            })
        {
            return None;
        }
        if let Some((engineer, _)) = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
            entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Infantry
                && sim.interner.resolve(entity.type_ref()) == opening.engineer
                && entity.lifecycle.object_alive()
                && self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        }) {
            self.bridge_engineer = Some(engineer);
            self.gate_idle_order(engineer, tick);
            self.bridge_retry_due = tick + BRIDGE_RETRY_TICKS;
            return Some(Command::CaptureBuilding {
                engineer_id: engineer,
                target_building_id: hut,
            });
        }
        self.commission_repair_engineer(sim, rules, opening, tick)
    }

    /// Queue the one repair engineer through its own production queue when
    /// none stands and none is on the line: only behind a barracks, a
    /// buildable type and a comfortably fat wallet ([`BRIDGE_ENGINEER_
    /// CREDITS`]), so the commission never wedges the growth ladder the way
    /// a broke base's queue would (the air wing's top-up guard, in
    /// miniature). The retry window paces it like any dispatch. The
    /// mechanical halves (stock count, buildability) are the capture
    /// chain's too — see [`Self::engineer_stock`] and
    /// [`Self::engineer_commission_type`].
    fn commission_repair_engineer(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        tick: u32,
    ) -> Option<Command> {
        if tick < self.bridge_retry_due {
            return None;
        }
        let (standing, queued) = self.engineer_stock(sim, rules, opening);
        if standing + queued > 0 {
            return None;
        }
        let type_id = self.engineer_commission_type(sim, rules, opening)?;
        if crate::sim::production::credits_for_owner(sim, sim.interner.resolve(self.house))
            < BRIDGE_ENGINEER_CREDITS
        {
            return None;
        }
        self.bridge_retry_due = tick + BRIDGE_RETRY_TICKS;
        Some(Command::QueueProduction { type_id })
    }

    /// The side's live engineer stock: how many stand alive and how many
    /// sit in the production queue. Both engineer chains' commission caps
    /// read it — the bridge's one-at-a-time and the capture chain's
    /// [`CAPTURE_ENGINEER_STOCK`] surplus.
    fn engineer_stock(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
    ) -> (usize, usize) {
        let standing = sim
            .substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Infantry
                    && sim.interner.resolve(entity.type_ref()) == opening.engineer
                    && entity.lifecycle.object_alive()
            })
            .count();
        let owner = sim.interner.resolve(self.house);
        let queued = crate::sim::production::queue_view_for_owner(sim, rules, owner)
            .iter()
            .filter(|item| sim.interner.resolve(item.type_id) == opening.engineer)
            .count();
        (standing, queued)
    }

    /// The engineer commission's shared admission: a standing barracks, a
    /// buildable type (`can_build` — the country-lock lesson), the interned
    /// id. The stock caps and wallet bars stay with the callers: the bridge
    /// chain keeps its stricter one-at-a-time, the capture chain spends
    /// only its surplus.
    fn engineer_commission_type(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
    ) -> Option<InternedId> {
        if self.count_structures_of_type(sim, opening.barracks) == 0 {
            return None;
        }
        let engineer_ty = rules.object(opening.engineer)?;
        if crate::sim::production::can_build(sim, rules, self.house, engineer_ty, false, false)
            != crate::sim::production::CanBuild::Yes
        {
            return None;
        }
        sim.interner.get(opening.engineer)
    }

    /// The engineer-capture limb's one order for this window, or `None`;
    /// see the call site in [`Self::orders`]. The survey half picks the
    /// nearest revealed neutral-owned tech building — the oil derrick's
    /// class first ([`capture_priority_rank`], its `ProduceCashStartup`
    /// pays the engineer back on entry), nearest within a class — and the
    /// dispatch half sends an idle engineer with the engine's own capture
    /// click: the engine walks him in, runs the arrival checks and consumes
    /// him, which the [`Self::capture_bound`] hygiene reads as success (a
    /// target that no longer stands neutral drops out with him — the chain
    /// does not retake lost ground). With no idle engineer the limb
    /// commissions one through the shared commission core, capped at
    /// [`CAPTURE_ENGINEER_STOCK`] total standing-plus-queued — the bridge
    /// repair's one-at-a-time rule outranks this limb and keeps its own,
    /// and the same never-wedge-the-ladder wallet bar stands in front of
    /// the queue. The bridge limb's own walker, the reserved boarders and
    /// raiders and the raid cycle's actors stay out.
    fn engineer_capture_orders(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        home_center: Option<(i16, i16)>,
        tick: u32,
    ) -> Option<Command> {
        if tick < self.capture_due {
            return None;
        }
        self.capture_due = tick + ENGINEER_CAPTURE_WINDOW_TICKS;
        let home = home_center?;

        // Hygiene: a consumed engineer (the capture succeeded — the engine
        // eats him on entry) or a lost one leaves the bound set, and so does
        // a target no longer held by a neutral house.
        self.capture_bound.retain(|&engineer, target| {
            let engineer_busy = sim
                .substrate
                .entities
                .get(engineer)
                .is_some_and(|entity| {
                    entity.owner() == self.house && entity.lifecycle.object_alive()
                });
            let target_neutral = sim.substrate.entities.get(target).is_some_and(|entity| {
                entity.category() == crate::map::entities::EntityCategory::Structure
                    && entity.lifecycle.object_alive()
                    && sim
                        .houses
                        .get(&entity.owner())
                        .is_some_and(|house| house.multiplay_passive())
            });
            engineer_busy && target_neutral
        });

        // The survey: the nearest revealed neutral tech building, oil
        // derrick first. The neutral owner is a passive house (the same
        // reading the spy's scan makes); anything a combatant holds is not
        // this chain's errand.
        let mut best: Option<(usize, i32, u64)> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.category() != crate::map::entities::EntityCategory::Structure
                || entity.lifecycle.in_limbo()
                || !entity.lifecycle.object_alive()
            {
                continue;
            }
            let type_name = sim.interner.resolve(entity.type_ref());
            let Some(rank) = capture_priority_rank(type_name) else {
                continue;
            };
            let owner = entity.owner();
            if owner == self.house
                || sim
                    .houses
                    .get(&owner)
                    .is_none_or(|house| !house.multiplay_passive())
                || !sim.fog.is_cell_revealed(self.house, entity.position.rx(), entity.position.ry())
            {
                continue;
            }
            let dx = i32::from(entity.position.rx()) - i32::from(home.0);
            let dy = i32::from(entity.position.ry()) - i32::from(home.1);
            let distance = dx.abs().max(dy.abs());
            if best.is_none_or(|(kept_rank, kept_distance, _)| {
                (rank, distance) < (kept_rank, kept_distance)
            }) {
                best = Some((rank, distance, id));
            }
        }
        let Some((_, _, target)) = best else {
            return None;
        };

        // The dispatch: an idle engineer free of every other chain. A bound
        // one whose order the command layer dropped stands mission-less here
        // and is re-pointed at the same target — the bridge limb's re-issue
        // rule, paced by this window instead of its retry clock.
        if let Some((engineer, _)) = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
            entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Infantry
                && sim.interner.resolve(entity.type_ref()) == opening.engineer
                && entity.lifecycle.object_alive()
                && !entity.lifecycle.in_limbo()
                && self.bridge_engineer != Some(*id)
                && !self.boarders.contains(id)
                && !self.raiders.contains(id)
                && !self.raid_involves(*id)
                && self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        }) {
            self.capture_bound.insert(engineer, target);
            self.gate_idle_order(engineer, tick);
            return Some(Command::CaptureBuilding {
                engineer_id: engineer,
                target_building_id: target,
            });
        }

        // No idle engineer: commission into the surplus only — total stock
        // (standing plus queued) under the cap, behind the same wallet bar
        // the bridge commission pays.
        let (standing, queued) = self.engineer_stock(sim, rules, opening);
        if standing + queued >= CAPTURE_ENGINEER_STOCK {
            return None;
        }
        let type_id = self.engineer_commission_type(sim, rules, opening)?;
        if crate::sim::production::credits_for_owner(sim, sim.interner.resolve(self.house))
            < BRIDGE_ENGINEER_CREDITS
        {
            return None;
        }
        Some(Command::QueueProduction { type_id })
    }

    /// Whether the live raid cycle leans on this entity: the wave pool and
    /// the capture dispatch keep their hands off both actors while the
    /// stage machine owns them.
    fn raid_involves(&self, unit: u64) -> bool {
        self.raid_stage
            .is_some_and(|raid| raid.actors().contains(&unit))
    }

    /// Tear the live raid cycle down and arm the cooldown: the terminal of
    /// every failure (a lost actor, a wedged leg) and of the success alike —
    /// the consumed engineer clears the stage the same way a dead one does.
    fn end_engineer_raid(&mut self, tick: u32) {
        self.raid_stage = None;
        self.raid_cooldown_until = tick + RAID_COOLDOWN_TICKS;
    }

    /// The engineer-ride raid's one order for this tick, or `None`; see the
    /// call site in [`Self::orders`]. An armed cycle (see [`EngineerRaid`])
    /// marches every tick — each leg is one command wide and the order gate
    /// paces the re-issues — while arming a fresh cycle is the paced half:
    /// once per window, behind the cooldown, the wallet bar
    /// ([`RAID_WALLET_CREDITS`]) and a buildable ride, it secures the pair
    /// (an idle engineer and an idle ride on hand, or one commissioned per
    /// window, queue dedup included) and latches
    /// [`EngineerRaid::Boarding`]. Every stage is bounded by
    /// [`RAID_STAGE_TIMEOUT_TICKS`] and every lost actor tears the cycle
    /// into the cooldown; Yuri's side keeps `None` in
    /// `Opening::engineer_ride` and never gets here.
    fn engineer_raid_orders(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        tick: u32,
    ) -> Option<Command> {
        let ride_name = opening.engineer_ride?;
        // The stage machine first — an armed cycle marches whatever the
        // wallet does afterwards.
        if let Some(stage) = self.raid_stage {
            return self.engineer_raid_stage(sim, stage, tick);
        }
        if tick < self.raid_cooldown_until || tick < self.raid_arm_due {
            return None;
        }
        self.raid_arm_due = tick + ENGINEER_CAPTURE_WINDOW_TICKS;
        // The arming gate: a fat wallet, a known enemy to aim at, a ride
        // the war factory can actually build.
        if crate::sim::production::credits_for_owner(sim, sim.interner.resolve(self.house))
            < RAID_WALLET_CREDITS
            || self.enemy_center(sim).is_none()
            || self.count_structures_of_type(sim, opening.war_factory) == 0
        {
            return None;
        }
        let Some(ride_ty) = rules.object(ride_name) else {
            return None;
        };
        if crate::sim::production::can_build(sim, rules, self.house, ride_ty, false, false)
            != crate::sim::production::CanBuild::Yes
        {
            return None;
        }
        // Secure the pair: idle units first — the same idle reading every
        // dispatch makes, each actor free of the other chains — else one
        // commission per window (the queue dedup is the pacing when both
        // are missing). The engineer commission honors the side-wide stock
        // cap: the capture chain and this one draw the same well.
        let engineer = self.find_raid_engineer(sim, opening, tick);
        let ride = self.find_raid_ride(sim, ride_name, tick);
        match (engineer, ride) {
            (Some(engineer), Some(ride)) => {
                self.raid_stage = Some(EngineerRaid::Boarding { engineer, ride });
                self.raid_stage_since = tick;
                None
            }
            (None, _) => {
                let (standing, queued) = self.engineer_stock(sim, rules, opening);
                if queued > 0 || standing + queued >= CAPTURE_ENGINEER_STOCK {
                    // One engineer already in the pipe serves whichever chain
                    // pops him; the stock cap keeps the side at two total.
                    return None;
                }
                self.engineer_commission_type(sim, rules, opening)
                    .map(|type_id| Command::QueueProduction { type_id })
            }
            (_, None) => {
                // The ride's queue dedup is this arm's pacing: a ride in the
                // pipe is the raid's already; another would pile up every
                // window the field's stay busy.
                let owner = sim.interner.resolve(self.house);
                let ride_queued = crate::sim::production::queue_view_for_owner(sim, rules, owner)
                    .iter()
                    .any(|item| sim.interner.resolve(item.type_id) == ride_name);
                if ride_queued {
                    return None;
                }
                let type_id = sim.interner.get(ride_name)?;
                Some(Command::QueueProduction { type_id })
            }
        }
    }

    /// An idle engineer free of every other chain, for the raid's arming
    /// half — the capture dispatch's reading minus the bound set (a bound
    /// engineer is the neutral-tech chain's own).
    fn find_raid_engineer(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
        tick: u32,
    ) -> Option<u64> {
        sim.substrate.entities.iter_sorted().find(|(id, entity)| {
            entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Infantry
                && sim.interner.resolve(entity.type_ref()) == opening.engineer
                && entity.lifecycle.object_alive()
                && !entity.lifecycle.in_limbo()
                && !self.capture_bound.contains_key(id)
                && self.bridge_engineer != Some(*id)
                && !self.boarders.contains(id)
                && !self.raiders.contains(id)
                && self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        }).map(|(id, _)| id)
    }

    /// An idle ride free of every other chain, for the raid's arming half.
    fn find_raid_ride(
        &self,
        sim: &crate::sim::world::Simulation,
        ride_name: &str,
        tick: u32,
    ) -> Option<u64> {
        sim.substrate.entities.iter_sorted().find(|(id, entity)| {
            entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Unit
                && sim.interner.resolve(entity.type_ref()) == ride_name
                && entity.lifecycle.object_alive()
                && !entity.lifecycle.in_limbo()
                && self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        }).map(|(id, _)| id)
    }

    /// The armed cycle's leg for this tick; see [`Self::engineer_raid_orders`].
    /// Each leg advances or re-issues one command wide, bounded by the stage
    /// clock and the actors' liveness.
    fn engineer_raid_stage(
        &mut self,
        sim: &crate::sim::world::Simulation,
        stage: EngineerRaid,
        tick: u32,
    ) -> Option<Command> {
        // The hard stage budget: a leg that has not landed in this long
        // tears the raid down into the cooldown.
        if tick.saturating_sub(self.raid_stage_since) >= RAID_STAGE_TIMEOUT_TICKS {
            self.end_engineer_raid(tick);
            return None;
        }
        // Liveness: any lost actor ends the cycle — the consumed engineer is
        // the capture's success, the dead one its failure; the engine does
        // not tell them apart and the cooldown does not care.
        if stage
            .actors()
            .into_iter()
            .any(|actor| {
                sim.substrate.entities.get(actor).is_none_or(|entity| {
                    entity.owner() != self.house || !entity.lifecycle.object_alive()
                })
            })
        {
            self.end_engineer_raid(tick);
            return None;
        }
        match stage {
            EngineerRaid::Boarding { engineer, ride } => {
                let aboard = sim.substrate.entities.get(ride).is_some_and(|entity| {
                    entity
                        .passenger_role
                        .cargo()
                        .is_some_and(|cargo| cargo.passengers().contains(&engineer))
                });
                if aboard {
                    // Latch the raid's aim now, zone-gated from the ride's
                    // own cell: the first enemy priority structure the
                    // ground rows still connect to. None reachable yet —
                    // more scouting may open one — costs this tick only,
                    // the stage clock bounds the wait.
                    let Some(ride_entity) = sim.substrate.entities.get(ride) else {
                        return None;
                    };
                    let from = (
                        (ride_entity.position.rx() as i16).max(0) as u16,
                        (ride_entity.position.ry() as i16).max(0) as u16,
                    );
                    let target = self.enemy_priority_targets(sim).into_iter().find(
                        |(_, cell)| {
                            zones_connected(
                                sim,
                                crate::rules::locomotor_type::MovementZone::Normal,
                                from,
                                (cell.0.max(0) as u16, cell.1.max(0) as u16),
                            )
                        },
                    );
                    if let Some((target, _)) = target {
                        self.raid_stage = Some(EngineerRaid::Sailing {
                            engineer,
                            ride,
                            target,
                        });
                        self.raid_stage_since = tick;
                    }
                    return None;
                }
                // The Enter walk: re-issued only when both stand idle again
                // (a dropped order, a refused Enter), gate-paced.
                let both_idle = sim
                    .substrate
                    .entities
                    .get(engineer)
                    .and_then(|engineer_entity| {
                        sim.substrate.entities.get(ride).map(|ride_entity| {
                            matches!(
                                engineer_entity.passive_acquire_mission(),
                                crate::sim::mission::MissionType::None
                                    | crate::sim::mission::MissionType::Sleep
                                    | crate::sim::mission::MissionType::Guard
                                    | crate::sim::mission::MissionType::AreaGuard
                            ) && matches!(
                                ride_entity.passive_acquire_mission(),
                                crate::sim::mission::MissionType::None
                                    | crate::sim::mission::MissionType::Sleep
                                    | crate::sim::mission::MissionType::Guard
                                    | crate::sim::mission::MissionType::AreaGuard
                            )
                        })
                    })
                    .unwrap_or(false);
                if both_idle
                    && self
                        .order_gate
                        .get(&engineer)
                        .is_none_or(|&due| tick >= due)
                {
                    self.gate_idle_order(engineer, tick);
                    return Some(Command::clicked_mission(
                        engineer,
                        crate::sim::mission::MissionType::Enter,
                        crate::sim::combat::TargetKind::Entity(ride),
                    ));
                }
                None
            }
            EngineerRaid::Sailing {
                engineer,
                ride,
                target,
            } => {
                let Some(target_entity) = sim.substrate.entities.get(target) else {
                    // The enemy razed the aim mid-sail: no delivery left.
                    self.end_engineer_raid(tick);
                    return None;
                };
                let aim = (
                    target_entity.position.rx() as i16,
                    target_entity.position.ry() as i16,
                );
                let Some(ride_entity) = sim.substrate.entities.get(ride) else {
                    return None;
                };
                let at = (
                    ride_entity.position.rx() as i16,
                    ride_entity.position.ry() as i16,
                );
                let distance = i32::from((at.0 - aim.0).abs().max(at.1 - aim.1).max(0));
                if distance <= RAID_ARRIVAL_DISTANCE {
                    self.raid_stage = Some(EngineerRaid::Unloading {
                        engineer,
                        ride,
                        target,
                    });
                    self.raid_stage_since = tick;
                    return None;
                }
                // Re-issue the march only when the ride stands idle again —
                // interrupted, or the arrival it managed fell short — and
                // the order gate lets it (a dropped order waits its gate,
                // never a per-tick churn).
                if matches!(
                    ride_entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                ) && self.order_gate.get(&ride).is_none_or(|&due| tick >= due)
                {
                    self.gate_idle_order(ride, tick);
                    return Some(Command::AttackMove {
                        entity_id: ride,
                        target: None,
                        destination: Some(crate::sim::combat::TargetKind::Cell(
                            aim.0.max(0) as u16,
                            aim.1.max(0) as u16,
                        )),
                    });
                }
                None
            }
            EngineerRaid::Unloading {
                engineer,
                ride,
                target,
            } => {
                // The hold's discharge delay: the engineer rides in limbo a
                // beat after the unload before he stands on foot again —
                // poll until then, the stage clock bounding the wait.
                let on_foot = sim.substrate.entities.get(engineer).is_some_and(|entity| {
                    entity.owner() == self.house
                        && entity.lifecycle.object_alive()
                        && !entity.lifecycle.in_limbo()
                });
                if on_foot {
                    self.raid_stage = Some(EngineerRaid::Capturing { engineer, target });
                    self.raid_stage_since = tick;
                    return None;
                }
                // Still aboard: the unload goes out when the ride stands
                // still (the arrival leg's own stop, or a refused attempt's
                // idle again), gate-paced like every re-issue.
                let ride_idle = sim.substrate.entities.get(ride).is_some_and(|entity| {
                    matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
                });
                if ride_idle && self.order_gate.get(&ride).is_none_or(|&due| tick >= due) {
                    self.gate_idle_order(ride, tick);
                    return Some(Command::UnloadPassengers { transport_id: ride });
                }
                None
            }
            EngineerRaid::Capturing { engineer, target } => {
                // The aim still standing is the point: razed mid-walk, the
                // click is void and the engineer walks home.
                let target_stands = sim
                    .substrate
                    .entities
                    .get(target)
                    .is_some_and(|entity| entity.lifecycle.object_alive());
                if !target_stands {
                    self.end_engineer_raid(tick);
                    return None;
                }
                // The capture click: re-issued only when the engineer stands
                // idle again (the dropped order), gate-paced like the rest.
                if sim.substrate.entities.get(engineer).is_some_and(|entity| {
                    matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
                }) && self
                    .order_gate
                    .get(&engineer)
                    .is_none_or(|&due| tick >= due)
                {
                    self.gate_idle_order(engineer, tick);
                    return Some(Command::CaptureBuilding {
                        engineer_id: engineer,
                        target_building_id: target,
                    });
                }
                None
            }
        }
    }

    /// The ferry's one order for this window, or `None` to fall through to
    /// the branches below; see the call site in [`Self::orders`]. The hull
    /// is any idle unit of the side's ferry names — the amphibious hover
    /// transport on a shoreline map, the fly-zone air hull where no naval
    /// yard ever stood; whichever answers first works the line. The legs,
    /// in order: a loaded transport at or past the enemy's side of the map
    /// unloads; an empty-seat one beached (or hovering) on the home side
    /// takes the nearest idle boarding candidate with the engine's Enter
    /// click; a loaded one whose pool has drained sails at the enemy's
    /// mass center (each candidate zone-gated in the hull's own
    /// movement-zone row — the hover hull's Amphibious, the air hull's
    /// Fly, whose rows connect the whole map); an empty one walks home —
    /// far out to reload, near home onto the yard's spread ring so the
    /// boarding happens on land (the fly hull merely repositions there:
    /// any cell is its ground). `self` is `&mut` for the order gate only —
    /// every other read is observation.
    fn ferry_orders(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        home_center: Option<(i16, i16)>,
        tick: u32,
    ) -> Option<Command> {
        let home = home_center?;
        let hulls: Vec<&'static str> = [opening.amphibious_transport, opening.air_ferry]
            .into_iter()
            .flatten()
            .collect();
        let (transport, entity) = sim.substrate.entities.iter_sorted().find(|(id, entity)| {
            entity.owner() == self.house
                && entity.category() == crate::map::entities::EntityCategory::Unit
                && hulls.contains(&sim.interner.resolve(entity.type_ref()))
                && !entity.lifecycle.in_limbo()
                && self.order_gate.get(id).is_none_or(|&due| tick >= due)
                && matches!(
                    entity.passive_acquire_mission(),
                    crate::sim::mission::MissionType::None
                        | crate::sim::mission::MissionType::Sleep
                        | crate::sim::mission::MissionType::Guard
                        | crate::sim::mission::MissionType::AreaGuard
                )
        })?;
        let transport_name = sim.interner.resolve(entity.type_ref());
        let has_cargo = entity
            .passenger_role
            .cargo()
            .is_some_and(|cargo| cargo.count() > 0);
        let has_seats = entity
            .passenger_role
            .cargo()
            .is_some_and(|cargo| cargo.count() < cargo.capacity());
        let at = (entity.position.rx() as i16, entity.position.ry() as i16);
        let from = (at.0.max(0) as u16, at.1.max(0) as u16);
        // i32 miles: the cell diffs are tiny, the thresholds below are i32.
        let chebyshev = |a: (i16, i16), b: (i16, i16)| {
            i32::from((a.0 - b.0).abs().max((a.1 - b.1).abs()))
        };
        let home_distance = chebyshev(at, home);
        // "The enemy's side": closer to their ground-mass center than to
        // home — a discharge test that also works on the close maps where
        // the whole front sits inside [`FERRY_DISCHARGE_DISTANCE`] of the
        // base.
        let enemy_side = self
            .enemy_center(sim)
            .is_some_and(|aim| chebyshev(at, aim) < home_distance);

        // Discharge: the beachhead. The Unload mission's own admission
        // sorts the passengers onto legal ground; pacing keeps a refused
        // unload from soaking the branch — one window per attempt.
        if has_cargo && (home_distance >= i32::from(FERRY_DISCHARGE_DISTANCE) || enemy_side) {
            self.gate_idle_order(transport, tick);
            return Some(Command::UnloadPassengers {
                transport_id: transport,
            });
        }

        // Dock: beached (or hovering) on the home side with seats open —
        // the nearest
        // idle boarding candidate takes the Enter order before anything
        // sails, so a loading transport waits for its pool instead of
        // ferrying one soldier at a time. The pool is the barracks line
        // (the core soldier plus the infantry roster): the hero never
        // boards, the dedicated scout pair stays on its chain (Yuri's
        // initiate both scouts and boards — the same soldier), and the
        // fortress's reserved cargo and live raiders stay out.
        let near_home =
            home_distance < i32::from(FERRY_DISCHARGE_DISTANCE) && !enemy_side;
        if has_seats && near_home {
            let mut pool: Vec<&'static str> = opening.infantry.iter().map(|e| e.name).collect();
            pool.push(opening.basic_infantry);
            let scout_reserved = opening.scout != opening.basic_infantry;
            let mut best: Option<(i32, u64)> = None;
            for (id, candidate) in sim.substrate.entities.iter_sorted() {
                let type_name = sim.interner.resolve(candidate.type_ref());
                if !pool.contains(&type_name)
                    || candidate.owner() != self.house
                    || candidate.category() != crate::map::entities::EntityCategory::Infantry
                    || candidate.lifecycle.in_limbo()
                    || type_name == opening.hero
                    || (scout_reserved && type_name == opening.scout)
                    || self.boarders.contains(&id)
                    || self.raiders.contains(&id)
                    || self.order_gate.get(&id).is_some_and(|&due| tick < due)
                    || !matches!(
                        candidate.passive_acquire_mission(),
                        crate::sim::mission::MissionType::None
                            | crate::sim::mission::MissionType::Sleep
                            | crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    )
                {
                    continue;
                }
                let dx = candidate.position.rx() as i16 - at.0;
                let dy = candidate.position.ry() as i16 - at.1;
                let distance = i32::from(dx.abs().max(dy.abs()));
                if distance > i32::from(FERRY_BOARD_RADIUS) {
                    continue;
                }
                if best.is_none_or(|(kept, _)| distance < kept) {
                    best = Some((distance, id));
                }
            }
            if let Some((_, boarder)) = best {
                self.gate_idle_order(boarder, tick);
                return Some(Command::clicked_mission(
                    boarder,
                    crate::sim::mission::MissionType::Enter,
                    crate::sim::combat::TargetKind::Entity(transport),
                ));
            }
        }

        // Sail: loaded, its pool drained (or its seats full, or the beach
        // behind it already far). The aim is the wave's — the enemy's
        // priority structures in the transport's rotation, else their mass
        // center spread — each candidate zone-gated before any order rides
        // out. Plain Move, not AttackMove: the cargo is the payload, and a
        // fight on the way must not sink the delivery. No connected aim
        // (the hover hull's sea road is cut; the fly hull's rows never
        // cut) costs only this window.
        if has_cargo {
            let movement_zone = rules
                .object(transport_name)
                .map(|ty| ty.movement_zone)
                .unwrap_or_default();
            let enemy_targets = self.enemy_priority_targets(sim);
            let raw = if enemy_targets.is_empty() {
                self.enemy_center(sim).and_then(|aim| {
                    (0..SPREAD_OFFSETS.len())
                        .map(|index| (transport as usize + index) % SPREAD_OFFSETS.len())
                        .find_map(|index| {
                            let spread = SPREAD_OFFSETS[index];
                            let cell = on_map_cell(
                                sim,
                                (aim.0 + i16::from(spread.0), aim.1 + i16::from(spread.1)),
                                aim,
                            );
                            zones_connected(sim, movement_zone, from, cell).then_some(cell)
                        })
                })
            } else {
                (0..enemy_targets.len())
                    .map(|attempt| {
                        enemy_targets[(transport as usize + attempt) % enemy_targets.len()]
                    })
                    .find_map(|(_, cell)| {
                        let cell = (cell.0.max(0) as u16, cell.1.max(0) as u16);
                        zones_connected(sim, movement_zone, from, cell).then_some(cell)
                    })
            };
            let (rx, ry) = raw?;
            self.gate_idle_order(transport, tick);
            return Some(Command::Move {
                entity_id: transport,
                target_rx: rx,
                target_ry: ry,
            });
        }

        // Return: empty and far out, walk back to the yard's spread ring
        // for the next load. No connected ring cell (the hover hull's sea
        // road home is cut; the fly hull is never cut) costs only this
        // window.
        if home_distance >= i32::from(FERRY_DISCHARGE_DISTANCE) {
            let cell =
                connected_ring_cell(sim, rules, home, from, transport, transport_name)?;
            self.gate_idle_order(transport, tick);
            return Some(Command::Move {
                entity_id: transport,
                target_rx: cell.0,
                target_ry: cell.1,
            });
        }
        // Beach: empty on the home side with nobody in reach — walk to the
        // yard's spread ring so the next windows board on land (a hover
        // hull spawned offshore boards nobody from the water; a beached —
        // or hovering — one sits in the infantry's own neighborhood).
        if let Some(cell) =
            connected_ring_cell(sim, rules, home, from, transport, transport_name)
        {
            self.gate_idle_order(transport, tick);
            return Some(Command::Move {
                entity_id: transport,
                target_rx: cell.0,
                target_ry: cell.1,
            });
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
    ///
    /// The reads are the orders pass's hottest scans — every ladder rung,
    /// growth limb and gate re-walks the whole entity set — so the counts
    /// are cached per tick: the first read rebuilds the census in one pass
    /// (the `own_structures` filter, keyed by type id instead of collected
    /// into Strings) and the rest of the pass reads the map. `orders` only
    /// reads the sim and issues commands — the commands apply after the
    /// pass returns — so a census latched to the pass's own tick is exact
    /// for every read inside it.
    fn count_structures_of_type(
        &mut self,
        sim: &crate::sim::world::Simulation,
        type_name: &str,
    ) -> usize {
        if self.census_tick != self.now_tick {
            self.census_counts.clear();
            for (_, entity) in sim.substrate.entities.iter_sorted() {
                if entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Structure
                {
                    *self.census_counts.entry(entity.type_ref()).or_insert(0) += 1;
                }
            }
            self.census_tick = self.now_tick;
        }
        // Read-only lookup: every retail type name is interned at rules
        // load; a miss would only report zero standing.
        let Some(type_id) = sim.interner.get(type_name) else {
            return 0;
        };
        self.census_counts.get(&type_id).copied().unwrap_or(0) as usize
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

    /// The next ring cell the wall placement should issue into, walking the
    /// targets first superweapon, then second superweapon, then construction
    /// yard — the pieces an early raider values most — and retiring every
    /// target whose ring is spent (every cell holds a wall, or was tried and
    /// refused) into [`Self::walled`]. A ring walk reads the live overlay
    /// grid, so cells already carrying a wall segment are skipped without
    /// spending a tried-cell mark or a placement strike on them. With
    /// `escape` set — a ready segment needs a home now — a walk that finds
    /// nothing gives the tried-cell book one clean second pass on the yard's
    /// ring, the perpetual fallback, so the walk can move again next tick;
    /// whatever still refuses re-fills the book and the strike counter paces
    /// the type, and a ring the auto-fill completed in the meantime answers
    /// `None`, which the caller answers by canceling the segment instead of
    /// wedging the one placement slot.
    fn wall_next_cell(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        overlays: &crate::rules::overlay_types::OverlayTypeRegistry,
        escape: bool,
    ) -> Option<(u16, u16)> {
        let grid = sim.overlay_grid.as_ref();
        let holds_wall = |(rx, ry): (u16, u16)| -> bool {
            grid.and_then(|grid| grid.cell(rx, ry).overlay_id)
                .and_then(|id| overlays.flags(id))
                .is_some_and(|flags| flags.wall)
        };
        for name in [
            opening.super_weapon_building,
            opening.second_super_weapon_building,
            opening.construction_yard,
        ] {
            let Some((id, rx, ry)) = sim
                .substrate
                .entities
                .iter_sorted()
                .find_map(|(eid, entity)| {
                    (entity.owner() == self.house
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && sim.interner.resolve(entity.type_ref()) == name)
                        .then_some((eid, entity.position.rx(), entity.position.ry()))
                })
            else {
                continue;
            };
            if self.walled.contains(&id) {
                continue;
            }
            let Some(ty) = rules.object(name) else {
                continue;
            };
            // The ring's relative cells go negative (a 0-based foundation's
            // top and left ring edges sit at -1), so the map-side guard
            // against a `u16` wrap belongs on the absolute cell, never on
            // the offset — filtering the offset would clip those two edges
            // off the ring entirely.
            let cell = wall_ring_cells(ty.foundation.cell_bounds())
                .into_iter()
                .map(|(dx, dy)| (rx as i16 + dx, ry as i16 + dy))
                .filter(|&(x, y)| x >= 0 && y >= 0)
                .map(|(x, y)| (x as u16, y as u16))
                .find(|&cell| !self.wall_cells.contains(&cell) && !holds_wall(cell));
            match cell {
                Some(cell) => return Some(cell),
                // The ring is spent. One and done either way: a ring fully
                // walked needs no more segments, and a fallen segment is
                // never rebuilt.
                None => {
                    self.walled.insert(id);
                }
            }
        }
        if escape {
            self.wall_cells.clear();
            // The yard is the target the ring can always fall back to, so a
            // spent yard comes out of `walled` with its book wiped — the
            // walk rediscovers it next tick. A supers-first priority means
            // the reset never revisits a superweapon ring (those stay
            // retired in `walled`).
            let yard = sim.substrate.entities.iter_sorted().find_map(|(eid, entity)| {
                (entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Structure
                    && sim.interner.resolve(entity.type_ref()) == opening.construction_yard)
                    .then_some(eid)
            });
            if let Some(id) = yard {
                self.walled.remove(&id);
            }
        }
        None
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
    /// discipline). Cells in `skip` (the scouting chain's blacklist) stand
    /// out of the rotation, and only the nearest
    /// [`SCOUT_ZONE_CANDIDATES`] candidates get the zone-connection check —
    /// the first the ground rows connect wins; a map whose every near
    /// candidate is across a breach answers `None` and the chain rests
    /// instead of marching into the water.
    fn nearest_unrevealed_cell(
        &self,
        sim: &crate::sim::world::Simulation,
        from: (i16, i16),
        skip: &std::collections::HashSet<(u16, u16)>,
    ) -> Option<(u16, u16)> {
        let fog = &sim.fog;
        let (width, height) = (fog.width(), fog.height());
        let mut candidates: Vec<(u32, (u16, u16))> = Vec::new();
        for ry in 0..height {
            for rx in 0..width {
                if fog.is_cell_revealed(self.house, rx, ry) || skip.contains(&(rx, ry)) {
                    continue;
                }
                let dx = i32::from(rx) - i32::from(from.0);
                let dy = i32::from(ry) - i32::from(from.1);
                let distance = (dx * dx + dy * dy) as u32;
                candidates.push((distance, (rx, ry)));
            }
        }
        candidates.sort_by_key(|&(distance, _)| distance);
        candidates
            .into_iter()
            .take(SCOUT_ZONE_CANDIDATES)
            .find(|&(_, cell)| {
                zones_connected(
                    sim,
                    crate::rules::locomotor_type::MovementZone::Normal,
                    (from.0.max(0) as u16, from.1.max(0) as u16),
                    cell,
                )
            })
            .map(|(_, cell)| cell)
    }

    /// How many combat units of exactly this retail type name stand under
    /// hostile flags — the mix's air-pressure reading counts the enemy's
    /// aircraft one type at a time. Layer 3 adds the `Aircraft` category to
    /// the sweep: the old Unit|Infantry pair read zero for a standing jet
    /// wing, so the anti-air answer never lifted against one.
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
                            | crate::map::entities::EntityCategory::Aircraft
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
    /// Layer 3 centers the front-most own combat vehicle: the ground mobile
    /// nearest the enemy mass center, skipping the MCVs, the miner fleet
    /// (and, pointless on infantry, the engineer) — the old substrate-first
    /// pick all too often curtained the expansion MCV standing at home.
    /// The genetic mutator devolves infantry, so it wants the hostile
    /// foot-mobile mass: the centroid of their infantry (the enemy-center
    /// walk narrowed to the category), falling through to the generic aim
    /// when none stands. Every other special — storm, dominator, the
    /// silo's nuke, the chronosphere — strikes the enemy where it hurts
    /// most: their war factories if any stand, else any structure, else
    /// the hostile ground-mass center (ground-only — a jet's flight path
    /// is not an aim point).
    fn superweapon_target(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
        sw_type_name: &str,
    ) -> Option<(u16, u16)> {
        // The genetic mutator's mass aim (the enemy_center walk narrowed
        // to the Infantry category — the engine files foot soldiers there,
        // tanks under Unit).
        if sw_type_name == "GeneticConverterSpecial" {
            let mut sum_x = 0i64;
            let mut sum_y = 0i64;
            let mut count = 0u64;
            for (_, entity) in sim.substrate.entities.iter_sorted() {
                if entity.category() != crate::map::entities::EntityCategory::Infantry
                    || entity.lifecycle.in_limbo()
                    || !self.is_enemy(sim, entity.owner())
                {
                    continue;
                }
                sum_x += i64::from(entity.position.rx());
                sum_y += i64::from(entity.position.ry());
                count += 1;
            }
            if count > 0 {
                return Some(((sum_x / count as i64) as u16, (sum_y / count as i64) as u16));
            }
            // No hostile infantry stands: the generic hostile aim below
            // still beats holding a charged special.
        }
        if opening.super_weapon_targets_self && sw_type_name == opening.super_weapon_type {
            let anchor = self
                .enemy_center(sim)
                .map(|center| (center.0.max(0) as u16, center.1.max(0) as u16))
                .or_else(|| {
                    self.structure_center(sim, opening.construction_yard)
                        .map(|center| (center.0.max(0) as u16, center.1.max(0) as u16))
                });
            let mut best: Option<(i64, (u16, u16))> = None;
            for (_, entity) in sim.substrate.entities.iter_sorted() {
                if entity.owner() != self.house
                    || entity.category() != crate::map::entities::EntityCategory::Unit
                    || entity.lifecycle.in_limbo()
                {
                    continue;
                }
                let name = sim.interner.resolve(entity.type_ref());
                if MCV_TYPES.contains(&name) || ENEMY_MINER_TYPES.contains(&name) {
                    continue;
                }
                let cell = (entity.position.rx(), entity.position.ry());
                let distance = match anchor {
                    Some((ax, ay)) => {
                        let dx = i32::from(cell.0) - i32::from(ax);
                        let dy = i32::from(cell.1) - i32::from(ay);
                        i64::from(dx * dx + dy * dy)
                    }
                    // No enemy ground mobiles to front against: the
                    // substrate's stable first pick still beats nothing.
                    None => 0,
                };
                if best.is_none_or(|(kept, _)| distance < kept) {
                    best = Some((distance, cell));
                }
            }
            return best.map(|(_, cell)| cell);
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
    /// Every combat type this house's order chains must see: the core
    /// three plus the whole roster mix — the hero, the specialists, the
    /// fortress — everything but the economy (the miner), the scout chain
    /// (its own branch owns the dogs) and the MCVs. The guard-post and
    /// wave chains used to know only the core three, so every specialist
    /// the roster built was orderless for life: heroes and mixes stood
    /// idle where they rolled out, jamming the factory door while the
    /// waves left without them (observed live: "many units and the hero
    /// never attack").
    fn combat_roster(opening: &Opening) -> Vec<&'static str> {
        let mut names = vec![
            opening.main_tank,
            opening.tier2_tank,
            opening.basic_infantry,
            opening.hero,
        ];
        if let Some(fortress) = opening.fortress {
            names.push(fortress);
        }
        names.extend(opening.vehicles.iter().map(|entry| entry.name));
        names.extend(opening.infantry.iter().map(|entry| entry.name));
        names
    }

    fn idle_combat_units(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
    ) -> Vec<u64> {
        let roster = Self::combat_roster(opening);
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
                        roster.contains(&name) || Self::is_naval_type(opening, name)
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

    // ---- Layer 3: combat & unit management helpers ----

    /// The futility backoff's read side: whether this order kind is
    /// currently suppressed for the entity. Picking loops use it to step
    /// past a backed-off unit instead of letting it eat the branch's whole
    /// window.
    fn futility_suppressed(&self, unit: u64, kind: FutilityKind, tick: u32) -> bool {
        self.futility
            .get(&unit)
            .is_some_and(|state| state.kind == kind && tick < state.suppress_until)
    }

    /// The futility backoff's record side. `cell` is the entity's own cell
    /// for post/wave/raid/hero orders, the aim cell for the scout's march.
    /// A repeat of the same kind with the same cell on a paced re-order
    /// window builds a strike streak; at [`FUTILITY_STRIKES`] the kind goes
    /// into backoff for [`FUTILITY_BACKOFF_TICKS`] (and the caller skips the
    /// order). A different kind or a moved aim restarts the streak; an
    /// expired backoff re-arms with a fresh one. Returns whether the caller
    /// must skip the order this tick.
    fn futile(&mut self, unit: u64, kind: FutilityKind, cell: (u16, u16), tick: u32) -> bool {
        let fresh = FutilityState {
            kind,
            cell,
            strikes: 1,
            suppress_until: 0,
        };
        let Some(state) = self.futility.get_mut(&unit) else {
            self.futility.insert(unit, fresh);
            return false;
        };
        if state.kind != kind || state.cell != cell {
            *state = fresh;
            return false;
        }
        if tick < state.suppress_until {
            return true;
        }
        if state.suppress_until != 0 {
            // The backoff expired: re-arm with a fresh streak.
            *state = fresh;
            return false;
        }
        state.strikes += 1;
        if state.strikes >= FUTILITY_STRIKES {
            state.suppress_until = tick + FUTILITY_BACKOFF_TICKS;
            return true;
        }
        false
    }

    /// The fortress's cargo mix in boarding preference order: the roster's
    /// anti-armor infantry first (the Guardian GI's deployed launcher is
    /// the classic loadout), then the anti-infantry entries. The hero never
    /// boards — the pick filters him out by name.
    fn boarding_cargo(opening: &Opening) -> Vec<&'static str> {
        let mut names = Vec::new();
        for role in [Role::AntiArmor, Role::AntiInfantry] {
            names.extend(
                opening
                    .infantry
                    .iter()
                    .filter(|entry| entry.role == role)
                    .map(|entry| entry.name),
            );
        }
        names
    }

    /// The nearest enemy economy unit (a harvester on the move between the
    /// enemy's refineries and its fields), with its cell — the Terror
    /// Drone's hunt target.
    fn nearest_enemy_harvester(
        &self,
        sim: &crate::sim::world::Simulation,
        from: (u16, u16),
    ) -> Option<((u16, u16), u64)> {
        let mut best: Option<(u32, u64, (u16, u16))> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.lifecycle.in_limbo()
                || entity.category() != crate::map::entities::EntityCategory::Unit
                || !self.is_enemy(sim, entity.owner())
                || !ENEMY_MINER_TYPES.contains(&sim.interner.resolve(entity.type_ref()))
            {
                continue;
            }
            let cell = (entity.position.rx(), entity.position.ry());
            let dx = i32::from(cell.0) - i32::from(from.0);
            let dy = i32::from(cell.1) - i32::from(from.1);
            let distance = (dx * dx + dy * dy) as u32;
            if best.is_none_or(|(kept, _, _)| distance < kept) {
                best = Some((distance, id, cell));
            }
        }
        best.map(|(_, id, cell)| (cell, id))
    }

    /// The Floating Disk's raid target: the enemy's first standing power
    /// plant (the blinding cut), else its first refinery. A live
    /// structure's own cell, limbo construction excluded.
    fn disc_raid_target(
        &self,
        sim: &crate::sim::world::Simulation,
    ) -> Option<(u64, (i16, i16))> {
        sim.substrate
            .entities
            .iter_sorted()
            .find(|(_, entity)| {
                entity.category() == crate::map::entities::EntityCategory::Structure
                    && !entity.lifecycle.in_limbo()
                    && self.is_enemy(sim, entity.owner())
                    && (ENEMY_POWER_TYPES.contains(&sim.interner.resolve(entity.type_ref()))
                        || ENEMY_REFINERY_TYPES.contains(&sim.interner.resolve(entity.type_ref())))
            })
            .map(|(id, entity)| {
                (
                    id,
                    (entity.position.rx() as i16, entity.position.ry() as i16),
                )
            })
    }

    /// The nearest revealed enemy structure the spy's arrival pays on,
    /// priority class first ([`spy_infiltration_rank`]: the enemy's
    /// superweapon, then its radar, its power plants, any standing
    /// structure) and nearest within a class — with the ground rows still
    /// connecting it to `from`, since a revealed aim across a breach is a
    /// walk the sim nulls, not an infiltration.
    fn nearest_enemy_infiltration_target(
        &self,
        sim: &crate::sim::world::Simulation,
        from: (i16, i16),
    ) -> Option<u64> {
        let from_cell = (from.0.max(0) as u16, from.1.max(0) as u16);
        let mut best: Option<(u8, u32, u64)> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.category() != crate::map::entities::EntityCategory::Structure
                || entity.lifecycle.in_limbo()
                || !self.is_enemy(sim, entity.owner())
            {
                continue;
            }
            let (rx, ry) = (entity.position.rx(), entity.position.ry());
            if !sim.fog.is_cell_revealed(self.house, rx, ry)
                || !zones_connected(
                    sim,
                    crate::rules::locomotor_type::MovementZone::Normal,
                    from_cell,
                    (rx, ry),
                )
            {
                continue;
            }
            let rank = spy_infiltration_rank(sim.interner.resolve(entity.type_ref()));
            let dx = i32::from(rx) - i32::from(from.0);
            let dy = i32::from(ry) - i32::from(from.1);
            let distance = (dx * dx + dy * dy) as u32;
            if best.is_none_or(|(kept_rank, kept_distance, _)| {
                (rank, distance) < (kept_rank, kept_distance)
            }) {
                best = Some((rank, distance, id));
            }
        }
        best.map(|(_, _, id)| id)
    }

    /// The nearest revealed neutral tech building the spy can run at, with
    /// the ground rows still connecting it to `from`: a revealed one across
    /// a breach is a walk the sim nulls, not an infiltration. Neutral means
    /// a passive house (the scenario's Neutral owner) — allied or hostile
    /// tech is not the spy's errand.
    fn nearest_revealed_tech(
        &self,
        sim: &crate::sim::world::Simulation,
        from: (i16, i16),
    ) -> Option<u64> {
        let from_cell = (from.0.max(0) as u16, from.1.max(0) as u16);
        let mut best: Option<(u32, u64)> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.category() != crate::map::entities::EntityCategory::Structure
                || entity.lifecycle.in_limbo()
                || !TECH_BUILDING_TYPES.contains(&sim.interner.resolve(entity.type_ref()))
            {
                continue;
            }
            let owner = entity.owner();
            if owner == self.house || self.is_enemy(sim, owner) {
                continue;
            }
            if sim
                .houses
                .get(&owner)
                .is_none_or(|house| !house.multiplay_passive())
            {
                continue;
            }
            let (rx, ry) = (entity.position.rx(), entity.position.ry());
            if !sim.fog.is_cell_revealed(self.house, rx, ry)
                || !zones_connected(
                    sim,
                    crate::rules::locomotor_type::MovementZone::Normal,
                    from_cell,
                    (rx, ry),
                )
            {
                continue;
            }
            let dx = i32::from(rx) - i32::from(from.0);
            let dy = i32::from(ry) - i32::from(from.1);
            let distance = (dx * dx + dy * dy) as u32;
            if best.is_none_or(|(kept, _)| distance < kept) {
                best = Some((distance, id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// The house's combat units free to take a march order, in the
    /// substrate's stable order: idle units plus the ones holding their
    /// guard post — an attack-move simply replaces that post. Units already
    /// marching or engaging a target are not re-pointed. Layer 3 keeps the
    /// hero out (his own branch owns him), the reserved boarders and
    /// raiders out (their orders run separately), and the fortress out
    /// while its seats are open and boarding is in progress — the loading
    /// infantry have to reach it first.
    /// Whether the type is one of the side's naval hulls — the combat
    /// tables are land rosters, so the ships join the idle and standing
    /// defense pools by name instead: the basic hull, the AA escort, the
    /// second hull and the capital ship. The wave's zone gate keeps them
    /// out of ground marches on its own (their water rows never connect to
    /// a land aim), and the guard-post limb filters them back out: a ship
    /// cannot hold a land ring. The amphibious transport is the ferry
    /// branch's own charge, not a defense pool member.
    fn is_naval_type(opening: &Opening, name: &str) -> bool {
        name == opening.naval_ship
            || opening.naval_aa == Some(name)
            || opening.naval_hull_extra == Some(name)
            || opening.naval_capital == Some(name)
    }

    /// Whether the type is one of the three sides' dogs — the roster's
    /// sentry hound (`ADOG` Allied, `DOG` Soviet, `YDOG` Yuri; all three
    /// `DetectDisguise=yes`). The wave dispatch reads it to keep the
    /// kennel home, the sentry ring to count and post it. Exact names:
    /// the sections are side twins, and "DOG" is a substring of its own
    /// siblings, so only `==` answers.
    fn is_dog(type_name: &str) -> bool {
        const DOG_TYPES: [&str; 3] = ["ADOG", "DOG", "YDOG"];
        DOG_TYPES.contains(&type_name)
    }

    /// The naval layer's production pick (Layer 2.7): the basic hull line
    /// up to [`NAVAL_SHIP_QUOTA`], with the AA escort claiming one turn
    /// once its extra prerequisite honestly passes `can_build` (the
    /// country-lock lesson: the cameo's brightness knows nothing of
    /// prerequisites). Past the quota the second hull rotates up to
    /// [`NAVAL_EXTRA_QUOTA`], and once the battle lab stands the capital
    /// ship fills [`NAVAL_CAPITAL_QUOTA`] — its retail lab prerequisite is
    /// exactly the gate the AI house's `CanBuild` would skip, so the lab
    /// count carries it here. `None` rests the limb — no yard, every
    /// quota met, or nothing honestly buildable yet.
    fn naval_pick(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
    ) -> Option<&'static str> {
        let buildable = |name: &'static str| {
            rules.object(name).is_some_and(|ty| {
                crate::sim::production::can_build(sim, rules, self.house, ty, false, false)
                    == crate::sim::production::CanBuild::Yes
            })
        };
        let hulls = self.count_units_of_type(sim, opening.naval_ship);
        if let Some(aa) = opening.naval_aa
            && hulls >= 1
            && self.count_units_of_type(sim, aa) < 1
            && buildable(aa)
        {
            return Some(aa);
        }
        if hulls < NAVAL_SHIP_QUOTA && buildable(opening.naval_ship) {
            return Some(opening.naval_ship);
        }
        if let Some(extra) = opening.naval_hull_extra
            && self.count_units_of_type(sim, extra) < NAVAL_EXTRA_QUOTA
            && buildable(extra)
        {
            return Some(extra);
        }
        if let Some(capital) = opening.naval_capital
            && self.owns_structure_of_type(sim, opening.battle_lab)
            && self.count_units_of_type(sim, capital) < NAVAL_CAPITAL_QUOTA
            && buildable(capital)
        {
            return Some(capital);
        }
        None
    }

    /// The naval sortie's target, read from the sailing force's own water
    /// rows: the nearest enemy hull — the fleet's war is the gun duel —
    /// else the nearest enemy structure those rows still reach (the shore
    /// bombardment), else `None` and the caller keeps the anchorage. The
    /// reach test runs in the first sailer's own movement zone, so an
    /// inland factory across a land bridge is simply not a naval target
    /// and no rules lookup is needed to say which fights are water
    /// fights.
    fn naval_sortie_target(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        force: &[u64],
    ) -> Option<u64> {
        let sailor = sim.substrate.entities.get(*force.first()?)?;
        let from = (
            sailor.position.rx() as i16,
            sailor.position.ry() as i16,
        );
        let zone = rules
            .object(sim.interner.resolve(sailor.type_ref()))
            .map(|ty| ty.movement_zone)
            .unwrap_or_default();
        let connected = |cell: (i16, i16)| {
            zones_connected(
                sim,
                zone,
                (from.0.max(0) as u16, from.1.max(0) as u16),
                (cell.0.max(0) as u16, cell.1.max(0) as u16),
            )
        };
        // The enemy hulls: mobile water fights first. Land vehicles share
        // the Unit category, but the rows decide — a cell the sailer's zone
        // never reaches is not a sortie target.
        let hulls: Vec<(u64, (i16, i16))> = sim
            .substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                entity.category() == crate::map::entities::EntityCategory::Unit
                    && !entity.lifecycle.in_limbo()
                    && self.is_enemy(sim, entity.owner())
            })
            .map(|(id, entity)| {
                (
                    id,
                    (
                        entity.position.rx() as i16,
                        entity.position.ry() as i16,
                    ),
                )
            })
            .collect();
        if let Some(target) = nearest_connected_target(&hulls, from, &connected) {
            return Some(target);
        }
        // No hull to duel: the shore line. The wave targets stand in for
        // it — the same factory-first list the ground waves march on.
        let shores = self.enemy_priority_targets(sim);
        nearest_connected_target(&shores, from, &connected)
    }

    fn standing_combat_units(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
    ) -> Vec<u64> {
        let roster = Self::combat_roster(opening);
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(id, entity)| {
                entity.owner() == self.house
                    && matches!(
                        entity.category(),
                        crate::map::entities::EntityCategory::Unit
                            | crate::map::entities::EntityCategory::Infantry
                    )
                    && {
                        let name = sim.interner.resolve(entity.type_ref());
                        (roster.contains(&name) || Self::is_naval_type(opening, name))
                            && name != opening.hero
                    }
                    && !self.boarders.contains(id)
                    && !self.raiders.contains(id)
                    && !(Some(sim.interner.resolve(entity.type_ref())) == opening.fortress
                        && !self.boarders.is_empty()
                        && entity
                            .passenger_role
                            .cargo()
                            .is_some_and(|cargo| cargo.count() < cargo.capacity()))
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

    /// Enemy infantry (the engine files foot soldiers under the Infantry
    /// category; allies and passive houses are not enemies) standing within
    /// Chebyshev `radius` cells of `cell` — the Desolator's engagement
    /// count.
    fn hostile_infantry_near(
        &self,
        sim: &crate::sim::world::Simulation,
        cell: (i16, i16),
        radius: i16,
    ) -> usize {
        sim.substrate
            .entities
            .iter_sorted()
            .filter(|(_, entity)| {
                let dx = i32::from(entity.position.rx() as i16) - i32::from(cell.0);
                let dy = i32::from(entity.position.ry() as i16) - i32::from(cell.1);
                entity.category() == crate::map::entities::EntityCategory::Infantry
                    && !entity.lifecycle.in_limbo()
                    && self.is_enemy(sim, entity.owner())
                    && dx.abs().max(dy.abs()) <= i32::from(radius)
            })
            .count()
    }

    /// The nearest hostile entity inside `radius` cells of `center`, ties
    /// broken by the substrate's stable order. Passive houses (observers,
    /// civilians) and allies do not count as intruders. An entity still
    /// held by its constructor sits in limbo at (0,0) by native semantics,
    /// so it is not an intruder anywhere.
    fn nearest_intruder(
        &self,
        sim: &crate::sim::world::Simulation,
        center: (i16, i16),
        radius: i16,
    ) -> Option<u64> {
        let limit = i32::from(radius) * i32::from(radius);
        let mut best: Option<(i32, u64)> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.lifecycle.in_limbo() || !self.is_enemy(sim, entity.owner()) {
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

    /// Whether the base alarm is armed: the house's native damage admission
    /// stamped a building hit within [`BASE_ALARM_TICKS`]. The stamp rides
    /// the strategy emergency block (`BuildingReceivePrelude` writes the
    /// current frame on every building damage admission), in the same tick
    /// units as `tick`. Zero is the never-hit native initial value.
    fn base_alarm_armed(&self, sim: &crate::sim::world::Simulation, tick: u32) -> bool {
        let Some(house) = sim.houses.get(&self.house) else {
            return false;
        };
        let last = house.strategy_emergency().last_building_attack_frame();
        last > 0 && i64::from(tick) - i64::from(last) <= i64::from(BASE_ALARM_TICKS)
    }

    /// The base-alarm limb's one order for this tick, or `None` to fall
    /// through to the idle perimeter and the branches below. `armed` says
    /// the damage stamp is fresh; the stance itself lives until the focus
    /// hold passes, so the defenders keep the mop-up after the last hit
    /// and the wave gate stays closed that whole while.
    fn base_defense_orders(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        idle: &[u64],
        tick: u32,
        armed: bool,
    ) -> Option<Command> {
        // Latch or refresh the focus: the intruder nearest any yard at the
        // alarm radius points it — stand-off fire sits outside the idle
        // perimeter — else a fresh alarm with no visible intruder (an air
        // pass already gone, artillery that died to the return fire) keeps
        // the standing focus or seeds one on the worst-wounded structure.
        let yards = self.own_yard_centers(sim, opening);
        let intruder = yards
            .iter()
            .find_map(|&yard| self.nearest_intruder(sim, yard, ALARM_RADIUS));
        if let Some(target) = intruder
            && let Some(entity) = sim.substrate.entities.get(target)
        {
            self.defense_focus = Some((
                (entity.position.rx(), entity.position.ry()),
                tick + DEFENSE_HOLD_TICKS,
            ));
        } else if armed {
            let until = tick + DEFENSE_HOLD_TICKS;
            let cell = self
                .defense_focus
                .map(|(cell, _)| cell)
                .or_else(|| self.worst_wounded_cell(sim, rules));
            self.defense_focus = cell.map(|cell| (cell, until));
        }
        // The stance lifts when its hold passes: the defenders return to
        // the idle flow (the guard-post limb re-posts them) and the wave
        // gate re-arms.
        if !self.defense_focus.is_some_and(|(_, until)| tick < until) {
            self.defense_focus = None;
            self.defenders.clear();
            return None;
        }
        // Dead defenders leave the roster — the substrate drops them.
        self.defenders
            .retain(|&unit| sim.substrate.entities.get(unit).is_some());
        let (focus, _) = self.defense_focus?;
        // A due idle unit attacks the intruder outright.
        if let Some(attacker) = self.due_idle_unit(idle, tick)
            && let Some(target) = intruder
        {
            self.gate_idle_order(attacker, tick);
            return Some(Command::Attack {
                attacker_id: attacker,
                target_id: target,
            });
        }
        // Short of the commit floor, the nearest standing unit leaves its
        // post and attack-moves on the focus cell, fighting whatever it
        // meets on the way. The standing set holds the posted and idle
        // combat roster (hero, boarders and raiders already excluded), so
        // this is the recall: the base's own garrison turns toward the
        // fight instead of holding its ring while the yards burn.
        if self.defenders.len() < DEFENSE_COMMIT_MIN {
            let standing = self.standing_combat_units(sim, opening);
            let mut best: Option<(i32, u64)> = None;
            for unit in standing {
                if self.defenders.contains(&unit)
                    || !self.order_gate.get(&unit).is_none_or(|&at| tick >= at)
                {
                    continue;
                }
                let Some(entity) = sim.substrate.entities.get(unit) else {
                    continue;
                };
                let dx = i32::from(entity.position.rx() as i16 - focus.0 as i16);
                let dy = i32::from(entity.position.ry() as i16 - focus.1 as i16);
                let distance_squared = dx * dx + dy * dy;
                if best.is_none_or(|(kept, _)| distance_squared < kept) {
                    best = Some((distance_squared, unit));
                }
            }
            if let Some((_, unit)) = best {
                self.defenders.push(unit);
                self.gate_idle_order(unit, tick);
                return Some(Command::AttackMove {
                    entity_id: unit,
                    target: None,
                    destination: Some(crate::sim::combat::TargetKind::Cell(focus.0, focus.1)),
                });
            }
        }
        // Enough defenders committed and no idle unit due: the stance holds
        // without spending this tick's order — the branches below keep
        // running (production, repairs, the growth chain).
        None
    }

    /// The cell of the house's worst-wounded standing structure, ranked by
    /// damage fraction — the alarm limb's fallback focus when no intruder
    /// is visible: the freshest fight's address is whatever bleeds hardest.
    fn worst_wounded_cell(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
    ) -> Option<(u16, u16)> {
        let mut best: Option<(i64, u16, u16)> = None;
        for (_, entity) in sim.substrate.entities.iter_sorted() {
            if entity.owner() != self.house
                || entity.category() != crate::map::entities::EntityCategory::Structure
                || entity.lifecycle.in_limbo()
                || !entity.lifecycle.object_alive()
            {
                continue;
            }
            let Some(ty) = rules.object(sim.interner.resolve(entity.type_ref())) else {
                continue;
            };
            let health = i64::from(entity.health.current().max(1));
            let strength = i64::from(ty.strength.max(1));
            // Health fraction in thousandths, lower = worse: cross-type
            // wounds compare fairly without floats.
            let fraction = health * 1000 / strength;
            if best.is_none_or(|(kept, _, _)| fraction < kept) {
                best = Some((
                    fraction,
                    entity.position.rx(),
                    entity.position.ry(),
                ));
            }
        }
        best.map(|(_, rx, ry)| (rx, ry))
    }

    /// The total drain of the house's queued-but-unlanded buildings: every
    /// Building and Defense item still in the production queues, priced at
    /// its retail `Power` (negative = drain; a queued plant's positive
    /// output pays nothing back until it stands, so only the drain side
    /// counts). Drain bites the instant an item places, so the margin rules
    /// must price the whole queue in — a grid that reads black with two
    /// coils queued browns out the moment they land.
    fn pending_structure_drain(
        &self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        owner: &str,
    ) -> i32 {
        crate::sim::production::queue_view_for_owner(sim, rules, owner)
            .into_iter()
            .filter(|item| {
                matches!(
                    item.queue_category,
                    crate::sim::production::ProductionCategory::Building
                        | crate::sim::production::ProductionCategory::Defense
                )
            })
            .filter_map(|item| {
                rules
                    .object(sim.interner.resolve(item.type_id))
                    .map(|ty| ty.power.min(0))
            })
            .sum()
    }

    /// The hostile structures a wave marches on, in the substrate's stable
    /// order with each one's cell: the enemy's war factories first — a wave
    /// that razes production starves the reinforcements and snowballs — or
    /// any hostile structure when no factory stands. Allies and passive
    /// houses never count as hostile. Wave dispatches cycle this list by unit
    /// id, so the group fans out over the target base; each destination is a
    /// live building's own cell, which is always reachable-adjacent. An
    /// entity still held by its constructor sits in limbo at (0,0) by native
    /// semantics (`telemetry.rs`), so an under-construction factory is not a
    /// (0,0) target — the replay's other half of the bridgehead jam.
    fn enemy_priority_targets(
        &self,
        sim: &crate::sim::world::Simulation,
    ) -> Vec<(u64, (i16, i16))> {
        let mut factories = Vec::new();
        let mut structures = Vec::new();
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.category() != crate::map::entities::EntityCategory::Structure
                || entity.lifecycle.in_limbo()
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
    /// building stands: the centroid of their ground mobiles. Sums over the
    /// entity set, so the result does not depend on iteration order. Limbo
    /// entities sit at (0,0) by native semantics and would drag the centroid
    /// off-map, so constructor-held units stay out — and Layer 3 keeps the
    /// aircraft out too: a flight path is not an aim point, and the old
    /// whole-mobiles centroid pulled the superweapon aim and the endgame
    /// wave toward wherever the jets happened to circle.
    fn enemy_center(&self, sim: &crate::sim::world::Simulation) -> Option<(i16, i16)> {
        let mut sum_x = 0i64;
        let mut sum_y = 0i64;
        let mut count = 0u64;
        for (_, entity) in sim.substrate.entities.iter_sorted() {
            if !matches!(
                entity.category(),
                crate::map::entities::EntityCategory::Unit
                    | crate::map::entities::EntityCategory::Infantry
            ) || entity.lifecycle.in_limbo()
                || !self.is_enemy(sim, entity.owner())
            {
                continue;
            }
            sum_x += i64::from(entity.position.rx());
            sum_y += i64::from(entity.position.ry());
            count += 1;
        }
        (count > 0).then(|| ((sum_x / count as i64) as i16, (sum_y / count as i64) as i16))
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

    /// The cells of the house's standing construction yards, in the
    /// substrate's stable order: the first is the home base every legacy
    /// ring scan centers on; any later one is an expansion the growth
    /// limbs route to.
    fn own_yard_centers(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
    ) -> Vec<(i16, i16)> {
        self.own_structures(sim)
            .iter()
            .filter(|(_, name)| name == opening.construction_yard)
            .filter_map(|(id, _)| {
                sim.substrate
                    .entities
                    .get(*id)
                    .map(|entity| (entity.position.rx() as i16, entity.position.ry() as i16))
            })
            .collect()
    }

    /// How many of the house's structures of exactly this retail type name
    /// stand within `radius` cells (Chebyshev) of `center`: the per-base
    /// cluster readings — the expansion ring's power and refinery standing,
    /// each yard's gun count for the defense quotas.
    fn count_structures_of_type_near(
        &self,
        sim: &crate::sim::world::Simulation,
        type_name: &str,
        center: (i16, i16),
        radius: i16,
    ) -> usize {
        self.own_structures(sim)
            .iter()
            .filter(|(_, name)| name == type_name)
            .filter(|(id, _)| {
                sim.substrate.entities.get(*id).is_some_and(|entity| {
                    (entity.position.rx() as i16 - center.0)
                        .abs()
                        .max((entity.position.ry() as i16 - center.1).abs())
                        <= radius
                })
            })
            .count()
    }

    /// Whether a standing yard of this house sits within `radius` cells
    /// (Chebyshev) of `site` — the remote-refinery routing's admission:
    /// the engine admits a placement only inside a friendly structure's
    /// build area, so a foundation beside a yard-less far field can never
    /// land.
    fn yard_near_site(
        &self,
        sim: &crate::sim::world::Simulation,
        opening: &Opening,
        site: Option<(u16, u16)>,
        radius: i16,
    ) -> bool {
        let Some((sx, sy)) = site else {
            return false;
        };
        self.own_yard_centers(sim, opening).iter().any(|cell| {
            (i32::from(cell.0) - i32::from(sx)).abs().max((i32::from(cell.1) - i32::from(sy)).abs())
                <= i32::from(radius)
        })
    }

    /// The building-repair limb's per-tick step; see the call site in
    /// [`Self::orders`]. `None` — nothing to issue this tick.
    ///
    /// A paced survey ([`REPAIR_SURVEY_TICKS`]) picks the worthiest
    /// damaged structure: health under three fifths of strength, no repair
    /// already running (the byte reading on — whoever turned it on), the
    /// wallet comfortably past [`STRUCTURE_REPAIR_CREDITS`]. Economy
    /// first: refineries, power plants and war factories outrank the rest;
    /// then the lowest health fraction wins, ties by the substrate's
    /// stable order. The issued [`Command::ToggleRepair`] is a toggle —
    /// a re-issue would switch the repair back off — so the activation
    /// latches in `repairing_structure` until the byte reads off again:
    /// repaired whole, unpaid, dead or sold.
    fn structure_repair_orders(
        &mut self,
        sim: &crate::sim::world::Simulation,
        rules: &crate::rules::ruleset::RuleSet,
        opening: &Opening,
        owner: &str,
        tick: u32,
    ) -> Option<Command> {
        // Forget an activation whose repair no longer runs: the engine's
        // repair step stops the byte at full health or an empty wallet,
        // and a dead or captured structure stops being a repair job.
        if let Some(id) = self.repairing_structure
            && !sim.substrate.entities.get(id).is_some_and(|entity| {
                entity.owner() == self.house
                    && entity.category() == crate::map::entities::EntityCategory::Structure
                    && entity.lifecycle.object_alive()
                    && entity.repairing.is_on()
            })
        {
            self.repairing_structure = None;
        }
        if tick < self.repair_due || self.repairing_structure.is_some() {
            return None;
        }
        self.repair_due = tick + REPAIR_SURVEY_TICKS;
        if crate::sim::production::credits_for_owner(sim, owner) < STRUCTURE_REPAIR_CREDITS {
            return None;
        }
        // The economy structures first: their downtime starves every other
        // limb behind them, so their damage outranks a wounded barracks.
        let economy = |name: &str| {
            name == opening.refinery || name == opening.power || name == opening.war_factory
        };
        let mut best: Option<(u64, (usize, i32))> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            if entity.owner() != self.house
                || entity.category() != crate::map::entities::EntityCategory::Structure
                || entity.lifecycle.in_limbo()
                || !entity.lifecycle.object_alive()
                || entity.repairing.is_on()
            {
                continue;
            }
            let name = sim.interner.resolve(entity.type_ref());
            let Some(ty) = rules.object(name) else {
                continue;
            };
            let health = entity.health.current();
            // Health fraction under three fifths, integer-safe: whole and
            // barely-scratched structures stay out.
            if health * REPAIR_FRACTION_DENOMINATOR >= ty.strength * REPAIR_FRACTION_NUMERATOR {
                continue;
            }
            let rank = (usize::from(!economy(name)), ty.strength - health);
            if best.as_ref().is_none_or(|(_, kept_rank)| rank < *kept_rank) {
                best = Some((id, rank));
            }
        }
        if let Some((id, _)) = best {
            self.repairing_structure = Some(id);
            return Some(Command::ToggleRepair { entity_id: id });
        }
        None
    }
}

/// The capture chain's priority class for a tech building type — the oil
/// derrick's class first ([`CAPTURE_PRIORITY_TYPES`] order, its
/// `ProduceCashStartup` pays the engineer back on entry) — and `None` for
/// a name outside the neutral-tech table, which the survey skips.
fn capture_priority_rank(type_name: &str) -> Option<usize> {
    CAPTURE_PRIORITY_TYPES
        .iter()
        .position(|&name| name == type_name)
}

/// The spy's infiltration priority for an enemy structure type: the
/// superweapon buildings first (the arrival resets the charging special's
/// clock), then the radars (the blackout), the power plants (the dip) —
/// and every other standing enemy structure last, the walk still worth the
/// walk even when the arrival teaches nothing.
fn spy_infiltration_rank(type_name: &str) -> u8 {
    if ENEMY_SUPERWEAPON_TYPES.contains(&type_name) {
        0
    } else if ENEMY_RADAR_TYPES.contains(&type_name) {
        1
    } else if ENEMY_POWER_TYPES.contains(&type_name) {
        2
    } else {
        3
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

/// Whether ground movement connects `from` to `to`: the native zone rows
/// (`MapClass::GetZoneID @ 0x0056D230`, the query the sim's own team builder
/// asks in `ai_team_creation`) resolve both cells under `movement_zone`'s raw
/// row and compare — the O(1) read of "can a walker get there". Zone edges
/// republish synchronously when a bridge collapses or an engineer rebuilds
/// one, so this is the whole gate. Unavailable topology at either end is not
/// a negative: the caller keeps its order rather than acting on an answer the
/// map cannot give (the same fail-open the team builder's zone comparison
/// takes with `None`).
fn zones_connected(
    sim: &crate::sim::world::Simulation,
    movement_zone: crate::rules::locomotor_type::MovementZone,
    from: (u16, u16),
    to: (u16, u16),
) -> bool {
    let Some(zones) = sim.zone_grid.as_ref() else {
        return true;
    };
    let Some(terrain) = sim.resolved_terrain.as_ref() else {
        return true;
    };
    match (
        zones.get_zone_id_native(terrain, from, movement_zone, false),
        zones.get_zone_id_native(terrain, to, movement_zone, false),
    ) {
        (Some(from_zone), Some(to_zone)) => from_zone == to_zone,
        _ => true,
    }
}

/// The first zone-connected spread cell on the ring around `center` for
/// `unit`'s rotation — the guard-post pick the defense posts and the hero's
/// home post share. An offset that lands off-map is clamped; an offset the
/// ground rows do not connect to the unit's own cell is passed over (a post
/// across a breach is not a post).
fn connected_ring_cell(
    sim: &crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    center: (i16, i16),
    from: (u16, u16),
    unit: u64,
    type_name: &str,
) -> Option<(u16, u16)> {
    let movement_zone = rules
        .object(type_name)
        .map(|ty| ty.movement_zone)
        .unwrap_or_default();
    (0..SPREAD_OFFSETS.len())
        .map(|index| (unit as usize + index) % SPREAD_OFFSETS.len())
        .find_map(|index| {
            let spread = SPREAD_OFFSETS[index];
            let cell = on_map_cell(
                sim,
                (
                    center.0 + i16::from(spread.0),
                    center.1 + i16::from(spread.1),
                ),
                center,
            );
            zones_connected(sim, movement_zone, from, cell).then_some(cell)
        })
}

/// The siege-role wave unit's standoff destination: a cell 10–12 cells short
/// of `target` along the home line (spread across the range by the unit's
/// rotation, so several siege units do not stack on one post), clamped onto
/// the map and zone-gated from the unit's own cell — a bombardment post the
/// walker cannot reach is not a post. A target already within pull-back
/// range of home answers `None` (no room to stand off: the caller keeps the
/// plain aim path's verdict — here, skipping the unit).
fn siege_standoff_cell(
    sim: &crate::sim::world::Simulation,
    home: (i16, i16),
    target: (u16, u16),
    from: (u16, u16),
    movement_zone: crate::rules::locomotor_type::MovementZone,
) -> Option<(u16, u16)> {
    let (tx, ty) = (i32::from(target.0), i32::from(target.1));
    let (dx, dy) = (tx - i32::from(home.0), ty - i32::from(home.1));
    let distance = dx.abs().max(dy.abs());
    if distance <= SIEGE_STANDOFF_MIN + 2 {
        return None;
    }
    let mut step = SIEGE_STANDOFF_MAX;
    while step >= SIEGE_STANDOFF_MIN {
        let sx = tx - (dx * step) / distance;
        let sy = ty - (dy * step) / distance;
        let cell = on_map_cell(sim, (sx as i16, sy as i16), home);
        // A pull-back clamped all the way onto the home cell is not a post.
        if (cell.0 as i16, cell.1 as i16) != home
            && zones_connected(sim, movement_zone, from, cell)
        {
            return Some(cell);
        }
        step -= 1;
    }
    None
}

/// The naval sortie's target pick: the nearest candidate cell the
/// connectivity predicate admits, reading `candidates` in their given
/// (stable) order so a distance tie keeps it. `None` — nothing reachable;
/// the caller keeps its force anchored. Pure over its arguments: the
/// connectivity predicate is injected so a test can stub the zone rows.
fn nearest_connected_target(
    candidates: &[(u64, (i16, i16))],
    from: (i16, i16),
    connected: &dyn Fn((i16, i16)) -> bool,
) -> Option<u64> {
    let mut best: Option<(u32, u64)> = None;
    for &(id, cell) in candidates {
        if !connected(cell) {
            continue;
        }
        let dx = i32::from(cell.0) - i32::from(from.0);
        let dy = i32::from(cell.1) - i32::from(from.1);
        let distance = (dx * dx + dy * dy) as u32;
        if best.is_none_or(|(kept, _)| distance < kept) {
            best = Some((distance, id));
        }
    }
    best.map(|(_, id)| id)
}

/// The perimeter ring one cell outside a foundation's cell bounds, walked
/// clockwise from the top-left corner: the top edge left to right, the right
/// edge top to bottom, the bottom edge right to left, the left edge bottom
/// to top. The order is the wall ring's placement order — fixed, so a cell
/// the engine refused is never revisited ahead of a fresher one, and a ring
/// fills edge to edge instead of dotting.
fn wall_ring_cells(bounds: (i16, i16, i16, i16)) -> Vec<(i16, i16)> {
    let (min_x, min_y, max_x, max_y) = bounds;
    let (left, top, right, bottom) = (min_x - 1, min_y - 1, max_x + 1, max_y + 1);
    let mut cells = Vec::new();
    for x in left..=right {
        cells.push((x, top));
    }
    for y in top + 1..=bottom {
        cells.push((right, y));
    }
    for x in (left..right).rev() {
        cells.push((x, bottom));
    }
    for y in (top + 1..bottom).rev() {
        cells.push((left, y));
    }
    cells
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
    find_site_around_capped(sim, rules, registry, ty, center, house, None, SITE_SEARCH_RADIUS)
}

/// [`find_site_around`] with a capped radius and, when `build_area_owner`
/// names a house, the engine's own build-area predicate as an extra
/// candidate filter — the same admission `PlaceReadyBuilding` runs at
/// apply time, so a candidate that passes here cannot be refused for
/// adjacency (`production_placement::is_within_build_area`, the
/// `Adjacent+1` ring over friendly `BaseNormal` providers). The legacy
/// home scans pass no filter: they center on a standing yard whose build
/// area already covers the reach those scans ever win on.
fn find_site_around_capped(
    sim: &crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    registry: &crate::rules::overlay_types::OverlayTypeRegistry,
    ty: &crate::rules::object_type::ObjectType,
    center: (i16, i16),
    house: InternedId,
    build_area_owner: Option<&str>,
    max_radius: i16,
) -> Option<(u16, u16)> {
    for radius in 1..=max_radius {
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
                ) && build_area_owner.is_none_or(|owner| {
                    crate::sim::production::is_within_build_area(
                        sim,
                        rules,
                        owner,
                        ty,
                        origin.0 as u16,
                        origin.1 as u16,
                    )
                }) {
                    return Some((origin.0 as u16, origin.1 as u16));
                }
            }
        }
    }
    None
}

/// The first guard-post cell around `base` that the ground rows connect the
/// unit to, rotating the fixed fan-out offsets by unit id so a group
/// spreads over neighboring cells instead of stacking on one. `None` — no
/// connected offset; the caller spends only the unit's idle window (the
/// order gate paces the retry) instead of marching it into the water.
fn pick_guard_post(
    sim: &crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    unit: u64,
    base: (i16, i16),
    offsets: &[(i8, i8)],
) -> Option<(u16, u16)> {
    let entity = sim.substrate.entities.get(unit)?;
    let from = (entity.position.rx(), entity.position.ry());
    let movement_zone = rules
        .object(sim.interner.resolve(entity.type_ref()))
        .map(|ty| ty.movement_zone)
        .unwrap_or_default();
    (0..offsets.len())
        .map(|index| (unit as usize + index) % offsets.len())
        .find_map(|index| {
            let spread = offsets[index];
            let cell = on_map_cell(
                sim,
                (base.0 + i16::from(spread.0), base.1 + i16::from(spread.1)),
                base,
            );
            zones_connected(sim, movement_zone, from, cell).then_some(cell)
        })
}

/// The first legal foundation for `ty` around `center` that also keeps
/// `min_distance` (Chebyshev) from `from` — the placement predicate the
/// expansion routing uses, so the foundation the survey validated is a
/// foundation the placement can still take. A field sitting at exactly
/// the survey's minimum can otherwise hand back a foundation the
/// [`SITE_SEARCH_RADIUS`] ring scan pulled toward home, the refinery
/// lands inside the very distance the remote latch demands, and the
/// whole second-base chain never arms (observed: the refinery stood 28
/// cells out and the latch never fired).
fn find_site_beyond(
    sim: &crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    registry: &crate::rules::overlay_types::OverlayTypeRegistry,
    ty: &crate::rules::object_type::ObjectType,
    center: (i16, i16),
    house: InternedId,
    from: (i16, i16),
    min_distance: i16,
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
                if (origin.0 - from.0).abs().max((origin.1 - from.1).abs()) < min_distance {
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
    use super::{
        capture_priority_rank, nearest_connected_target, spy_infiltration_rank, Role,
        EXPANSION_MIN_DISTANCE, OPENINGS, SuperAi, SuperAiDifficulty, TECH_BUILDING_TYPES,
    };
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

    /// The expanded tables (the engineer split, the naval layer's second
    /// hull and capital, the roster additions and the ferry transports):
    /// fast table reads, one row per side. The production logic itself is
    /// exercised by the retail drives; what this pins is that each side's
    /// table names the rules sections that side can actually field — the
    /// Soviet AA escort is the Sea Scorpion, not the anti-hull Squid, the
    /// Yuri navy keeps its empty pair (the Boomer is both hull and
    /// capital), and the three engineers are three distinct sections.
    #[test]
    fn openings_name_the_expanded_naval_roster_and_ferry_entries() {
        let opening = |yard: &str| {
            OPENINGS
                .iter()
                .find(|opening| opening.construction_yard == yard)
                .expect("each side's opening")
        };
        let allied = opening("GACNST");
        assert_eq!(allied.engineer, "ENGINEER");
        assert_eq!(allied.naval_aa, Some("AEGIS"));
        assert_eq!(allied.naval_hull_extra, Some("DLPH"));
        assert_eq!(allied.naval_capital, Some("CARRIER"));
        assert_eq!(allied.amphibious_transport, Some("LCRF"));
        assert!(allied
            .infantry
            .iter()
            .any(|entry| entry.name == "CLEG" && entry.needs == Some("GATECH")));

        let soviet = opening("NACNST");
        assert_eq!(soviet.engineer, "SENGINEER");
        assert_eq!(
            soviet.naval_aa,
            Some("HYD"),
            "the Sea Scorpion, not the anti-hull Squid, is the AA escort"
        );
        assert_eq!(soviet.naval_hull_extra, Some("SQD"));
        assert_eq!(soviet.naval_capital, Some("DRED"));
        assert_eq!(soviet.amphibious_transport, Some("SAPC"));
        assert!(soviet
            .infantry
            .iter()
            .any(|entry| entry.name == "TERROR" && entry.needs.is_none()));
        assert!(soviet
            .vehicles
            .iter()
            .any(|entry| entry.name == "DTRUCK" && entry.needs == Some("NARADR")));

        let yuri = opening("YACNST");
        assert_eq!(yuri.engineer, "YENGINEER");
        assert_eq!(yuri.naval_aa, None);
        assert_eq!(
            yuri.naval_hull_extra.and(yuri.naval_capital),
            None,
            "the Boomer is both hull and capital"
        );
        assert_eq!(yuri.amphibious_transport, Some("YHVR"));
        assert!(yuri
            .infantry
            .iter()
            .any(|entry| entry.name == "YDOG" && entry.needs.is_none()));

        // The engineer bug's own shape: the three sides name three distinct
        // rules sections — a shared "ENGINEER" is ForbiddenHouses-refused
        // outside an Allied base.
        let mut engineers: Vec<&str> = OPENINGS.iter().map(|opening| opening.engineer).collect();
        engineers.sort_unstable();
        engineers.dedup();
        assert_eq!(engineers, vec!["ENGINEER", "SENGINEER", "YENGINEER"]);
    }

    /// The air ferry and the stolen-tech trio. Only the Allies field a
    /// fly-zone transport — the Night Hawk, the inland ferry where no
    /// naval yard ever stands; the Soviet and Yuri hover transports keep
    /// their naval-yard road, so their `air_ferry` stays `None`. All three
    /// sides carry all three stolen-tech sections (retail Owner= names
    /// every country): each row needs no prerequisite because the gate is
    /// the theft — `can_build` refuses until a spy's lab infiltration
    /// flips the house's stolen-tech byte, so army_pick never spends a
    /// slot on one early.
    #[test]
    fn openings_field_the_air_ferry_and_the_stolen_tech_trio() {
        let opening = |yard: &str| {
            OPENINGS
                .iter()
                .find(|opening| opening.construction_yard == yard)
                .expect("each side's opening")
        };
        assert_eq!(opening("GACNST").air_ferry, Some("SHAD"));
        assert_eq!(opening("NACNST").air_ferry, None);
        assert_eq!(opening("YACNST").air_ferry, None);

        for yard in ["GACNST", "NACNST", "YACNST"] {
            let opening = opening(yard);
            for (name, role) in [
                ("CCOMAND", Role::AntiInfantry),
                ("CIVAN", Role::AntiArmor),
                ("PTROOP", Role::Siege),
            ] {
                let rows: Vec<_> = opening
                    .infantry
                    .iter()
                    .filter(|entry| entry.name == name)
                    .collect();
                assert_eq!(rows.len(), 1, "{yard} fields exactly one {name} row");
                assert_eq!(rows[0].role, role);
                assert_eq!(rows[0].needs, None, "{name} waits for no prerequisite");
            }
        }
    }

    /// The dogs' roster rows: each side's infantry table carries exactly one
    /// hound (`ADOG` Allied, `DOG` Soviet, `YDOG` Yuri) — filed as a plain
    /// AntiInfantry entry with no prerequisite, so the rotation fields them
    /// like any other specialist (and [`SuperAi::is_dog`] answers for
    /// exactly those three sections: the hook the wave dispatch uses to
    /// keep the kennel on its sentry ring instead of the ground marches,
    /// and the sentry limb to count it by).
    #[test]
    fn openings_field_one_dog_per_side_and_is_dog_answers_for_them() {
        let opening = |yard: &str| {
            OPENINGS
                .iter()
                .find(|opening| opening.construction_yard == yard)
                .expect("each side's opening")
        };
        for (yard, dog) in [("GACNST", "ADOG"), ("NACNST", "DOG"), ("YACNST", "YDOG")] {
            let opening = opening(yard);
            let hounds: Vec<_> = opening
                .infantry
                .iter()
                .filter(|entry| SuperAi::is_dog(entry.name))
                .collect();
            assert_eq!(hounds.len(), 1, "{yard} fields exactly one hound");
            let entry = hounds[0];
            assert_eq!(entry.name, dog);
            assert_eq!(entry.role, Role::AntiInfantry);
            assert_eq!(entry.needs, None, "the hound waits for no prerequisite");
        }
        for not_a_dog in ["E1", "E2", "INIT", "GGI", "TANY", "BRUTE", "ADOGG", "DOGGE"] {
            assert!(
                !SuperAi::is_dog(not_a_dog),
                "{not_a_dog} is not a hound section"
            );
        }
    }

    /// The sortie's target pick: the nearest reachable candidate wins, an
    /// unreachable nearer one is passed over, nothing reachable anchors the
    /// fleet, and a distance tie keeps the stable order.
    #[test]
    fn nearest_connected_target_prefers_the_nearest_reachable() {
        let candidates = [
            (1u64, (20i16, 20i16)),
            (2u64, (14i16, 14i16)),
            (3u64, (60i16, 60i16)),
        ];
        // Everything reachable: the nearest wins.
        assert_eq!(
            nearest_connected_target(&candidates, (15, 15), &|_| true),
            Some(2)
        );
        // The nearer cells across the water line are skipped: the far
        // reachable one takes the shot.
        assert_eq!(
            nearest_connected_target(&candidates, (15, 15), &|cell| cell == (60, 60)),
            Some(3)
        );
        // Nothing reachable: no sortie.
        assert_eq!(
            nearest_connected_target(&candidates, (15, 15), &|_| false),
            None
        );
        // A distance tie keeps the candidates' own (stable) order.
        let tied = [(7u64, (0i16, 0i16)), (9u64, (2i16, 2i16))];
        assert_eq!(nearest_connected_target(&tied, (1, 1), &|_| true), Some(7));
    }

    /// The defense extension's table rows: the wall ring's segment, the gap
    /// generator and the French Grand Cannon, one row per side. The limbs
    /// queue and place whatever these fields name, so a typo here would
    /// queue a rules section that does not exist — the asserts pin the
    /// retail pieces: GAWALL/NAWALL/[GAFWLL] are the only `Wall=yes`
    /// building sections, GAGAP's lab prerequisite is Allied-only, and
    /// GTGCAN's `RequiredHouses=French` keeps the cannon behind the limb's
    /// own country gate.
    #[test]
    fn openings_name_the_wall_gap_and_country_cannon_entries() {
        let opening = |yard: &str| {
            OPENINGS
                .iter()
                .find(|opening| opening.construction_yard == yard)
                .expect("each side's opening")
        };
        let allied = opening("GACNST");
        assert_eq!(allied.wall, Some("GAWALL"));
        assert_eq!(allied.gap_generator, Some("GAGAP"));
        assert_eq!(allied.country_cannon, Some("GTGCAN"));

        let soviet = opening("NACNST");
        assert_eq!(soviet.wall, Some("NAWALL"));
        assert_eq!(soviet.gap_generator, None, "GAGAP is Allied retail");
        assert_eq!(soviet.country_cannon, None, "GTGCAN is French-only");

        let yuri = opening("YACNST");
        assert_eq!(
            yuri.wall,
            Some("GAFWLL"),
            "the Citadel Wall — the [YAWALL] half of the section header is commented out"
        );
        assert_eq!(yuri.gap_generator, None);
        assert_eq!(yuri.country_cannon, None);
    }

    /// The engineer chains' table rows and pure rankings. The raid ride per
    /// side (the Allied IFV, the Soviet Flak Track — both already on their
    /// roster's anti-air line — and Yuri none, his only `Passengers=` hull
    /// being the ferry's amphibious transport), the capture priority's
    /// class order covering the neutral-tech table exactly once with the
    /// oil derrick leading, and the spy retarget's rank order: enemy
    /// superweapon, radar, power, then any standing structure.
    #[test]
    fn openings_name_the_engineer_ride_and_the_capture_and_spy_ranks_hold() {
        let opening = |yard: &str| {
            OPENINGS
                .iter()
                .find(|opening| opening.construction_yard == yard)
                .expect("each side's opening")
        };
        let allied = opening("GACNST");
        assert_eq!(allied.engineer_ride, Some("FV"));
        assert!(allied.vehicles.iter().any(|entry| entry.name == "FV"));

        let soviet = opening("NACNST");
        assert_eq!(soviet.engineer_ride, Some("HTK"));
        assert!(soviet.vehicles.iter().any(|entry| entry.name == "HTK"));

        let yuri = opening("YACNST");
        assert_eq!(
            yuri.engineer_ride,
            None,
            "Yuri fields no land transport; the ferry's hover hull is not a raid ride"
        );

        // The capture priority ranks every neutral-tech type exactly once,
        // the oil derrick's class leading, and nothing outside the table.
        let ranks: Vec<Option<usize>> = TECH_BUILDING_TYPES
            .iter()
            .map(|name| capture_priority_rank(name))
            .collect();
        assert!(
            ranks.iter().all(|rank| rank.is_some()),
            "every neutral-tech type ranks in the capture priority"
        );
        let mut ranks: Vec<usize> = ranks.into_iter().flatten().collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(
            ranks.len(),
            TECH_BUILDING_TYPES.len(),
            "every neutral-tech type ranks exactly once"
        );
        assert_eq!(ranks[0], 0, "the ranked classes start at the oil derrick");
        assert_eq!(
            capture_priority_rank("CAOILD"),
            Some(0),
            "the oil derrick's startup cash leads the capture order"
        );
        assert_eq!(
            capture_priority_rank("GACNST"),
            None,
            "a non-tech structure ranks nowhere"
        );

        // The spy's retarget order: the enemy's superweapon first, then the
        // radar, the power plants, and any standing structure last.
        assert_eq!(spy_infiltration_rank("GAWEAT"), 0);
        assert_eq!(spy_infiltration_rank("YAGNTC"), 0);
        assert_eq!(spy_infiltration_rank("NARADR"), 1);
        assert_eq!(spy_infiltration_rank("AMRADR"), 1);
        assert_eq!(spy_infiltration_rank("YAPOWR"), 2);
        assert_eq!(
            spy_infiltration_rank("GAWEAP"),
            3,
            "any standing enemy structure still outranks the neutral tour"
        );
    }

    /// The ring walk's geometry: a 1x1 foundation's ring is its eight
    /// neighbors in the fixed clockwise-from-top-left order, and a w x h
    /// foundation's ring is the expanded rectangle's border (2(w+2 + h+2) -
    /// 4 cells) — the order the wall placement walks, so a refused cell is
    /// never revisited ahead of a fresher one.
    #[test]
    fn wall_ring_walks_the_perimeter_clockwise_from_the_top_left() {
        assert_eq!(
            wall_ring_cells((0, 0, 0, 0)),
            vec![
                (-1, -1),
                (0, -1),
                (1, -1),
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0)
            ]
        );
        // A 3x2 foundation (bounds x 0..=2, y 0..=1): the expanded 5x4
        // rectangle's border is 14 cells, top-left corner first.
        let ring = wall_ring_cells((0, 0, 2, 1));
        assert_eq!(ring.len(), 14);
        assert_eq!(ring[0], (-1, -1));
        assert_eq!(ring[4], (3, -1));
        assert_eq!(ring[13], (-1, 0));
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

    /// The user's lobby shape end to end: several MAX (Unlimited) houses on
    /// one team, no starting units, drivers attached exactly as the app does
    /// (`MatchState::rebuild_super_players`: one per command-driven house,
    /// tiers read back from the houses, no second marking pass). A frozen
    /// opening here is the in-game "no action at all" report.
    #[test]
    #[ignore = "slow; retail; loads a match from the retail install"]
    fn unlimited_team_houses_open_without_starting_units() {
        use crate::map::entities::EntityCategory;
        use crate::skirmish_launch::{AiDifficulty, LaunchCountry, LaunchTeam};

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        let map = "XTerritor.MAP";
        let mut session = crate::headless_scenario::battle_session(
            map,
            &[
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
            ],
        );
        for opponent in &mut session.opponents {
            opponent.super_tier = Some(SuperAiDifficulty::Unlimited);
            opponent.team = LaunchTeam::Team(1);
        }
        session.options.unit_count = 0;
        let mut scenario =
            crate::headless_scenario::load_session(&ra2, map, 0x014B_BC60, session).unwrap();

        let mut drivers: Vec<SuperAi> = scenario
            .sim()
            .houses()
            .iter()
            .filter(|(_, state)| {
                state.command_driven() && !state.multiplay_passive() && !state.is_human()
            })
            .map(|(&house, state)| {
                SuperAi::new(house, state.super_tier().unwrap_or(SuperAiDifficulty::Brutal))
            })
            .collect();
        assert!(
            drivers.len() >= 2,
            "the launch command-drove {} houses",
            drivers.len()
        );

        let mut tally = std::collections::BTreeMap::<String, u32>::new();
        for tick in 0..1500u32 {
            let mut orders = Vec::new();
            for ai in drivers.iter_mut() {
                orders.extend(ai.orders(&scenario.runtime, tick));
            }
            let label = orders.first().map_or_else(
                || "empty".to_string(),
                |env| {
                    format!("{:?}", env.payload)
                        .split(['{', '('])
                        .next()
                        .unwrap_or("payload")
                        .trim()
                        .to_string()
                },
            );
            *tally.entry(label).or_insert(0) += 1;
            scenario
                .runtime
                .advance_frame(
                    &orders,
                    crate::headless_scenario::SIM_TICK_MS,
                    crate::sim::world::TickLane::Ordinary,
                    crate::sim::world::FrameEffects::default(),
                )
                .expect("advance");
        }

        let yard_names: Vec<&str> = OPENINGS.iter().map(|o| o.construction_yard).collect();
        let mut census = String::new();
        let mut yards_total = 0usize;
        for ai in &drivers {
            let sim = scenario.sim();
            let rules = &scenario.runtime.resources.rules;
            // Which deploy gate refuses here: the Event9 slope gate, the
            // CanDeploySlashUnload tunnel-mouth exclusion, the WeaponsFactory
            // cell, the engine's own admission, the queued mission, and the
            // lifted foundation test the conversion runs. One census line
            // names the refusing gate outright.
            let gate_probe = ai
                .deployable_mcv(sim, rules)
                .and_then(|mcv| {
                    let entity = sim.substrate.entities.get(mcv)?;
                    let mcv_cell = (entity.position.rx(), entity.position.ry());
                    let mut gates = format!(
                        "mission={:?} navcom={} ",
                        entity.passive_acquire_mission(),
                        entity.navigation.nav_com().is_some()
                    );
                    gates.push_str(&format!(
                        "engine_admission={} ",
                        sim.can_deploy_slash_unload(mcv, rules)
                    ));
                    if let Some(terrain) = sim.resolved_terrain.as_ref() {
                        if !entity.on_bridge() {
                            let cells =
                                crate::map::resolved_terrain::NativeCellQuery::canonical(terrain);
                            let [x, y] =
                                crate::sim::movement::ground_pose::position_world_xy(
                                    &entity.position,
                                );
                            let slope_blocked = cells.ground_fields(cells.lookup_world(x, y)).1
                                == 0
                                && crate::sim::movement::ground_pose::terrain_projection_differs(
                                    &cells, [x, y],
                                );
                            gates.push_str(&format!("slope_blocked={slope_blocked} "));
                        } else {
                            gates.push_str("on_bridge ");
                        }
                        gates.push_str(&format!(
                            "tube={:?} ",
                            terrain.is_tube_deploy_neighborhood((mcv_cell.0 as i16, mcv_cell.1 as i16))
                        ));
                    }
                    let weapons_factory = sim
                        .substrate
                        .occupancy
                        .first_building_on_layer(
                            mcv_cell.0,
                            mcv_cell.1,
                            crate::sim::movement::locomotor::MovementLayer::Ground,
                        )
                        .and_then(|building| sim.substrate.entities.get(building))
                        .and_then(|building| sim.object_type(building.type_ref(), rules))
                        .is_some_and(|kind| kind.weapons_factory);
                    gates.push_str(&format!("weapons_factory={weapons_factory} "));
                    // The lifted foundation test the conversion itself runs
                    // (`Deploy @ 0x00739422`): the same call the strategy's
                    // clear-check makes.
                    let yard_ty = rules
                        .object(sim.interner.resolve(entity.type_ref()))
                        .and_then(|ty| ty.deploys_into.as_deref())
                        .and_then(|name| rules.object(name));
                    let clear = yard_ty.is_some_and(|yard_ty| {
                        let lifted = crate::sim::build_site::LiftedUnit::read(sim, mcv);
                        crate::sim::build_site::can_place_building_at(
                            sim,
                            rules,
                            Some(&scenario.runtime.resources.overlay_registry),
                            yard_ty,
                            (mcv_cell.0 as i16 - 1, mcv_cell.1 as i16 - 1),
                            None,
                            None,
                            Some(&lifted),
                        )
                    });
                    gates.push_str(&format!("clear={clear}"));
                    Some(gates)
                })
                .unwrap_or_else(|| "no-mcv ".to_string());
            let yards = scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == ai.house
                        && entity.category() == EntityCategory::Structure
                        && yard_names.contains(&scenario.sim().interner.resolve(entity.type_ref()))
                })
                .count();
            let structures = scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == ai.house && entity.category() == EntityCategory::Structure
                })
                .count();
            let units = scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == ai.house && entity.category() == EntityCategory::Unit
                })
                .count();
            yards_total += yards;
            census.push_str(&format!(
                "{}: yards={yards} structures={structures} units={units} mcv={:?} [{gate_probe}]\n",
                scenario.sim().interner.resolve(ai.house),
                ai.deployable_mcv(scenario.sim(), &scenario.runtime.resources.rules),
            ));
        }
        assert!(
            yards_total >= drivers.len(),
            "every MAX house stood its yard\ncensus:\n{census}tally={tally:?}"
        );
    }

    /// AI-vs-AI late game, no human interference: the same six-Unlimited
    /// lobby driven far past the opening, census-first. This is the probe
    /// for the two live-play reports — "the AI charges with its base" (an
    /// expansion MCV whose walk aim lies deep in hostile territory) and
    /// "it stopped fielding units" (a vehicle queue that stopped ordering).
    /// Everything prints into the failure census; nothing is asserted yet.
    #[test]
    #[ignore = "slow; retail; loads a match and drives tens of thousands of frames"]
    fn unlimited_ai_vs_ai_late_game_census() {
        use crate::map::entities::EntityCategory;
        use crate::skirmish_launch::{AiDifficulty, LaunchCountry, LaunchTeam};

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        let map = "XTerritor.MAP";
        let mut session = crate::headless_scenario::battle_session(
            map,
            &[
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
                (LaunchCountry::Russia, AiDifficulty::Hard),
            ],
        );
        // The live lobby's shape: three MAX houses on team A, the rest on
        // team D, the human alone — AI-vs-AI warfare, not one allied bloc.
        for (index, opponent) in session.opponents.iter_mut().enumerate() {
            opponent.super_tier = Some(SuperAiDifficulty::Unlimited);
            opponent.team = if index < 3 {
                LaunchTeam::Team(1)
            } else {
                LaunchTeam::Team(2)
            };
        }
        session.options.unit_count = 0;
        let mut scenario =
            crate::headless_scenario::load_session(&ra2, map, 0x014B_BC60, session).unwrap();

        let mut drivers: Vec<SuperAi> = scenario
            .sim()
            .houses()
            .iter()
            .filter(|(_, state)| {
                state.command_driven() && !state.multiplay_passive() && !state.is_human()
            })
            .map(|(&house, state)| {
                SuperAi::new(house, state.super_tier().unwrap_or(SuperAiDifficulty::Brutal))
            })
            .collect();

        // The MCV type names, once: each opening's yard names what it
        // undeploys into.
        let mcv_names: Vec<String> = OPENINGS
            .iter()
            .filter_map(|o| {
                scenario
                    .runtime
                    .resources
                    .rules
                    .object(o.construction_yard)
                    .and_then(|ty| ty.undeploys_into.clone())
            })
            .collect();
        let mut produced: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u32>> =
            Default::default();
        let mut moved_mcv: std::collections::BTreeMap<String, (u64, u16, u16)> = Default::default();
        for tick in 0..20_000u32 {
            let mut orders = Vec::new();
            for ai in drivers.iter_mut() {
                orders.extend(ai.orders(&scenario.runtime, tick));
            }
            for envelope in &orders {
                match &envelope.payload {
                    crate::sim::command::Command::QueueProduction { type_id } => {
                        let owner = scenario.sim().interner.resolve(envelope.owner).to_string();
                        let name = scenario
                            .sim()
                            .interner
                            .resolve(*type_id)
                            .to_string();
                        *produced.entry(owner).or_default().entry(name).or_insert(0) += 1;
                    }
                    crate::sim::command::Command::Move { entity_id, target_rx, target_ry } => {
                        if let Some(entity) = scenario.sim().substrate.entities.get(*entity_id)
                            && entity.category() == EntityCategory::Unit
                            && mcv_names.iter().any(|m| {
                                m == scenario.sim().interner.resolve(entity.type_ref())
                            })
                        {
                            let owner = scenario.sim().interner.resolve(envelope.owner).to_string();
                            moved_mcv.insert(owner, (*entity_id, *target_rx, *target_ry));
                        }
                    }
                    _ => {}
                }
            }
            scenario
                .runtime
                .advance_frame(
                    &orders,
                    crate::headless_scenario::SIM_TICK_MS,
                    crate::sim::world::TickLane::Ordinary,
                    crate::sim::world::FrameEffects::default(),
                )
                .expect("advance");
        }

        // The census: per house, army size versus queue history, the wallet,
        // the expansion state, and every live MCV's distance from its home
        // yard — the "charging base" number.
        let yard_names: Vec<&str> = OPENINGS.iter().map(|o| o.construction_yard).collect();
        let mut census = String::new();
        for ai in &drivers {
            let sim = scenario.sim();
            let name = sim.interner.resolve(ai.house).to_string();
            let wallet = crate::sim::production::credits_for_owner(sim, &name);
            let home = sim
                .substrate
                .entities
                .iter_sorted()
                .find(|(_, e)| {
                    e.owner() == ai.house
                        && e.category() == EntityCategory::Structure
                        && yard_names.contains(&sim.interner.resolve(e.type_ref()))
                })
                .map(|(_, e)| (e.position.rx() as i16, e.position.ry() as i16));
            let tanks: usize = OPENINGS
                .iter()
                .map(|o| {
                    ai.count_units_of_type(sim, o.main_tank) + ai.count_units_of_type(sim, o.tier2_tank)
                })
                .sum();
            let units = sim
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, e)| e.owner() == ai.house && e.category() == EntityCategory::Unit)
                .count();
            let mcv_walks = moved_mcv.get(&name);
            let mut mcv_line = String::from("mcv_walks=no-orders ");
            if let Some(entity) = mcv_walks
                .and_then(|(id, _, _)| sim.substrate.entities.get(*id))
            {
                if let Some((hx, hy)) = home {
                    let dist = (entity.position.rx() as i16 - hx).abs()
                        .max(entity.position.ry() as i16 - hy).abs();
                    mcv_line = format!(
                        "live_mcv_pos=({},{}) dist_from_home={} ",
                        entity.position.rx(),
                        entity.position.ry(),
                        dist
                    );
                }
            }
            if let Some((_, tx, ty)) = mcv_walks
                && let Some((hx, hy)) = home
            {
                let dist = (*tx as i16 - hx).abs().max(*ty as i16 - hy).abs();
                mcv_line.push_str(&format!("last_walk_aim=({tx},{ty}) aim_dist={dist} "));
            }
            mcv_line.push_str(&format!(
                "expansion2_site={:?} abandoned={} yard_issued={} ",
                ai.expansion2_site, ai.expansion_abandoned, ai.expansion_yard_issued
            ));
            let queues: Vec<String> = produced
                .get(&name)
                .map(|per| per.iter().map(|(ty, n)| format!("{ty}x{n}")).collect())
                .unwrap_or_default();
            census.push_str(&format!(
                "{name}: wallet={wallet} units={units} tanks={tanks} {mcv_line}\n  queued: {}\n",
                queues.join(" ")
            ));
        }
        // Always print (probe), and keep a soft floor so total collapse
        // still fails the run.
        println!("== AI-vs-AI late-game census ==\n{census}");
        let any_units = drivers.iter().any(|ai| {
            scenario.sim().substrate.entities.iter_sorted().any(|(_, e)| {
                e.owner() == ai.house && e.category() == EntityCategory::Unit
            })
        });
        assert!(any_units, "some house still fields units\n{census}");
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
            /// The expansion pair's proof: a refinery that stood at the
            /// expansion distance from the home yard (phase 1), and the
            /// yard count crossing one (the expansion MCV's deploy,
            /// phase 2). Both are max, not end.
            remote_refinery: usize,
            yards: usize,
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
                seen.yards = seen.yards.max(structures(scenario, "GACNST"));
                // The remote refinery: any own refinery standing at the
                // expansion distance from the first standing yard (the
                // home anchor, mirroring `structure_center`'s pick).
                if let Some((hx, hy)) = scenario
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
                {
                    let remote =
                        scenario
                            .sim()
                            .substrate
                            .entities
                            .iter_sorted()
                            .any(|(_, entity)| {
                                entity.owner() == human
                                    && entity.category()
                                        == crate::map::entities::EntityCategory::Structure
                                    && scenario.sim().interner.resolve(entity.type_ref())
                                        == "GAREFN"
                                    && (entity.position.rx() as i16 - hx)
                                        .abs()
                                        .max((entity.position.ry() as i16 - hy).abs())
                                        >= EXPANSION_MIN_DISTANCE
                            });
                    if remote {
                        seen.remote_refinery = 1;
                    }
                }
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
                    && seen.remote_refinery >= 1
                    && seen.yards >= 2
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
        // Whether the end-state world admits an MCV at all: separates a
        // starved commission (admission Yes, no queue order in the tally)
        // from a refused one (the verdict names the gate).
        let probe_amcv = {
            let runtime = &scenario.runtime;
            let rules = &runtime.resources.rules;
            rules.object("AMCV").map(|ty| {
                format!(
                    "{:?}",
                    crate::sim::production::can_build(
                        &runtime.simulation,
                        rules,
                        human,
                        ty,
                        false,
                        false
                    )
                )
            })
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
        let mcv_probe = scenario
            .sim()
            .substrate
            .entities
            .iter_sorted()
            .find(|(_, entity)| {
                entity.owner() == human
                    && entity.category() == crate::map::entities::EntityCategory::Unit
                    && scenario.sim().interner.resolve(entity.type_ref()) == "AMCV"
            })
            .map(|(_, entity)| {
                format!(
                    "mission={:?} cell=({},{}) pos=({},{})",
                    entity.passive_acquire_mission(),
                    entity.position.rx(),
                    entity.position.ry(),
                    entity.position.sub_x(),
                    entity.position.sub_y(),
                )
            });
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
                && seen.fortress_seats >= 1
                && seen.remote_refinery >= 1
                && seen.yards >= 2,
            "growth slice incomplete at tick {tick}: GAWEAT={} GAWEAP={} refineries={} \
             power={} defenses={} FV={} GGI={} TANY={} jets={} credits={} miners={} MTNK={} \
             SREF={} E1={} queue=[{}] AMRADR={} GAAIRC={} commission_ready={:?} \
             ready=[{}] yard={} yard_last_tick={} mcv={} \
             barracks_end={} factories_end={} amradr_end={} docks_max={} docks_end={} \
             probe_orca={} trace_len={} amradr_opened={} revealed_pct={} \
             fortress={} fortress_seats={} remote_refinery={} yards={} probe_amcv={:?} \
             ai_state={:?} mcv_probe={:?} tally={:?}",
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
            seen.remote_refinery,
            seen.yards,
            probe_amcv,
            (
                ai.expansion_site,
                ai.remote_refinery_stood,
                ai.expansion_yard_issued,
                ai.expansion_abandoned,
                ai.expansion2_site,
                ai.expansion_deploy_target,
                ai.expansion_deploy_attempts,
                ai.next_expansion_deploy_tick,
                ai.deploy_attempts,
                ai.next_deploy_tick
            ),
            mcv_probe,
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
        assert_eq!(
            structures(&scenario, "GACSPH"),
            0,
            "the second superweapon building never stood either"
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
        // The second super's chain is the toggle's too: the chronosphere
        // (the Allied second super's `SuperWeapon=`) is likewise never
        // granted while the lobby keeps the specials off.
        let sphere = scenario
            .sim()
            .interner
            .get("ChronoSphereSpecial")
            .and_then(|sw_type| {
                scenario
                    .sim()
                    .super_weapons()
                    .get(&human)
                    .and_then(|house_sws| house_sws.get(&sw_type))
            });
        assert!(
            sphere.is_none_or(|sw| !sw.is_active()),
            "the chronosphere special is never granted while the toggle is off"
        );
    }

    /// The ladder's second superweapon rung: with the lobby toggle on, the
    /// same climb that stands the weather controller goes on to stand the
    /// side's second super (`GACSPH`, retail
    /// `SuperWeapon=ChronoSphereSpecial`) — a rung the walk opens only
    /// once the first super has stood (its once-per-game latch).
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn ladder_builds_the_second_superweapon_after_the_first_stood() {
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

        // Leg one runs to the first super standing; leg two keeps the
        // same climb running until the second super joins it (`drive_until`
        // restarts its tick budget per call, so the leg budgets are each
        // sized for the full climb — the wide Soviet-style budget covers
        // the two 5000-credit monuments the wallet must bank in turn).
        drive_until(&mut scenario, &mut ai, 34000, |scenario| {
            structures(scenario, "GAWEAT") >= 1
        });
        assert!(
            structures(&scenario, "GAWEAT") >= 1,
            "the first superweapon stood"
        );
        drive_until(&mut scenario, &mut ai, 68000, |scenario| {
            structures(scenario, "GACSPH") >= 1
        });
        assert!(
            structures(&scenario, "GACSPH") >= 1,
            "the second superweapon stood after the first"
        );
    }

    // ---- broken-bridge gating and the engineer repair chain ----
    //
    // The retail Pacific scene the sim's own bridge witnesses use
    // (`bridge_test_evidence`): an actual CABHUT beside an actual high
    // bridge over water, so the zone rows really split at a real breach.

    /// The Pacific bridge scene's fixed landmarks (the evidence's own
    /// coordinates): the repair hut, the deck receiver the damage enters
    /// at, and the shore cell the witnesses' engineer starts from.
    const PACIFIC_HUT: (u16, u16) = (72, 93);
    const PACIFIC_BRIDGE: (u16, u16) = (81, 95);
    const PACIFIC_SHORE: (u16, u16) = (70, 95);

    /// A loaded Pacific.mmx one-human Battle (no roaming units), the AI
    /// house bound to the session's local house, and a hostile house at war
    /// with it for the far-shore target.
    fn pacific_bridge_battle() -> (
        crate::headless_scenario::HeadlessScenario,
        InternedId,
        InternedId,
    ) {
        use crate::skirmish_launch::LaunchCountry;

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        let map = "Pacific.mmx";
        let mut session = crate::headless_scenario::battle_session(map, &[]);
        session.options.unit_count = 0;
        let mut scenario =
            crate::headless_scenario::load_session(&ra2, map, 0x0B21_D6E5, session).unwrap();
        let ai = scenario
            .sim()
            .houses
            .iter()
            .find(|(_, house)| house.is_controlled_by_human(true))
            .map(|(&id, _)| id)
            .expect("the local house");
        let enemy = scenario.add_human_house("WaveTargetHouse", LaunchCountry::Russia);
        (scenario, ai, enemy)
    }

    /// [`pacific_bridge_battle`] with the local house's lobby country
    /// chosen by the caller: the Soviet engineer's commission needs a
    /// Soviet house, whose opening resolves through a standing NACNST.
    fn pacific_bridge_battle_as(
        country: crate::skirmish_launch::LaunchCountry,
    ) -> (
        crate::headless_scenario::HeadlessScenario,
        InternedId,
        InternedId,
    ) {
        use crate::skirmish_launch::LaunchCountry;

        let ra2 = std::path::PathBuf::from(
            std::env::var("RA2_DIR").expect("set RA2_DIR to the retail RA2/YR install directory"),
        );
        let map = "Pacific.mmx";
        let mut session = crate::headless_scenario::battle_session(map, &[]);
        session.local_slot_mut().country = country;
        session.options.unit_count = 0;
        let mut scenario =
            crate::headless_scenario::load_session(&ra2, map, 0x0B21_D6E5, session).unwrap();
        let ai = scenario
            .sim()
            .houses
            .iter()
            .find(|(_, house)| house.is_controlled_by_human(true))
            .map(|(&id, _)| id)
            .expect("the local house");
        let enemy = scenario.add_human_house("WaveTargetHouse", LaunchCountry::Russia);
        (scenario, ai, enemy)
    }

    /// Spawn `type_name` for `owner` at the first legal cell scanning a
    /// small spiral outward from `at` (a spawn refuses occupied or illegal
    /// ground), so the tests do not hard-code Pacific's clear cells.
    fn spawn_near(
        scenario: &mut crate::headless_scenario::HeadlessScenario,
        type_name: &str,
        owner: InternedId,
        at: (u16, u16),
    ) -> u64 {
        let owner_name = scenario.sim().interner.resolve(owner).to_owned();
        let runtime = &mut scenario.runtime;
        for radius in 0..10i32 {
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx.abs() != radius && dy.abs() != radius {
                        continue;
                    }
                    let (x, y) = (i32::from(at.0) + dx, i32::from(at.1) + dy);
                    if x < 0 || y < 0 {
                        continue;
                    }
                    if let Some(id) = runtime.simulation.spawn_object_with_overlay_registry(
                        type_name,
                        &owner_name,
                        x as u16,
                        y as u16,
                        0,
                        &runtime.resources.rules,
                        &runtime.resources.overlay_registry,
                    ) {
                        return id;
                    }
                }
            }
        }
        panic!("no spawn cell for {type_name} near {at:?}");
    }

    /// Collapse the scene's high bridge the way the sim's own bridge
    /// witnesses do: ordinary HE damage past the bridge strength at the
    /// deck receiver until the structural facts clear.
    fn collapse_pacific_bridge(
        scenario: &mut crate::headless_scenario::HeadlessScenario,
    ) {
        use crate::sim::bridge_state::BridgeDamageEvent;

        let runtime = &mut scenario.runtime;
        let damage = runtime.resources.rules.bridge_rules.strength + 1;
        let mut collapsed = false;
        for _ in 0..8 {
            let event = BridgeDamageEvent {
                rx: PACIFIC_BRIDGE.0,
                ry: PACIFIC_BRIDGE.1,
                damage,
                warhead_ref: runtime.simulation.intern("HE"),
                impact_z_leptons: 624,
                is_ion_cannon: false,
            };
            collapsed |= crate::sim::world::bridge_orchestrator::apply_bridge_damage_events_with_overlay_registry(
                &mut runtime.simulation,
                &runtime.resources.rules,
                &[event],
                Some(&runtime.resources.overlay_registry),
                crate::sim::world::FrameEffects::default(),
            );
            if !runtime
                .simulation
                .resolved_terrain
                .as_ref()
                .unwrap()
                .cell(PACIFIC_BRIDGE.0, PACIFIC_BRIDGE.1)
                .unwrap()
                .bridge_facts
                .has_structural_bridge()
            {
                break;
            }
        }
        assert!(
            collapsed,
            "the retail bridge must collapse under ordinary HE damage"
        );
        assert!(
            !runtime
                .simulation
                .resolved_terrain
                .as_ref()
                .unwrap()
                .cell(PACIFIC_BRIDGE.0, PACIFIC_BRIDGE.1)
                .unwrap()
                .bridge_facts
                .has_structural_bridge()
        );
    }

    /// The engine's own Normal-row zone id for `cell` — the oracle the
    /// gating assertions read (the helper under test wraps exactly this
    /// query, so the oracle stays the sim's, not the wrapper's).
    fn ground_zone(
        scenario: &crate::headless_scenario::HeadlessScenario,
        cell: (u16, u16),
    ) -> u32 {
        let sim = scenario.sim();
        sim.zone_grid
            .as_ref()
            .expect("the retail map's zone grid")
            .get_zone_id_native(
                sim.resolved_terrain.as_ref().expect("map terrain"),
                cell,
                crate::rules::locomotor_type::MovementZone::Normal,
                false,
            )
            .expect("the retail map's zone topology")
    }

    /// The map CABHUT entity nearest the evidence's hut coordinate, with
    /// its cell.
    fn pacific_hut(
        scenario: &crate::headless_scenario::HeadlessScenario,
    ) -> (u64, (u16, u16)) {
        scenario
            .sim()
            .substrate
            .entities
            .iter_sorted()
            .find(|(_, entity)| {
                scenario.sim().interner.resolve(entity.type_ref()) == super::BRIDGE_HUT_TYPE
            })
            .map(|(id, entity)| {
                (
                    id,
                    (entity.position.rx(), entity.position.ry()),
                )
            })
            .expect("the retail map's bridge repair hut")
    }

    /// The loaded span's ground endpoints: the zone grid's own bridge
    /// record whose endpoints sit nearest the evidence's deck receiver.
    fn span_endpoints(
        scenario: &crate::headless_scenario::HeadlessScenario,
    ) -> ((u16, u16), (u16, u16)) {
        let record = scenario
            .sim()
            .zone_grid
            .as_ref()
            .expect("the retail map's zone grid")
            .bridge_records()
            .iter()
            .copied()
            .min_by_key(|record| {
                chebyshev(record.endpoint_a, PACIFIC_BRIDGE)
                    .min(chebyshev(record.endpoint_b, PACIFIC_BRIDGE))
            })
            .expect("the retail span's bridge record");
        (record.endpoint_a, record.endpoint_b)
    }

    /// The far-shore span end: the endpoint further from the witnesses'
    /// home-shore start.
    fn far_shore(scenario: &crate::headless_scenario::HeadlessScenario) -> (u16, u16) {
        let (a, b) = span_endpoints(scenario);
        if chebyshev(a, PACIFIC_SHORE) > chebyshev(b, PACIFIC_SHORE) {
            a
        } else {
            b
        }
    }

    /// The broken-bridge gate at the wave dispatch: with the span intact a
    /// gathered wave attack-moves onto the far-shore structure; after a
    /// programmatic collapse the same wave arms, finds no connected target
    /// in its whole rotation, and holds every unit home — no order lands in
    /// the far zone. The internal `launched` list proves the episode really
    /// armed and ended clean (it is cleared at arm and nothing re-enters).
    #[test]
    #[ignore = "slow; retail; loads a retail map with a real bridge"]
    fn broken_bridge_gate_holds_the_wave_at_the_breach() {
        let (mut scenario, ai_house, enemy_house) = pacific_bridge_battle();
        let far = far_shore(&scenario);
        let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
        scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

        // Home base on the hut's shore, the hostile structure on the far
        // shore, and a full wave's worth of standing tanks at home. An
        // under-construction enemy war factory (constructor-held limbo,
        // native-semantics position (0,0)) is also on the map: factories
        // are the wave's prioritized class, so an unfiltered enumeration
        // would aim the wave at the origin.
        spawn_near(&mut scenario, "GACNST", ai_house, (64, 92));
        spawn_near(&mut scenario, "GAPOWR", enemy_house, far);
        {
            let enemy_name = scenario.sim().interner.resolve(enemy_house).to_owned();
            let runtime = &mut scenario.runtime;
            runtime
                .simulation
                .spawn_object_limbo_at_height(
                    "NAWEAP",
                    &enemy_name,
                    far.0,
                    far.1,
                    0,
                    0,
                    &runtime.resources.rules,
                )
                .expect("the under-construction factory");
        }
        for _ in 0..super::ATTACK_WAVE_SIZE + 1 {
            spawn_near(&mut scenario, "MTNK", ai_house, (64, 94));
        }
        let far_target = far_target_cell(&scenario);

        // Intact span: the wave launches across it. Every destination is
        // the far structure's own cell — never the limbo factory's (0,0).
        let mut intact_far_moves = 0;
        for tick in 0..24u32 {
            for order in ai.orders(&scenario.runtime, tick) {
                if let crate::sim::command::Command::AttackMove {
                    destination: Some(crate::sim::combat::TargetKind::Cell(rx, ry)),
                    ..
                } = order.payload
                {
                    assert_ne!(
                        (rx, ry),
                        (0, 0),
                        "a constructor-held factory is not a target (tick {tick})"
                    );
                    assert_eq!(
                        (rx, ry),
                        far_target,
                        "an intact span sends the wave at the far structure"
                    );
                    intact_far_moves += 1;
                }
            }
        }
        assert!(
            intact_far_moves > 0,
            "the wave must launch across the intact span (positive control)"
        );
        assert!(
            !ai.launched.is_empty(),
            "the intact-span episode dispatched its units"
        );

        collapse_pacific_bridge(&mut scenario);
        let far_zone = ground_zone(&scenario, far_target);
        assert_ne!(
            far_zone,
            ground_zone(&scenario, PACIFIC_SHORE),
            "the breach must split the zone rows (setup oracle)"
        );

        // Broken span: several wave-retry windows' worth of ticks — long
        // enough for the marched tanks to idle out at the breach and rejoin
        // the standing set. No order of any kind may land in the cut-off
        // zone, and an armed episode must end with nothing launched: the
        // pinned `launched` list entered a call non-empty (the intact-span
        // dispatches), the arm clears it, and no unreachable destination
        // refills it — that one-tick transition is observable from outside.
        let mut guard_posts = 0;
        let mut armed_and_blocked = false;
        for tick in 24..8 * super::WAVE_RETRY_TICKS {
            let launched_before = !ai.launched.is_empty();
            for order in ai.orders(&scenario.runtime, tick) {
                let destination = match &order.payload {
                    crate::sim::command::Command::AttackMove { destination, .. }
                    | crate::sim::command::Command::Guard {
                        target: destination,
                        ..
                    } => *destination,
                    crate::sim::command::Command::Move {
                        target_rx,
                        target_ry,
                        ..
                    } => Some(crate::sim::combat::TargetKind::Cell(*target_rx, *target_ry)),
                    _ => None,
                };
                if let Some(crate::sim::combat::TargetKind::Cell(rx, ry)) = destination {
                    assert_ne!(
                        (rx, ry),
                        (0, 0),
                        "no order may target the limbo origin (tick {tick})"
                    );
                    assert_ne!(
                        ground_zone(&scenario, (rx, ry)),
                        far_zone,
                        "no order may land in the zone the breach cut off (tick {tick}, \
                         order {:?} at ({rx},{ry}))",
                        order.payload
                    );
                    if matches!(order.payload, crate::sim::command::Command::Guard { .. }) {
                        guard_posts += 1;
                    }
                }
            }
            if launched_before && !ai.wave_launching && ai.launched.is_empty() {
                armed_and_blocked = true;
            }
        }
        assert!(
            armed_and_blocked,
            "the wave must re-arm after the breach and end with no dispatch"
        );
        assert_eq!(
            ai.launched,
            Vec::<u64>::new(),
            "no unit may launch into the cut-off zone after the breach"
        );
        assert!(
            guard_posts > 0,
            "the held-back units must still take their home guard posts"
        );
    }

    /// The far-shore structure's cell (the wave's one target).
    fn far_target_cell(
        scenario: &crate::headless_scenario::HeadlessScenario,
    ) -> (u16, u16) {
        scenario
            .sim()
            .substrate
            .entities
            .iter_sorted()
            .find(|(_, entity)| {
                entity.owner() != crate::sim::intern::InternedId::default()
                    && entity.category() == crate::map::entities::EntityCategory::Structure
                    && scenario.sim().interner.resolve(entity.type_ref()) == "GAPOWR"
            })
            .map(|(_, entity)| (entity.position.rx(), entity.position.ry()))
            .expect("the far-shore hostile structure")
    }

    /// The full repair chain: after the breach the limb latches the hut and
    /// sends the idle engineer with the engine's capture click; the walk
    /// repairs the span (the hut stops wanting repair, the zone rows fuse
    /// again), the consumed engineer clears the tracker, and the wave —
    /// which held at the breach the whole walk — attack-moves onto the
    /// far-shore structure again, with no re-issued capture in between.
    #[test]
    #[ignore = "slow; retail; loads a retail map and drives a real engineer walk"]
    fn engineer_repairs_the_breach_and_the_wave_crosses() {
        let (mut scenario, ai_house, enemy_house) = pacific_bridge_battle();
        let far = far_shore(&scenario);
        let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
        scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

        spawn_near(&mut scenario, "GACNST", ai_house, (64, 92));
        spawn_near(&mut scenario, "GAPOWR", enemy_house, far);
        for _ in 0..super::ATTACK_WAVE_SIZE + 1 {
            spawn_near(&mut scenario, "MTNK", ai_house, (64, 94));
        }
        collapse_pacific_bridge(&mut scenario);
        let engineer = spawn_near(&mut scenario, "ENGINEER", ai_house, PACIFIC_SHORE);
        let (hut_id, hut_cell) = pacific_hut(&scenario);
        assert!(
            crate::sim::world::bridge_orchestrator::bridge_hut_can_repair(
                scenario.sim(),
                hut_cell
            ),
            "setup: the collapsed span wants repair"
        );

        // The limb's first survey runs on the first tick: the capture order
        // is due within a handful of ticks, addressed to our engineer at
        // the map's own hut.
        let mut capture = None;
        for tick in 0..10u32 {
            for order in ai.orders(&scenario.runtime, tick) {
                if let crate::sim::command::Command::CaptureBuilding {
                    engineer_id,
                    target_building_id,
                } = order.payload
                {
                    capture = Some((engineer_id, target_building_id));
                }
            }
        }
        assert_eq!(
            capture,
            Some((engineer, hut_id)),
            "the limb must send the idle engineer to the broken hut (tracker {:?})",
            ai.bridge_hut
        );

        // Drive the real walk: the engineer enters the hut, the repair runs
        // and republishes. The budget mirrors the sim witnesses' own walk
        // bound with headroom.
        let mut repaired = false;
        for tick in 10..4000u32 {
            let orders = ai.orders(&scenario.runtime, tick);
            scenario
                .runtime
                .advance_frame(
                    &orders,
                    crate::headless_scenario::SIM_TICK_MS,
                    crate::sim::world::TickLane::Ordinary,
                    crate::sim::world::FrameEffects::default(),
                )
                .expect("simulation frame failed");
            if !crate::sim::world::bridge_orchestrator::bridge_hut_can_repair(
                scenario.sim(),
                hut_cell,
            ) {
                repaired = true;
                break;
            }
        }
        assert!(
            repaired,
            "the dispatched engineer must rebuild the span within the budget"
        );
        assert_eq!(
            ground_zone(&scenario, far_target_cell(&scenario)),
            ground_zone(&scenario, PACIFIC_SHORE),
            "the repair must fuse the zone rows again"
        );
        assert!(
            scenario
                .sim()
                .substrate
                .entities
                .get(engineer)
                .is_none_or(|entity| !entity.lifecycle.object_alive()),
            "the engine consumes the engineer on hut entry"
        );

        // With the span back the held wave crosses: the next episode sends
        // attack-moves at the far structure, and the repair limb stays
        // quiet — no re-issued capture, the tracker cleared with the
        // consumed engineer.
        let mut far_moves = 0;
        let mut recaptures = 0;
        for tick in 4001..4001 + 3 * super::WAVE_RETRY_TICKS {
            for order in ai.orders(&scenario.runtime, tick) {
                match order.payload {
                    crate::sim::command::Command::CaptureBuilding { .. } => recaptures += 1,
                    crate::sim::command::Command::AttackMove {
                        destination: Some(crate::sim::combat::TargetKind::Cell(rx, ry)),
                        ..
                    } => {
                        assert_eq!(
                            ground_zone(&scenario, (rx, ry)),
                            ground_zone(&scenario, far_target_cell(&scenario)),
                            "the resumed wave crosses to the far zone"
                        );
                        far_moves += 1;
                    }
                    _ => {}
                }
            }
            if far_moves > 0 {
                break;
            }
        }
        assert!(
            far_moves > 0,
            "the wave must resume onto the far structure once the span stands"
        );
        assert_eq!(
            recaptures, 0,
            "a standing span must not re-arm the repair limb"
        );
    }

    /// The Soviet engineer's commission (the bridge-limb's engineer fix):
    /// the three sides' engineers are three rules sections, so a Soviet
    /// house that latches the broken hut must queue [SENGINEER]. The old
    /// shared "ENGINEER" name is `ForbiddenHouses`-refused in a Soviet
    /// base — the commission passed `can_build`, answered No, and the
    /// bridge stayed broken for the game.
    #[test]
    #[ignore = "slow; retail; loads a retail map"]
    fn soviet_bridge_limb_commissions_the_soviet_engineer() {
        let (mut scenario, ai_house, enemy_house) = pacific_bridge_battle_as(
            crate::skirmish_launch::LaunchCountry::Russia,
        );
        let far = far_shore(&scenario);
        let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
        scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

        // A Soviet yard and barracks at the witnesses' home shore, the
        // hostile structure beyond the breach, then the collapse.
        spawn_near(&mut scenario, "NACNST", ai_house, (64, 92));
        spawn_near(&mut scenario, "NAHAND", ai_house, (66, 92));
        spawn_near(&mut scenario, "GAPOWR", enemy_house, far);
        collapse_pacific_bridge(&mut scenario);

        // The commission rides the first survey: within a few orders
        // passes the infantry queue receives the Soviet engineer — and
        // never the Allied section name.
        let mut queued: Vec<String> = Vec::new();
        for tick in 0..10u32 {
            for order in ai.orders(&scenario.runtime, tick) {
                if let crate::sim::command::Command::QueueProduction { type_id } = order.payload {
                    queued.push(scenario.sim().interner.resolve(type_id).to_string());
                }
            }
        }
        assert!(
            queued.iter().any(|name| name == "SENGINEER"),
            "the Soviet house must commission SENGINEER for the hut: {queued:?}"
        );
        assert!(
            queued.iter().all(|name| name != "ENGINEER"),
            "the Allied section is ForbiddenHouses-refused in a Soviet base: {queued:?}"
        );
    }

    /// The helper's two answers on the real map: an intact span connects
    /// the two shores under the ground rows, the collapse cuts them, and
    /// the home shore itself stays connected throughout.
    #[test]
    #[ignore = "slow; retail; loads a retail map with a real bridge"]
    fn zones_connected_tracks_the_bridge_span() {
        let (mut scenario, _ai, _enemy) = pacific_bridge_battle();
        let home = PACIFIC_SHORE;

        // The far shore: the loaded span's own records name its two ground
        // endpoints; the one further from home is the far side.
        let record = scenario
            .sim()
            .zone_grid
            .as_ref()
            .expect("zone grid")
            .bridge_records()
            .iter()
            .min_by_key(|record| {
                std::cmp::min(
                    chebyshev(record.endpoint_a, PACIFIC_BRIDGE),
                    chebyshev(record.endpoint_b, PACIFIC_BRIDGE),
                )
            })
            .copied()
            .expect("the retail span's bridge record");
        let far = if chebyshev(record.endpoint_a, home) > chebyshev(record.endpoint_b, home) {
            record.endpoint_a
        } else {
            record.endpoint_b
        };

        assert!(
            super::zones_connected(
                scenario.sim(),
                crate::rules::locomotor_type::MovementZone::Normal,
                home,
                far,
            ),
            "an intact span connects the shores"
        );
        assert!(
            super::zones_connected(
                scenario.sim(),
                crate::rules::locomotor_type::MovementZone::Normal,
                home,
                PACIFIC_HUT,
            ),
            "the hut sits on the home shore"
        );

        collapse_pacific_bridge(&mut scenario);
        {
            let sim = scenario.sim();
            for record in sim.zone_grid.as_ref().unwrap().bridge_records() {
                eprintln!(
                    "PROBE record a={:?} b={:?} active={} high={}",
                    record.endpoint_a,
                    record.endpoint_b,
                    record.active,
                    record.is_high()
                );
            }
            for y in [70u16, 95, 120] {
                let mut row = String::new();
                for x in (60..126).step_by(4) {
                    let zone = sim
                        .zone_grid
                        .as_ref()
                        .unwrap()
                        .get_zone_id_native(
                            sim.resolved_terrain.as_ref().unwrap(),
                            (x, y),
                            crate::rules::locomotor_type::MovementZone::Normal,
                            false,
                        );
                    row.push_str(&format!("{x}:{:?} ", zone));
                }
                eprintln!("PROBE y={y} {row}");
            }
        }
        assert!(
            !super::zones_connected(
                scenario.sim(),
                crate::rules::locomotor_type::MovementZone::Normal,
                home,
                far,
            ),
            "the breach must cut the shores"
        );
        assert!(
            super::zones_connected(
                scenario.sim(),
                crate::rules::locomotor_type::MovementZone::Normal,
                home,
                PACIFIC_HUT,
            ),
            "the home shore stays connected behind the breach"
        );
    }

    /// Chebyshev distance between two cells, for the test's endpoint pick.
    fn chebyshev(a: (u16, u16), b: (u16, u16)) -> u32 {
        i32::from(a.0).abs_diff(i32::from(b.0))
            .max(i32::from(a.1).abs_diff(i32::from(b.1))) as u32
    }

    /// The MCV blocker-shove budget: an opening MCV jammed by its own idle
    /// unit gets at most [`SHOVE_ATTEMPTS`] shove Moves while it stands on
    /// one cell (the old loop re-armed on any movement and shoved all
    /// game), and past the cap the stuck window owns it — a bounded number
    /// of re-site walks, then silence. No yard stands, so the deploy branch
    /// is the only thing speaking.
    #[test]
    #[ignore = "slow; retail; loads a retail map"]
    fn jammed_mcv_shoves_bounded_then_resites_or_stops() {
        let (mut scenario, ai_house, _enemy) = pacific_bridge_battle();
        let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
        scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

        // The jam: one MCV, one idle own tank inside its foundation, no
        // yard anywhere (the deploy branch owns the whole command budget).
        // The anchor is the witnesses' known-land shore.
        let mcv = spawn_near(&mut scenario, "AMCV", ai_house, (68, 94));
        spawn_near(&mut scenario, "MTNK", ai_house, (68, 94));

        let mut shoves = 0;
        let mut mcv_moves = 0;
        let mut shoves_after_cap = 0;
        for tick in 0..8 * super::WAVE_RETRY_TICKS {
            for order in ai.orders(&scenario.runtime, tick) {
                match order.payload {
                    crate::sim::command::Command::Move {
                        entity_id,
                        target_rx,
                        target_ry,
                    } => {
                        assert_ne!(
                            (target_rx, target_ry),
                            (0, 0),
                            "no order targets the origin (tick {tick})"
                        );
                        if entity_id == mcv {
                            mcv_moves += 1;
                        } else {
                            shoves += 1;
                            if tick > super::SHOVE_ATTEMPTS * super::IDLE_REORDER_TICKS {
                                shoves_after_cap += 1;
                            }
                        }
                    }
                    crate::sim::command::Command::DeployMcv { entity_id } if entity_id == mcv => {
                        // a deploy draw means the site cleared; bounded either way
                    }
                    _ => {}
                }
            }
        }
        assert!(
            shoves <= super::SHOVE_ATTEMPTS,
            "one stuck MCV gets at most {} shoves per cell (saw {shoves})",
            super::SHOVE_ATTEMPTS
        );
        assert_eq!(
            shoves_after_cap, 0,
            "the shove cap must hold past the budget window"
        );
        assert!(
            mcv_moves <= 4,
            "the stuck window re-sites the MCV a bounded number of times (saw {mcv_moves})"
        );
    }

    /// TEMPORARY probe: which retail maps actually split a ground zone when
    /// a bridge span collapses.
    #[test]
    #[ignore = "temporary probe"]
    fn probe_zone_split_maps() {
        use crate::sim::bridge_state::BridgeDamageEvent;

        for map in [
            "XMP22S8.MAP",
            "Hills.mmx",
            "C3Y03MD.MAP",
            "XShrapnel.MAP",
            "XMP03T4.MAP",
        ] {
            let ra2 = std::path::PathBuf::from(std::env::var_os("RA2_DIR").unwrap());
            let Ok(mut scenario) = crate::headless_scenario::load(&ra2, map, 0x0B21_D6E5) else {
                eprintln!("PROBE {map}: LOAD FAILED");
                continue;
            };
            let rules = scenario.runtime.resources.rules.clone();
            let registry = scenario.runtime.resources.overlay_registry.clone();
            let records: Vec<_> = scenario
                .sim()
                .zone_grid
                .as_ref()
                .unwrap()
                .bridge_records()
                .to_vec();
            eprintln!("PROBE {map}: {} records", records.len());
            for record in &records {
                if !record.is_high() {
                    continue;
                }
                let (a, b) = (record.endpoint_a, record.endpoint_b);
                let mid = ((a.0 + b.0) / 2, (a.1 + b.1) / 2);
                let z = scenario
                    .sim()
                    .resolved_terrain
                    .as_ref()
                    .and_then(|t| t.cell(mid.0, mid.1))
                    .map(|c| i32::from(c.level) * 208)
                    .unwrap_or(416);
                let connected_before = super::zones_connected(
                    scenario.sim(),
                    crate::rules::locomotor_type::MovementZone::Normal,
                    a,
                    b,
                );
                let damage = rules.bridge_rules.strength + 1;
                let mut collapsed = false;
                'span: for _ in 0..10 {
                    for (dx, dy) in
                        [(0i32, 0i32), (1, 0), (0, 1), (-1, 0), (0, -1), (2, 0), (0, 2)]
                    {
                        let (rx, ry) = (
                            i32::from(mid.0) + dx,
                            i32::from(mid.1) + dy,
                        );
                        if rx < 0 || ry < 0 {
                            continue;
                        }
                        let event = BridgeDamageEvent {
                            rx: rx as u16,
                            ry: ry as u16,
                            damage,
                            warhead_ref: scenario.runtime.simulation.intern("HE"),
                            impact_z_leptons: z,
                            is_ion_cannon: false,
                        };
                        collapsed |= crate::sim::world::bridge_orchestrator::apply_bridge_damage_events_with_overlay_registry(
                            &mut scenario.runtime.simulation,
                            &rules,
                            &[event],
                            Some(&registry),
                            crate::sim::world::FrameEffects::default(),
                        );
                        let still = scenario
                            .sim()
                            .resolved_terrain
                            .as_ref()
                            .and_then(|t| t.cell(mid.0, mid.1))
                            .is_some_and(|c| c.bridge_facts.has_structural_bridge());
                        if collapsed && !still {
                            break 'span;
                        }
                    }
                }
                let connected_after = super::zones_connected(
                    scenario.sim(),
                    crate::rules::locomotor_type::MovementZone::Normal,
                    a,
                    b,
                );
                eprintln!(
                    "PROBE {map} span {a:?}-{b:?} mid={mid:?} z={z} collapsed={collapsed} \
                     before={connected_before} after={connected_after}"
                );
            }
        }
    }

    // ---- Layer 2: base growth and survival ----

    /// The expansion base grows (Layer 2 A/B): once a second construction
    /// yard stands, its cluster must fill in — at least two non-yard
    /// structures (the power plant, the refinery, the guns) inside its
    /// [`BASE_CLUSTER_RADIUS`] ring with at least one defense — instead of
    /// standing bare for the rest of the game (the observed "only grabs
    /// ore, builds nothing" expansion base).
    #[test]
    #[ignore = "slow; retail; loads a match and drives tens of thousands of frames"]
    fn expansion_grows_around_second_yard() {
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
        let yard_cells = |scenario: &crate::headless_scenario::HeadlessScenario| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && scenario.sim().interner.resolve(entity.type_ref()) == "GACNST"
                })
                .map(|(_, entity)| (entity.position.rx() as i16, entity.position.ry() as i16))
                .collect::<Vec<_>>()
        };

        // Phase 1: the second yard stands — the ladder arms the MCV
        // commission, the escort walks it out, the deploy lands.
        let tick = drive_until(&mut scenario, &mut ai, 42000, |scenario| {
            structures_of_type(scenario, "GACNST") >= 2
        });
        assert!(
            structures_of_type(&scenario, "GACNST") >= 2,
            "the expansion yard deployed (tick {tick}, yards {})",
            structures_of_type(&scenario, "GACNST")
        );
        // Phase 2: the growth limbs fill the expansion cluster. Manual
        // pacing keeps the AI's tick clock monotonic across the two drives
        // (`drive_until` restarts its own counter at zero).
        let mut tick = tick;
        let budget = tick + 12000;
        while tick < budget {
            let orders = ai.orders(&scenario.runtime, tick);
            scenario
                .runtime
                .advance_frame(
                    &orders,
                    crate::headless_scenario::SIM_TICK_MS,
                    crate::sim::world::TickLane::Ordinary,
                    crate::sim::world::FrameEffects::default(),
                )
                .expect("simulation frame failed");
            tick += 1;
        }

        // The expansion yard: the one furthest from the home yard (the
        // first in stable order), the AI's own pick mirrored.
        let yards = yard_cells(&scenario);
        let home = yards[0];
        let expansion = *yards
            .iter()
            .max_by_key(|cell| (cell.0 - home.0).abs().max(cell.1 - home.1).abs())
            .expect("two yards stood");
        let cluster = |names: &[&str]| {
            scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .filter(|(_, entity)| {
                    entity.owner() == human
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && names.contains(&scenario.sim().interner.resolve(entity.type_ref()))
                })
                .filter(|(_, entity)| {
                    (entity.position.rx() as i16 - expansion.0)
                        .abs()
                        .max((entity.position.ry() as i16 - expansion.1).abs())
                        <= super::BASE_CLUSTER_RADIUS
                })
                .count()
        };
        let non_yard = cluster(&[
            "GAPOWR",
            "GAREFN",
            "GAPILE",
            "GAWEAP",
            "GAAIRC",
            "AMRADR",
            "GATECH",
            "GADEPT",
            "GAPILL",
            "NASAM",
            "ATESLA",
        ]);
        let defenses_near = cluster(&["GAPILL", "NASAM", "ATESLA"]);
        assert!(
            non_yard >= 2 && defenses_near >= 1,
            "the expansion cluster stayed bare: expansion yard at {expansion:?}, non-yard \
             structures {non_yard}, defenses {defenses_near}, yards {yards:?}, tick {tick}, \
             ai_state={:?}",
            (
                ai.expansion_site,
                ai.remote_refinery_stood,
                ai.expansion_yard_issued,
                ai.expansion_abandoned,
                ai.expansion2_site,
                ai.placement_intent.map(|(ty, expansion)| (
                    expansion,
                    scenario.sim().interner.resolve(ty).to_string()
                )),
            ),
        );
    }

    /// The building-repair limb (Layer 2 C): a damaged refinery under the
    /// health threshold earns exactly one ToggleRepair activation — the
    /// command is a toggle, so the limb must never re-issue on its latched
    /// target and flip the paid repair back off.
    #[test]
    #[ignore = "slow; retail; loads a retail map"]
    fn repair_toggles_on_damaged_structure() {
        let (mut scenario, ai_house, _enemy) = pacific_bridge_battle();
        let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
        scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

        // A yard (the opening table resolves through it) and a refinery on
        // the witnesses' known-land shore; the launch options' 10000-credit
        // wallet clears the limb's bar. The refinery sits at half strength,
        // under the three-fifths threshold.
        spawn_near(&mut scenario, "GACNST", ai_house, (64, 92));
        let refinery = spawn_near(&mut scenario, "GAREFN", ai_house, (66, 92));
        {
            let runtime = &mut scenario.runtime;
            let strength = runtime
                .resources
                .rules
                .object("GAREFN")
                .expect("the retail refinery type")
                .strength;
            let entity = runtime
                .simulation
                .substrate
                .entities
                .get_mut(refinery)
                .expect("the spawned refinery");
            entity.health.set_for_test(strength / 2);
        }
        assert!(
            !scenario
                .sim()
                .substrate
                .entities
                .get(refinery)
                .expect("the spawned refinery")
                .repairing
                .is_on(),
            "setup: the damaged refinery repairs for nobody yet"
        );

        // One activation within the first survey; no re-issue — not on the
        // later surveys, not after the healed structure stops wanting one.
        let mut toggles = 0;
        for tick in 0..4 * super::REPAIR_SURVEY_TICKS {
            let orders = ai.orders(&scenario.runtime, tick);
            for order in &orders {
                if let crate::sim::command::Command::ToggleRepair { entity_id } = &order.payload {
                    assert_eq!(
                        *entity_id, refinery,
                        "the limb repairs the wounded economy structure, nothing else"
                    );
                    toggles += 1;
                }
            }
            scenario
                .runtime
                .advance_frame(
                    &orders,
                    crate::headless_scenario::SIM_TICK_MS,
                    crate::sim::world::TickLane::Ordinary,
                    crate::sim::world::FrameEffects::default(),
                )
                .expect("simulation frame failed");
        }
        assert_eq!(
            toggles, 1,
            "one activation, never flipped again while the repair byte reads on"
        );
        assert!(
            scenario
                .sim()
                .substrate
                .entities
                .get(refinery)
                .is_some_and(|entity| {
                    let strength = scenario
                        .runtime
                        .resources
                        .rules
                        .object("GAREFN")
                        .expect("the retail refinery type")
                        .strength;
                    entity.repairing.is_on() || entity.health.is_full(strength)
                }),
            "the activation left the refinery repairing (or repaired)"
        );
    }

    /// The Robot Tank fix (Layer 2 E): America's Robot Control Center rung
    /// queues once the battle lab stands, and a Robot Tank — the roster
    /// entry whose `needs` gate the center satisfies — rolls out. The
    /// commission used to be dead forever: the center named in no build
    /// list, the gate never cleared, the cameo lit all game.
    #[test]
    #[ignore = "slow; retail; loads a match and drives tens of thousands of frames"]
    fn robot_center_fields_robots() {
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

        let tick = drive_until(&mut scenario, &mut ai, 60000, |scenario| {
            structures_of_type(scenario, "GAROBO") >= 1 && units_of_type(scenario, "ROBO") >= 1
        });
        assert!(
            structures_of_type(&scenario, "GAROBO") >= 1,
            "the Robot Control Center stood (tick {tick}, battle lab {})",
            structures_of_type(&scenario, "GATECH")
        );
        assert!(
            units_of_type(&scenario, "ROBO") >= 1,
            "a Robot Tank rolled out once its control center stood (tick {tick})"
        );
    }

    /// The ore-field exclusivity (Layer 2 F): a candidate field with a
    /// standing refinery beside it — here a hostile one — loses the survey
    /// to the free ground beyond the exclusion radius, so neighboring
    /// bases stop latching the same field.
    #[test]
    #[ignore = "slow; retail; loads a retail map"]
    fn ore_field_exclusivity_prefers_the_free_field() {
        let (mut scenario, human) = retail_battle();
        let enemy_house = scenario
            .sim()
            .houses
            .iter()
            .find(|(_, house)| !house.is_controlled_by_human(true))
            .map(|(&id, _)| id)
            .expect("the battle's opponent");
        let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

        // No yard stands yet, so the launch MCV's cell anchors the survey
        // exactly as the latch does at game time.
        let mcv_cell = scenario
            .sim()
            .substrate
            .entities
            .iter_sorted()
            .find(|(_, entity)| {
                entity.owner() == human
                    && entity.category() == crate::map::entities::EntityCategory::Unit
                    && scenario.sim().interner.resolve(entity.type_ref()) == "AMCV"
            })
            .map(|(_, entity)| (entity.position.rx() as i16, entity.position.ry() as i16))
            .expect("the launch MCV");
        let survey = |scenario: &crate::headless_scenario::HeadlessScenario,
                      ai: &SuperAi,
                      from: (i16, i16)| {
            ai.best_ore_field(
                scenario.sim(),
                &scenario.runtime.resources.rules,
                &scenario.runtime.resources.overlay_registry,
                from,
                None,
                Some("GAREFN"),
            )
        };
        let first = survey(&scenario, &ai, mcv_cell)
            .expect("the retail battle map hosts a reachable far ore field");

        // A hostile refinery stands beside the winning field; the same
        // survey must now hand back different ground, out of the exclusion
        // radius — or no ground at all when the map has no second field.
        let neighbor = spawn_near(&mut scenario, "NAREFN", enemy_house, first);
        let neighbor_cell = scenario
            .sim()
            .substrate
            .entities
            .get(neighbor)
            .map(|entity| (entity.position.rx(), entity.position.ry()))
            .expect("the spawned neighbor refinery");
        let second = survey(&scenario, &ai, mcv_cell);
        let distance = |a: (u16, u16), b: (u16, u16)| {
            (i32::from(a.0) - i32::from(b.0))
                .abs()
                .max((i32::from(a.1) - i32::from(b.1)).abs()) as u32
        };
        assert!(
            second.is_none_or(|cell| cell != first
                && distance(cell, neighbor_cell) >= super::ORE_FIELD_EXCLUSION_RADIUS as u32),
            "the survey latched the contested field again: first {first:?}, second {second:?}, \
             neighbor refinery at {neighbor_cell:?}"
        );
        assert!(
            distance(first, neighbor_cell) < super::ORE_FIELD_EXCLUSION_RADIUS as u32,
            "setup: the neighbor refinery must contest the first pick (at {neighbor_cell:?})"
        );
    }

    // ---- Layer 3: combat & unit management ----

    /// The combat layer's fixtures and reads live in a nested module so the
    /// bridge fixtures above stay shared without re-declaring them.
    mod combat_layer_tests {
        use super::*;
        use super::super::{ATTACK_WAVE_SIZE, BOARD_MAX_ATTEMPTS, FutilityKind, IDLE_REORDER_TICKS, WAVE_RETRY_TICKS};

        use std::collections::BTreeMap;

        /// The futility backoff's own contract, on a bare AI: three same-
        /// kind orders at the same cell put the kind into backoff, the
        /// backoff holds, a moved aim restarts the streak, a different kind
        /// is independent, and an expired backoff re-arms.
        #[test]
        fn futility_helper_strikes_suppresses_and_expires() {
            let (_runtime, house) = runtime();
            let mut ai = SuperAi::new(house, SuperAiDifficulty::Fair);

            assert!(
                !ai.futile(1, FutilityKind::GuardPost, (5, 5), 0),
                "the first order of a streak goes out"
            );
            assert!(
                !ai.futile(1, FutilityKind::GuardPost, (5, 5), IDLE_REORDER_TICKS),
                "the second repeat goes out"
            );
            assert!(
                ai.futile(1, FutilityKind::GuardPost, (5, 5), 2 * IDLE_REORDER_TICKS),
                "the third repeat is suppressed"
            );
            assert!(
                ai.futility_suppressed(1, FutilityKind::GuardPost, 1000),
                "the backoff holds inside its window"
            );
            assert!(
                !ai.futility_suppressed(1, FutilityKind::Wave, 1000),
                "a different order kind is not suppressed by it"
            );
            assert!(
                !ai.futile(1, FutilityKind::GuardPost, (5, 6), 1499),
                "a moved aim restarts the streak"
            );
            assert!(!ai.futile(1, FutilityKind::GuardPost, (5, 6), 1600));
            assert!(
                ai.futile(1, FutilityKind::GuardPost, (5, 6), 1700),
                "the restarted streak suppresses again at three"
            );
            assert!(
                !ai.futile(1, FutilityKind::GuardPost, (5, 6), 3000),
                "an expired backoff re-arms the kind"
            );
        }

        /// The air wing's rearm cycle: a jet sorties, comes home, the dock
        /// cycle parks it on a Guard post with fresh ordnance — and the
        /// next strike window sends it out again (the old None|Sleep-only
        /// flight filter read a parked Guard jet as busy and the wing sat
        /// full-ammo forever). Every strike envelope carries a jet that
        /// actually holds ordnance at issue time.
        #[test]
        #[ignore = "slow; retail; loads a match and drives tens of thousands of frames"]
        fn air_wing_restrikes_after_rearm() {
            use crate::map::entities::EntityCategory;

            let (mut scenario, human) = retail_battle();
            let mut ai = SuperAi::new(human, SuperAiDifficulty::Fair);

            let mut strikes: Vec<(u32, u64)> = Vec::new();
            // Per jet: the tick it was first seen parked (Guard/AreaGuard,
            // full ammo) after having flown once.
            let mut parked_since: BTreeMap<u64, Option<u32>> = BTreeMap::new();
            let mut restrike = false;
            let mut tick = 0u32;
            while tick < 46000 && !restrike {
                let orders = ai.orders(&scenario.runtime, tick);
                for envelope in &orders {
                    if let crate::sim::command::Command::Attack { attacker_id, .. } =
                        envelope.payload
                    {
                        let sim = scenario.sim();
                        if let Some(entity) = sim.substrate.entities.get(attacker_id)
                            && entity.owner() == human
                            && entity.category() == EntityCategory::Aircraft
                        {
                            let dry = entity.ammo().is_some_and(|ammo| ammo.current() == 0);
                            assert!(
                                !dry,
                                "only armed jets fly (tick {tick}, jet {attacker_id})"
                            );
                            if parked_since
                                .get(&attacker_id)
                                .is_some_and(|parked| parked.is_some())
                            {
                                restrike = true;
                            }
                            strikes.push((tick, attacker_id));
                        }
                    }
                }
                scenario
                    .runtime
                    .advance_frame(
                        &orders,
                        crate::headless_scenario::SIM_TICK_MS,
                        crate::sim::world::TickLane::Ordinary,
                        crate::sim::world::FrameEffects::default(),
                    )
                    .expect("simulation frame failed");
                // Parked read: a jet that flew before, holding Guard/AreaGuard
                // with ordnance back.
                for (id, entity) in scenario.sim().substrate.entities.iter_sorted() {
                    if entity.owner() != human
                        || entity.category() != EntityCategory::Aircraft
                        || !strikes.iter().any(|&(_, jet)| jet == id)
                    {
                        continue;
                    }
                    let parked = matches!(
                        entity.passive_acquire_mission(),
                        crate::sim::mission::MissionType::Guard
                            | crate::sim::mission::MissionType::AreaGuard
                    ) && entity.ammo().is_none_or(|ammo| ammo.current() != 0);
                    let entry = parked_since.entry(id).or_insert(None);
                    if parked && entry.is_none() {
                        *entry = Some(tick);
                    }
                }
                tick += 1;
            }
            assert!(
                restrike,
                "a rearmed jet must fly again: strikes={strikes:?} parked={parked_since:?}"
            );
            assert!(
                strikes.len() >= 2,
                "the wing flew at least two sorties (saw {})",
                strikes.len()
            );
            assert!(
                strikes.iter().any(|&(_, jet)| {
                    parked_since
                        .get(&jet)
                        .is_some_and(|parked| parked.is_some())
                }),
                "the restriked jet was observed parked between its sorties"
            );
        }

        /// The hero stands apart: the gathered wave never takes Tanya (no
        /// AttackMove envelope may name her), and her own branch sends her
        /// sabotage run at the reachable enemy structure instead — the
        /// PlantC4 envelope the engine's C4 path accepts.
        #[test]
        #[ignore = "slow; retail; loads a retail map with real terrain zones"]
        fn wave_spares_hero_and_tanya_plants_c4() {
            let (mut scenario, ai_house, enemy_house) = pacific_bridge_battle();
            let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
            scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

            spawn_near(&mut scenario, "GACNST", ai_house, (64, 92));
            let far = far_shore(&scenario);
            spawn_near(&mut scenario, "GAPOWR", enemy_house, far);
            let tany = spawn_near(&mut scenario, "TANY", ai_house, (66, 90));
            for _ in 0..ATTACK_WAVE_SIZE {
                spawn_near(&mut scenario, "MTNK", ai_house, (64, 94));
            }
            let enemy_structure = scenario
                .sim()
                .substrate
                .entities
                .iter_sorted()
                .find(|(_, entity)| {
                    entity.owner() == enemy_house
                        && entity.category() == crate::map::entities::EntityCategory::Structure
                        && scenario.sim().interner.resolve(entity.type_ref()) == "GAPOWR"
                })
                .map(|(id, _)| id)
                .expect("the hostile structure");

            let mut hero_wave_moves = 0usize;
            let mut wave_moves = 0usize;
            let mut plant: Option<(u64, u64)> = None;
            for tick in 0..3 * WAVE_RETRY_TICKS + 600 {
                for order in ai.orders(&scenario.runtime, tick) {
                    match order.payload {
                        crate::sim::command::Command::AttackMove { entity_id, .. } => {
                            if entity_id == tany {
                                hero_wave_moves += 1;
                            } else {
                                wave_moves += 1;
                            }
                        }
                        crate::sim::command::Command::PlantC4 {
                            attacker_id,
                            target_building_id,
                        } => {
                            if attacker_id == tany {
                                plant = Some((attacker_id, target_building_id));
                            }
                        }
                        _ => {}
                    }
                }
            }
            assert!(
                wave_moves > 0,
                "positive control: the gathered wave marched"
            );
            assert_eq!(
                hero_wave_moves, 0,
                "the hero never rides the generic wave"
            );
            assert_eq!(
                plant,
                Some((tany, enemy_structure)),
                "Tanya's own branch sent her sabotage run"
            );
        }

        /// The fortress boards a mix — anti-armor first, then anti-infantry
        /// — with every boarder reserved out of the wave pool and the
        /// fortress itself held home while boarding runs; a refused pair
        /// stops at two Enters instead of retrying forever.
        #[test]
        #[ignore = "slow; retail; loads a retail map with real terrain zones"]
        fn fortress_boards_a_mix_and_stops_retrying() {
            use crate::sim::mission::MissionType;

            let (mut scenario, ai_house, _enemy) = pacific_bridge_battle();
            let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
            scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

            spawn_near(&mut scenario, "GACNST", ai_house, (64, 92));
            let fortress = spawn_near(&mut scenario, "BFRT", ai_house, (66, 92));
            let ggi = spawn_near(&mut scenario, "GGI", ai_house, (67, 93));
            let snipe = spawn_near(&mut scenario, "SNIPE", ai_house, (67, 94));

            let opening = ai
                .opening(scenario.sim())
                .expect("the Allied opening resolves through the standing yard");
            let mut enters: BTreeMap<u64, u32> = BTreeMap::new();
            let mut boarded_reserve_checked = false;
            for tick in 0..1000u32 {
                for order in ai.orders(&scenario.runtime, tick) {
                    if let crate::sim::command::Command::MegaMission {
                        entity_id,
                        mission: MissionType::Enter,
                        ..
                    } = order.payload
                    {
                        *enters.entry(entity_id).or_insert(0) += 1;
                    }
                }
                // While boarding is pending the reserved cargo and the open
                // fortress stand out of the wave pool.
                if !ai.boarders.is_empty() && !boarded_reserve_checked {
                    let standing = ai.standing_combat_units(scenario.sim(), opening);
                    assert!(
                        !standing.contains(&fortress),
                        "the fortress with open seats stays home while boarding runs"
                    );
                    assert!(
                        standing.iter().all(|unit| !ai.boarders.contains(unit)),
                        "reserved boarders are not wave members"
                    );
                    boarded_reserve_checked = true;
                }
            }
            assert!(
                enters.contains_key(&ggi) && enters.contains_key(&snipe),
                "both cargo classes got their boarding call: {enters:?}"
            );
            assert!(
                enters.values().all(|&count| count <= BOARD_MAX_ATTEMPTS),
                "no pair retries past the budget: {enters:?}"
            );
            assert_eq!(
                enters.get(&ggi),
                Some(&BOARD_MAX_ATTEMPTS),
                "the anti-armor cargo boarded first and stopped at its budget"
            );
        }

        /// Siege-role units bombard from standoff: the wave's destination
        /// for a V3 is a cell short of the target along the home line, not
        /// the target cell the rest of the wave walks into.
        #[test]
        #[ignore = "slow; retail; loads a retail map with real terrain zones"]
        fn siege_units_hold_standoff() {
            use crate::sim::combat::TargetKind;

            let (mut scenario, ai_house, enemy_house) = pacific_bridge_battle();
            let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
            scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

            spawn_near(&mut scenario, "NACNST", ai_house, (64, 92));
            let far = far_shore(&scenario);
            spawn_near(&mut scenario, "GAPOWR", enemy_house, far);
            let v3 = spawn_near(&mut scenario, "V3", ai_house, (66, 92));
            for _ in 0..ATTACK_WAVE_SIZE {
                spawn_near(&mut scenario, "HTNK", ai_house, (64, 94));
            }
            let target_cell = far_target_cell(&scenario);

            let mut v3_destinations: Vec<(u16, u16)> = Vec::new();
            let mut tank_destinations: Vec<(u16, u16)> = Vec::new();
            for tick in 0..3 * WAVE_RETRY_TICKS {
                for order in ai.orders(&scenario.runtime, tick) {
                    if let crate::sim::command::Command::AttackMove {
                        entity_id,
                        destination: Some(TargetKind::Cell(rx, ry)),
                        ..
                    } = order.payload
                    {
                        if entity_id == v3 {
                            v3_destinations.push((rx, ry));
                        } else {
                            tank_destinations.push((rx, ry));
                        }
                    }
                }
            }
            assert!(
                !tank_destinations.is_empty(),
                "positive control: the wave marched on the target"
            );
            assert!(
                tank_destinations
                    .iter()
                    .any(|&cell| cell == target_cell),
                "the line units walk all the way in: {tank_destinations:?}"
            );
            assert!(
                !v3_destinations.is_empty(),
                "the siege unit joined the wave"
            );
            for &cell in &v3_destinations {
                let distance = chebyshev(cell, target_cell);
                assert_ne!(cell, target_cell, "the V3 never aims the target cell");
                assert!(
                    (9..=12).contains(&distance),
                    "the V3 holds 10-12 cells short of the target (saw {distance} at {cell:?})"
                );
            }
        }

        /// The Terror Drone hunts the enemy's economy: with a harvester on
        /// the map the drone's own order is the march at its cell, and the
        /// drone stands reserved out of the generic wave while the raid is
        /// live.
        #[test]
        #[ignore = "slow; retail; loads a retail map with real terrain zones"]
        fn drones_hunt_harvesters() {
            use crate::sim::combat::TargetKind;

            let (mut scenario, ai_house, enemy_house) = pacific_bridge_battle();
            let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
            scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

            spawn_near(&mut scenario, "NACNST", ai_house, (64, 92));
            let drone = spawn_near(&mut scenario, "DRON", ai_house, (64, 94));
            let harv_cell = {
                let id = spawn_near(&mut scenario, "HARV", enemy_house, (60, 90));
                let entity = scenario
                    .sim()
                    .substrate
                    .entities
                    .get(id)
                    .expect("the enemy harvester");
                (entity.position.rx(), entity.position.ry())
            };

            let mut drone_moves: Vec<(u16, u16)> = Vec::new();
            for tick in 0..2 * WAVE_RETRY_TICKS {
                for order in ai.orders(&scenario.runtime, tick) {
                    if let crate::sim::command::Command::AttackMove {
                        entity_id,
                        destination: Some(TargetKind::Cell(rx, ry)),
                        ..
                    } = order.payload
                        && entity_id == drone
                    {
                        drone_moves.push((rx, ry));
                    }
                }
            }
            assert!(
                !drone_moves.is_empty(),
                "the drone took its hunt order"
            );
            assert!(
                drone_moves.iter().any(|&cell| cell == harv_cell),
                "the drone marches at the harvester's cell: {drone_moves:?} vs {harv_cell:?}"
            );
            assert!(
                ai.raiders.contains(&drone),
                "the hunting drone is reserved out of the wave pool"
            );
            let opening = ai
                .opening(scenario.sim())
                .expect("the Soviet opening resolves through the standing yard");
            assert!(
                !ai.standing_combat_units(scenario.sim(), opening).contains(&drone),
                "a live raid keeps the drone out of the wave pool"
            );
        }

        /// The guard posts stop churning: with no intruder and no frames,
        /// the same idle unit is re-picked every reorder window — and the
        /// futility backoff stops the third identical Guard for a unit that
        /// has not moved.
        #[test]
        #[ignore = "slow; retail; loads a retail map with real terrain zones"]
        fn guard_posts_stop_churning() {
            let (mut scenario, ai_house, _enemy) = pacific_bridge_battle();
            let mut ai = SuperAi::new(ai_house, SuperAiDifficulty::Fair);
            scenario.apply_super_ai_tier(ai_house, SuperAiDifficulty::Fair);

            spawn_near(&mut scenario, "GACNST", ai_house, (64, 92));
            for _ in 0..4 {
                spawn_near(&mut scenario, "MTNK", ai_house, (64, 94));
            }

            let mut guards: BTreeMap<u64, u32> = BTreeMap::new();
            // Nine reorder windows: the third identical post would land near
            // the third window and its backoff runs 1200 ticks — the whole
            // drive stays inside the suppressed span.
            for tick in 0..9 * IDLE_REORDER_TICKS {
                for order in ai.orders(&scenario.runtime, tick) {
                    if let crate::sim::command::Command::Guard { entity_id, .. } = order.payload {
                        *guards.entry(entity_id).or_insert(0) += 1;
                    }
                }
            }
            assert!(
                guards.values().any(|&count| count > 0),
                "positive control: the units took their posts: {guards:?}"
            );
            assert!(
                guards.values().all(|&count| count <= 2),
                "a standing unit stops at two identical Guard posts (futility): {guards:?}"
            );
        }
    }
}
