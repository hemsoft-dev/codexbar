//! Shared score rendering and fleet aggregation for the dashboard.
use crate::{account_table::AccountState, settings_hub::SettingsHub};
use chrono::{DateTime, Utc};
use codexbar_core::{
    AccountSnapshot, Metric,
    sus::{self, Band, Score},
};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, tooltip::Tooltip};
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};
use std::collections::{BTreeMap, HashMap};

pub fn badge(score: Option<Score>, id: &str, label: &str, cx: &App) -> AnyElement {
    let text = score
        .map(|s| format!("SUS {}", s.value()))
        .unwrap_or_else(|| "SUS n/a".into());
    let (color, band) = match score.map(Score::band) {
        Some(Band::Green) => (cx.theme().success, "green"),
        Some(Band::Yellow) => (
            gpui_kit::rgb(if cx.theme().is_dark() { 0xe3b341 } else { 0x9a6700 }).into(),
            "yellow",
        ),
        Some(Band::Red) => (cx.theme().danger, "red"),
        None => (cx.theme().muted_foreground, "unavailable"),
    };
    let spoken = format!("{label}: {text}, {band}");
    let tooltip:gpui_kit::SharedString="Weekly Sub Utilization Score, 0–100. 100 means using the full allowance at reset. Green ≥90, yellow ≥75, red <75. n/a means too little data, a window younger than 12 hours, or no quota forecast.".into();
    div()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label(spoken)
        .flex_none()
        .text_xs()
        .font_semibold()
        .text_color(color)
        .child(text)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}
pub fn metric(
    account: &AccountSnapshot,
    metric: &Metric,
    now: DateTime<Utc>,
    id: &str,
    cx: &App,
) -> Option<AnyElement> {
    if !account.supports_sus() || !account.is_status_metric(metric) {
        return None;
    }
    metric_badge(metric, now, id, cx)
}
fn metric_badge(metric: &Metric, now: DateTime<Utc>, id: &str, cx: &App) -> Option<AnyElement> {
    if sus::is_weekly(metric) {
        Some(badge(sus::weekly(metric, now), id, metric.label(), cx))
    } else if matches!(metric,Metric::Activity{label,..} if label=="Turns/7d") {
        Some(badge(None, id, "Tracked activity has no quota", cx))
    } else {
        None
    }
}

pub fn fleet(
    accounts: &[AccountSnapshot],
    states: &HashMap<String, AccountState>,
    owned: &HashMap<String, String>,
    now: DateTime<Utc>,
    cx: &App,
) -> sus::Fleet {
    let hub = SettingsHub::global(cx);
    let mut subscriptions = BTreeMap::new();
    for account in accounts {
        if states
            .get(account.id().as_str())
            .is_some_and(|state| !matches!(state, AccountState::Fresh))
        {
            continue;
        }
        let score = sus::subscription_score(account, now);
        let record = hub
            .settings()
            .accounts()
            .iter()
            .filter(|r| r.enabled)
            .find(|r| Some(r.id.as_str()) == account.subscription_record())
            .or_else(|| {
                hub.settings().accounts().iter().filter(|r| r.enabled).find(|r| {
                    account.subscription_record().is_none()
                        && (r.id == account.id().as_str()
                            || owned.iter().any(|(record, account_id)| {
                                record.split('#').next() == Some(r.id.as_str()) && account_id == account.id().as_str()
                            }))
                })
            });
        let price = record
            .and_then(|r| r.sus_price.as_ref())
            .or_else(|| account.subscription_price())
            .and_then(|p| p.at(now));
        subscriptions.entry(account.id().as_str()).or_insert((score, price));
    }
    sus::fleet(subscriptions.into_values())
}

