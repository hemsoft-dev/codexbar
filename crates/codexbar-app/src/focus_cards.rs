//! Chart cards for the focused account, in the gpui-kit Chart gallery's anatomy:
//! title, period, chart, a takeaway sentence, and a quiet caption.

use chrono::{DateTime, Local, Utc};
use codexbar_core::{AccountSnapshot, Metric, Severity, format};
use gpui_kit::component::chart::{AreaChart, BarChart, PieChart};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Background, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _,
};

use crate::history_view::ValueKind;
use crate::status::severity_color;

/// One card. Built fresh each frame; the charts own their own hover state by id.
struct Card {
    title: SharedString,
    period: SharedString,
    legend: Vec<(Hsla, SharedString)>,
    chart: AnyElement,
    takeaway: SharedString,
    alert: Option<Hsla>,
    caption: SharedString,
}

impl Card {
    fn new(title: impl Into<SharedString>, period: impl Into<SharedString>, chart: impl IntoElement) -> Self {
        Self {
            title: title.into(),
            period: period.into(),
            legend: Vec::new(),
            chart: chart.into_any_element(),
            takeaway: SharedString::default(),
            alert: None,
            caption: SharedString::default(),
        }
    }

    fn legend(mut self, color: Hsla, label: impl Into<SharedString>) -> Self {
        self.legend.push((color, label.into()));
        self
    }

    fn takeaway(mut self, text: impl Into<SharedString>) -> Self {
        self.takeaway = text.into();
        self
    }

    /// Marks the takeaway as a warning, with an icon so the state is not carried by color alone.
    fn alert(mut self, color: Hsla) -> Self {
        self.alert = Some(color);
        self
    }

    fn caption(mut self, text: impl Into<SharedString>) -> Self {
        self.caption = text.into();
        self
    }

    fn render(self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_4()
            .gap_1()
            .bg(cx.theme().group_box)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_shrink_0()
                            .child(div().font_semibold().child(self.title))
                            .child(div().text_sm().text_color(muted).child(self.period)),
                    )
                    .when(!self.legend.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_3()
                                .text_xs()
                                .text_color(muted)
                                .children(self.legend.into_iter().map(|(color, label)| {
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .child(div().size_2().rounded_full().bg(color))
                                        .child(label)
                                })),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().py_3().child(self.chart))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .text_sm()
                    .font_semibold()
                    .when_some(self.alert, |this, color| {
                        this.child(Icon::new(IconName::TriangleAlert).small().text_color(color))
                    })
                    .child(self.takeaway),
            )
            .child(div().text_sm().text_color(muted).child(self.caption))
    }
}

fn area_fill(color: Hsla) -> Background {
    linear_gradient(
        0.,
        linear_color_stop(color.opacity(0.4), 1.),
        linear_color_stop(color.opacity(0.), 0.),
    )
}

