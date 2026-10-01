//! Date strings without a time crate: civil-date math (Howard Hinnant's algorithm), the same
//! math the fixture recorder uses.

/// Today as `YYYY-MM-DD`.
#[must_use]
pub fn humantime_date() -> String {
    // Days since epoch → civil date; the same math the fixture recorder uses, kept local so
    // the daemon pulls no new dependency for a date.
    let days = hatchery_protocol::Timestamp::now().as_unix_millis() / 86_400_000;
    civil_from_days(days)
}

/// A `YYYY-MM-DD` string `days_before` days before today. `YYYY-MM-DD` sorts
/// lexicographically, so these strings compare chronologically — the retention pruner leans
/// on that.
#[must_use]
pub fn date_days_before(days_before: i64) -> String {
    let days = hatchery_protocol::Timestamp::now().as_unix_millis() / 86_400_000 - days_before;
    civil_from_days(days)
}

fn civil_from_days(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates_survive_the_math() {
        assert_eq!(civil_from_days(0), "1970-01-01");
        assert_eq!(civil_from_days(19_723), "2024-01-01"); // 54 years, incl. 13 leap days
        assert_eq!(civil_from_days(20_726), "2026-09-30"); // today, pinned while it is today
    }

    #[test]
    fn the_retention_cutoff_sorts_before_today() {
        let today = humantime_date();
        let cutoff = date_days_before(14);
        assert!(cutoff < today, "{cutoff} should sort before {today}");
        assert_eq!(date_days_before(0), today);
    }
}
