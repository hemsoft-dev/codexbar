//! Chart cards for the focused account, in the gpui-kit Chart gallery's anatomy:
//! title, period, chart, a takeaway sentence, and a quiet caption.

use chrono::{DateTime, Local, Utc};
use codexbar_core::{AccountSnapshot, Metric, Severity, format};
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Background, ElementId, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, rems,
};

use crate::history_view::ValueKind;
use gpui_kit::{InteractiveElement as _, TestSupportExt as _};

/// Where a share axis ends (#200): a quarter above 100%, so a smooth curve's swing past a limit and the marker on a
/// point at 100% stay inside the plot. The ticks land on 0, 25, 50, 75 and 100%, and the headroom tick at the top
/// draws its gridline without a label.
const SHARE_TOP: f64 = 1.25;
const SHARE_TICKS: usize = 6;
/// The least height a card's chart slot keeps, in rems so it follows the zoom (#200); below it the card grows and
/// the dashboard scrolls rather than the chart drawing past its box.
const CHART_MIN_HEIGHT_REMS: f32 = 11.;
/// The largest the ring gauge grows, in unzoomed pixels (#200), so a tall card on a big display doesn't balloon it.
const RING_MAX_RADIUS: f32 = 120.;

/// A trend tick's label: the share ticks for a share trend (#200), the headline's own unit for a count or money.
fn trend_tick(kind: ValueKind, counted: bool, value: f64) -> String {
    if counted { kind.value(value) } else { share_tick(value) }
}

/// A share tick's label: a percentage up to 100%, nothing for the headroom above it.
fn share_tick(value: f64) -> String {
    // The chart reads each tick's value back from its pixel row in f32, so 100% can arrive a hair above 1.0.
    if value > 1.0 + 1e-3 {
        String::new()
    } else {
        format!("{:.0}%", value * 100.0)
    }
}
use crate::status::severity_color;

/// One card. Built fresh each frame; the charts own their own hover state by id.
struct Card {
    /// The id of the card and, suffixed, of its parts, so a headless test can check that its content stays inside
    /// it (#200). Derived from the title unless set.
    id: SharedString,
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
        let title = title.into();
        Self {
            id: format!("focus-card-{}", title.to_lowercase().replace(' ', "-")).into(),
            title,
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

    fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = id.into();
        self
    }

