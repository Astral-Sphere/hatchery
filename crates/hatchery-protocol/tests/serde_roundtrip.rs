//! Shape tests over the whole public vocabulary.
//!
//! The fixture registry in `tests/support/mod.rs` is the single list of what the protocol owns.
//! These tests ask what the registry itself cannot: that every entry deserializes into the type
//! that owns it, that optional fields stay absent rather than null, that absent-field shapes stay
//! absent, and that the registry covers every variant of every event and item enum. The exact
//! bytes are `golden_fixtures.rs`'s question and the stored ones are `version_compat.rs`'s.

mod support;

use serde_json::Value;

use hatchery_protocol::{DaemonEvent, ItemKindTag, ServerEvent, SessionEvent, SessionPatch};

use support::all;

#[test]
fn every_value_deserializes_into_its_own_type() {
    for fixture in all() {
        (fixture.check)(&fixture.value).unwrap_or_else(|error| {
            panic!(
                "{} ({}) no longer deserializes: {error}",
                fixture.stem, fixture.shape
            )
        });
    }
}

#[test]
fn optionals_are_absent_rather_than_null() {
    // `null` and "absent" must not be conflated: `SessionPatch::title` uses `null` to mean
    // "clear the title" while an absent field means "leave it alone". Every other optional in the
    // vocabulary is `skip_serializing_if`, so a `null` anywhere else is a wire-format change.
    for fixture in all() {
        let exempt = (fixture.stem == "session_patch_clear_title").then_some("$.title");
        if exempt.is_some() {
            assert_eq!(
                fixture.value.get("title"),
                Some(&Value::Null),
                "{} must carry the clear marker as an explicit null",
                fixture.stem
            );
        }
        assert_no_nulls(fixture.stem, &fixture.value, "$", exempt);
    }
}

/// Fails on the first `null` in the tree, naming the path that holds it.
fn assert_no_nulls(stem: &str, value: &Value, path: &str, exempt: Option<&str>) {
    match value {
        Value::Null => panic!(
            "{stem} writes a null at {path}; absent optional fields must be omitted, and a null \
             would have to be told apart from \"not supplied\" on the far side"
        ),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_no_nulls(stem, item, &format!("{path}[{index}]"), exempt);
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                let child = format!("{path}.{key}");
                if exempt == Some(child.as_str()) {
                    continue;
                }
                assert_no_nulls(stem, item, &child, exempt);
            }
        }
        _ => {}
    }
}

#[test]
#[should_panic(expected = "writes a null at $.list[1]")]
fn the_null_walk_sees_inside_arrays() {
    // No fixture holds a null inside an array today, and the substring check this replaced
    // (`!text.contains(":null")`) would have missed one, so the walker is pinned directly.
    assert_no_nulls(
        "synthetic",
        &serde_json::json!({"list": [1, null]}),
        "$",
        None,
    );
}

#[test]
fn the_clearing_patch_reads_back_as_clear() {
    // The value, not just the type: serde maps JSON `null` onto the *outer* option, so a patch
    // that lost its clear marker still deserializes — as "leave the title alone".
    let fixture = support::fixture_by_stem("session_patch_clear_title");
    let patch: SessionPatch =
        serde_json::from_value(fixture.value.clone()).expect("the fixture deserializes");
    assert_eq!(
        patch.title,
        Some(None),
        "a stored null title must read back as 'clear it', not 'leave it alone'"
    );
    assert!(!patch.is_empty(), "a clearing patch is not a no-op");
}

#[test]
fn the_absent_field_shapes_really_are_absent() {
    let root = support::fixture_by_stem("item_root");
    for key in ["parent", "turn"] {
        assert!(
            root.value.get(key).is_none(),
            "item_root must not carry {key} at all: {}",
            root.value
        );
    }

    let fresh = support::fixture_by_stem("session_fresh");
    for key in ["title", "workspace", "active_branch_head", "config_patch"] {
        assert!(
            fresh.value.get(key).is_none(),
            "session_fresh must not carry {key} at all: {}",
            fresh.value
        );
    }
}

#[test]
fn the_fixture_set_covers_every_event_and_item_kind() {
    let stems: Vec<&str> = all().iter().map(|fixture| fixture.stem).collect();

    for event in support::events() {
        let (stem, event) = event;
        assert!(
            stems.contains(&stem),
            "event {stem} is missing from the fixture set"
        );
        assert!(
            stem.contains(event.type_name()),
            "fixture {stem} does not name its event type {}",
            event.type_name()
        );
    }

    // Every `ItemKind` variant must have a fixture, so adding a variant cannot skip the golden.
    let kinds: Vec<&str> = support::items()
        .iter()
        .map(|(stem, item)| {
            assert!(
                stem.contains(item.kind_tag().as_str()),
                "fixture {stem} does not name its kind {}",
                item.kind_tag()
            );
            *stem
        })
        .collect();
    assert_eq!(kinds.len(), ItemKindTag::ALL.len());
}

