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
//! hostile bases' mass center. Scouting, army mix beyond the first tanks and
//! micro are later milestones; the difficulty tiers still change nothing
//! (bonuses land with them).

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

/// How many combat units the army must count before the offensive starts
/// sending waves: production has to outpace attrition first.
const ATTACK_WAVE_SIZE: usize = 12;

/// How many standing combat units stay home whatever the offensive does: a
/// base emptied of defenders loses to the first counterattack that walks
/// past the marching wave.
const GUARD_RESERVE: usize = 4;

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
    /// The side's miner, if the war factory can build it (Yuri's slave miner
    /// is not a plain vehicle and stays out of the opening).
    harvester: Option<&'static str>,
    /// The side's first battle tank (Grizzly, Rhino, Lasher).
    main_tank: &'static str,
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
        harvester: Some("CMIN"),
        main_tank: "MTNK",
        basic_infantry: "E1",
    },
    Opening {
        construction_yard: "NACNST",
        power: "NAPOWR",
        refinery: "NAREFN",
        barracks: "NAHAND",
        war_factory: "NAWEAP",
        harvester: Some("HARV"),
        main_tank: "HTNK",
        basic_infantry: "E2",
    },
    Opening {
        construction_yard: "YACNST",
        power: "YAPOWR",
        refinery: "YAREFN",
        barracks: "YABRCK",
        war_factory: "YAWEAP",
        harvester: None,
        main_tank: "LTNK",
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
}

