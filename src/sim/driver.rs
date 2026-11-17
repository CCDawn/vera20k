//! Fork-only driver policy: the per-house driver-support payload
//! (`docs/superai.md`).
//!
//! A command-driven house carries its launch treatment as a [`DriverPolicy`]
//! instead of a bare tier mark. That lets an external driver plugin launch
//! with arbitrary knob combinations while the launch kernel, the per-tick
//! credit loop and the driver rebuild all read the same fields. The knobs
//! reproduce existing behavior only:
//!
//! - `native_hard_grant` — the native Hard AI's opening grant
//!   ([`crate::sim::scenario_bootstrap::native_ai_opening_grant`], the
//!   retail `[General] MultiplayerAICM` Hard row via
//!   `ScenarioClass__Post_Map_Init @ 0x00686A52`).
//! - `credit_drip` — a recurring credit bonus.
//! - `build_time_scale` — the production build-time scale
//!   (`HouseState::set_native_build_time_scale`).
//! - `wallet_floor` — a per-tick wallet top-up.
//!
//! The Super-AI difficulty ladder is the driver plugin's
//! (`vera20k-superai`), which maps each tier onto this policy and infers
//! the tier back (strongest knob wins: floor → scale → drip → none), the
//! semantics the driver rebuild heuristic uses.

use crate::sim::intern::InternedId;
use crate::sim::runtime::SimRuntime;
use crate::util::native_x87::NativeF32Bits;
use serde::{Deserialize, Serialize};

/// A recurring credit bonus: `amount` every `interval_ticks` ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditDrip {
    pub amount: i32,
    pub interval_ticks: u32,
}

/// The driver-support treatment a command-driven house carries
/// (`docs/superai.md`). All knobs off ([`Self::neutral`]) is the
/// treatment-free policy; the driver plugin's difficulty ladder maps each
/// tier onto one instance of this struct.
///
/// Serialized as the policy struct; every field defaults when absent. The
/// snapshot codec's bincode layout is not self-describing, and its layout
/// digest already rejects pre-policy saves wholesale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverPolicy {
    /// Open with the native Hard AI's credit grant, whatever the house's
    /// own difficulty.
    #[serde(default)]
    pub native_hard_grant: bool,
    /// Recurring credit bonus, applied by [`Self::apply_credit_support`].
    #[serde(default)]
    pub credit_drip: Option<CreditDrip>,
    /// Production build-time scale (`HouseState::set_native_build_time_scale`).
    #[serde(default)]
    pub build_time_scale: Option<NativeF32Bits>,
    /// Per-tick wallet top-up ([`Self::apply_credit_support`]).
    #[serde(default)]
    pub wallet_floor: Option<i32>,
}

impl DriverPolicy {
    /// Every knob off: the treatment-free policy.
    pub const fn neutral() -> Self {
        Self {
            native_hard_grant: false,
            credit_drip: None,
            build_time_scale: None,
            wallet_floor: None,
        }
    }

    /// This policy's per-tick credit support for `house` at sim `tick`:
    /// the wallet floor tops the balance up to the floor on every tick, the
    /// drip grants its amount on interval ticks (skipping tick 0, where the
    /// opening grant already ran). The floor wins over the drip: a floored
    /// policy tops up and drips nothing. The launch's per-tick loop (the
    /// app's sim_tick and the sim-bench super scenario) applies this
    /// through [`SimRuntime::grant_house_credits`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::house_state::HouseState;
    use crate::sim::world::Simulation;

    #[test]
    fn policy_payload_round_trips() {
        let policy = DriverPolicy {
            native_hard_grant: true,
            credit_drip: Some(CreditDrip {
                amount: 1000,
                interval_ticks: 600,
            }),
            build_time_scale: Some(NativeF32Bits::from_bits(0x3F00_0000)),
            wallet_floor: Some(1_000_000),
        };
        let payload = serde_json::to_string(&policy).unwrap();
        assert_eq!(
            serde_json::from_str::<DriverPolicy>(&payload).unwrap(),
            policy
        );
        let absent: Option<DriverPolicy> = serde_json::from_str("null").unwrap();
        assert_eq!(absent, None);
    }

    #[test]
    fn command_driven_policy_survives_a_snapshot_round_trip() {
        // The snapshot codec is not self-describing, so the policy struct
        // itself rides the wire.
        let policy = DriverPolicy {
            native_hard_grant: true,
            credit_drip: Some(CreditDrip {
                amount: 1000,
                interval_ticks: 600,
            }),
            build_time_scale: Some(NativeF32Bits::from_bits(0x3F00_0000)),
            wallet_floor: None,
        };
        let mut sim = Simulation::with_seed(1);
        let id = sim.interner.intern("Computer1");
        let mut house = HouseState::new(id, 0, Some(id), true, 0, 10);
        house.make_command_driven(policy);
        sim.houses.register(house);

        let bytes = crate::sim::snapshot::GameSnapshot::save(&sim, 0, 0, "driver-policy", 0);
        let restored = crate::sim::snapshot::GameSnapshot::load(&bytes)
            .expect("current snapshot")
            .sim;
        assert_eq!(
            restored.houses[&id].super_tier_policy(),
            Some(&policy),
            "the exact policy survives save/load"
        );
    }

    fn runtime_with_house(policy: DriverPolicy) -> (SimRuntime, InternedId) {
        let mut sim = Simulation::with_seed(1);
        let id = sim.interner.intern("Computer1");
        let mut house = HouseState::new(id, 0, Some(id), true, 0, 10);
        house.make_command_driven(policy);
        sim.houses.register(house);
        (SimRuntime::from_simulation(sim), id)
    }

    #[test]
    fn credit_support_applies_the_floor_every_tick_and_the_drip_on_interval_ticks() {
        // A floored policy: the wallet floor applies on every tick,
        // including 0, and the policy carries no drip.
        const FLOOR: i32 = 1234;
        let floored = DriverPolicy {
            native_hard_grant: false,
            credit_drip: None,
            build_time_scale: None,
            wallet_floor: Some(FLOOR),
        };
        let (mut runtime, id) = runtime_with_house(floored);
        floored.apply_credit_support(&mut runtime, id, 0);
        assert_eq!(
            runtime.simulation.houses[&id].economy().available_money(),
            FLOOR
        );

        // A dripping policy: the drip lands on its interval ticks only.
        let dripping = DriverPolicy {
            native_hard_grant: false,
            credit_drip: Some(CreditDrip {
                amount: 1000,
                interval_ticks: 600,
            }),
            build_time_scale: None,
            wallet_floor: None,
        };
        let (mut runtime, id) = runtime_with_house(dripping);
        dripping.apply_credit_support(&mut runtime, id, 599);
        assert_eq!(
            runtime.simulation.houses[&id].economy().available_money(),
            0,
            "tick 599 is not a drip tick"
        );
        dripping.apply_credit_support(&mut runtime, id, 600);
        assert_eq!(
            runtime.simulation.houses[&id].economy().available_money(),
            1000
        );
    }
}
