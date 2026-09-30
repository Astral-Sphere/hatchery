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

/// The major component of a `major.minor[.patch]` string.
///
/// A version must carry at least two dot-separated numeric segments: `"1.0"` and `"1.0.0"` parse,
/// `"1"`, `"1."` and `"1.x"` do not. A bare major is refused rather than trusted, because
/// [`is_compatible`] decides per major and `"1"` says nothing about which minor a peer speaks.
/// Later segments are not inspected, so a pre-release spelling (`"1.0.0-rc1"`) still negotiates.
///
/// Hand-rolled rather than via a semver crate: the only question asked of a version anywhere in
/// the protocol is "same major?", and a dependency that parses ranges nobody uses would be
/// weight without value (ADR-0009's anti-premature-abstracting brake).
#[must_use]
pub fn major_of(version: &str) -> Option<u32> {
    let mut segments = version.split('.');
    let major = segments.next()?;
    let minor = segments.next()?;
    if !is_numeric(major) || !is_numeric(minor) {
        return None;
    }
    major.parse().ok()
}

fn is_numeric(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
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
        assert_eq!(
            major_of("1"),
            None,
            "a bare major says nothing about the minor"
        );
        assert_eq!(major_of("1."), None);
        assert_eq!(major_of("1.x"), None);
        assert!(!is_compatible("latest"));
        assert!(!is_compatible(""));
        for junk in ["1", "1.", "1.x"] {
            assert!(!is_compatible(junk), "{junk} must not pass as compatible");
        }
    }

    #[test]
    fn a_major_and_a_minor_are_enough_to_negotiate() {
        assert_eq!(major_of("1.0"), Some(1));
        assert_eq!(major_of("1.0.0"), Some(1));
        assert!(is_compatible("1.0"));
        assert_eq!(
            major_of("1.0.0-rc1"),
            Some(1),
            "a pre-release of a supported major still negotiates"
        );
    }

    #[test]
    fn the_fixture_directory_tracks_the_major() {
        // The version-compat test derives its directory from PROTOCOL_MAJOR, so a major bump
        // forces a new fixture set instead of silently reusing the old goldens.
        assert_eq!(format!("protocol-v{PROTOCOL_MAJOR}"), "protocol-v1");
    }
}