/// Every metric the account reports, compactly (#82): the cards chart two of them, and a provider like Claude can
/// return more (model-scoped weekly limits, usage credits, a credit balance). Each says its value, reset and projection.
pub fn metric_list(account: &AccountSnapshot, now: DateTime<Utc>, cx: &App) -> Option<AnyElement> {
    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    if account.metrics().len() < 2 {
        return None;
    }
    let muted = cx.theme().muted_foreground;
    let entries = account.metrics().iter().enumerate().map(|(ix, metric)| {
        let mut detail = Vec::new();
        if let Some(at) = metric.resets_at().filter(|at| *at > now) {
            detail.push(format!("resets in {}", format::countdown(at - now)));
        }
        if let Some(projected) = metric.projected_at_reset(now) {
            detail.push(format!("on pace for {:.0}%", projected * 100.0));
        }
        // What is left under a spend limit, derived from spend and limit; never presented as a balance.
        if let Some(headroom) = metric.headroom() {
            detail.push(format!("{} left to the limit", headroom.display()));
        }
        if let Some(spent) = credit_spend(metric) {
            detail.push(spent);
        }
        let detail = detail.join(" · ");
        let spoken = if detail.is_empty() {
            format!("{}: {}", metric.label(), metric.used_display())
        } else {
            format!("{}: {}, {detail}", metric.label(), metric.used_display())
        };
        let severity = codexbar_core::assess(metric, now).severity();
        v_flex()
            .id(("account-metric", ix))
            .role(Role::Note)
            .test_support()
            .aria_label(spoken)
            .min_w(gpui_kit::rems(9.))
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .child(div().text_xs().text_color(muted).child(metric.label().to_owned()))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .when(severity >= Severity::Watch, |this| {
                        this.child(div().size_2().rounded_full().bg(severity_color(severity, cx)))
                    })
                    .child(div().text_sm().font_semibold().child(metric.used_display())),
            )
            .when(!detail.is_empty(), |this| {
                this.child(div().text_xs().text_color(muted).child(detail))
            })
    });
    Some(
        h_flex()
            .id("account-metrics")
            .flex_wrap()
            .gap_2()
            .children(entries)
            .into_any_element(),
    )
}

/// The three cards for `account`, left to right.
pub fn focus_cards(account: &AccountSnapshot, now: DateTime<Utc>, cx: &App) -> Vec<AnyElement> {
    vec![
        usage_card(account, now, cx).render(cx).into_any_element(),
        share_card(account, now, cx).render(cx).into_any_element(),
        activity_card(account, cx).render(cx).into_any_element(),
    ]
}

/// The wide card: the current window against the previous one, or the 14-day trend when no curve exists.
fn usage_card(account: &AccountSnapshot, now: DateTime<Utc>, cx: &App) -> Card {
    let current_color = cx.theme().chart_1;
    let previous_color = cx.theme().chart_4;
    let primary = account.primary();
    let severity = account.assess(now).severity();

    let card = if let Some(curve) = account.detail().window_curve() {
        let points: Vec<(SharedString, f64, f64)> = curve
            .labels()
            .iter()
            .zip(curve.current())
            .zip(curve.previous())
            .map(|((label, current), previous)| (SharedString::from(label.clone()), *current, *previous))
            .collect();
        let chart = AreaChart::new(points)
            .id("focus-window")
            .x(|(label, _, _)| label.clone())
            .y(|(_, current, _)| *current)
            .stroke(current_color)
            .fill(area_fill(current_color))
            .name("This window")
            .y(|(_, _, previous)| *previous)
            .stroke(previous_color)
            .fill(area_fill(previous_color.opacity(0.5)))
            .name("Previous window")
            .y_domain(0.0, 1.0)
            .y_axis(true)
            .y_padding(0., 0.)
            .y_tick_count(5)
            .point_count(curve.labels().len())
            .tick_margin(6)
            .y_tick_format(|value| format!("{:.0}%", value * 100.0));
        let title = primary
            .map(|m| format!("{} usage", m.label()))
            .unwrap_or_else(|| "Usage".into());
        let start = curve.labels().first().cloned().unwrap_or_default();
        Card::new(title, format!("Today, {start} – now"), chart)
            .legend(current_color, "This window")
            .legend(previous_color, "Previous window")
    } else if account.trend().len() < 2 {
        Card::new("Usage over time", "History", no_history(cx))
    } else {
        let points: Vec<(SharedString, f64)> = account
            .trend()
            .iter()
            .enumerate()
            .map(|(ix, value)| (SharedString::from(day_label(account.trend().len(), ix)), *value))
            .collect();
        // The trend is in the headline's own unit: a fraction, credits left (#150), a count (#103) or money (#104).
        let kind = trend_kind(account);
        let counted = counted_name(account);
        let high = account.trend().iter().copied().fold(0.0, f64::max);
        let chart = AreaChart::new(points)
            .id("focus-trend")
            .x(|(label, _)| label.clone())
            .y(|(_, value)| *value)
            .stroke(current_color)
            .fill(area_fill(current_color))
            .name(counted.clone().unwrap_or_else(|| "Pressure".into()))
            .y_domain(0.0, if counted.is_some() { (high * 1.15).max(1.0) } else { 1.0 })
            .tick_margin(3)
            .y_tick_format(move |value| kind.value(value));
        let title = counted.unwrap_or_else(|| "Daily pressure".into());
        Card::new(title, "Last 14 days", chart)
    };

    let card = match primary {
        Some(metric) => card
            .takeaway(pace_takeaway(metric, now))
            .caption(reset_caption(metric, now)),
        None => card,
    };
    if severity >= Severity::AtRisk {
        card.alert(severity_color(severity, cx))
    } else {
        card
    }
}

