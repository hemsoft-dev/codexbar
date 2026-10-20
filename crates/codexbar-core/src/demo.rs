//! Synthetic accounts for design work and first-run previews. Never shown as real data.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc, Weekday};

use crate::account::{AccountDetail, AccountId, AccountSnapshot, Provider, WindowCurve};
use crate::metric::{Currency, Metric, Money, Pace};

/// The eight demo accounts of the approved dashboard comp, relative to `now`, plus two Codex accounts whose weekly
/// window is used up (#150): one still working on credits, one blocked.
pub fn demo_accounts<Tz: TimeZone>(now: DateTime<Utc>, tz: &Tz) -> Vec<AccountSnapshot> {
    let fetched = now - Duration::seconds(14);
    let next_month = first_of_next_month(now, tz);
    let thursday_nine = next_weekday_at(now, tz, Weekday::Thu, 9);
    let in_days = |days: i64| now + Duration::days(days);

    vec![
        AccountSnapshot::new(
            AccountId::new("codex-personal"),
            Provider::Codex,
            vec![
                Metric::Window {
                    label: "5-hour window".into(),
                    used: 0.91,
                    resets_at: now + Duration::minutes(38),
                    pace: Some(Pace::per_hour(0.216)),
                    length: Some(Duration::hours(5)),
                },
                Metric::Window {
                    label: "Weekly".into(),
                    used: 0.47,
                    resets_at: thursday_nine,
                    pace: Some(Pace::per_day(0.09)),
                    length: Some(Duration::days(7)),
                },
            ],
            fetched,
        )
        .with_label("personal")
        .with_trend(vec![
            0.31, 0.42, 0.38, 0.55, 0.47, 0.62, 0.58, 0.66, 0.6, 0.71, 0.69, 0.8, 0.84, 0.91,
        ])
        .with_detail(codex_detail()),
        AccountSnapshot::new(
            AccountId::new("copilot-work"),
            Provider::Copilot,
            vec![Metric::Quota {
                label: "Premium requests".into(),
                used: 1284,
                limit: 1500,
                resets_at: next_month,
                pace: Some(Pace::per_day(0.0064)),
                length: Some(Duration::days(30)),
            }],
            fetched,
        )
        .with_label("work")
        .with_trend(vec![
            0.52, 0.55, 0.58, 0.6, 0.63, 0.66, 0.7, 0.72, 0.75, 0.78, 0.8, 0.82, 0.84, 0.86,
        ]),
        AccountSnapshot::new(
            AccountId::new("claude-personal"),
            Provider::Claude,
            vec![
                Metric::Window {
                    label: "Weekly".into(),
                    used: 0.82,
                    resets_at: thursday_nine,
                    pace: Some(Pace::per_day(0.05)),
                    length: Some(Duration::days(7)),
                },
                Metric::Window {
                    label: "5-hour window".into(),
                    used: 0.64,
                    resets_at: now + Duration::minutes(72),
                    pace: Some(Pace::per_hour(0.12)),
                    length: Some(Duration::hours(5)),
                },
                // #215: Extra usage bills in the account's own currency, so the Spend view totals euros apart.
                Metric::Spend {
                    label: "Extra usage".into(),
                    spent: Money::new(420, Currency::Eur),
                    limit: Some(Money::new(2000, Currency::Eur)),
                    resets_at: None,
                    pace: None,
                },
            ],
            fetched,
        )
        .with_label("personal")
        .with_trend(vec![
            0.4, 0.44, 0.5, 0.47, 0.55, 0.6, 0.58, 0.63, 0.66, 0.7, 0.72, 0.76, 0.79, 0.82,
        ]),
        AccountSnapshot::new(
            AccountId::new("cursor"),
            Provider::Cursor,
            vec![
                Metric::Window {
                    label: "Included usage".into(),
                    used: 0.71,
                    resets_at: in_days(14),
                    pace: Some(Pace::per_day(0.015)),
                    length: Some(Duration::days(30)),
                },
                // #215: on-demand spend against its monthly limit, with a pace, so the Spend view projects it.
                Metric::Spend {
                    label: "On-demand".into(),
                    spent: Money::from_cents(1240),
                    limit: Some(Money::from_cents(5000)),
                    resets_at: Some(in_days(14)),
                    pace: Some(Pace::per_day(0.02)),
                },
            ],
            fetched,
        )
        .with_trend(vec![
            0.5, 0.52, 0.53, 0.55, 0.57, 0.58, 0.6, 0.62, 0.63, 0.65, 0.66, 0.68, 0.7, 0.71,
        ]),
        AccountSnapshot::new(
            AccountId::new("copilot-hemsoft"),
            Provider::Copilot,
            vec![Metric::Quota {
                label: "Premium requests".into(),
                used: 612,
                limit: 1500,
                resets_at: next_month,
                pace: Some(Pace::per_day(0.012)),
                length: Some(Duration::days(30)),
            }],
            fetched,
        )
        .with_label("HemSoft")
        // #217: last month's requests climb to nearly the whole allowance, then the monthly reset drops them to a
        // few percent, a sharp change both trend charts must draw without leaving their plots.
        .with_trend(vec![
            0.6, 0.65, 0.7, 0.74, 0.79, 0.83, 0.88, 0.93, 0.97, 0.03, 0.12, 0.22, 0.32, 0.41,
        ]),
        AccountSnapshot::new(
            AccountId::new("openrouter"),
            Provider::OpenRouter,
            vec![Metric::Balance {
                label: "Credits".into(),
                remaining: Money::from_cents(1842),
                burn_per_day: Some(Money::from_cents(310)),
            }],
            fetched,
        )
        // Balances trend in dollars, as History stores them.
        .with_trend(vec![
            58.72, 55.6, 52.48, 49.41, 46.3, 43.22, 40.1, 37.02, 33.9, 30.81, 27.7, 24.62, 21.5, 18.42,
        ]),
        AccountSnapshot::new(
            AccountId::new("opencode"),
            Provider::OpenCode,
            vec![
                Metric::Window {
                    label: "Go usage".into(),
                    used: 0.38,
                    resets_at: in_days(9),
                    pace: Some(Pace::per_day(0.02)),
                    length: None,
                },
                Metric::Balance {
                    label: "Zen balance".into(),
                    remaining: Money::from_cents(1260),
                    burn_per_day: None,
                },
            ],
            fetched,
        )
        .with_trend(vec![
            0.2, 0.22, 0.21, 0.25, 0.27, 0.26, 0.3, 0.29, 0.31, 0.33, 0.32, 0.35, 0.36, 0.38,
        ]),
        AccountSnapshot::new(
            AccountId::new("codex-team"),
            Provider::Codex,
            vec![
                Metric::Window {
                    label: "5-hour window".into(),
                    used: 0.12,
                    resets_at: now + Duration::minutes(210),
                    pace: None,
                    length: Some(Duration::hours(5)),
                },
                Metric::Window {
                    label: "Weekly".into(),
                    used: 1.0,
                    resets_at: thursday_nine,
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                Metric::Credits {
                    label: "Credits".into(),
                    balance: Some(18_400.0),
                    start: Some(50_000.0),
                },
            ],
            fetched,
        )
        .with_label("team")
        .with_credits_placed()
        .with_trend(vec![
            50_000.0, 48_200.0, 45_900.0, 43_100.0, 40_800.0, 37_500.0, 34_900.0, 32_000.0, 29_600.0, 27_100.0,
            24_800.0, 22_300.0, 20_100.0, 18_400.0,
        ]),
        AccountSnapshot::new(
            AccountId::new("codex-work"),
            Provider::Codex,
            vec![
                Metric::Window {
                    label: "Weekly".into(),
                    used: 1.0,
                    resets_at: thursday_nine,
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                Metric::Credits {
                    label: "Credits".into(),
                    balance: Some(0.0),
                    start: Some(5_000.0),
                },
            ],
            fetched,
        )
        .with_label("work")
        .with_credits_placed()
        .with_trend(vec![
            0.3, 0.38, 0.45, 0.52, 0.6, 0.66, 0.72, 0.79, 0.85, 0.9, 0.94, 0.97, 1.0, 1.0,
        ]),
        AccountSnapshot::new(
            AccountId::new("moonshot"),
            Provider::Moonshot,
            vec![Metric::Balance {
                label: "Balance".into(),
                remaining: Money::from_cents(785),
                burn_per_day: None,
            }],
            fetched,
        )
        .with_trend(vec![
            8.6, 8.6, 8.47, 8.47, 8.35, 8.35, 8.21, 8.21, 8.1, 8.1, 7.98, 7.98, 7.85, 7.85,
        ]),
        // #103: a count with no limit, so its trend is the count too.
        AccountSnapshot::new(
            AccountId::new("greptile"),
            Provider::Greptile,
            vec![Metric::Activity {
                label: "Completed reviews".into(),
                count: 37,
            }],
            fetched,
        )
        .with_label("HemSoft")
        .with_message("Also 2 pending.")
        .with_trend(vec![
            12.0, 14.0, 15.0, 15.0, 18.0, 21.0, 22.0, 24.0, 27.0, 29.0, 30.0, 33.0, 35.0, 37.0,
        ]),
        // #104: spend with no limit, so nothing is projected against one.
        AccountSnapshot::new(
            AccountId::new("github-billing"),
            Provider::GitHubBilling,
            vec![
                Metric::Spend {
                    label: "Net spend".into(),
                    spent: Money::from_cents(2389),
                    limit: None,
                    resets_at: Some(next_month),
                    pace: None,
                },
                Metric::Spend {
                    label: "Gross charges".into(),
                    spent: Money::from_cents(60552),
                    limit: None,
                    resets_at: Some(next_month),
                    pace: None,
                },
                Metric::Spend {
                    label: "Discounts".into(),
                    spent: Money::from_cents(58163),
                    limit: None,
                    resets_at: Some(next_month),
                    pace: None,
                },
                // #105: included allowances, as a share of the plan's.
                Metric::Window {
                    label: "Actions minutes".into(),
                    used: 0.41,
                    resets_at: next_month,
                    pace: None,
                    length: Some(Duration::days(30)),
                },
                Metric::Window {
                    label: "Git LFS storage".into(),
                    used: 0.08,
                    resets_at: next_month,
                    pace: None,
                    length: Some(Duration::days(30)),
                },
            ],
            fetched,
        )
        .with_label("HemSoft")
        .with_message("At this pace, about $61.72 net by the end of the month.")
        .with_message("Included with Pro:")
        .with_message("Actions minutes: 1,234 minutes of 3,000 minutes included used (41%), 1,766 minutes left.")
        .with_message("Git LFS storage: 0.83 GB of 10 GB included used (8%), 9.17 GB left.")
        .with_message("Packages storage: not shown, GitHub doesn't say which package usage is free.")
        .with_message(
            "Actions: $23.89 net of $600.72 gross, $576.83 discounts. actions_linux 16,596 minutes at \
             $0.006/minute; actions_macos 8,083 minutes at $0.062/minute.",
        )
        .with_trend(vec![
            2.1, 3.4, 4.0, 5.2, 7.9, 9.0, 10.4, 12.2, 14.8, 16.0, 18.3, 20.1, 22.4, 23.89,
        ]),
        // #106: consumer Gemini Apps limits, with Gemini's own resets.
        AccountSnapshot::new(
            AccountId::new("gemini"),
            Provider::Gemini,
            vec![
                Metric::Window {
                    label: "Gemini Apps five-hour".into(),
                    used: 0.18,
                    resets_at: now + Duration::minutes(150),
                    pace: None,
                    length: Some(Duration::hours(5)),
                },
                Metric::Window {
                    label: "Gemini Apps weekly".into(),
                    used: 0.41,
                    resets_at: thursday_nine,
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                // #107: coding quotas from the Antigravity CLI.
                Metric::Window {
                    label: "Gemini Models five-hour".into(),
                    used: 0.0,
                    resets_at: now + Duration::minutes(290),
                    pace: None,
                    length: Some(Duration::hours(5)),
                },
                Metric::Window {
                    label: "Gemini Models weekly".into(),
                    used: 0.12,
                    resets_at: thursday_nine,
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                Metric::Window {
                    label: "Other models five-hour".into(),
                    used: 0.04,
                    resets_at: now + Duration::minutes(290),
                    pace: None,
                    length: Some(Duration::hours(5)),
                },
                Metric::Window {
                    label: "Other models weekly".into(),
                    used: 0.15,
                    resets_at: thursday_nine,
                    pace: None,
                    length: Some(Duration::days(7)),
                },
            ],
            fetched,
        )
        .with_label("Personal")
        .with_trend(vec![
            0.05, 0.1, 0.08, 0.15, 0.2, 0.12, 0.18, 0.25, 0.22, 0.3, 0.28, 0.35, 0.38, 0.41,
        ]),
        // #108: a paid plan's weekly pool and Extra Usage Credits, kept apart.
        AccountSnapshot::new(
            AccountId::new("grok"),
            Provider::Grok,
            vec![
                Metric::Window {
                    label: "Weekly usage".into(),
                    used: 0.27,
                    resets_at: thursday_nine,
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                Metric::Balance {
                    label: "Extra usage credits".into(),
                    remaining: Money::from_cents(500),
                    burn_per_day: None,
                },
            ],
            fetched,
        )
        .with_label("SuperGrok")
        .with_message("Plan: SuperGrok.")
        .with_trend(vec![
            0.02, 0.04, 0.05, 0.07, 0.1, 0.12, 0.13, 0.15, 0.18, 0.2, 0.21, 0.23, 0.25, 0.27,
        ]),
        // #172: the X Subscription's two weekly limits keep their own resets, apart from the credits.
        AccountSnapshot::new(
            AccountId::new("x-subscription"),
            Provider::XSubscription,
            vec![
                Metric::Window {
                    label: "Weekly SuperGrok limit".into(),
                    used: 0.12,
                    resets_at: in_days(6),
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                Metric::Window {
                    label: "Weekly Grok Bot limit".into(),
                    used: 0.58,
                    resets_at: in_days(2),
                    pace: None,
                    length: Some(Duration::days(7)),
                },
                Metric::Balance {
                    label: "Extra usage credits".into(),
                    remaining: Money::from_cents(0),
                    burn_per_day: None,
                },
            ],
            fetched,
        )
        .with_label("Personal")
        .with_trend(vec![
            0.2, 0.25, 0.3, 0.35, 0.4, 0.05, 0.1, 0.15, 0.2, 0.3, 0.38, 0.45, 0.52, 0.58,
        ]),
    ]
}

fn codex_detail() -> AccountDetail {
    // Ten-minute samples across the 5-hour window (10:00 to 15:00); the current window is still running.
    let labels = (0..=30)
        .map(|i| {
            let minutes = 10 * 60 + i * 10;
            format!("{}:{:02}", minutes / 60, minutes % 60)
        })
        .collect();
    let current = vec![
        0.02, 0.08, 0.15, 0.2, 0.24, 0.3, 0.38, 0.45, 0.47, 0.46, 0.48, 0.52, 0.55, 0.53, 0.56, 0.6, 0.63, 0.62, 0.66,
        0.7, 0.72, 0.71, 0.75, 0.79, 0.83, 0.86, 0.91,
    ];
    let previous = vec![
        0.01, 0.04, 0.08, 0.1, 0.12, 0.15, 0.2, 0.22, 0.21, 0.24, 0.27, 0.26, 0.29, 0.33, 0.3, 0.28, 0.31, 0.35, 0.38,
        0.36, 0.4, 0.44, 0.47, 0.5, 0.52, 0.55, 0.57, 0.6, 0.61, 0.63, 0.64,
    ];
    let requests = vec![
        12, 8, 6, 9, 14, 22, 35, 48, 74, 92, 81, 105, 160, 210, 286, 180, 140, 120, 96, 70, 64, 52, 41, 30,
    ];
    AccountDetail::default()
        .with_window_curve(WindowCurve::new(labels, current, previous))
        .with_requests_by_hour(requests)
}

fn first_of_next_month<Tz: TimeZone>(now: DateTime<Utc>, tz: &Tz) -> DateTime<Utc> {
    let local = now.with_timezone(tz);
    let (year, month) = if local.month() == 12 {
        (local.year() + 1, 1)
    } else {
        (local.year(), local.month() + 1)
    };
    tz.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now + Duration::days(30))
}

fn next_weekday_at<Tz: TimeZone>(now: DateTime<Utc>, tz: &Tz, weekday: Weekday, hour: u32) -> DateTime<Utc> {
    let local = now.with_timezone(tz);
    let mut date = local.date_naive();
    for _ in 0..8 {
        if date.weekday() == weekday
            && let Some(candidate) = date
                .and_hms_opt(hour, 0, 0)
                .and_then(|naive| tz.from_local_datetime(&naive).earliest())
                .map(|dt| dt.with_timezone(&Utc))
                .filter(|candidate| *candidate > now)
        {
            return candidate;
        }
        date = date.succ_opt().unwrap_or(date);
    }
    now + Duration::days(7)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::sort_by_urgency;
    use crate::severity::Severity;
    use chrono::FixedOffset;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    #[test]
    fn demo_accounts_sorted_ranks_by_severity_then_pressure() {
        let tz = FixedOffset::west_opt(4 * 3600).unwrap();
        let mut accounts = demo_accounts(now(), &tz);
        sort_by_urgency(&mut accounts, now());
        let names: Vec<String> = accounts.iter().map(AccountSnapshot::display_name).collect();
        assert_eq!(
            names,
            [
                "ChatGPT · Codex (work)",
                "ChatGPT · Codex (personal)",
                "Copilot · work",
                "Claude · personal",
                "Cursor",
                "OpenRouter",
                "ChatGPT · Codex (team)",
                "X Subscription · Personal",
                "Gemini · Personal",
                "GitHub Billing · HemSoft",
                "Copilot · HemSoft",
                "OpenCode Go + Zen",
                "Grok · SuperGrok",
                "Greptile · HemSoft",
                "Moonshot (Kimi)",
            ]
        );
    }

    #[test]
    fn demo_accounts_severities_match_comp_labels() {
        let tz = FixedOffset::west_opt(4 * 3600).unwrap();
        let accounts = demo_accounts(now(), &tz);
        let severity = |id: &str| {
            accounts
                .iter()
                .find(|a| a.id().as_str() == id)
                .unwrap()
                .assess(now())
                .severity()
        };
        assert_eq!(severity("codex-personal"), Severity::LimitSoon);
        assert_eq!(severity("copilot-work"), Severity::AtRisk);
        assert_eq!(severity("claude-personal"), Severity::Watch);
        assert_eq!(severity("cursor"), Severity::Normal);
        // #150: the used-up weekly window blocks the account without credits, not the one still on credits.
        assert_eq!(severity("codex-work"), Severity::LimitSoon);
        assert_eq!(severity("codex-team"), Severity::Normal);
        let attention = accounts
            .iter()
            .filter(|a| a.assess(now()).severity().needs_attention())
            .count();
        assert_eq!(attention, 3);
    }

    #[test]
    fn next_weekday_at_skips_past_time_today() {
        let tz = FixedOffset::west_opt(4 * 3600).unwrap();
        let thursday = next_weekday_at(now(), &tz, Weekday::Thu, 9);
        assert_eq!(thursday, "2026-10-08T13:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn first_of_next_month_december_rolls_year() {
        let tz = FixedOffset::west_opt(0).unwrap();
        let dec = "2026-12-15T00:00:00Z".parse().unwrap();
        assert_eq!(
            first_of_next_month(dec, &tz),
            "2027-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }
}
