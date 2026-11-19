//! Delegation registry and receiver tests (`sim::delegation`).

use super::{DelegatedGroup, DelegationIntent, DelegationRegistry};
use crate::map::entities::EntityCategory;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::sim::combat::Health;
use crate::sim::command::Command;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::InternedId;
use crate::sim::world::Simulation;

/// Registry-level contract: delegate, partial and full revoke, prune_dead,
/// re-delegation and the read iteration.
#[test]
fn delegate_revoke_and_prune_manage_membership_and_groups() {
    let house = InternedId::from_index(0);
    let other = InternedId::from_index(1);
    let mut registry = DelegationRegistry::default();

    let group = registry.delegate(house, &[1, 2, 3], DelegationIntent::AttackLocation((4, 5)));
    assert_eq!(group, 1, "the first group a fresh registry hands out is 1");
    assert!(registry.is_delegated(house, 1));
    assert!(registry.is_delegated(house, 3));
    assert!(!registry.is_delegated(house, 4), "never delegated");
    let groups: Vec<_> = registry.groups_of(house).collect();
    assert_eq!(
        groups,
        vec![(
            1u32,
            &[1u64, 2, 3][..],
            &DelegationIntent::AttackLocation((4, 5))
        )]
    );
    assert_eq!(
        registry.intent_for_group(house, 1),
        Some(&DelegationIntent::AttackLocation((4, 5)))
    );

    // A partial revoke keeps the rest of the group; taking its last member
    // drops the group, and the read surface answers accordingly.
    registry.revoke(house, &[2, 99]);
    assert!(registry.is_delegated(house, 1));
    assert!(!registry.is_delegated(house, 2));
    registry.revoke(house, &[1, 3]);
    assert!(!registry.is_delegated(house, 1) && !registry.is_delegated(house, 3));
    assert_eq!(registry.groups_of(house).count(), 0);
    assert_eq!(registry.intent_for_group(house, 1), None);

    // prune_dead drops dead ids and the groups they empty; other houses'
    // live members stay.
    registry.delegate(house, &[10, 11], DelegationIntent::AutoReact);
    registry.delegate(other, &[12], DelegationIntent::DefendLocation((1, 2)));
    registry.prune_dead(|id| id != 10);
    assert!(!registry.is_delegated(house, 10), "dead member pruned");
    assert!(registry.is_delegated(house, 11));
    assert!(registry.is_delegated(other, 12));

    // Re-delegation moves an id into the new group: revoked from its older
    // group of the same house first, so it stays a member of exactly one.
    let moved = registry.delegate(house, &[11], DelegationIntent::DefendLocation((7, 7)));
    assert_eq!(moved, 2, "ids increase monotonically per registry");
    let groups: Vec<_> = registry.groups_of(house).collect();
    assert_eq!(
        groups,
        vec![(
            2u32,
            &[11u64][..],
            &DelegationIntent::DefendLocation((7, 7))
        )]
    );
    // The public group record is exactly the contract shape.
    assert_eq!(
        registry
            .groups_of(house)
            .next()
            .map(|(id, members, intent)| (
                DelegatedGroup {
                    id,
                    members: members.to_vec()
                },
                intent
            )),
        Some((
            DelegatedGroup {
                id: 2,
                members: vec![11]
            },
            &DelegationIntent::DefendLocation((7, 7))
        ))
    );
}

/// Spawn one bare owned entity of `type_name` for the receiver fixtures.
fn spawn_member(
    sim: &mut Simulation,
    stable_id: u64,
    type_name: &str,
    owner_name: &str,
    category: EntityCategory,
    rules: &RuleSet,
) {
    let owner = sim.interner.intern(owner_name);
    let type_ref = sim.interner.intern(type_name);
    let obj = rules.object(type_name).expect("fixture object type");
    let mut entity = GameEntity::new_at_frame_zero_for_test(
        stable_id,
        20,
        20,
        0,
        0,
        owner,
        Health::new(obj.strength),
        type_ref,
        category,
        0,
        obj.sight.max(0) as u16,
        category == EntityCategory::Unit,
    );
    // A directly-inserted GameEntity keeps the constructed `in_limbo` byte;
    // production spawns clear it through Reveal.
    entity.lifecycle.set_in_limbo_for_test(false);
    sim.substrate.entities.insert(entity);
}

