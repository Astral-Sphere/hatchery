//! Round-trip and shape tests over the whole public vocabulary.
//!
//! The fixture registry in `tests/support/mod.rs` is the single list of what the protocol owns;
//! these tests prove each entry survives serde, and `golden_fixtures.rs` pins the exact bytes.

mod support;

use serde_json::Value;

use hatchery_protocol::{ItemKindTag, SessionEvent};

use support::all;

#[test]
fn every_value_survives_a_json_round_trip() {
    for fixture in all() {
        let text = serde_json::to_string(&fixture.value).unwrap_or_else(|e| {
            panic!(
                "{} ({}) is not serializable: {e}",
                fixture.stem, fixture.shape
            )
        });
        let reparsed: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("{} ({}) did not reparse: {e}", fixture.stem, fixture.shape)
        });
        assert_eq!(
            reparsed, fixture.value,
            "{} ({}) changed during a JSON round trip",
            fixture.stem, fixture.shape
        );
    }
}

#[test]
fn every_value_deserializes_into_its_own_type() {
    for fixture in all() {
        let parsed = (fixture.check)(&fixture.value).unwrap_or_else(|error| {
            panic!(
                "{} ({}) no longer deserializes: {error}",
                fixture.stem, fixture.shape
            )
        });
        assert!(
            !parsed.is_empty(),
            "{} ({}) produced no readable summary",
            fixture.stem,
            fixture.shape
        );
    }
}

#[test]
fn optionals_are_absent_rather_than_null() {
    // `null` and "absent" must not be conflated: `SessionPatch::title` uses `null` to mean
    // "clear the title" while an absent field means "leave it alone".
    for fixture in all() {
        let text = serde_json::to_string(&fixture.value).expect("serializable");
        if fixture.stem == "session_patch_clear_title" {
            assert!(text.contains(r#""title":null"#), "{text}");
            continue;
        }
        assert!(
            !text.contains(":null"),
            "{} ({}) writes a null; absent optional fields must be omitted: {text}",
            fixture.stem,
            fixture.shape
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

    // Thirteen session-scoped variants plus one daemon-wide event (docs/design/protocol.md §4).
    assert_eq!(covered.len(), 13);
    assert_eq!(support::daemon_events().len(), 1);
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
