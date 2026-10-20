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
//! once the war factory stands. Scouting, army mix, defense and micro are
//! later milestones; the difficulty tiers still change nothing (bonuses land
//! with them).

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
}

const OPENINGS: [Opening; 3] = [
    Opening {
        construction_yard: "GACNST",
        power: "GAPOWR",
        refinery: "GAREFN",
        barracks: "GAPILE",
        war_factory: "GAWEAP",
        harvester: Some("CMIN"),
    },
    Opening {
        construction_yard: "NACNST",
        power: "NAPOWR",
        refinery: "NAREFN",
        barracks: "NAHAND",
        war_factory: "NAWEAP",
        harvester: Some("HARV"),
    },
    Opening {
        construction_yard: "YACNST",
        power: "YAPOWR",
        refinery: "YAREFN",
        barracks: "YABRCK",
        war_factory: "YAWEAP",
        harvester: None,
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
    /// place a ready building, keep the building queue walking the opening,
    /// keep the vehicle queue building harvesters. See the module docs for
    /// the determinism contract.
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

            // The opening stands: keep the vehicle queue building miners.
            let vehicle_busy = queue.iter().any(|item| {
                item.queue_category == crate::sim::production::ProductionCategory::Vehicle
            });
            if !vehicle_busy
                && let Some(harvester) = opening.harvester
                && self.owns_structure_of_type(sim, opening.war_factory)
                && let Some(ty) = rules.object(harvester)
                && crate::sim::production::cameo_draw_state(sim, rules, self.house, ty).enabled
                && let Some(type_id) = sim.interner.get(harvester)
            {
                return vec![CommandEnvelope::new(
                    self.house,
                    at,
                    Command::QueueProduction { type_id },
                )];
            }
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

    /// The M1 opening on a retail Battle: the Super AI drives the local
    /// house from its launch MCV and must own a construction yard, a power
    /// plant and a refinery well inside the tick budget.
    #[test]
    #[ignore = "slow; retail; loads a match and drives thousands of frames"]
    fn fair_opening_deploys_and_builds_the_first_structures() {
        use crate::sim::world::{FrameEffects, TickLane};
        use crate::skirmish_launch::{AiDifficulty, LaunchCountry};

        let Ok(ra2) = std::env::var("RA2_DIR") else {
            return;
        };
        // A four-start map: fewer start waypoints than houses would seed the
        // extra starts at random, which the lobby never allows.
        let map = "XMP03T4.MAP";
        let mut session = crate::headless_scenario::battle_session(
            map,
            &[(LaunchCountry::Russia, AiDifficulty::Easy)],
        );
        session.local_slot_mut().country = LaunchCountry::America;
        session.options.unit_count = 0;
        let mut scenario = crate::headless_scenario::load_session(
            &std::path::PathBuf::from(ra2),
            map,
            0x00C0_FFEE,
            session,
        )
        .unwrap();
        let human = scenario
            .sim()
            .houses
            .iter()
            .find(|(_, house)| house.is_controlled_by_human(true))
            .map(|(&id, _)| id)
            .expect("the local house");
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

        let mut tick = 0u32;
        while tick < 6000
            && !(owns(&scenario, "GACNST")
                && owns(&scenario, "GAPOWR")
                && owns(&scenario, "GAREFN"))
        {
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
}
