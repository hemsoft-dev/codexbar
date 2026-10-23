use chrono::{DateTime, Duration, Utc};
use codexbar_core::Pace;

/// Pace is unreliable early in a period: a few minutes of a weekly window extrapolate to a false lockout.
/// It is only projected once this much time, and this share of the period, has elapsed.
const MIN_ELAPSED_MINUTES: i64 = 10;
const MIN_ELAPSED_FRACTION: f64 = 0.1;

/// Usage so far divided by time elapsed in the period `[start, resets_at)`, or `None` while the period is too young.
pub fn elapsed_pace(used: f64, start: DateTime<Utc>, resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Pace> {
    let length = resets_at - start;
    let minimum = Duration::minutes(MIN_ELAPSED_MINUTES).max(Duration::seconds(
        (length.num_seconds() as f64 * MIN_ELAPSED_FRACTION) as i64,
    ));
    elapsed_pace_after(used, start, resets_at, now, minimum)
}

/// Projects after an explicit maturity point. Antigravity weekly pools mature with SUS at 12 hours (#262).
pub fn elapsed_pace_after(
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
