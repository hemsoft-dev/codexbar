use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// How dates and times read on screen (#84). The app formats them in the Windows user's own locale (12- or 24-hour
/// time, local month and weekday names, local order); `English` is the built-in style the tests use.
pub trait DateStyle: Sync {
    /// A time of day: "09:00", or "9:00 AM" where that is the custom.
    fn time(&self, at: NaiveDateTime) -> String;
    /// A short weekday: "Thu".
    fn weekday(&self, at: NaiveDate) -> String;
    /// A month and day: "Nov 1", or "1. Nov." in German.
    fn month_day(&self, at: NaiveDate) -> String;

    /// "Thu 09:00".
    fn weekday_time(&self, at: NaiveDateTime) -> String {
        format!("{} {}", self.weekday(at.date()), self.time(at))
    }

    /// "Nov 1 09:00".
    fn month_day_time(&self, at: NaiveDateTime) -> String {
        format!("{} {}", self.month_day(at.date()), self.time(at))
    }

    /// "Thu Nov 1, 09:00".
    fn full(&self, at: NaiveDateTime) -> String {
        format!(
            "{} {}, {}",
            self.weekday(at.date()),
            self.month_day(at.date()),
            self.time(at)
        )
    }
}

/// English with a 24-hour clock: the built-in style.
pub struct English;

impl DateStyle for English {
    fn time(&self, at: NaiveDateTime) -> String {
        at.format("%H:%M").to_string()
    }

    fn weekday(&self, at: NaiveDate) -> String {
        at.format("%a").to_string()
    }

    fn month_day(&self, at: NaiveDate) -> String {
        at.format("%b %-d").to_string()
    }
}

/// Describes when a limit resets, relative to `now` and in the viewer's timezone `tz`, in the English style.
pub fn reset_label<Tz: TimeZone>(resets_at: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String {
    reset_label_in(resets_at, now, tz, &English)
}

/// Describes when a limit resets, relative to `now`, in the viewer's timezone `tz` and date style.
///
/// Under a day reads as a countdown ("in 38m", "in 1h 12m"), under a week as a weekday and time
/// ("Thu 09:00"), and anything later as a date ("Nov 1"). Provider instants are only converted for display.
pub fn reset_label_in<Tz: TimeZone>(
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
    tz: &Tz,
    style: &dyn DateStyle,
) -> String {
    let remaining = resets_at - now;
    if remaining <= Duration::zero() {
        return "now".to_owned();
    }
    if remaining < Duration::days(1) {
        return format!("in {}", countdown(remaining));
    }
    let local = resets_at.with_timezone(tz).naive_local();
    if remaining < Duration::days(7) {
        style.weekday_time(local)
    } else {
        style.month_day(local.date())
    }
}

/// Compact duration: "25 min", "38m", "1h 12m", "2d 4h". Rounds to whole minutes.
pub fn countdown(duration: Duration) -> String {
    let minutes = (duration.num_seconds() + 30) / 60;
    let (days, hours, mins) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m"),
        (0, _) if mins == 0 => format!("{hours}h"),
        (0, _) => format!("{hours}h {mins}m"),
        _ if hours == 0 => format!("{days}d"),
        _ => format!("{days}d {hours}h"),
    }
}

/// "Updated 14s ago" style age of a snapshot.
pub fn age_label(fetched_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let age = (now - fetched_at).max(Duration::zero());
    if age < Duration::minutes(1) {
        format!("{}s ago", age.num_seconds())
    } else if age < Duration::hours(1) {
        format!("{}m ago", age.num_minutes())
    } else {
        format!("{} ago", countdown(age))
    }
}

/// "1:46" style countdown to the next refresh.
pub fn clock_countdown(duration: Duration) -> String {
    let secs = duration.num_seconds().max(0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn now() -> DateTime<Utc> {
        // Tuesday 2026-10-06 22:00 in UTC-4.
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    fn eastern() -> FixedOffset {
        FixedOffset::west_opt(4 * 3600).unwrap()
    }

    /// A 12-hour, day-first style, like en-GB with a 12-hour clock.
    struct DayFirst;

    impl DateStyle for DayFirst {
        fn time(&self, at: NaiveDateTime) -> String {
            at.format("%-I:%M %p").to_string()
        }

        fn weekday(&self, at: NaiveDate) -> String {
            at.format("%a").to_string()
        }

        fn month_day(&self, at: NaiveDate) -> String {
            at.format("%-d %b").to_string()
        }
    }

    #[test]
    fn reset_labels_follow_the_date_style() {
        let thursday_nine = now() + Duration::hours(35);
        assert_eq!(
            reset_label_in(thursday_nine, now(), &eastern(), &DayFirst),
            "Thu 9:00 AM"
        );
        let nov_first = "2026-11-01T16:00:00Z".parse().unwrap();
        assert_eq!(reset_label_in(nov_first, now(), &eastern(), &DayFirst), "1 Nov");
        // Countdowns and the instant itself don't depend on the style.
        assert_eq!(
            reset_label_in(now() + Duration::minutes(38), now(), &eastern(), &DayFirst),
            "in 38m"
        );
    }

    #[test]
    fn reset_label_under_a_day_returns_countdown() {
        assert_eq!(reset_label(now() + Duration::minutes(38), now(), &eastern()), "in 38m");
        assert_eq!(
            reset_label(now() + Duration::minutes(72), now(), &eastern()),
            "in 1h 12m"
        );
    }

    #[test]
    fn reset_label_within_week_returns_local_weekday_and_time() {
        let thursday_nine = "2026-10-08T13:00:00Z".parse().unwrap();
        assert_eq!(reset_label(thursday_nine, now(), &eastern()), "Thu 09:00");
    }

    #[test]
    fn reset_label_beyond_week_returns_local_date() {
        let nov_first = "2026-11-01T04:00:00Z".parse().unwrap();
        assert_eq!(reset_label(nov_first, now(), &eastern()), "Nov 1");
    }

    #[test]
    fn reset_label_in_past_returns_now() {
        assert_eq!(reset_label(now() - Duration::minutes(1), now(), &eastern()), "now");
    }

    #[test]
    fn countdown_mixed_units_formats_compactly() {
        assert_eq!(countdown(Duration::minutes(25)), "25m");
        assert_eq!(countdown(Duration::hours(2)), "2h");
        assert_eq!(countdown(Duration::hours(52)), "2d 4h");
        assert_eq!(countdown(Duration::days(3)), "3d");
    }

    #[test]
    fn age_label_seconds_and_minutes_formats() {
        assert_eq!(age_label(now() - Duration::seconds(14), now()), "14s ago");
        assert_eq!(age_label(now() - Duration::minutes(5), now()), "5m ago");
        assert_eq!(age_label(now() + Duration::seconds(3), now()), "0s ago");
    }

    #[test]
    fn clock_countdown_pads_seconds() {
        assert_eq!(clock_countdown(Duration::seconds(106)), "1:46");
        assert_eq!(clock_countdown(Duration::seconds(-5)), "0:00");
    }
}
