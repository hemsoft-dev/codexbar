use chrono::{DateTime, Duration, Utc};
use codexbar_core::Pace;

/// Pace is unreliable early in a period: a few minutes of a weekly window extrapolate to a false lockout.
/// It is only projected once this much time, and this share of the period, has elapsed.
const MIN_ELAPSED_MINUTES: i64 = 10;
const MIN_ELAPSED_FRACTION: f64 = 0.1;

/// Usage so far divided by time elapsed in the period `[start, resets_at)`, or `None` while the period is too young.
pub fn elapsed_pace(used: f64, start: DateTime<Utc>, resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Pace> {
    let length = resets_at - start;
    let elapsed = now - start;
    if length <= Duration::zero() || now >= resets_at {
        return None;
    }
    let min_elapsed = Duration::minutes(MIN_ELAPSED_MINUTES).max(Duration::seconds(
        (length.num_seconds() as f64 * MIN_ELAPSED_FRACTION) as i64,
    ));
    if elapsed < min_elapsed {
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
