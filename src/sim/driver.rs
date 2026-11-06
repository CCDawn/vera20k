//! Fork-only driver policy: the per-house driver-support payload
//! (`docs/superai.md`).
//!
//! The Super-AI tiers are four points in one policy space, so a
//! command-driven house carries its launch treatment as a [`DriverPolicy`]
//! instead of a bare tier mark. That lets an external driver plugin launch
//! with combinations beyond the four tiers while the launch kernel, the
//! per-tick credit loop and the driver rebuild all read the same fields.
//! The knobs reproduce existing behavior only:
//!
//! - `native_hard_grant` — the native Hard AI's opening grant
//!   ([`crate::sim::scenario_bootstrap::native_ai_opening_grant`], the
//!   retail `[General] MultiplayerAICM` Hard row via
//!   `ScenarioClass__Post_Map_Init @ 0x00686A52`).
//! - `credit_drip` — a recurring credit bonus (the tiers drip +1000 per
//!   600 ticks).
//! - `build_time_scale` — the production build-time scale (the tiers use
//!   `0.5f32`, the fork's one documented simulation-side exception,
//!   `HouseState::set_native_build_time_scale`).
//! - `wallet_floor` — a per-tick wallet top-up (the tiers floor at
//!   `UNLIMITED_WALLET_FLOOR`).
//!
//! [`DriverPolicy::from`] maps a tier onto its policy and
//! [`DriverPolicy::infer_tier`] infers the tier back (strongest knob wins:
//! floor → Unlimited, scale → Super, drip → Brutal, nothing → Fair), which
//! is also the driver rebuild heuristic's semantics.

use crate::sim::intern::InternedId;
use crate::sim::runtime::SimRuntime;
use crate::util::native_x87::NativeF32Bits;
use serde::{Deserialize, Serialize};

/// The ladder the Super AI plays at.
///
/// The tiers describe the target behaviour, not what the current milestone
/// does; see the `super_ai` module docs for the milestone split. The type
/// lives here, beside the policy it maps onto, so the engine can carry a
/// tier in launch slots and saved houses without reaching into the AI
/// module; `super_ai` re-exports it unchanged.
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

/// A recurring credit bonus: `amount` every `interval_ticks` ticks. The
/// tiers' drip is +1000 per 600 ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditDrip {
    pub amount: i32,
    pub interval_ticks: u32,
}

/// The driver-support treatment a command-driven house carries
/// (`docs/superai.md`). All knobs off ([`Self::neutral`]) is the treatment-
/// free Fair policy; every tier is one [`SuperAiDifficulty`] mapping.
///
/// Serialized as the policy struct. Text payloads written before the policy
/// stored the bare [`SuperAiDifficulty`] tier deserialize into the equivalent
/// policy; the snapshot codec's bincode layout is not self-describing, so it
/// reads the policy struct alone and its layout digest already rejects
/// pre-policy saves wholesale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DriverPolicy {
    /// Open with the native Hard AI's credit grant, whatever the house's
    /// own difficulty (the tier kernel's Brutal-and-up opening treatment).
    pub native_hard_grant: bool,
    /// Recurring credit bonus, applied by [`Self::apply_credit_support`].
    pub credit_drip: Option<CreditDrip>,
    /// Production build-time scale (`HouseState::set_native_build_time_scale`).
    pub build_time_scale: Option<NativeF32Bits>,
    /// Per-tick wallet top-up (`Self::apply_credit_support`); the tiers use
    /// [`UNLIMITED_WALLET_FLOOR`].
    pub wallet_floor: Option<i32>,
}

impl DriverPolicy {
    /// Every knob off: the treatment-free Fair policy.
    pub const fn neutral() -> Self {
        Self {
            native_hard_grant: false,
            credit_drip: None,
            build_time_scale: None,
            wallet_floor: None,
        }
    }

    /// The tier this policy reads back as: the strongest knob decides
    /// (floor → Unlimited, scale → Super, drip → Brutal, nothing → Fair).
    /// Every tier-launched house infers its launch tier exactly, so the
    /// driver rebuild keeps its semantics.
    pub const fn infer_tier(&self) -> SuperAiDifficulty {
        if self.wallet_floor.is_some() {
            SuperAiDifficulty::Unlimited
        } else if self.build_time_scale.is_some() {
            SuperAiDifficulty::Super
        } else if self.credit_drip.is_some() {
            SuperAiDifficulty::Brutal
        } else {
            SuperAiDifficulty::Fair
        }
    }

