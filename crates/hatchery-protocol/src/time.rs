//! Wall-clock timestamps.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC.
///
/// A single integer instead of an RFC 3339 string or `chrono::DateTime`: the database column is
/// `INTEGER` (ADR-0010), the wire form must not depend on a formatting crate, and every consumer
/// in the workspace only compares and renders it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: Self = Self(0);

    /// Wraps a millisecond count read back from storage.
    #[must_use]
    pub const fn from_unix_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The millisecond count, for storing or comparing.
    #[must_use]
    pub const fn as_unix_millis(self) -> i64 {
        self.0
    }

    /// The current time.
    ///
    /// A clock set before 1970 is saturated to [`Timestamp::UNIX_EPOCH`] rather than panicking:
    /// a wrong clock must not take the daemon down.
    #[must_use]
    pub fn now() -> Self {
        Self::from(SystemTime::now())
    }

    /// True when this instant is not before `earlier`.
    #[must_use]
    pub fn is_after(self, earlier: Self) -> bool {
        self.0 > earlier.0
    }
}

impl From<SystemTime> for Timestamp {
    fn from(time: SystemTime) -> Self {
        let millis = time.duration_since(UNIX_EPOCH).map_or(0, |since_epoch| {
            i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
        });
        Self(millis)
    }
}

impl From<Timestamp> for SystemTime {
    fn from(timestamp: Timestamp) -> Self {
        let millis = u64::try_from(timestamp.0).unwrap_or(0);
        UNIX_EPOCH + Duration::from_millis(millis)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_zero_millis() {
        assert_eq!(Timestamp::from(UNIX_EPOCH), Timestamp::UNIX_EPOCH);
        assert_eq!(Timestamp::UNIX_EPOCH.as_unix_millis(), 0);
    }

    #[test]
    fn pre_epoch_clocks_saturate_instead_of_panicking() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(60);
        assert_eq!(Timestamp::from(before_epoch), Timestamp::UNIX_EPOCH);
        // And the reverse direction is clamped rather than wrapping.
        assert_eq!(
            SystemTime::from(Timestamp::from_unix_millis(-5)),
            UNIX_EPOCH
        );
    }

    #[test]
    fn now_is_not_ordered_before_the_epoch() {
        assert!(Timestamp::now().is_after(Timestamp::UNIX_EPOCH));
    }

    #[test]
    fn wire_form_is_a_number() {
        let timestamp = Timestamp::from_unix_millis(1_700_000_000_000);
        assert_eq!(
            serde_json::to_string(&timestamp).expect("serialize"),
            "1700000000000",
            "the database column is INTEGER, so the wire form must be a plain number"
        );
        assert_eq!(
            serde_json::from_str::<Timestamp>("1700000000000").expect("deserialize"),
            timestamp
        );
    }

    #[test]
    fn system_time_roundtrip_keeps_millisecond_precision() {
        let timestamp = Timestamp::from_unix_millis(1_234_567_890_123);
        assert_eq!(Timestamp::from(SystemTime::from(timestamp)), timestamp);
    }
}