/// A donut for the secondary window (or the primary one), or the balance when there is no limit.
fn share_card(account: &AccountSnapshot, now: DateTime<Utc>, cx: &App) -> Card {
    let metric = secondary_metric(account, now).or(account.primary());
    let track = cx.theme().muted;
    let Some(metric) = metric else {
        return Card::new("No limits", "", div()).takeaway("This account reports no limits");
    };

    if let Metric::Credits { balance, .. } = metric {
        return credits_card(metric, *balance, cx);
    }
    let Some(used) = metric.used_fraction() else {
        let amount = match metric {
            Metric::Balance { remaining, .. } => remaining.display(),
            Metric::Spend { spent, .. } => spent.display(),
            Metric::Activity { .. } => metric.used_display(),
            _ => "—".into(),
        };
        let body = v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(div().text_3xl().font_semibold().child(amount))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(metric.label().to_owned()),
            );
        if let Metric::Spend { .. } = metric {
            return Card::new(metric.label().to_owned(), "Spent so far", body)
                .takeaway("No spend limit is set for this account");
        }
        if let Metric::Activity { .. } = metric {
            return Card::new(metric.label().to_owned(), "Activity", body).takeaway("A count with no limit or reset");
        }
        let takeaway = metric
            .days_of_credit()
            .map(|days| format!("About {days:.0} days of credit left"))
            .unwrap_or_else(|| "No spend recorded this week".into());
        return Card::new(metric.label().to_owned(), "Current balance", body).takeaway(takeaway);
    };

    let color = severity_color(codexbar_core::assess(metric, now).severity(), cx);
    let slices = vec![(used, color), ((1.0 - used).max(0.0), track)];
    let donut = div()
        .relative()
        .size_full()
        .child(
            PieChart::new(slices)
                .id("focus-share")
                .value(|(value, _)| *value as f32)
                .color(|(_, color)| *color)
                .inner_radius(62.)
                .outer_radius(84.)
                .tooltip_name(|_| "Used".into())
                .tooltip_value(|(value, _), _, _| format!("{:.0}%", value * 100.0).into()),
        )
        .child(
            v_flex()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .child(div().text_2xl().font_semibold().child(format!("{:.0}%", used * 100.0)))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child("used")),
        );
    let takeaway = pool_takeaway(1.0 - used);
    let title = if metric.label().to_lowercase().contains("window") {
        metric.label().to_owned()
    } else {
        format!("{} window", metric.label())
    };
    Card::new(title, window_period(metric, now), donut)
        .takeaway(takeaway)
        .caption(reset_caption(metric, now))
}

