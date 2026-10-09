//! Version compatibility: a fixture written by an earlier build must still deserialize.
//!
//! Methods and fields may only be added (`docs/design/protocol.md` §6), so a released major must
//! keep reading everything its own fixtures contain. This test reads the **stored JSON** and
//! deserializes it with the type that owns it — it deliberately does not compare against the
//! current in-memory sample, because "has the wire format changed?" is `golden_fixtures.rs`'s
//! question and mixing the two would make a fixture update look like a compatibility break.
//!
//! §6 has a second half, pinned here too: enum values are frozen within a major, so an unknown
//! spelling is refused rather than defaulted into something this build does know.

mod support;

use std::fs;

use serde_json::Value;

use hatchery_protocol::{PROTOCOL_MAJOR, SUPPORTED_PROTOCOL_VERSIONS, is_compatible, major_of};

use support::{all, fixture_dir, updating};

#[test]
fn every_stored_fixture_still_deserializes() {
    if updating() {
        // `golden_fixtures.rs` is rewriting the directory from another test binary right now, and
        // nextest runs binaries concurrently: reading it mid-update would report a compatibility
        // break that is only a torn read.
        return;
    }

    for fixture in all() {
        let path = fixture_dir().join(format!("{}.json", fixture.stem));
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()));
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{} is not valid JSON: {error}", path.display()));

        (fixture.check)(&value).unwrap_or_else(|error| {
            panic!(
                "the stored fixture {} ({}) no longer deserializes: {error}\n\
                 A field was renamed or removed. Add a new field instead of repurposing one — \
                 that is what the version-compatibility promise means (docs/design/protocol.md §6)",
                fixture.stem, fixture.shape
            )
        });
    }
}

#[test]
fn every_type_tolerates_a_field_a_newer_peer_added() {
    // Forward compatibility is a property of every registered type, not of one hand-picked
    // sample: an older build must read what a newer one writes, and "fields may only be added"
    // only means something if no type here rejects an unknown key.
    const NOTHING_TO_EXTEND: &[&str] = &[
        "table_methods",
        "table_item_kinds",
        "value_protocol_version",
        "value_jsonrpc_version",
    ];

    let mut swept = 0;
    for fixture in all() {
        if !has_object(&fixture.value) {
            assert!(
                NOTHING_TO_EXTEND.contains(&fixture.stem),
                "{} ({}) has no object a newer peer could add a field to, but is not one of the \
                 known bare values: {}",
                fixture.stem,
                fixture.shape,
                fixture.value
            );
            continue;
        }

        let mut value = fixture.value.clone();
        add_future_field(&mut value);
        swept += 1;
        (fixture.check)(&value).unwrap_or_else(|error| {
            panic!(
                "{} ({}) rejected a field added by a newer peer: {error}\n\
                 A wire type must ignore keys it does not know (docs/design/protocol.md §6) — \
                 `deny_unknown_fields` would break every older client",
                fixture.stem, fixture.shape
            )
        });
    }

    assert!(swept > 0, "the sweep covered nothing");
}

#[test]
fn an_unknown_enum_value_is_refused_rather_than_defaulted() {
    // The other side of the same rule: fields grow, enum values do not. Guessing a fallback for a
    // spelling this build does not know would turn a version mismatch into a confidently wrong
    // answer, so it must fail instead.
    for (stem, field, unknown) in [
        ("event_text_delta", "type", "future_event"),
        ("item_user_message", "kind", "future_kind"),
        ("session", "status", "hibernating"),
    ] {
        let fixture = support::fixture_by_stem(stem);
        let mut value = fixture.value.clone();
        value[field] = Value::String(unknown.to_owned());

        let error = match (fixture.check)(&value) {
            Ok(_) => panic!(
                "{stem} accepted {field} = {unknown:?}, which this build does not know; an unknown \
                 enum value must be refused rather than defaulted"
            ),
            Err(error) => error,
        };
        assert!(
            error.contains(unknown),
            "{stem} refused {field} = {unknown:?} without saying so: {error}"
        );
    }
}

/// True when the value, or anything inside it, is an object a newer peer could add a field to.
fn has_object(value: &Value) -> bool {
    match value {
        Value::Object(_) => true,
        Value::Array(items) => items.iter().any(has_object),
        _ => false,
    }
}

/// Adds the field a newer peer would add — to the value itself, and to every object inside it, so
/// a list of structs is swept element by element rather than skipped.
fn add_future_field(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.insert("__future_field".to_owned(), Value::Bool(true));
        }
        Value::Array(items) => {
            for item in items {
                add_future_field(item);
            }
        }
        _ => {}
    }
}

#[test]
fn the_fixture_set_matches_the_major_we_support() {
    let directory = fixture_dir();
    let name = directory
        .file_name()
        .expect("the fixture directory has a name")
        .to_string_lossy()
        .into_owned();
    let fixture_major: u32 = name
        .strip_prefix("protocol-v")
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("{name} is not named protocol-v<major>"));

    assert_eq!(
        fixture_major, PROTOCOL_MAJOR,
        "the fixtures describe a different major than this build speaks"
    );
    assert!(
        is_compatible(&format!("{fixture_major}.0.0")),
        "this build must accept the versions whose fixtures it keeps"
    );
    assert!(
        SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .any(|version| major_of(version) == Some(fixture_major)),
        "the advertised supported-version list omits the fixture major"
    );
}

#[test]
fn a_fixture_from_another_major_would_be_refused() {
    // The negative case, so the check above is not vacuous: compatibility is per major, and a
    // fixture set from a different major must not be silently accepted.
    assert!(!is_compatible(&format!("{}.0.0", PROTOCOL_MAJOR + 1)));
    assert!(!is_compatible(&format!(
        "{}.0.0",
        PROTOCOL_MAJOR.saturating_sub(1)
    )));
}
