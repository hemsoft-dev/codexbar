use chrono::{DateTime, Duration, Utc};
use codexbar_core::{
    Pace,
    sus::{WEEKLY_MATURITY, is_week_length},
};

/// Pace is unreliable early in a period: a few minutes of a weekly window extrapolate to a false lockout.
/// A week projects from the SUS maturity; any other period once this much time, and this share of it, has elapsed.
const MIN_ELAPSED_MINUTES: i64 = 10;
const MIN_ELAPSED_FRACTION: f64 = 0.1;

/// Usage so far divided by time elapsed in the period `[start, resets_at)`, or `None` while the period is too young.
pub fn elapsed_pace(used: f64, start: DateTime<Utc>, resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Pace> {
    elapsed_pace_after(used, start, resets_at, now, maturity(resets_at - start))
}

/// How long a period of `length` runs before it projects.
fn maturity(length: Duration) -> Duration {
    if is_week_length(length) {
        WEEKLY_MATURITY
    } else {
        Duration::minutes(MIN_ELAPSED_MINUTES).max(Duration::seconds(
            (length.num_seconds() as f64 * MIN_ELAPSED_FRACTION) as i64,
        ))
    }
}

fn elapsed_pace_after(
    used: f64,
    start: DateTime<Utc>,
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
    minimum: Duration,
) -> Option<Pace> {
    let elapsed = now - start;
    if resets_at <= start || now >= resets_at || elapsed < minimum {
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
        assert_eq!(zero.unwrap().fraction_per_hour(), 0.);
    }

    #[test]
    fn weeks_mature_at_the_sus_boundary_and_other_periods_at_ten_percent() {
        // Calendar weeks across an offset change are weeks too; a five-hour window and a month keep 10%.
        for (length, maturity) in [
            (Duration::days(7), WEEKLY_MATURITY),
            (Duration::hours(167), WEEKLY_MATURITY),
            (Duration::hours(169), WEEKLY_MATURITY),
            (Duration::hours(5), Duration::minutes(30)),
            (Duration::days(30), Duration::hours(72)),
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