/// Requests by hour when the provider reports them, otherwise daily pressure as bars.
fn activity_card(account: &AccountSnapshot, cx: &App) -> Card {
    let color = cx.theme().chart_1;
    let bar_fill = move |_: &(SharedString, f64), _, _, _| -> Background {
        linear_gradient(
            180.,
            linear_color_stop(color, 0.),
            linear_color_stop(color.opacity(0.55), 1.),
        )
    };
    let requests = account.detail().requests_by_hour();
    if !requests.is_empty() {
        let bars: Vec<(SharedString, f64)> = requests
            .iter()
            .enumerate()
            .map(|(hour, count)| (SharedString::from(format!("{hour:02}:00")), *count as f64))
            .collect();
        let (peak_hour, _) = requests
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .unwrap_or((0, &0));
        let total: u32 = requests.iter().sum();
        let chart = BarChart::new(bars)
            .id("focus-requests")
            .band(|(label, _)| label.clone())
            .value(|(_, value)| *value)
            .fill(bar_fill)
            .tick_margin(6)
            .value_axis(true)
            .value_tick_count(4)
            .value_tick_format(|value| format!("{value:.0}"))
            .name("Requests");
        return Card::new("Requests by hour", "Last 24 hours", chart)
            .takeaway(format!("Peak at {peak_hour:02}:00"))
            .caption(format!(
                "{} requests today",
                codexbar_core::group_thousands(u64::from(total))
            ));
    }

    let trend = account.trend();
    if trend.len() < 2 {
        return Card::new("Usage by day", "Last 7 days", no_history(cx))
            .takeaway("No history yet")
            .caption("Daily usage appears once history is stored");
    }
    let bars: Vec<(SharedString, f64)> = trend
        .iter()
        .enumerate()
        .skip(trend.len().saturating_sub(7))
        .map(|(ix, value)| (SharedString::from(day_label(trend.len(), ix)), *value))
        .collect();
    let kind = trend_kind(account);
    let counted = counted_name(account);
    let chart = BarChart::new(bars)
        .id("focus-days")
        .band(|(label, _)| label.clone())
        .value(|(_, value)| *value)
        .fill(bar_fill)
        .value_tick_format(move |value| kind.value(value))
        .name(counted.clone().unwrap_or_else(|| "Pressure".into()));
    let change = match trend {
        [.., before, last] if *before > 0.0 => (last - before) / before * 100.0,
        _ => 0.0,
    };
    let topped_up = on_credits(account) && matches!(trend, [.., before, last] if *before <= 0.0 && *last > 0.0);
    // A count (#103) or money (#104) changes by an amount, not a share.
    let by_amount = matches!(kind, ValueKind::Count | ValueKind::Money(_));
    let takeaway = if topped_up {
        // From zero there is no percentage to give (#150).
        "Topped up since yesterday".to_owned()
    } else if let (true, [.., before, last]) = (by_amount, trend) {
        let delta = last - before;
        if kind.change(delta) == "no change" {
            "Flat since yesterday".to_owned()
        } else {
            format!(
                "{} {} since yesterday",
                if delta > 0.0 { "Up" } else { "Down" },
                kind.value(delta.abs())
            )
        }
    } else if change.abs() < 1.0 {
        "Flat since yesterday".to_owned()
    } else {
        format!(
            "{} {:.0}% since yesterday",
            if change > 0.0 { "Up" } else { "Down" },
            change.abs()
        )
    };
    let caption = match counted {
        Some(name) => format!("{name} at the end of each day"),
        None => "Share of the tightest limit".to_owned(),
    };
    Card::new("Usage by day", "Last 7 days", chart)
        .takeaway(takeaway)
        .caption(caption)
}

/// True when the account's headline is its credits (#150), so its trend is a count of credits left.
fn on_credits(account: &AccountSnapshot) -> bool {
    matches!(account.primary(), Some(Metric::Credits { .. }))
}

/// What the trend holds when the headline isn't a fraction: credits left (#150), an activity count such as completed
/// reviews (#103), or money left or spent (#104).
fn counted_name(account: &AccountSnapshot) -> Option<String> {
    match account.primary()? {
        Metric::Credits { .. } => Some("Credits left".to_owned()),
        Metric::Activity { label, .. } | Metric::Spend { label, .. } | Metric::Balance { label, .. } => {
            Some(label.clone())
        }
        _ => None,
    }
}

