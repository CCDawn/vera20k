//! VERA's Command Mode delegation mark: the captured flag and the corner
//! bracket the status producer emits from it. The delegation registry itself
//! arrives with the commander-mode simulation task, so the capture half pins
//! the seam's false default and the draw half hand-flags the record.

use super::frame::ObjectRecord;
use super::ui_overlays::{StatusViewport, delegated_mark_commands, delegated_mark_tint};
use crate::map::entities::EntityCategory;
use crate::sim::game_entity::GameEntity;

fn viewport() -> StatusViewport {
    StatusViewport {
        camera: [0.0, 0.0],
        size: [800.0, 600.0],
        zoom: 1.0,
        ignore_visibility: false,
    }
}

/// A record captures `delegated: false` while the registry seam is stubbed,
/// and once flagged it yields exactly one bracket at its class's top-left
/// corner with the fixed mark tint: two one-pixel strokes apart from the
/// veteran pip's bottom-right cross.
#[test]
fn a_flagged_record_yields_one_corner_bracket_and_an_ordinary_one_none() {
    let mut entity =
        GameEntity::test_default_of_category(1, "GAPOWR", "A", 0, 0, EntityCategory::Structure);
    let mut record = ObjectRecord::of_test(&entity);
    assert!(
        !record.delegated,
        "nothing is delegated before the registry lands"
    );
    assert!(delegated_mark_commands(&record, (100.0, 50.0), viewport()).is_empty());

    record.delegated = true;
    let bracket = delegated_mark_commands(&record, (100.0, 50.0), viewport());
    assert_eq!(bracket.len(), 2, "one horizontal and one vertical stroke");
    for command in &bracket {
        assert_eq!(command.position, [88.0, 42.0], "structure top-left offset");
        assert_eq!(command.tint, delegated_mark_tint(), "the fixed mark tint");
        assert_eq!(command.alpha, 1.0);
    }
    assert_eq!(
        [bracket[0].size, bracket[1].size],
        [[4.0, 1.0], [1.0, 4.0]],
        "an L of two one-pixel strokes"
    );

    // An infantry record keeps its own, tighter corner offset.
    entity = GameEntity::test_default_of_category(2, "E1", "A", 0, 0, EntityCategory::Infantry);
    let mut soldier = ObjectRecord::of_test(&entity);
    soldier.delegated = true;
    let bracket = delegated_mark_commands(&soldier, (100.0, 50.0), viewport());
    assert_eq!(bracket.len(), 2);
    assert_eq!(
        bracket[0].position,
        [93.0, 46.0],
        "infantry top-left offset"
    );
}
