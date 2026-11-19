//! Sim-side ownership of delegated assets ("Commander Mode").
//!
//! The player hands a subset of their house's units and armed defense
//! buildings to an external AI plugin (owned outside sim; sim never
//! references it). The delegation state is authoritative simulation
//! state: it changes only through the synchronized command stream
//! ([`crate::sim::command::Command::DelegateAssets`] /
//! [`crate::sim::command::Command::RevokeAssets`], applied in
//! `world_commands`), so every peer derives the same groups in the same
//! order, and it is snapshot-persisted as a `#[serde(default)]` field of
//! `Simulation` (carried inside `GameSnapshot::sim` whole). The plugin and
//! presentation read it through `Simulation::delegation`.
//!
//! Lifecycle: a despawned member id never stays delegated — the shared
//! physical destructor owner (`finalize_and_remove_common`) prunes dead ids
//! through [`DelegationRegistry::prune_dead`].
//!
//! No native gamemd mechanism owns this state; it is a VERA extension, so
//! this module carries no native address.
//!
//! Dependency rules: part of sim/; never depends on render/, ui/, audio/ or
//! net/.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sim::intern::InternedId;

/// One map cell named by a delegation intent (rx, ry), the same signed
/// coordinate pair the click-path cell commands carry.
pub type DelegationCell = (i16, i16);

/// What the delegated AI should do with its group. The VERA extension
/// carries the click's meaning; the plugin reads it, sim never acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelegationIntent {
    /// Guard and react around the delegated members themselves.
    AutoReact,
    /// Press an attack onto this cell.
    AttackLocation(DelegationCell),
    /// Hold and defend this cell.
    DefendLocation(DelegationCell),
}

impl Default for DelegationIntent {
    /// The passive default, so a `#[serde(default)]` read of an intent-less
    /// record arms a harmless group.
    fn default() -> Self {
        Self::AutoReact
    }
}

/// One delegation action's group: the stable entity ids handed over
/// together. The owning house is implied by the registry's per-house
/// storage, so it is not part of the group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedGroup {
    /// The delegation action's id, unique per registry and monotonically
    /// increasing (the first group a registry hands out is 1).
    pub id: u32,
    /// The delegated entities' stable ids, in the command's member order.
    pub members: Vec<u64>,
}

/// One stored group beside its intent. Internal record; readers see the
/// pieces through [`DelegationRegistry::groups_of`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredGroup {
    group: DelegatedGroup,
    intent: DelegationIntent,
}

/// The per-house delegated-asset groups. The single authoritative owner of
/// delegation state: its mutations happen only through the command
/// receivers and the lifecycle death prune; every reader goes through the
/// shared methods below. `BTreeMap` keyed by the interned house id keeps
/// iteration deterministic across peers and saves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRegistry {
    #[serde(default)]
    groups: BTreeMap<InternedId, Vec<StoredGroup>>,
    /// The next group id to hand out, monotonically increasing per registry
    /// and persisted with the groups.
    #[serde(default)]
    next_group_id: u32,
}

impl Default for DelegationRegistry {
    fn default() -> Self {
        Self {
            groups: BTreeMap::new(),
            next_group_id: 1,
        }
    }
}

impl DelegationRegistry {
    /// Record a delegation: the members join a fresh group of `house` with
    /// `intent`, and the group's id is returned. Re-delegating an
    /// already-delegated id moves it to the new group: the id is revoked
    /// from the house's older groups first, so an id is a member of at most
    /// one group of its house.
    pub fn delegate(
        &mut self,
        house: InternedId,
        members: &[u64],
        intent: DelegationIntent,
    ) -> u32 {
        self.revoke(house, members);
        // Group ids start at 1: 0 never names a group, even on a restored
        // registry whose cursor field defaulted.
        let id = self.next_group_id.max(1);
        self.next_group_id = id.saturating_add(1);
        let house_groups = self.groups.entry(house).or_default();
        house_groups.push(StoredGroup {
            group: DelegatedGroup {
                id,
                members: members.to_vec(),
            },
            intent,
        });
        id
    }

    /// Take members back: remove the ids from every group of `house` and
    /// drop the groups those ids emptied. Ids the house never delegated are
    /// ignored.
    pub fn revoke(&mut self, house: InternedId, entity_ids: &[u64]) {
        let Some(house_groups) = self.groups.get_mut(&house) else {
            return;
        };
        let removed: std::collections::BTreeSet<u64> = entity_ids.iter().copied().collect();
        for stored in house_groups.iter_mut() {
            stored
                .group
                .members
                .retain(|member| !removed.contains(member));
        }
        house_groups.retain(|stored| !stored.group.members.is_empty());
        if house_groups.is_empty() {
            self.groups.remove(&house);
        }
    }

    /// Drop every member id `live` refuses, across all houses, and drop the
    /// groups (and houses) those drops emptied. The lifecycle despawn
    /// choke point calls this so a dead id never stays delegated.
    pub fn prune_dead(&mut self, live: impl Fn(u64) -> bool) {
        self.groups.retain(|_, house_groups| {
            for stored in house_groups.iter_mut() {
                stored.group.members.retain(|member| live(*member));
            }
            house_groups.retain(|stored| !stored.group.members.is_empty());
            !house_groups.is_empty()
        });
    }

    /// The intent a house's group carries, by group id.
    pub fn intent_for_group(&self, house: InternedId, group_id: u32) -> Option<&DelegationIntent> {
        self.groups
            .get(&house)?
            .iter()
            .find(|stored| stored.group.id == group_id)
            .map(|stored| &stored.intent)
    }

    /// The house's groups in registry order: `(group id, member ids, intent)`.
    pub fn groups_of<'s>(
        &'s self,
        house: InternedId,
    ) -> impl Iterator<Item = (u32, &'s [u64], &'s DelegationIntent)> + 's {
        self.groups.get(&house).into_iter().flatten().map(|stored| {
            (
                stored.group.id,
                stored.group.members.as_slice(),
                &stored.intent,
            )
        })
    }

    /// Whether `house` currently delegates `entity_id`.
    pub fn is_delegated(&self, house: InternedId, entity_id: u64) -> bool {
        self.groups.get(&house).is_some_and(|house_groups| {
            house_groups
                .iter()
                .any(|stored| stored.group.members.contains(&entity_id))
        })
    }
}

#[cfg(all(test, tests_sim_c_g))]
#[path = "delegation_tests.rs"]
mod delegation_tests;
