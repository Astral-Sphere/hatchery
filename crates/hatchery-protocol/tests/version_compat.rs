//! Version compatibility: a fixture written by an earlier build must still deserialize.
//!
//! Methods and fields may only be added (`docs/design/protocol.md` §6), so a released major must
//! keep reading everything its own fixtures contain. This test reads the **stored JSON** and
//! deserializes it with the type that owns it — it deliberately does not compare against the
//! current in-memory sample, because "has the wire format changed?" is `golden_fixtures.rs`'s
//! question and mixing the two would make a fixture update look like a compatibility break.

mod support;

use std::fs;

use serde_json::Value;

use hatchery_protocol::{PROTOCOL_MAJOR, SUPPORTED_PROTOCOL_VERSIONS, is_compatible, major_of};

use support::{all, fixture_dir};

#[test]
fn every_stored_fixture_still_deserializes() {
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