/// The unit the trend is in, as History stores it: a fraction of a limit, money, credits or a count.
fn trend_kind(account: &AccountSnapshot) -> ValueKind {
    account.primary().map_or(ValueKind::Percent, ValueKind::of)
}

/// How much of a window's pool is left, in words.
fn pool_takeaway(remaining: f64) -> String {
    match remaining {
        r if r >= 0.9 => "Nearly all of the pool left".to_owned(),
        r if (0.45..=0.55).contains(&r) => "About half the pool left".to_owned(),
        r if r <= 0.0 => "Pool used up".to_owned(),
        r => format!("{:.0}% of the pool left", r * 100.0),
    }
}

/// The empty state for charts that need stored history.
fn no_history(cx: &App) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(IconName::Calendar))
        .child("History builds up as CodexBar refreshes")
}

/// The metric for the second card: the most severe one after the primary, so whatever raises the account's status
/// (such as Cursor's on-demand spend) is on screen. Ties keep provider order, so it is usually the second metric.
/// How much of the remembered starting balance the credits have used (#150): "63% of 50,000 used".
fn credit_spend(metric: &Metric) -> Option<String> {
    let Metric::Credits { start: Some(start), .. } = metric else {
        return None;
    };
    let progress = metric.progress()?;
    Some(format!(
        "{:.0}% of {} used",
        progress * 100.0,
        codexbar_core::format_credits(*start)
    ))
}

/// Credits left, with a bar running down from the remembered start when there is one (#150).
fn credits_card(metric: &Metric, balance: Option<f64>, cx: &App) -> Card {
    let amount = balance.map_or_else(|| "Unlimited".to_owned(), codexbar_core::format_credits);
    let mut body = v_flex()
        .size_full()
        .gap_2()
        .items_center()
        .justify_center()
        .child(div().text_3xl().font_semibold().child(amount))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("credits left"),
        );
    if let Some(progress) = metric.progress() {
        body = body.child(
            div().w_full().px_4().child(
                gpui_kit::component::progress::Progress::new("focus-credits")
                    .value(progress as f32 * 100.0)
                    .color(cx.theme().chart_1)
                    .accessibility_label("Credits used since the last top-up"),
            ),
        );
    }
    let takeaway = match (balance, credit_spend(metric)) {
        (None, _) => "This account's credits are unlimited".to_owned(),
        (Some(_), Some(spent)) => format!("{spent} since the last top-up"),
        (Some(_), None) => "The bar starts with the next refresh".to_owned(),
    };
    Card::new(metric.label().to_owned(), "Credits pool", body).takeaway(takeaway)
}