impl SuperAi {
    /// Bind the AI to `house` at `difficulty`.
    pub fn new(house: InternedId, difficulty: SuperAiDifficulty) -> Self {
        Self {
            house,
            difficulty,
            next_deploy_tick: 0,
            deploy_attempts: 0,
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
    /// else march a gathered group on the enemy center; else post idle units
    /// on an Area Guard), keep the building queue walking the opening, hold
    /// the war factory's rally on the construction yard, keep the vehicle
    /// queue on miners-then-tanks, keep the barracks queue on the side's
    /// basic soldier. See the module docs for the determinism contract.
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
        // nearest intruder inside the base perimeter. Otherwise, once the
        // army has grown to a full group its standing units (idle or on
        // their guard post) march on the enemy base — attack-move, so the
        // wave fights whatever it meets — and while the group is still
        // forming newcomers hold an Area Guard post on the yard. Orders
        // issue one unit per tick; an engaged, marching or posted unit
        // leaves the idle set, so no spam follows, and a wave that dies
        // reforms on its own once production refills the ranks.
        if let Some(center) = self.structure_center(sim, opening.construction_yard) {
            let idle = self.idle_combat_units(sim, opening);
            if let Some(&attacker) = idle.first()
                && let Some(target) = self.nearest_intruder(sim, center)
            {
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
            if self.combat_strength(sim, opening) >= ATTACK_WAVE_SIZE
                && standing.len() > GUARD_RESERVE
                && let Some(enemy_center) = self.enemy_center(sim)
                && let Some(&unit) = standing.first()
            {
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::AttackMove {
                        entity_id: unit,
                        target: None,
                        destination: Some(crate::sim::combat::TargetKind::Cell(
                            enemy_center.0 as u16,
                            enemy_center.1 as u16,
                        )),
                    },
                )];
            }
            if let Some(&unit) = idle.first() {
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::Guard {
                        entity_id: unit,
                        target: Some(crate::sim::combat::TargetKind::Cell(
                            center.0 as u16,
                            center.1 as u16,
                        )),
                    },
                )];
            }
        }

        // Walk the opening while the building queue idles.
        let queue = crate::sim::production::queue_view_for_owner(sim, rules, &owner);
        let building_busy = queue.iter().any(|item| {
            matches!(
                item.queue_category,
                crate::sim::production::ProductionCategory::Building
                    | crate::sim::production::ProductionCategory::Defense
            )
        });
        if self
            .structure_center(sim, opening.construction_yard)
            .is_some()
            && !building_busy
        {
            for name in [
                opening.power,
                opening.refinery,
                opening.barracks,
                opening.war_factory,
                opening.refinery,
            ] {
                if self.owns_structure_of_type(sim, name) {
                    continue;
                }
                if let Some(ty) = rules.object(name)
                    && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
                    // Read-only lookup: every retail type name is interned at
                    // rules load; a miss would only skip this candidate.
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

        // Keep the war factory's rally on the construction yard so its output
        // gathers in the base instead of jamming the factory exit. The
        // command archives the cell per factory (`SetRallyPoint 0x00443860`),
        // so one correction whenever the archived cell differs suffices.
        if let Some(center) = self.structure_center(sim, opening.construction_yard)
            && let Some((factory_id, _)) = self
                .own_structures(sim)
                .into_iter()
                .find(|(_, name)| name == opening.war_factory)
            && sim
                .substrate
                .entities
                .get(factory_id)
                .is_some_and(|factory| {
                    factory.rally_cell() != Some((center.0 as u16, center.1 as u16))
                })
        {
            return vec![CommandEnvelope::new(
                self.house,
                at,
                Command::SetRally {
                    rx: center.0 as u16,
                    ry: center.1 as u16,
                    producer_ids: vec![factory_id],
                },
            )];
        }

        // The war factory stands: keep its queue busy — the economy first
        // (miners up to the target), then the side's main battle tank without
        // pause. A queue entered while short on credits simply waits for the
        // money, as a native production event does.
        let vehicle_busy = queue
            .iter()
            .any(|item| item.queue_category == crate::sim::production::ProductionCategory::Vehicle);
        let economy_first = opening
            .harvester
            .is_some_and(|miner| self.count_units_of_type(sim, miner) < HARVESTER_TARGET);
        let vehicle_want = if economy_first {
            opening.harvester
        } else {
            Some(opening.main_tank)
        };
        if !vehicle_busy
            && self.owns_structure_of_type(sim, opening.war_factory)
            && let Some(name) = vehicle_want
            && let Some(ty) = rules.object(name)
            && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
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

    /// The house's living combat units of the opening's army types (tanks
    /// and basic soldiers), whatever their mission.
    fn combat_strength(&self, sim: &crate::sim::world::Simulation, opening: &Opening) -> usize {
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
                        name == opening.main_tank || name == opening.basic_infantry
                    }
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
                        name == opening.main_tank || name == opening.basic_infantry
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
                        name == opening.main_tank || name == opening.basic_infantry
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

    /// The nearest hostile entity inside `BASE_DEFENSE_RADIUS` cells of
    /// `center`, ties broken by the substrate's stable order. Passive houses
    /// (observers, civilians) do not count as intruders.
    fn nearest_intruder(
        &self,
        sim: &crate::sim::world::Simulation,
        center: (i16, i16),
    ) -> Option<u64> {
        let limit = i32::from(BASE_DEFENSE_RADIUS) * i32::from(BASE_DEFENSE_RADIUS);
        let mut best: Option<(i32, u64)> = None;
        for (id, entity) in sim.substrate.entities.iter_sorted() {
            let owner = entity.owner();
            if owner == self.house
                || !sim
                    .houses
                    .iter()
                    .any(|(&house_id, house)| house_id == owner && !house.multiplay_passive())
            {
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

    /// The mass center of the hostile houses' structures — the march target a
    /// gathered wave attack-moves on — or of their mobile forces when no
    /// hostile building stands. Sums over the entity set, so the result does
    /// not depend on iteration order.
    fn enemy_center(&self, sim: &crate::sim::world::Simulation) -> Option<(i16, i16)> {
        let hostile = |owner: InternedId| {
            owner != self.house
                && sim
                    .houses
                    .iter()
                    .any(|(&house_id, house)| house_id == owner && !house.multiplay_passive())
        };
        let mut center = None;
        for want_structure in [true, false] {
            let mut sum_x = 0i64;
            let mut sum_y = 0i64;
            let mut count = 0u64;
            for (_, entity) in sim.substrate.entities.iter_sorted() {
                let is_structure =
                    entity.category() == crate::map::entities::EntityCategory::Structure;
                if is_structure != want_structure || !hostile(entity.owner()) {
                    continue;
                }
                sum_x += i64::from(entity.position.rx());
                sum_y += i64::from(entity.position.ry());
                count += 1;
            }
            if count > 0 {
                center = Some(((sum_x / count as i64) as i16, (sum_y / count as i64) as i16));
                break;
            }
        }
        center
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

        let tick = drive_until(&mut scenario, &mut ai, 15000, |scenario| {
            combat(scenario) >= 8 && marching(scenario)
        });
        assert!(
            combat(&scenario) >= 8 && marching(&scenario),
            "a full group formed and marched (tick {tick}, combat {})",
            combat(&scenario)
        );
    }
}
