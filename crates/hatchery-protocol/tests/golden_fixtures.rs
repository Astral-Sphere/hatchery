//! Golden files: the exact bytes on the wire, reviewed like code.
//!
//! # Why not `insta`
//!
//! The project standardises on insta for snapshot review, but insta derives a snapshot's file
//! name from the assertion's expression and requires the name to be a **literal** — a
//! data-driven registry like `tests/support/mod.rs` cannot drive it without fifty hand-written
//! assertions duplicating the registry. The fixtures here are therefore plain pretty-printed
//! JSON, which has a second benefit: a version-compatibility fixture is readable by any
//! implementation, not only by Rust.
//!
//! # Generating
//!
//! ```text
//! UPDATE_FIXTURES=1 cargo nextest run -p hatchery-protocol
//! ```
//!
//! then review `git diff` — the diff *is* the protocol change under review. The gate
//! (`scripts/ci.sh`) exports `INSTA_UPDATE=no`, and the updater refuses to run while that is
//! set, so a CI run can never rewrite its own contract.
//!
//! # Canonical form
//!
//! Fixtures are key-sorted, because they are rendered from `serde_json::Value`s and a `Value` is
//! a `BTreeMap` in this build. That makes them deterministic — the same code always produces the
//! same bytes — which is the property the gate relies on. Object key order carries no meaning in
//! JSON, and the declaration order the typed serializers emit is pinned separately by
//! `serde_roundtrip.rs`'s `typed_serialization_keeps_declaration_order`.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::Value;

use support::{all, fixture_dir};

/// True when the developer asked for regeneration *and* the gate is not running.
fn updating() -> bool {
    let asked = std::env::var("UPDATE_FIXTURES").is_ok_and(|value| value == "1");
    let gated = std::env::var("INSTA_UPDATE").is_ok_and(|value| value == "no");
    asked && !gated
}

fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("a fixture is pretty-printable");
    text.push('\n');
    text
}

#[test]
fn every_fixture_matches_its_golden_file() {
    let dir = fixture_dir();
    if updating() {
        fs::create_dir_all(&dir).expect("create the fixture directory");
    }

    let mut written = Vec::new();
    for fixture in all() {
        let path = dir.join(format!("{}.json", fixture.stem));
        let expected = render(&fixture.value);

        if updating() {
            fs::write(&path, &expected).expect("write a fixture");
            written.push(fixture.stem);
            continue;
        }

        let actual = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "fixture {} ({}) is missing at {}: {error}\n\
                 run `UPDATE_FIXTURES=1 cargo nextest run -p hatchery-protocol` and review the diff",
                fixture.stem,
                fixture.shape,
                path.display()
            )
        });
        assert_eq!(
            actual, expected,
            "fixture {} ({}) drifted from the wire format\n\
             run `UPDATE_FIXTURES=1 cargo nextest run -p hatchery-protocol` and review the diff",
            fixture.stem, fixture.shape
        );
    }

    if updating() {
        panic!(
            "rewrote {} fixtures in {} — review the diff with `git diff`",
            written.len(),
            dir.display()
        );
    }
}

#[test]
fn no_golden_file_is_orphaned() {
    if updating() {
        // The directory is being rewritten by `every_fixture_matches_its_golden_file`, and
        // tests run in parallel: reading it now would race the writer.
        return;
    }

    let dir = fixture_dir();
    let entries = fs::read_dir(&dir).unwrap_or_else(|error| {
        panic!(
            "cannot read {}: {error}\n\
             run `UPDATE_FIXTURES=1 cargo nextest run -p hatchery-protocol` first",
            dir.display()
        )
    });

    let registered: BTreeSet<String> = all()
        .iter()
        .map(|fixture| format!("{}.json", fixture.stem))
        .collect();

    let mut present = BTreeSet::new();
    for entry in entries {
        let entry = entry.expect("readable directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        assert!(
            path.extension()
                .is_some_and(|extension| extension == "json"),
            "{} holds a file that is not a JSON fixture: {name}",
            dir.display()
        );
        assert!(
            registered.contains(&name),
            "{name} is a leftover fixture: no sample claims it. Either register it in \
             tests/support/mod.rs or delete it"
        );
        present.insert(name);
    }

    let missing: Vec<&String> = registered.difference(&present).collect();
    assert!(
        missing.is_empty(),
        "registered fixtures with no file on disk: {missing:?}"
    );
    assert_eq!(present.len(), registered.len());
}

#[test]
fn the_golden_directory_is_named_after_the_major_version() {
    let dir = fixture_dir();
    let name = dir
        .file_name()
        .expect("the fixture directory has a name")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        name,
        format!("protocol-v{}", hatchery_protocol::PROTOCOL_MAJOR),
        "compatibility is decided per major, so the fixtures must be filed per major"
    );
    assert!(
        !Path::new("tests/fixtures").join("protocol-v0").exists(),
        "a v0 fixture set would be dead weight: nothing ever released it"
    );
}