fn secondary_metric(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<&Metric> {
    account
        .metrics()
        .iter()
        .skip(1)
        .enumerate()
        .max_by(|(a_ix, a), (b_ix, b)| {
            let severity = |metric: &Metric| codexbar_core::assess(metric, now).severity();
            severity(a).cmp(&severity(b)).then(b_ix.cmp(a_ix))
        })
        .map(|(_, metric)| metric)
}

fn pace_takeaway(metric: &Metric, now: DateTime<Utc>) -> String {
    if let Some(days) = metric.days_of_credit() {
        return format!("About {days:.0} days of credit left");
    }
    match metric.time_to_limit() {
        Some(to_limit) if metric.exhausts_before_reset(now) => {
            format!(
                "Hits the {} limit in ~{} at this pace",
                metric.label().to_lowercase(),
                format::countdown(to_limit)
            )
        }
        Some(_) => match metric.projected_at_reset(now) {
            Some(projected) => format!("On pace for about {:.0}% by the reset", projected * 100.0),
            None => "On pace to stay under the limit".to_owned(),
        },
        None => "Steady".to_owned(),
    }
}

fn reset_caption(metric: &Metric, now: DateTime<Utc>) -> String {
    match metric.resets_at() {
        Some(at) => {
            let label = format::reset_label_in(at, now, &Local, crate::locale::style());
            match label.strip_prefix("in ") {
                Some(rest) => format!("Resets in {rest}"),
                None => format!("Resets {label}"),
            }
        }
        None => String::new(),
    }
}

/// "Oct 2 – Oct 9" for a window that resets within a week; otherwise its reset date.
fn window_period(metric: &Metric, now: DateTime<Utc>) -> String {
    match metric.resets_at() {
        Some(at) if at - now <= chrono::Duration::days(7) => {
            let start = (at - chrono::Duration::days(7)).with_timezone(&Local);
            let end = at.with_timezone(&Local);
            format!(
                "{} – {}",
                crate::locale::style().month_day(start.date_naive()),
                crate::locale::style().month_day(end.date_naive())
            )
        }
        Some(at) => format!(
            "Until {}",
            crate::locale::style().month_day(at.with_timezone(&Local).date_naive())
        ),
        None => String::new(),
    }
}

/// Weekday initials for a trend whose last point is today.
fn day_label(len: usize, ix: usize) -> String {
    let days_ago = (len - 1 - ix) as i64;
    crate::locale::style().weekday((Local::now() - chrono::Duration::days(days_ago)).date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use codexbar_core::Pace;

    fn window(used: f64, per_hour: f64) -> Metric {
        Metric::Window {
            label: "Included usage".into(),
            used,
            resets_at: now() + Duration::hours(10),
            pace: Some(Pace::per_hour(per_hour)),
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    #[test]
    fn the_second_card_shows_the_metric_that_raises_the_status() {
        use codexbar_core::{AccountId, Money, Provider};
        let account = |on_demand_spent: i64| {
            AccountSnapshot::new(
                AccountId::new("cursor"),
                Provider::Cursor,
                vec![
                    window(0.3, 0.01),
                    window(0.2, 0.01),
                    window(0.1, 0.01),
                    Metric::Spend {
                        label: "On-demand".into(),
                        spent: Money::from_cents(on_demand_spent),
                        limit: Some(Money::from_cents(2000)),
                        resets_at: Some(now() + Duration::hours(10)),
                        pace: None,
                    },
                ],
                now(),
            )
        };
        let calm = account(100);
        assert_eq!(
            secondary_metric(&calm, now()),
            calm.metrics().get(1),
            "provider order when calm"
        );
        let urgent = account(1980);
        assert_eq!(secondary_metric(&urgent, now()).unwrap().label(), "On-demand");
    }

    #[test]
    fn pace_takeaway_shows_the_projection_or_when_the_limit_is_hit() {
        assert_eq!(
            pace_takeaway(&window(0.4, 0.03), now()),
            "On pace for about 70% by the reset"
        );
        assert!(pace_takeaway(&window(0.4, 0.1), now()).starts_with("Hits the included usage limit in ~"));
    }

    #[test]
    fn a_trend_is_charted_in_its_headlines_own_unit() {
        use codexbar_core::{AccountId, Currency, Money, Provider};
        let account =
            |metric: Metric| AccountSnapshot::new(AccountId::new("a"), Provider::GitHubBilling, vec![metric], now());
        let spend = account(Metric::Spend {
            label: "Net spend".into(),
            spent: Money::from_cents(2389),
            limit: None,
            resets_at: None,
            pace: None,
        });
        assert_eq!(trend_kind(&spend), ValueKind::Money(Currency::Usd));
        assert_eq!(counted_name(&spend).as_deref(), Some("Net spend"));
        assert_eq!(trend_kind(&spend).value(23.89), "$23.89");
        let reviews = account(Metric::Activity {
            label: "Completed reviews".into(),
            count: 3,
        });
        assert_eq!(trend_kind(&reviews), ValueKind::Count);
        let window = account(window(0.4, 0.1));
        assert_eq!(trend_kind(&window), ValueKind::Percent);
        assert_eq!(counted_name(&window), None, "a fraction keeps the pressure chart");
    }
}
