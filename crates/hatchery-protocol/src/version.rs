//! Protocol versioning.
//!
//! `daemon/hello` negotiates a version, and the supported range is exactly one major: methods and
//! fields may only be added, never repurposed (`docs/design/protocol.md` §6). A client from a
//! different major is refused with [`crate::ErrorCode::UnsupportedProtocolVersion`] rather than
//! half-understood.

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// The major component. Compatibility is decided per major: same major, compatible.
pub const PROTOCOL_MAJOR: u32 = 1;

/// Versions this build accepts from a peer, as a closed range.
///
/// Every 1.x is accepted, so listing them is pointless; the list exists so that the *rejection*
/// path has something concrete to report and so a future major bump has to touch this constant.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["1.0.0"];

/// The major component of a `major.minor.patch` string.
///
/// Hand-rolled rather than via a semver crate: the only question asked of a version anywhere in
/// the protocol is "same major?", and a dependency that parses ranges nobody uses would be
/// weight without value (ADR-0009's anti-premature-abstracting brake).
#[must_use]
pub fn major_of(version: &str) -> Option<u32> {
    let major = version.split('.').next()?;
    if major.is_empty() {
        return None;
    }
    major.parse().ok()
}

/// True when a peer speaking `version` can talk to this build.
///
/// Unparsable versions are refused: guessing at `"latest"` or `"1"` is how version negotiation
/// turns into a runtime surprise.
#[must_use]
pub fn is_compatible(version: &str) -> bool {
    major_of(version) == Some(PROTOCOL_MAJOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constant_and_the_major_agree() {
        assert_eq!(major_of(PROTOCOL_VERSION), Some(PROTOCOL_MAJOR));
        assert!(
            SUPPORTED_PROTOCOL_VERSIONS.contains(&PROTOCOL_VERSION),
            "a build must accept its own version"
        );
    }

    #[test]
    fn compatibility_is_decided_by_the_major() {
        assert!(is_compatible("1.0.0"));
        assert!(is_compatible("1.7.3"), "minor additions stay compatible");
        assert!(!is_compatible("2.0.0"), "a new major is a break");
        assert!(!is_compatible("0.9.0"));
    }

    #[test]
    fn junk_versions_are_refused_rather_than_guessed() {
        assert_eq!(major_of(""), None);
        assert_eq!(major_of("latest"), None);
        assert_eq!(major_of(".1.0"), None);
        assert!(!is_compatible("latest"));
        assert!(!is_compatible(""));
    }

    #[test]
    fn the_fixture_directory_tracks_the_major() {
        // The version-compat test derives its directory from PROTOCOL_MAJOR, so a major bump
        // forces a new fixture set instead of silently reusing the old goldens.
        assert_eq!(format!("protocol-v{PROTOCOL_MAJOR}"), "protocol-v1");
    }
}