    /// This policy's per-tick credit support for `house` at sim `tick`:
    /// the wallet floor tops the balance up to the floor on every tick, the
    /// drip grants its amount on interval ticks (skipping tick 0, where the
    /// opening grant already ran). The floor wins over the drip, mirroring
    /// the tiers: Unlimited tops up and drips nothing; Fair is inert. The
    /// launch's per-tick loop (the app's sim_tick and the sim-bench super
    /// scenario) applies this through [`SimRuntime::grant_house_credits`].
    pub fn apply_credit_support(&self, runtime: &mut SimRuntime, house: InternedId, tick: u32) {
        if let Some(floor) = self.wallet_floor {
            let owner = runtime.simulation.interner().resolve(house).to_string();
            let wallet = crate::sim::production::credits_for_owner(&runtime.simulation, &owner);
            if wallet < floor {
                runtime.grant_house_credits(house, floor - wallet);
            }
        } else if let Some(drip) = self.credit_drip {
            // A zero interval would divide by zero — treat it as inert
            // instead of panicking mid-tick on a malformed payload.
            if tick > 0 && drip.interval_ticks != 0 && tick % drip.interval_ticks == 0 {
                runtime.grant_house_credits(house, drip.amount);
            }
        }
    }
}

impl From<SuperAiDifficulty> for DriverPolicy {
    fn from(tier: SuperAiDifficulty) -> Self {
        // 0.5f32: build twice as fast as the native chain (the fork's one
        // documented simulation-side exception, applied by the tier kernel).
        const HALVED_BUILD_TIME: NativeF32Bits = NativeF32Bits::from_bits(0x3F00_0000);
        match tier {
            SuperAiDifficulty::Fair => Self::neutral(),
            SuperAiDifficulty::Brutal => Self {
                native_hard_grant: true,
                credit_drip: Some(CreditDrip {
                    amount: 1000,
                    interval_ticks: 600,
                }),
                build_time_scale: None,
                wallet_floor: None,
            },
            SuperAiDifficulty::Super => Self {
                native_hard_grant: true,
                credit_drip: Some(CreditDrip {
                    amount: 1000,
                    interval_ticks: 600,
                }),
                build_time_scale: Some(HALVED_BUILD_TIME),
                wallet_floor: None,
            },
            SuperAiDifficulty::Unlimited => Self {
                native_hard_grant: true,
                credit_drip: None,
                build_time_scale: Some(HALVED_BUILD_TIME),
                wallet_floor: Some(UNLIMITED_WALLET_FLOOR),
            },
        }
    }
}