#[cfg(any(test, feature = "capture"))]
pub fn fixture(base: DateTime<Utc>, phase: u8) -> Vec<AccountSnapshot> {
    use chrono::Duration;
    use codexbar_core::{AccountId, Pace, Provider};
    let window = |label: &str, used: f64, rate: f64, hours: i64| Metric::Window {
        label: label.into(),
        used,
        resets_at: base + Duration::hours(hours),
        pace: Some(Pace::per_hour(rate)),
        length: Some(Duration::days(7)),
    };
    let account = |id: &str, provider, metrics| {
        AccountSnapshot::new(AccountId::new(id), provider, metrics, base).with_label("Demo")
    };
    let price = codexbar_core::sus::Price {
        monthly_cents: Some(20_000),
        scheduled: Some((Some(10_000), base + Duration::seconds(1))),
    };
    let claude = account(
        "claude-personal",
        Provider::Claude,
        vec![
            Metric::Window {
                label: "5-hour window".into(),
                used: 0.2,
                resets_at: base + Duration::hours(2),
                pace: None,
                length: Some(Duration::hours(5)),
            },
            window("Weekly", 0.584, 0.008, 92),
        ],
    )
    .with_plan(if phase == 1 { "Max 5x" } else { "Max 20x" })
    .with_subscription_price(if phase == 2 {
        codexbar_core::sus::Price {
            monthly_cents: None,
            scheduled: None,
        }
    } else {
        price.clone()
    });
    let codex = account(
        "codex-personal",
        Provider::Codex,
        if phase == 7 {
            vec![window("Weekly", 1.1, 0.02, 100)]
        } else {
            vec![window("Weekly", 1. - 25. * 1.08 / 88., 1.08 / 88., 113)]
        },
    )
    .with_plan(if phase == 1 { "Pro Lite" } else { "Pro" })
    .with_subscription_price(price);
    let mut argon = window("Argon weekly", 0.42, 0.42 / 96., 96);
    if phase == 3 {
        argon = Metric::Window {
            label: "Argon weekly".into(),
            used: 0.01,
            resets_at: base + Duration::days(7) - Duration::hours(6),
            pace: None,
            length: Some(Duration::days(7)),
        };
    }
    let ag = account(
        "gemini-personal",
        Provider::Gemini,
        vec![
            window("Gemini Models weekly", 0.46, 0.46 / 96., 96),
            argon,
            window("Other models weekly", 0.5, 0.5 / 96., 96),
        ],
    )
    .with_plan("Google AI Pro")
    .with_subscription_price(codexbar_core::sus::Price::fixed(1999));
    let x = account(
        "x-subscription",
        Provider::XSubscription,
        if phase == 6 {
            vec![Metric::Window {
                label: "Weekly SuperGrok limit".into(),
                used: 0.74,
                resets_at: base + Duration::hours(143),
                pace: Some(Pace::per_hour(0.02)),
                length: Some(Duration::hours(167)),
            }]
        } else if phase == 4 {
            vec![window("Weekly SuperGrok limit", 0.4, 0.4 / 96., 96)]
        } else {
            vec![Metric::Activity {
                label: "Turns/7d".into(),
                count: 1,
            }]
        },
    )
    .with_message("Synthetic fixture. Activity covers this CLI home only; other activity is unknown.");
    let mut accounts = vec![claude, codex, ag, x];
    if phase == 5 {
        accounts.push(
            account(
                "codex-second",
                Provider::Codex,
                vec![window("Weekly", 0.5, 0.5 / 96., 96)],
            )
            .with_plan("Pro Lite")
            .with_subscription_price(codexbar_core::sus::Price::fixed(10_000)),
        );
    }
    accounts
}

#[cfg(any(test, feature = "capture"))]
struct CaptureClock(DateTime<Utc>);
#[cfg(any(test, feature = "capture"))]
impl gpui_kit::Global for CaptureClock {}
#[cfg(any(test, feature = "capture"))]
pub fn capture_time(now: DateTime<Utc>, cx: &mut App) {
    cx.set_global(CaptureClock(now));
}
pub fn now(cx: &App) -> DateTime<Utc> {
    #[cfg(any(test, feature = "capture"))]
    if let Some(clock) = cx.try_global::<CaptureClock>() {
        return clock.0;
    }
    let _ = cx;
    Utc::now()
}
