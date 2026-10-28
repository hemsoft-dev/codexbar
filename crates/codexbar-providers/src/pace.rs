use chrono::{DateTime, Duration, Utc};
use codexbar_core::Pace;

/// Usage so far divided by time elapsed in the period `[start, resets_at)`, or `None` while the period is too young.
pub fn elapsed_pace(used: f64, start: DateTime<Utc>, resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Pace> {
    elapsed_pace_after(used, start, resets_at, now, codexbar_core::sus::WEEKLY_MATURITY)
}

fn elapsed_pace_after(
    used: f64,
    start: DateTime<Utc>,
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
    minimum: Duration,
) -> Option<Pace> {
    let elapsed = now - start;
    if resets_at <= start || now >= resets_at || elapsed < minimum || !used.is_finite() || used <= 0.0 {
        return None;
    }
    Some(Pace::per_hour(used / (elapsed.num_seconds() as f64 / 3600.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    #[test]
    fn explicit_maturity_rejects_invalid_future_and_expired_periods() {
        let at = now();
        for (start, reset) in [
            (at, at),
            (at, at - Duration::hours(1)),
            (at + Duration::hours(1), at + Duration::days(7)),
            (at - Duration::days(7), at),
        ] {
            assert_eq!(elapsed_pace_after(0.03, start, reset, at, Duration::hours(12)), None);
        }
        let zero = elapsed_pace_after(
            0.,
            at - Duration::hours(12),
            at + Duration::hours(156),
            at,
            Duration::hours(12),
        );
        assert_eq!(zero, None);
    }

    #[test]
    fn all_periods_mature_at_one_hour() {
        // Calendar weeks, five-hour windows and monthly quotas all firm up together.
        for (length, maturity) in [
            (Duration::days(7), Duration::hours(1)),
            (Duration::hours(167), Duration::hours(1)),
            (Duration::hours(169), Duration::hours(1)),
            (Duration::hours(5), Duration::hours(1)),
            (Duration::days(30), Duration::hours(1)),
        ] {
            let start = now() - maturity;
            let early = now() - Duration::seconds(1);
            assert_eq!(elapsed_pace(0.1, start, start + length, early), None, "{length}");
            assert!(elapsed_pace(0.1, start, start + length, now()).is_some(), "{length}");
        }
    }

    #[test]
    fn elapsed_pace_young_period_returns_none() {
        let start = now() - Duration::minutes(20);
        assert_eq!(elapsed_pace(0.02, start, start + Duration::days(7), now()), None);
    }

    #[test]
    fn elapsed_pace_mature_period_divides_by_elapsed_hours() {
        let start = now() - Duration::hours(2);
        let pace = elapsed_pace(0.4, start, start + Duration::hours(5), now()).unwrap();
        assert!((pace.fraction_per_hour() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn elapsed_pace_after_reset_returns_none() {
        let start = now() - Duration::hours(6);
        assert_eq!(elapsed_pace(0.4, start, start + Duration::hours(5), now()), None);
    }
}