#[test]
fn the_event_fixtures_cover_every_variant_exactly_once() {
    let covered: Vec<&str> = support::events()
        .iter()
        .map(|(_, event)| event.type_name())
        .collect();
    let mut unique = covered.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(covered.len(), unique.len(), "duplicate event fixture");

    // Set equality against the compiler-generated list, not against a count: `ALL_TYPE_NAMES`
    // comes from the same macro that generates `type_name`'s exhaustive match, so a variant
    // added to the enum without a fixture fails here instead of slipping through.
    let mut declared = ServerEvent::ALL_TYPE_NAMES.to_vec();
    declared.sort_unstable();
    assert_eq!(
        unique, declared,
        "every ServerEvent variant needs a fixture, and every fixture a variant \
         (docs/design/protocol.md §4)"
    );

    let daemon: Vec<&str> = support::daemon_events()
        .iter()
        .map(|(_, event)| event.type_name())
        .collect();
    assert_eq!(daemon, DaemonEvent::ALL_TYPE_NAMES.to_vec());
}

#[test]
fn every_method_has_a_parameter_fixture() {
    let params = support::method_params();
    assert_eq!(
        params.len(),
        hatchery_protocol::method::ALL.len(),
        "every method needs a parameter sample"
    );
    for method in hatchery_protocol::method::ALL {
        assert!(
            params.iter().any(|(name, _)| name == method),
            "{method} has no parameter fixture"
        );
    }
}

#[test]
fn every_method_has_a_result_fixture_checked_by_its_type() {
    // `as_object_with(&[])` once stood in for every result check: any object passed, and the
    // fifteen result types with no fixture at all were pinned by nothing. A check that accepts an
    // empty object pins no type, so each registered result check is probed with one.
    let results = support::method_results();
    assert_eq!(
        results.len(),
        hatchery_protocol::method::ALL.len(),
        "every method needs a result sample"
    );

    let mut registered = Vec::new();
    for fixture in all() {
        if fixture.shape != "result" {
            continue;
        }
        (fixture.check)(&fixture.value).unwrap_or_else(|error| {
            panic!(
                "{} does not deserialize into its result type: {error}",
                fixture.stem
            )
        });
        assert!(
            (fixture.check)(&serde_json::json!({})).is_err(),
            "{}'s check accepts an empty object, so it pins no result type. If that result really \
             is all-optional, assert on its fields here instead.",
            fixture.stem
        );
        registered.push(fixture.stem);
    }

    for method in hatchery_protocol::method::ALL {
        assert!(
            registered.contains(&support::result_stem(method)),
            "{method} has no result fixture"
        );
    }
    assert_eq!(
        registered.len(),
        hatchery_protocol::method::ALL.len(),
        "a result fixture is registered twice, or for a method that does not exist"
    );
}

#[test]
fn typed_serialization_keeps_declaration_order() {
    // The golden files are key-sorted, because they are rendered from `serde_json::Value`s and a
    // `Value` is a `BTreeMap` here. JSON object order is not semantic, but it *is* what a peer
    // sees, so the declaration order of the hot wire types is pinned here rather than nowhere.
    let item = support::items().remove(0).1;
    assert_key_order(
        &item,
        &[
            "id",
            "session",
            "parent",
            "turn",
            "kind",
            "payload",
            "created_at",
        ],
    );

    let event = SessionEvent::new(support::session_id(), 3, support::events().remove(1).1);
    // The envelope's coordinates come first, then the event: an internally tagged enum writes its
    // tag before the payload fields, so `type` leads the event's own keys.
    assert_key_order(&event, &["session", "generation", "type", "item", "text"]);

    let request = hatchery_protocol::Request::new(1_i64, hatchery_protocol::method::SESSION_LIST);
    assert_key_order(&request, &["jsonrpc", "id", "method"]);
}

/// Asserts that `keys` appear in `value`'s serialization in exactly this order.
fn assert_key_order<T: serde::Serialize>(value: &T, keys: &[&str]) {
    let text = serde_json::to_string(value).expect("serializable");
    let mut last = 0;
    for key in keys {
        let needle = format!("\"{key}\"");
        let position = text
            .find(&needle)
            .unwrap_or_else(|| panic!("{needle} is missing from {text}"));
        assert!(
            position >= last,
            "{key} appears out of order in {text}; expected {keys:?}"
        );
        last = position;
    }
}

#[test]
fn envelopes_carry_session_and_generation_next_to_the_event() {
    for (stem, event) in support::events() {
        let envelope = SessionEvent::new(support::session_id(), 3, event.clone());
        let value = serde_json::to_value(&envelope).expect("serialize");
        let object = value.as_object().expect("an envelope is an object");
        assert!(
            object.contains_key("session") && object.contains_key("generation"),
            "{stem} lost its envelope coordinates"
        );
        assert_eq!(
            object.get("type").and_then(Value::as_str),
            Some(event.type_name()),
            "{stem} lost its event tag"
        );
        assert_eq!(
            serde_json::from_value::<SessionEvent>(value).expect("deserialize"),
            envelope,
            "{stem} did not round-trip inside its envelope"
        );
    }
}