/// The receiver refuses a command whose envelope owner differs from an
/// entity's owner: nothing is delegated, deterministically and silently.
#[test]
fn delegate_receiver_drops_a_command_naming_another_houses_entity() {
    let rules = RuleSet::from_ini(&IniFile::from_str(
        "[VehicleTypes]\n0=MTNK\n[MTNK]\nStrength=100\nSpeed=4\n",
    ))
    .expect("rules");
    let mut sim = Simulation::new();
    spawn_member(
        &mut sim,
        42,
        "MTNK",
        "Americans",
        EntityCategory::Unit,
        &rules,
    );
    // Both names must stand in the interner before any command resolves an
    // owner; a real scenario interns its houses at init.
    sim.interner.intern("Russians");

    assert!(
        !sim.apply_command(
            "Russians",
            &Command::DelegateAssets {
                entity_ids: vec![42],
                intent: DelegationIntent::AutoReact,
            },
            Some(&rules),
        ),
        "a foreign envelope owner drops the whole command"
    );
    let russians = sim.interner.get("Russians").expect("interned owner");
    assert!(!sim.delegation().is_delegated(russians, 42));
    assert_eq!(sim.delegation().groups_of(russians).count(), 0);

    // The owning house's identical command is admitted.
    assert!(sim.apply_command(
        "Americans",
        &Command::DelegateAssets {
            entity_ids: vec![42],
            intent: DelegationIntent::AutoReact,
        },
        Some(&rules),
    ));
    let americans = sim.interner.get("Americans").expect("interned owner");
    assert!(sim.delegation().is_delegated(americans, 42));
}

/// Unarmed structures are silently skipped at apply time (base buildings
/// never delegate); the armed defense building and the unit stay.
#[test]
fn delegate_receiver_skips_unarmed_structures() {
    let rules = RuleSet::from_ini(&IniFile::from_str(
        "[BuildingTypes]\n0=ARMEDB\n1=PLAINB\n\
         [ARMEDB]\nStrength=1000\nPrimary=GUN\n\
         [PLAINB]\nStrength=1000\n\
         [VehicleTypes]\n0=MTNK\n\
         [MTNK]\nStrength=100\nSpeed=4\n\
         [WeaponTypes]\n0=GUN\n\
         [GUN]\nDamage=10\nRange=5\n",
    ))
    .expect("rules");
    let mut sim = Simulation::new();
    spawn_member(
        &mut sim,
        1,
        "PLAINB",
        "Americans",
        EntityCategory::Structure,
        &rules,
    );
    spawn_member(
        &mut sim,
        2,
        "ARMEDB",
        "Americans",
        EntityCategory::Structure,
        &rules,
    );
    spawn_member(
        &mut sim,
        3,
        "MTNK",
        "Americans",
        EntityCategory::Unit,
        &rules,
    );

    assert!(sim.apply_command(
        "Americans",
        &Command::DelegateAssets {
            entity_ids: vec![1, 2, 3],
            intent: DelegationIntent::AutoReact,
        },
        Some(&rules),
    ));
    let americans = sim.interner.get("Americans").expect("interned owner");
    let groups: Vec<_> = sim.delegation().groups_of(americans).collect();
    assert_eq!(groups.len(), 1, "one delegation action, one group");
    assert_eq!(
        groups[0].1,
        &[2u64, 3],
        "unarmed structure skipped; armed structure and unit kept"
    );
    assert!(!sim.delegation().is_delegated(americans, 1));
    assert!(sim.delegation().is_delegated(americans, 2));
}
