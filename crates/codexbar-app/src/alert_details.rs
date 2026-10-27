//! The focused account's alerts (#88): the strongest one with its context, the others below it, and whether the
//! account's status comes from observed usage or from the pace projection.

use chrono::{DateTime, Utc};
use codexbar_core::alerts::{AlertDetail, AlertKind};
use codexbar_core::{AccountSnapshot, Severity, observed_severity};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, tag::Tag, v_flex};
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Role, StatefulInteractiveElement as _,
    Styled as _, div,
};

use crate::status::severity_color;

/// The account's status from observed usage alone, when the projection raised it above that.
pub fn projected_from(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<Severity> {
    let effective = account.assess(now).severity();
    let observed = account
        .status_metrics()
        .filter(|metric| !account.credits_cover(metric) && account.evaluation_time(metric, now).is_some())
        .map(observed_severity)
        .max()
        .unwrap_or_default();
    (effective > observed).then_some(observed)
}

/// A quiet tag beside the status when the projection raised it: "Projected · now Normal".
pub fn projected_tag(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<impl IntoElement> {
    let observed = projected_from(account, now)?;
    let now_label = observed.label().unwrap_or("Normal");
    Some(
        div()
            .id("projected-status")
            .role(Role::Note)
            .test_support()
            .aria_label(format!(
                "Projected from the current pace; observed usage alone is {now_label}"
            ))
            .child(Tag::secondary().outline().child(format!("Projected · now {now_label}"))),
    )
}

/// What an alert is called on screen.
fn title(detail: &AlertDetail) -> String {
    let what = match detail.kind {
        AlertKind::Critical => "Limit soon",
        AlertKind::Warning => "At risk",
        AlertKind::Usage => "Usage alert",
        AlertKind::Balance => "Balance alert",
    };
    let basis = if detail.is_projected() { " (projected)" } else { "" };
    format!("{what}{basis}: {}", detail.metric)
}

/// The alert block for the focused account, or nothing when no alert holds.
pub fn alert_details(details: &[AlertDetail], now: DateTime<Utc>, cx: &App) -> Option<AnyElement> {
    let (strongest, others) = details.split_first()?;
    let color = severity_color(strongest.severity.max(Severity::AtRisk), cx);
    let muted = cx.theme().muted_foreground;
    let line = |ix: usize, detail: &AlertDetail| {
        let text = format!("{}. {}", title(detail), detail.summary(now));
        div()
            .id(("alert-detail", ix))
            .role(Role::Note)
            .test_support()
            .aria_label(text.clone())
            .text_sm()
            .text_color(muted)
            .child(text)
    };
    Some(
        v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(color.opacity(0.6))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(color))
                    .child(
                        div()
                            .id("alert-strongest")
                            .role(Role::Note)
                            .test_support()
                            .aria_label(format!("{}. {}", title(strongest), strongest.summary(now)))
                            .font_semibold()
                            .text_sm()
                            .child(title(strongest)),
                    ),
            )
            .child(div().text_sm().child(strongest.summary(now)))
            .children(others.iter().enumerate().map(|(ix, detail)| line(ix, detail)))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use codexbar_core::{AccountId, Metric, Pace, Provider};

    fn account(used: f64, per_hour: Option<f64>) -> AccountSnapshot {
        let now = now();
        AccountSnapshot::new(
            AccountId::new("c"),
            Provider::Claude,
            vec![Metric::Window {
                label: "Weekly".into(),
                used,
                resets_at: now + Duration::days(5),
                pace: per_hour.map(Pace::per_hour),
                length: None,
            }],
            now,
        )
    }

    fn now() -> DateTime<Utc> {
        "2026-10-07T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn projected_from_reports_the_observed_status_only_when_the_pace_raised_it() {
        // 50% used, but on pace to run out two days early: At risk, from Normal usage.
        assert_eq!(
            projected_from(&account(0.5, Some(0.2 / 24.0)), now()),
            Some(Severity::Normal)
        );
        // High usage with no pace: the status is observed, not projected.
        assert_eq!(projected_from(&account(0.96, None), now()), None);
        assert_eq!(projected_from(&account(0.2, Some(0.001)), now()), None);
    }
}