    fn render(self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let part = |name: &str| ElementId::Name(format!("{}-{name}", self.id).into());
        let legend_id = part("legend");
        let takeaway_id = part("takeaway");
        v_flex()
            .id(ElementId::Name(self.id.clone()))
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_4()
            .gap_1()
            .bg(cx.theme().group_box)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            // The legend sits beside the title when there is room, otherwise under it (#200): zoomed in, a wide
            // legend used to run past the card's edge.
            .child(
                h_flex()
                    .flex_wrap()
                    .items_start()
                    .justify_between()
                    .gap_x_3()
                    .gap_y_1()
                    .child(
                        v_flex()
                            .flex_shrink_0()
                            .child(div().font_semibold().child(self.title))
                            .child(div().text_sm().text_color(muted).child(self.period)),
                    )
                    .when(!self.legend.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .id(legend_id)
                                .test_support()
                                .flex_wrap()
                                .min_w_0()
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
            // Never squeezed below what a chart needs (#200): the card grows and the dashboard scrolls instead.
            .child(
                div()
                    .flex_1()
                    .min_h(rems(CHART_MIN_HEIGHT_REMS))
                    .py_3()
                    .child(self.chart),
            )
            // The takeaway wraps instead of running past the card (#200).
            .child(
                h_flex()
                    .id(takeaway_id)
                    .test_support()
                    .gap_1p5()
                    .items_start()
                    .text_sm()
                    .font_semibold()
                    .when_some(self.alert, |this, color| {
                        this.child(Icon::new(IconName::TriangleAlert).small().text_color(color))
                    })
                    .child(div().flex_1().min_w_0().child(self.takeaway)),
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
        if let Some(outlook) = metric.outlook(now) {
            detail.push(format::outlook_text(outlook));
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
            .linear()
            .y(|(_, _, previous)| *previous)
            .stroke(previous_color)
            .fill(area_fill(previous_color.opacity(0.5)))
            .name("Previous window")
            .linear()
            .y_domain(0.0, SHARE_TOP)
            .y_axis(true)
            .y_padding(0., 0.)
            .y_tick_count(SHARE_TICKS)
            .point_count(curve.labels().len())
            .tick_margin(6)
            .y_tick_format(share_tick);
        let title = primary
            .map(|m| format!("{} usage", m.label()))
            .unwrap_or_else(|| "Usage".into());
        let start = curve.labels().first().cloned().unwrap_or_default();
        // Findable by the headless tests, as the day plot is (#200).
        let plot = div().id("focus-window-plot").test_support().size_full().child(chart);
        Card::new(title, format!("Today, {start} – now"), plot)
            .id("focus-window-card")
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
        let is_counted = counted.is_some();
        let (low, high) = if is_counted {
            day_domain(account.trend(), kind)
        } else {
            (0.0, SHARE_TOP)
        };
        let chart = AreaChart::new(points)
            .id("focus-trend")
            .x(|(label, _)| label.clone())
            .y(|(_, value)| *value)
            .stroke(current_color)
            .fill(area_fill(current_color))
            .name(counted.clone().unwrap_or_else(|| "Pressure".into()))
            .linear()
            .y_domain(low, high)
            .tick_margin(3)
            .y_tick_format(move |value| trend_tick(kind, is_counted, value));
        // A share trend takes the share ticks (#200), so the 100% line keeps its label and the headroom has none.
        let chart = if is_counted {
            chart
        } else {
            chart.y_tick_count(SHARE_TICKS)
        };
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

/// The share of a pool as a ring gauge (#174), or the credits, balance or spend when no metric has a share.
fn share_card(account: &AccountSnapshot, now: DateTime<Utc>, cx: &App) -> Card {
    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    let Some(metric) = share_metric(account, now) else {
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

    // The number leads (#174): large in the ring, with the pace in the ring's opening and the reset in the caption.
    let color = severity_color(codexbar_core::assess(metric, now).severity(), cx);
    let projected = metric.projected_at_reset(now);
    let outlook = metric.outlook(now);
    let pace = outlook.map(format::outlook_text);
    let spoken = match (outlook, &pace) {
        (Some(codexbar_core::Outlook::Share(_)), Some(pace)) => {
            format!("{}: {:.0}% used, {pace} by the reset", metric.label(), used * 100.0)
        }
        (_, Some(pace)) => format!("{}: {:.0}% used, {pace}", metric.label(), used * 100.0),
        (_, None) => format!("{}: {:.0}% used", metric.label(), used * 100.0),
    };
    // The ring paints its number and pace itself, on its own geometry, so it fits any card width and zoom (#176).
    // Hovering it says what the table's bars say (#197).
    let hover: gpui_kit::SharedString = crate::usage_bar::Projection::of(metric, used, now).tooltip.into();
    let ring = div()
        .id("focus-share")
        .role(Role::Note)
        .test_support()
        .aria_label(spoken)
        .size_full()
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hover.clone()).build(window, cx))
        .child(
            crate::gauge::Gauge::new(used, projected, format!("{:.0}", used * 100.0), color, cx)
                .pace(pace)
                .max_radius(crate::zoom::scaled(RING_MAX_RADIUS, cx))
                .render(),
        );
    let takeaway = pool_takeaway(1.0 - used);
    let title = if metric.label().to_lowercase().contains("window") {
        metric.label().to_owned()
    } else {
        format!("{} window", metric.label())
    };
    let mut caption = reset_caption(metric, now);
    if let Some(credits) = displaced_credits(account) {
        caption = if caption.is_empty() {
            credits
        } else {
            format!("{caption} · {credits}")
        };
    }
    Card::new(title, window_period(metric, now), ring)
        .takeaway(takeaway)
        .caption(caption)
}

/// The metric for the second card: the one after the headline that raises the status when it has a share of a pool
/// (so Cursor's on-demand spend stays on screen), otherwise the headline's share, otherwise the most pressing share
/// further down (GitHub Billing lists its allowances after two uncapped spends) (#174). Only when no metric has a
/// share is it whatever the second one is: credits, a balance, a spend with no limit.
fn share_metric(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<&Metric> {
    let primary = account.primary()?;
    let secondary = secondary_metric(account, now);
    let has_share = |metric: &Metric| metric.used_fraction().is_some();
    secondary
        .filter(|metric| has_share(metric))
        .or_else(|| primary.used_fraction().map(|_| primary))
        .or_else(|| most_severe_after_primary(account, now, has_share))
        .or(secondary)
        .or(Some(primary))
}

/// The credits line for the ring's caption when the share took the card (#174): the credits after the headline,
/// wherever the provider lists them (Codex puts both windows before them). Credits that lead the account (#150)
/// already have the first card, so they get no line.
fn displaced_credits(account: &AccountSnapshot) -> Option<String> {
    account.metrics().iter().skip(1).find_map(|metric| match metric {
        Metric::Credits {
            balance: Some(balance), ..
        } => Some(format!("{} credits left", codexbar_core::format_credits(*balance))),
        Metric::Credits { balance: None, .. } => Some("Unlimited credits".to_owned()),
        _ => None,
    })
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

    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    let trend = account.trend();
    if trend.len() < 2 {
        let empty = div()
            .id("focus-days-empty")
            .role(Role::Note)
            .test_support()
            .aria_label("No daily usage yet; it appears once history is stored")
            .size_full()
            .child(no_history(cx));
        return Card::new("Usage by day", "Last 7 days", empty)
            .takeaway("No history yet")
            .caption("Daily usage appears once history is stored");
    }
    let days: Vec<(SharedString, f64)> = trend
        .iter()
        .enumerate()
        .skip(trend.len().saturating_sub(7))
        .map(|(ix, value)| (SharedString::from(day_label(trend.len(), ix)), *value))
        .collect();
    let kind = trend_kind(account);
    let counted = counted_name(account);
    let values: Vec<f64> = days.iter().map(|(_, value)| *value).collect();
    let (low, high) = day_domain(&values, kind);
    let ticks = day_ticks((low, high), kind);
    let spoken = format!(
        "Usage by day: {}",
        days.iter()
            .map(|(day, value)| format!("{day} {}", kind.value(*value)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // A line through the week's points, one marker per day, in the headline's unit (#180). The axis is pinned to the
    // unit's range with no pixel padding, so its ticks read 0, 33, 67, 100%: the chart spaces ticks over the padded
    // extent, and padding made them read -8% and 108%. Segments are straight (#217): gpui-kit's smooth curve is a
    // natural spline, which swings past 0% or 100% around a sharp change such as a reset and was clipped there, and
    // it has no smooth curve that stays between its points.
    let chart = LineChart::new(days)
        .id("focus-days")
        .x(|(label, _)| label.clone())
        .y(|(_, value)| *value)
        .stroke(color)
        .linear()
        .dot()
        .y_domain(low, high)
        .y_padding(0., 0.)
        .y_axis(true)
        .y_tick_count(ticks)
        .y_tick_format(move |value| {
            if kind == ValueKind::Percent {
                share_tick(value)
            } else {
                kind.value(value)
            }
        })
        .tooltip_value(move |_, value| kind.value(value).into())
        .name(counted.clone().unwrap_or_else(|| "Pressure".into()));
    let plot = div()
        .id("focus-days-plot")
        .role(Role::Note)
        .test_support()
        .aria_label(spoken)
        .size_full()
        .child(chart);
    Card::new("Usage by day", "Last 7 days", plot)
        .takeaway(day_takeaway(trend, kind, on_credits(account)))
        .caption(day_caption(counted.as_deref()))
}

/// The day line's axis range: a share is pinned to 0–100%; a count, credits or money runs from zero (or below the
/// week's lowest value, for a balance gone negative, #104) to 15% over its highest day, never flat.
fn day_domain(values: &[f64], kind: ValueKind) -> (f64, f64) {
    if kind == ValueKind::Percent {
        return (0.0, SHARE_TOP);
    }
    let low = values.iter().copied().fold(0.0, f64::min);
    let high = values.iter().copied().fold(0.0, f64::max);
    let low = if low < 0.0 { low * 1.15 } else { 0.0 };
    (low, (high * 1.15).max(low + 1.0))
}

/// How many ticks the day line's axis gets: four when the unit's formatting tells them apart, otherwise the most
/// that still read distinctly, down to two. Ticks sit evenly over `low..=high` before formatting, so a week of one
/// or two activities, or of credits down to a fraction, never reads "0, 1, 1, 2".
fn day_ticks((low, high): (f64, f64), kind: ValueKind) -> usize {
    if kind == ValueKind::Percent {
        return SHARE_TICKS;
    }
    (2..=4)
        .rev()
        .find(|count| {
            let labels: Vec<String> = (0..*count)
                .map(|ix| kind.value(low + (high - low) * ix as f64 / (count - 1) as f64))
                .collect();
            labels.iter().collect::<std::collections::HashSet<_>>().len() == labels.len()
        })
        .unwrap_or(2)
}

/// How yesterday compares: "Up 12% since yesterday" for a share, "Down $2.50 since yesterday" for an amount (#103,
/// #104), "Flat since yesterday", or "Topped up since yesterday" for credits that came back from zero (#150).
fn day_takeaway(trend: &[f64], kind: ValueKind, on_credits: bool) -> String {
    let change = match trend {
        [.., before, last] if *before > 0.0 => (last - before) / before * 100.0,
        _ => 0.0,
    };
    let topped_up = on_credits && matches!(trend, [.., before, last] if *before <= 0.0 && *last > 0.0);
    // A count (#103) or money (#104) changes by an amount, not a share.
    let by_amount = matches!(kind, ValueKind::Count | ValueKind::Money(_));
    if topped_up {
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
    }
}

/// What the day card's points are: the share of the tightest limit, or the counted headline at each day's end.
fn day_caption(counted: Option<&str>) -> String {
    match counted {
        Some(name) => format!("{name} at the end of each day"),
        None => "Share of the tightest limit".to_owned(),
    }
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
        // The same bar as the table's, with its tooltip (#197); credits project nothing, and it says so.
        body = body.child(div().w_full().px_4().child(crate::usage_bar::usage_bar(
            "focus-credits",
            &crate::usage_bar::Projection::of(metric, progress, Utc::now()),
            cx.theme().chart_1,
            crate::usage_bar::medium(),
            "Credits since the last top-up",
            cx,
        )));
    }
    let takeaway = match (balance, credit_spend(metric)) {
        (None, _) => "This account's credits are unlimited".to_owned(),
        (Some(_), Some(spent)) => format!("{spent} since the last top-up"),
        (Some(_), None) => "The bar starts with the next refresh".to_owned(),
    };
    Card::new(metric.label().to_owned(), "Credits pool", body).takeaway(takeaway)
}

/// The most severe metric after the primary, so whatever raises the account's status (such as Cursor's on-demand
/// spend) is on screen. Ties keep provider order, so it is usually the second metric.
fn secondary_metric(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<&Metric> {
    most_severe_after_primary(account, now, |_| true)
}

/// The most severe of the metrics after the primary that `keep` accepts; ties keep provider order.
fn most_severe_after_primary(
    account: &AccountSnapshot,
    now: DateTime<Utc>,
    keep: impl Fn(&Metric) -> bool,
) -> Option<&Metric> {
    account
        .metrics()
        .iter()
        .skip(1)
        .enumerate()
        .filter(|(_, metric)| keep(metric))
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
    // The outlook picks the wording (#206), so the card and its tile agree even at the minute where a limit hit
    // just before the reset counts as a full share.
    match (metric.outlook(now), metric.time_to_limit()) {
        (Some(outlook @ codexbar_core::Outlook::Early(_)), Some(to_limit)) => format!(
            "Hits the {} limit in ~{} at this pace, {}",
            metric.label().to_lowercase(),
            format::countdown(to_limit),
            format::outlook_text(outlook)
        ),
        (Some(codexbar_core::Outlook::Share(share)), _) => {
            format!("On pace for about {:.0}% by the reset", share * 100.0)
        }
        (_, Some(_)) => "On pace to stay under the limit".to_owned(),
        (_, None) => "Steady".to_owned(),
    }
}

/// "Resets in 2d 4h", or "Resets now": always time left (#196), as the table's Resets column says it.
fn reset_caption(metric: &Metric, now: DateTime<Utc>) -> String {
    match metric.resets_at() {
        Some(at) => format!("Resets {}", format::time_left_label(at, now)),
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
    fn the_share_leads_the_second_card_and_displaced_credits_keep_a_line() {
        use codexbar_core::{AccountId, Provider};
        let credits = |balance: f64| Metric::Credits {
            label: "Credits".into(),
            balance: Some(balance),
            start: None,
        };
        let account = |metrics| AccountSnapshot::new(AccountId::new("codex"), Provider::Codex, metrics, now());

        // A Pro account mid-week (#174): the weekly share takes the card, and the credits keep a caption line.
        let pro = account(vec![window(0.59, 0.02), credits(55_234.7)]);
        assert_eq!(share_metric(&pro, now()).unwrap().label(), "Included usage");
        assert_eq!(displaced_credits(&pro).as_deref(), Some("55,234.70 credits left"));

        // Codex lists both windows before the credits: the ring shows the second window, the credits keep the line.
        let three = account(vec![window(0.91, 0.2), window(0.47, 0.01), credits(55_234.7)]);
        assert_eq!(share_metric(&three, now()).unwrap().label(), "Included usage");
        assert_eq!(displaced_credits(&three).as_deref(), Some("55,234.70 credits left"));
        let unlimited = account(vec![
            window(0.3, 0.01),
            Metric::Credits {
                label: "Credits".into(),
                balance: None,
                start: None,
            },
        ]);
        assert_eq!(displaced_credits(&unlimited).as_deref(), Some("Unlimited credits"));

        // With the window used up the credits lead (#150) and have the first card, so the ring gets no line.
        let used_up = account(vec![window(1.0, 0.0), credits(18_400.0)]).with_credits_placed();
        assert!(matches!(used_up.primary(), Some(Metric::Credits { .. })));
        assert_eq!(share_metric(&used_up, now()).unwrap().label(), "Included usage");
        assert_eq!(displaced_credits(&used_up), None);

        // Credits alone keep their card.
        let only = account(vec![credits(10.0)]);
        assert!(matches!(share_metric(&only, now()), Some(Metric::Credits { .. })));
        assert_eq!(displaced_credits(&only), None);

        // GitHub Billing: two uncapped spends lead, and the allowance after them is the share the ring shows.
        use codexbar_core::Money;
        let spend = |label: &str| Metric::Spend {
            label: label.into(),
            spent: Money::from_cents(2389),
            limit: None,
            resets_at: None,
            pace: None,
        };
        let billing = account(vec![
            spend("Net spend"),
            spend("Gross charges"),
            Metric::Quota {
                label: "Actions minutes".into(),
                used: 600,
                limit: 2000,
                resets_at: now() + Duration::days(10),
                pace: None,
            },
        ]);
        assert_eq!(share_metric(&billing, now()).unwrap().label(), "Actions minutes");
        let uncapped = account(vec![spend("Net spend"), spend("Gross charges")]);
        assert_eq!(
            share_metric(&uncapped, now()).unwrap().label(),
            "Gross charges",
            "no share anywhere: the second metric keeps the card"
        );
    }

    #[test]
    fn pace_takeaway_shows_the_projection_or_when_the_limit_is_hit() {
        assert_eq!(
            pace_takeaway(&window(0.4, 0.03), now()),
            "On pace for about 70% by the reset"
        );
        // 60% left at 10%/h: the limit in 6h, four hours before the 10h reset (#206).
        assert_eq!(
            pace_takeaway(&window(0.4, 0.1), now()),
            "Hits the included usage limit in ~6h at this pace, 4h early"
        );
        // 60% left at 6%/h lands on the 10h reset: a full share, as the tile says, not "hits the limit".
        assert_eq!(
            pace_takeaway(&window(0.4, 0.06), now()),
            "On pace for about 100% by the reset"
        );
    }

    #[test]
    fn the_day_card_says_how_yesterday_compares_in_the_headlines_unit() {
        use codexbar_core::Currency;
        assert_eq!(
            day_takeaway(&[0.5, 0.6], ValueKind::Percent, false),
            "Up 20% since yesterday"
        );
        assert_eq!(
            day_takeaway(&[0.6, 0.6], ValueKind::Percent, false),
            "Flat since yesterday"
        );
        assert_eq!(
            day_takeaway(&[3.0, 5.0], ValueKind::Count, false),
            "Up 2 since yesterday"
        );
        assert_eq!(
            day_takeaway(&[10.0, 7.5], ValueKind::Money(Currency::Usd), false),
            "Down $2.50 since yesterday"
        );
        assert_eq!(
            day_takeaway(&[0.0, 500.0], ValueKind::Credits, true),
            "Topped up since yesterday"
        );
        assert_eq!(
            day_takeaway(&[500.0, 400.0], ValueKind::Credits, true),
            "Down 20% since yesterday"
        );
        assert_eq!(day_takeaway(&[0.4], ValueKind::Percent, false), "Flat since yesterday");
        assert_eq!(day_caption(None), "Share of the tightest limit");
        assert_eq!(day_caption(Some("Credits left")), "Credits left at the end of each day");
    }

    /// #217: the charts draw straight segments, which never leave the range of their own points, so a line stays
    /// in its plot exactly when every point is inside the axis range. A reset's sharp drop to near zero, a peak at
    /// the whole allowance and a balance gone negative all have to fit.
    #[test]
    fn every_trend_point_sits_inside_its_axis() {
        use codexbar_core::Currency;
        let usd = ValueKind::Money(Currency::Usd);
        let cases: [(&[f64], ValueKind); 4] = [
            (&[0.93, 0.97, 0.03, 0.12, 0.22, 0.32, 0.41], ValueKind::Percent),
            (&[0.0, 1.0, 0.0, 1.0], ValueKind::Percent),
            (&[12.0, -4.5, 3.0, 0.0], usd),
            (&[40.0, 0.0, 37.0], ValueKind::Count),
        ];
        for (values, kind) in cases {
            let (low, high) = day_domain(values, kind);
            for value in values {
                assert!(
                    (low..=high).contains(value),
                    "{value} outside {low}..={high} for {kind:?}"
                );
            }
        }
    }

    #[test]
    fn the_day_axis_fits_the_unit_and_labels_whole_counts() {
        use codexbar_core::Currency;
        let usd = ValueKind::Money(Currency::Usd);
        assert_eq!(
            day_domain(&[0.2, 1.0], ValueKind::Percent),
            (0.0, SHARE_TOP),
            "a share is pinned, with headroom over 100% (#200)"
        );
        assert_eq!(day_ticks((0.0, SHARE_TOP), ValueKind::Percent), SHARE_TICKS);
        assert_eq!(share_tick(1.0), "100%");
        assert_eq!(share_tick(1.000_000_1), "100%", "a tick read back from its pixel row");
        assert_eq!(share_tick(0.75), "75%");
        assert_eq!(share_tick(SHARE_TOP), "", "the headroom gridline carries no label");
        // The fallback share trend (no window curve) labels like the other share charts; a count keeps its unit.
        assert_eq!(trend_tick(ValueKind::Percent, false, SHARE_TOP), "");
        assert_eq!(trend_tick(ValueKind::Percent, false, 1.0), "100%");
        assert_eq!(
            trend_tick(ValueKind::Percent, true, SHARE_TOP),
            ValueKind::Percent.value(SHARE_TOP)
        );
        assert_eq!(day_domain(&[0.0, 0.0], ValueKind::Count), (0.0, 1.0), "never flat");
        assert_eq!(
            day_domain(&[10.0, 20.0], ValueKind::Credits),
            (0.0, 23.0),
            "headroom over the week"
        );
        // A balance that went negative (#104) keeps its points on the plot.
        let (low, high) = day_domain(&[-4.0, -2.0], usd);
        assert!(
            (low - -4.6).abs() < 1e-9 && high == 0.0,
            "all negative keeps the zero line: {low} {high}"
        );
        let (low, high) = day_domain(&[-4.0, 6.0], usd);
        assert!((low - -4.6).abs() < 1e-9 && (high - 6.9).abs() < 1e-9, "{low} {high}");
        // Ticks are as many as the unit's formatting tells apart: whole counts, credits down to a fraction, money.
        let ticks = |values: &[f64], kind: ValueKind| day_ticks(day_domain(values, kind), kind);
        assert_eq!(ticks(&[0.0, 1.0], ValueKind::Count), 2);
        assert_eq!(ticks(&[2.0, 1.0], ValueKind::Count), 3);
        assert_eq!(ticks(&[37.0], ValueKind::Count), 4);
        assert_eq!(ticks(&[1.0], ValueKind::Credits), 2);
        assert_eq!(ticks(&[1.1], ValueKind::Credits), 2, "0, 1, 1 would repeat");
        assert_eq!(ticks(&[36_800.0], ValueKind::Credits), 4);
        assert_eq!(ticks(&[0.5, 1.0], ValueKind::Percent), SHARE_TICKS);
        assert_eq!(ticks(&[1.0], usd), 4);
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