impl<'de> Deserialize<'de> for DriverPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// The policy fields themselves.
        #[derive(Deserialize)]
        struct Policy {
            #[serde(default)]
            native_hard_grant: bool,
            #[serde(default)]
            credit_drip: Option<CreditDrip>,
            #[serde(default)]
            build_time_scale: Option<NativeF32Bits>,
            #[serde(default)]
            wallet_floor: Option<i32>,
        }
        // The snapshot codec and its layout tracer are not self-describing
        // (no `deserialize_any`), so there the policy struct is read
        // directly; pre-policy saves are rejected by the layout digest
        // before any field is read.
        if !deserializer.is_human_readable() {
            let Policy {
                native_hard_grant,
                credit_drip,
                build_time_scale,
                wallet_floor,
            } = Policy::deserialize(deserializer)?;
            return Ok(Self {
                native_hard_grant,
                credit_drip,
                build_time_scale,
                wallet_floor,
            });
        }
        /// Text payloads written before the policy stored the bare tier;
        /// both shapes load into the equivalent treatment.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            LegacyTier(SuperAiDifficulty),
            Policy(Policy),
        }
        match Shape::deserialize(deserializer)? {
            Shape::LegacyTier(tier) => Ok(Self::from(tier)),
            Shape::Policy(Policy {
                native_hard_grant,
                credit_drip,
                build_time_scale,
                wallet_floor,
            }) => Ok(Self {
                native_hard_grant,
                credit_drip,
                build_time_scale,
                wallet_floor,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::house_state::HouseState;
    use crate::sim::world::Simulation;

    const ALL_TIERS: [SuperAiDifficulty; 4] = [
        SuperAiDifficulty::Fair,
        SuperAiDifficulty::Brutal,
        SuperAiDifficulty::Super,
        SuperAiDifficulty::Unlimited,
    ];

    #[test]
    fn tiers_map_to_distinct_policies_and_infer_back() {
        assert_eq!(
            DriverPolicy::from(SuperAiDifficulty::Fair),
            DriverPolicy::neutral()
        );

        let brutal = DriverPolicy::from(SuperAiDifficulty::Brutal);
        assert!(brutal.native_hard_grant);
        assert_eq!(
            brutal.credit_drip,
            Some(CreditDrip {
                amount: 1000,
                interval_ticks: 600,
            })
        );
        assert_eq!(brutal.build_time_scale, None);
        assert_eq!(brutal.wallet_floor, None);

        let super_tier = DriverPolicy::from(SuperAiDifficulty::Super);
        assert_eq!(
            super_tier.build_time_scale,
            Some(NativeF32Bits::from_bits(0x3F00_0000))
        );
        assert_eq!(super_tier.wallet_floor, None);

        let unlimited = DriverPolicy::from(SuperAiDifficulty::Unlimited);
        assert_eq!(
            unlimited.wallet_floor,
            Some(UNLIMITED_WALLET_FLOOR)
        );
        assert_eq!(unlimited.credit_drip, None);

        for tier in ALL_TIERS {
            assert_eq!(
                DriverPolicy::from(tier).infer_tier(),
                tier,
                "{tier:?} round-trips through its policy"
            );
        }
    }

    #[test]
    fn legacy_tier_payload_deserializes_to_the_same_policy() {
        // HouseState saves written before the policy stored the field as the
        // bare tier; both shapes must load into the equivalent treatment.
        for tier in ALL_TIERS {
            let payload = serde_json::to_string(&tier).unwrap();
            let policy: DriverPolicy = serde_json::from_str(&payload).unwrap();
            assert_eq!(policy, DriverPolicy::from(tier), "{payload}");
        }
        let absent: Option<DriverPolicy> = serde_json::from_str("null").unwrap();
        assert_eq!(absent, None);
    }

    #[test]
    fn policy_payload_round_trips() {
        let policy = DriverPolicy::from(SuperAiDifficulty::Unlimited);
        let payload = serde_json::to_string(&policy).unwrap();
        assert_eq!(
            serde_json::from_str::<DriverPolicy>(&payload).unwrap(),
            policy
        );
    }

    #[test]
    fn command_driven_policy_survives_a_snapshot_round_trip() {
        // The snapshot codec is not self-describing: its layout tracer and
        // deserializer cannot run the text-only legacy-tier shape, so the
        // policy struct itself must ride the wire.
        let mut sim = Simulation::with_seed(1);
        let id = sim.interner.intern("Computer1");
        let mut house = HouseState::new(id, 0, Some(id), true, 0, 10);
        house.make_command_driven(SuperAiDifficulty::Super);
        sim.houses.register(house);

        let bytes = crate::sim::snapshot::GameSnapshot::save(&sim, 0, 0, "driver-policy", 0);
        let restored = crate::sim::snapshot::GameSnapshot::load(&bytes)
            .expect("current snapshot")
            .sim;
        assert_eq!(
            restored.houses[&id].super_tier_policy(),
            Some(&DriverPolicy::from(SuperAiDifficulty::Super)),
            "the exact policy survives save/load"
        );
    }

    fn runtime_with_house(tier: SuperAiDifficulty) -> (SimRuntime, InternedId) {
        let mut sim = Simulation::with_seed(1);
        let id = sim.interner.intern("Computer1");
        let mut house = HouseState::new(id, 0, Some(id), true, 0, 10);
        house.make_command_driven(tier);
        sim.houses.register(house);
        (SimRuntime::from_simulation(sim), id)
    }

    #[test]
    fn credit_support_applies_the_floor_every_tick_and_the_drip_on_interval_ticks() {
        // Unlimited: the wallet floor applies on every tick, including 0,
        // and the policy carries no drip.
        let (mut runtime, id) = runtime_with_house(SuperAiDifficulty::Unlimited);
        DriverPolicy::from(SuperAiDifficulty::Unlimited).apply_credit_support(&mut runtime, id, 0);
        assert_eq!(
            runtime.simulation.houses[&id].economy().available_money(),
            UNLIMITED_WALLET_FLOOR
        );

        // Brutal: the drip lands on its interval ticks only.
        let (mut runtime, id) = runtime_with_house(SuperAiDifficulty::Brutal);
        let drip = DriverPolicy::from(SuperAiDifficulty::Brutal);
        drip.apply_credit_support(&mut runtime, id, 599);
        assert_eq!(
            runtime.simulation.houses[&id].economy().available_money(),
            0,
            "tick 599 is not a drip tick"
        );
        drip.apply_credit_support(&mut runtime, id, 600);
        assert_eq!(
            runtime.simulation.houses[&id].economy().available_money(),
            1000
        );
    }
}
