//! The Super AI: a VERA-native decision layer that plays one house through
//! the same [`CommandEnvelope`] stream a human client sends.
//!
//! This is a fork-original feature (not a gamemd port): nothing here traces a
//! native body, and the tiers below describe VERA's own design goals.
//!
//! **Determinism contract.** For an identical simulation view and an identical
//! `tick`, [`SuperAi::orders`] must produce the identical command sequence on
//! every platform and run: the decision layer is a pure function of its
//! arguments plus its own pinned state. Later milestones that need randomness
//! must draw from an RNG the instance seeds itself; reading the system clock,
//! global process state or any other ambient source is forbidden. Feeding the
//! returned envelopes to the simulation never mutates the runtime, so the
//! caller can re-derive the same batch at will.
//!
//! **M0 scope.** This module is the wiring skeleton only: the difficulty
//! ladder and the per-frame entry point exist, and every tier currently emits
//! zero commands. Strategies, economy bonuses and the build-speed knob land in
//! later milestones.

use crate::sim::command::CommandEnvelope;
use crate::sim::intern::InternedId;
use crate::sim::runtime::SimRuntime;

/// The ladder the Super AI plays at.
///
/// The tiers describe the target behaviour, not what M0 does; see the module
/// docs for the milestone split.
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
    /// minimal simulation-side change; M0 changes nothing.
    Super,
}

/// One Super AI player bound to the house it commands.
pub struct SuperAi {
    /// The house whose commands this player issues; the same identity a
    /// [`CommandEnvelope`]'s `owner` carries.
    house: InternedId,
    difficulty: SuperAiDifficulty,
}

impl SuperAi {
    /// Bind the AI to `house` at `difficulty`.
    pub fn new(house: InternedId, difficulty: SuperAiDifficulty) -> Self {
        Self { house, difficulty }
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
    /// M0: always empty. The skeleton only wires the pipeline; see the module
    /// docs for the determinism contract the later strategies must keep.
    pub fn orders(&mut self, runtime: &SimRuntime, tick: u32) -> Vec<CommandEnvelope> {
        let _ = (runtime, tick);
        Vec::new()
    }
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
    fn every_tier_emits_no_commands_on_every_tick() {
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
                    "{difficulty:?} must emit nothing at tick {tick}"
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
}
