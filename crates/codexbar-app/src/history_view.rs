//! The History view (#86): one account's stored history for a chosen metric and range, with a summary, a chart that
//! can be read with the keyboard, and a per-account switch that hides history without deleting it.

use std::sync::{Arc, Mutex};

use chrono::{Duration, Local, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Currency, Metric};
use codexbar_store::HistoryStore;
use codexbar_store::summary::{ChartPoint, GAP_THRESHOLD, HistorySummary, chart_series, summarize};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::{
    App, Context, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, rems,
};

use crate::prefs_hub::PrefsHub;
use crate::status::severity_dot_color;

/// The most points a history chart draws; longer ranges are reduced, keeping spikes.
const MAX_CHART_POINTS: usize = 120;

/// How a metric's stored values read: limits are fractions used, balances and uncapped spend are money in the
/// metric's currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Percent,
    Money(Currency),
    /// A count of credits (#150), never a percentage.
    Credits,
    /// A count of activity with no limit (#103), such as completed reviews.
    Count,
}

impl ValueKind {
    pub fn of(metric: &Metric) -> Self {
        match metric {
            // Spend is stored as money whether or not it has a cap.
            Metric::Balance { remaining, .. } => Self::Money(remaining.currency()),
            Metric::Spend { spent, .. } => Self::Money(spent.currency()),
            Metric::Credits { .. } => Self::Credits,
            Metric::Activity { .. } => Self::Count,
            _ => Self::Percent,
        }
    }

    pub fn value(self, value: f64) -> String {
        match self {
            Self::Percent => format!("{:.0}%", value * 100.0),
            Self::Money(currency) if value < 0.0 => format!("\u{2212}{}", amount(currency, value)),
            Self::Money(currency) => amount(currency, value),
            Self::Credits if value < 0.0 => format!("\u{2212}{}", codexbar_core::format_credits(value.abs().round())),
            Self::Credits | Self::Count => codexbar_core::format_credits(value.round()),
        }
    }

    /// A signed change: "+18 pts", "\u{2212}$9.30", "no change".
    pub fn change(self, delta: f64) -> String {
        let sign = if delta > 0.0 { "+" } else { "\u{2212}" };
        match self {
            Self::Percent if (delta * 100.0).abs() < 0.5 => "no change".into(),
            Self::Percent => format!("{sign}{:.0} pts", (delta * 100.0).abs()),
            Self::Money(currency) if negligible(currency, delta) => "no change".into(),
            Self::Money(currency) => format!("{sign}{}", amount(currency, delta)),
            Self::Credits | Self::Count if delta.abs() < 0.5 => "no change".into(),
            Self::Credits | Self::Count => format!("{sign}{}", codexbar_core::format_credits(delta.abs().round())),
        }
    }

    /// The change in words, for screen readers: "up 18 points", "down $9.30", "unchanged".
    fn change_words(self, delta: f64) -> String {
        let direction = if delta > 0.0 { "up" } else { "down" };
        match self {
            Self::Percent if (delta * 100.0).abs() < 0.5 => "unchanged".into(),
            Self::Percent => format!("{direction} {:.0} points", (delta * 100.0).abs()),
            Self::Money(currency) if negligible(currency, delta) => "unchanged".into(),
            Self::Money(currency) => format!("{direction} {}", amount(currency, delta)),
            Self::Credits | Self::Count if delta.abs() < 0.5 => "unchanged".into(),
            Self::Count => format!("{direction} {}", codexbar_core::format_credits(delta.abs().round())),
            Self::Credits => format!(
                "{direction} {} credits",
                codexbar_core::format_credits(delta.abs().round())
            ),
        }
    }
}

/// The absolute amount with its currency symbol and minor units: "$9.30", "\u{a5}1200".
fn amount(currency: Currency, value: f64) -> String {
    let digits = currency.minor_digits() as usize;
    format!("{}{:.digits$}", currency.symbol(), value.abs())
}

/// Smaller than half the currency's smallest unit, so it would display as zero.
fn negligible(currency: Currency, delta: f64) -> bool {
    delta.abs() < 0.5 / 10f64.powi(currency.minor_digits() as i32)
}

/// The ranges the History view offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryRange {
    Day,
    Week,
    Month,
}

impl HistoryRange {
    pub const ALL: [Self; 3] = [Self::Day, Self::Week, Self::Month];

    pub fn duration(self) -> Duration {
        match self {
            Self::Day => Duration::days(1),
            Self::Week => Duration::days(7),
            Self::Month => Duration::days(30),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "24 hours",
            Self::Week => "7 days",
            Self::Month => "30 days",
        }
    }
}

/// "6d 23h", "5h 10m", "12m": how long a summary's samples span.
pub fn span_label(span: Duration) -> String {
    let minutes = span.num_minutes().max(0);
    let (days, hours, mins) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m"),
        (0, _) => format!("{hours}h {mins}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// One sentence covering latest value, change, range and sample window, for screen readers and tooltips.
pub fn describe(summary: &HistorySummary, kind: ValueKind) -> String {
    let samples = match summary.samples {
        1 => "1 sample".to_owned(),
        n => format!("{n} samples"),
    };
    let mut text = if summary.samples == 1 {
        format!("Latest {}. Only one sample so far.", kind.value(summary.latest))
    } else {
        format!(
            "Latest {}, {} over {}. Low {}, high {}. {samples}.",
            kind.value(summary.latest),
            kind.change_words(summary.change),
            span_label(summary.span()),
            kind.value(summary.low),
            kind.value(summary.high),
        )
    };
    if let Some(gap) = summary.longest_gap {
        text.push_str(&format!(" Longest gap without data: {}.", span_label(gap)));
    }
    if let Some(stale) = summary.stale_for {
        text.push_str(&format!(" No new data for {}.", span_label(stale)));
    }
    text
}

/// The selected account and metric, with their history over the selected range.
struct Selection<'a> {
    account: &'a AccountSnapshot,
    metric: &'a Metric,
    kind: ValueKind,
    series: Vec<ChartPoint>,
    summary: Option<HistorySummary>,
}

pub struct HistoryView {
    history: Arc<Mutex<HistoryStore>>,
    accounts: Vec<AccountSnapshot>,
    account: Option<AccountId>,
    /// True once an account is picked here; until then the view follows the Usage view's selection.
    pinned: bool,
    metric: Option<String>,
    range: HistoryRange,
    /// The chart point the keyboard is on; `None` reads the latest.
    cursor: Option<usize>,
    chart_focus: FocusHandle,
}

impl HistoryView {
    pub fn new(history: Arc<Mutex<HistoryStore>>, cx: &mut Context<Self>) -> Self {
        Self {
            history,
            accounts: Vec::new(),
            account: None,
            pinned: false,
            metric: None,
            range: HistoryRange::Week,
            cursor: None,
            chart_focus: cx.focus_handle().tab_index(0).tab_stop(true),
        }
    }

    /// New accounts after a refresh. `preferred` (the account selected on the Usage view) is used until one is
    /// picked here.
    pub fn set_accounts(
        &mut self,
        accounts: Vec<AccountSnapshot>,
        preferred: Option<&AccountId>,
        cx: &mut Context<Self>,
    ) {
        self.accounts = accounts;
        let known = |id: &AccountId| self.accounts.iter().any(|account| account.id() == id);
        if !self.account.as_ref().is_some_and(known) {
            self.pinned = false;
        }
        self.follow(preferred, cx);
    }

    /// Shows the Usage view's selected account, unless an account was picked here.
    pub fn follow(&mut self, preferred: Option<&AccountId>, cx: &mut Context<Self>) {
        let known = |id: &AccountId| self.accounts.iter().any(|account| account.id() == id);
        if !self.pinned {
            let next = preferred
                .filter(|id| known(id))
                .or_else(|| self.accounts.first().map(AccountSnapshot::id))
                .cloned();
            if next != self.account {
                self.account = next;
                self.metric = None;
                self.cursor = None;
            }
        }
        cx.notify();
    }

    /// Shows `account` and, when it still reports it, `metric` (a widget tile, #95).
    pub fn focus(&mut self, account: &AccountId, metric: Option<&str>, cx: &mut Context<Self>) {
        self.select_account(account.clone(), cx);
        self.metric = metric.map(str::to_owned);
        // The chart starts at its latest point, as when a metric is picked here.
        self.cursor = None;
    }

    /// The account and metric key shown, for the headless UI tests.
    #[cfg(test)]
    pub fn shown(&self) -> Option<(String, String)> {
        let selection = self.selection()?;
        Some((selection.account.id().as_str().to_owned(), selection.metric.key()))
    }

    fn select_account(&mut self, id: AccountId, cx: &mut Context<Self>) {
        self.pinned = true;
        if self.account.as_ref() != Some(&id) {
            self.account = Some(id);
            self.metric = None;
            self.cursor = None;
        }
        cx.notify();
    }

    fn selection(&self) -> Option<Selection<'_>> {
        let account = self
            .account
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|account| account.id() == id))?;
        let metrics: Vec<&Metric> = charted_metrics(account).collect();
        let metric = self
            .metric
            .as_ref()
            .and_then(|key| metrics.iter().find(|metric| &metric.key() == key))
            .or_else(|| metrics.first())
            .copied()?;
        // The range rolls with the clock, not the last refresh, so it stays right with automatic refresh off.
        let now = Utc::now();
        let since = now - self.range.duration();
        let points = {
            let history = self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            history.points(account.id().as_str(), &metric.key(), since, now)
        };
        let summary = summarize(&points, now, GAP_THRESHOLD);
        Some(Selection {
            account,
            metric,
            kind: ValueKind::of(metric),
            series: chart_series(&points, MAX_CHART_POINTS),
            summary,
        })
    }

    /// Left/Right move the chart cursor one point, Home/End jump to the ends. Unmodified keys only.
    fn on_chart_key(&mut self, event: &KeyDownEvent, len: usize, cx: &mut Context<Self>) {
        if len == 0 || event.keystroke.modifiers.modified() {
            return;
        }
        let current = self.cursor.unwrap_or(len - 1).min(len - 1);
        let next = match event.keystroke.key.as_str() {
            "left" => current.saturating_sub(1),
            "right" => (current + 1).min(len - 1),
            "home" => 0,
            "end" => len - 1,
            _ => return,
        };
        cx.stop_propagation();
        self.cursor = Some(next);
        cx.notify();
    }
}

/// Metrics that have numeric history, in the account's order.
fn charted_metrics(account: &AccountSnapshot) -> impl Iterator<Item = &Metric> {
    account
        .metrics()
        .iter()
        .filter(|metric| metric.history_value().is_some())
}

fn point_label(point: &ChartPoint, range: HistoryRange) -> String {
    let local = point.at.with_timezone(&Local);
    match range {
        HistoryRange::Day => crate::locale::style().time(local.naive_local()),
        _ => crate::locale::style().month_day_time(local.naive_local()),
    }
}

impl Render for HistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.accounts.is_empty() {
            return empty_state(IconName::Calendar, "No accounts connected yet", None, cx).into_any_element();
        }
        let selection = self.selection();
        h_flex()
            .size_full()
            .items_start()
            .gap_4()
            .child(self.render_account_list(cx))
            .child(match selection {
                Some(selection) => self.render_detail(selection, cx).into_any_element(),
                None => empty_state(IconName::Inbox, "This account has no metrics with history", None, cx)
                    .into_any_element(),
            })
            .into_any_element()
    }
}

impl HistoryView {
    fn render_account_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("history-accounts")
            .role(Role::List)
            .aria_label("Accounts")
            .w(rems(16.))
            .flex_shrink_0()
            .gap_0p5()
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Accounts"),
            )
            .children(self.accounts.iter().enumerate().map(|(ix, account)| {
                let id = account.id().clone();
                let selected = self.account.as_ref() == Some(account.id());
                let shown = PrefsHub::shows(cx, account.id().as_str());
                let severity = account.assess(Utc::now()).severity();
                Button::new(("history-account", ix))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .selected(selected)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(div().size_2().rounded_full().bg(severity_dot_color(severity, cx)))
                            .child(div().flex_1().min_w_0().truncate().child(account.display_name()))
                            .when(!shown, |this| {
                                this.child(
                                    Icon::new(IconName::EyeOff)
                                        .small()
                                        .text_color(cx.theme().muted_foreground),
                                )
                            }),
                    )
                    .accessibility_label(if shown {
                        account.display_name()
                    } else {
                        format!("{}, history hidden", account.display_name())
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_account(id.clone(), cx)))
            }))
    }

    fn render_detail(&self, selection: Selection<'_>, cx: &mut Context<Self>) -> impl IntoElement {
        let account_id = selection.account.id().as_str().to_owned();
        let shown = PrefsHub::shows(cx, &account_id);
        let metrics: Vec<&Metric> = charted_metrics(selection.account).collect();
        let metric_ix = metrics
            .iter()
            .position(|metric| metric.key() == selection.metric.key())
            .unwrap_or(0);
        let metric_keys: Vec<String> = metrics.iter().map(|metric| metric.key()).collect();
        let range_ix = HistoryRange::ALL
            .iter()
            .position(|range| *range == self.range)
            .unwrap_or(0);

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().text_xl().font_semibold().child(selection.account.display_name()))
            .child(
                Switch::new("history-show")
                    .label("Show history")
                    .checked(shown)
                    .on_click({
                        let account_id = account_id.clone();
                        move |show, _, cx| PrefsHub::set(cx, &account_id, *show)
                    }),
            );

        let controls = h_flex()
            .gap_3()
            .flex_wrap()
            .when(metrics.len() > 1, |this| {
                this.child(
                    ButtonGroup::new("history-metric")
                        .outline()
                        .small()
                        .children(metrics.iter().enumerate().map(|(ix, metric)| {
                            Button::new(("history-metric", ix))
                                .label(metric.label().to_owned())
                                .selected(ix == metric_ix)
                        }))
                        .on_click(cx.listener(move |this, clicks: &Vec<usize>, _, cx| {
                            if let Some(key) = clicks.first().and_then(|ix| metric_keys.get(*ix)) {
                                this.metric = Some(key.clone());
                                this.cursor = None;
                                cx.notify();
                            }
                        })),
                )
            })
            .child(
                ButtonGroup::new("history-range")
                    .outline()
                    .small()
                    .children(HistoryRange::ALL.iter().enumerate().map(|(ix, range)| {
                        Button::new(("history-range", ix))
                            .label(range.label())
                            .selected(ix == range_ix)
                    }))
                    .on_click(cx.listener(|this, clicks: &Vec<usize>, _, cx| {
                        if let Some(range) = clicks.first().and_then(|ix| HistoryRange::ALL.get(*ix)) {
                            this.range = *range;
                            this.cursor = None;
                            cx.notify();
                        }
                    })),
            );

        let body = if !shown {
            empty_state(
                IconName::EyeOff,
                "History is hidden for this account",
                Some("Stored history is kept. Turn on Show history to see it here and in the table."),
                cx,
            )
            .into_any_element()
        } else {
            match &selection.summary {
                None => empty_state(
                    IconName::Inbox,
                    format!(
                        "No {} history in the last {}",
                        selection.metric.label(),
                        self.range.label()
                    ),
                    Some("CodexBar records usage on every refresh."),
                    cx,
                )
                .into_any_element(),
                Some(summary) => self.render_chart(&selection, summary, cx).into_any_element(),
            }
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(header)
            .child(controls)
            .children(crate::prefs_hub::PrefsHub::error(cx).map(|error| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(Icon::new(IconName::TriangleAlert).small())
                    .child(error)
            }))
            .child(body)
    }

    fn render_chart(
        &self,
        selection: &Selection<'_>,
        summary: &HistorySummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let kind = selection.kind;
        let range = self.range;
        let muted = cx.theme().muted_foreground;
        let stat = |label: &'static str, value: String| {
            v_flex()
                .gap_0p5()
                .child(div().text_sm().text_color(muted).child(label))
                .child(div().text_lg().font_semibold().child(value))
        };
        let stats = h_flex()
            .id("history-stats")
            .gap_8()
            .flex_wrap()
            .child(stat("Latest", kind.value(summary.latest)))
            .child(stat("Change", kind.change(summary.change)))
            .child(stat(
                "Range",
                if summary.is_flat() {
                    kind.value(summary.low)
                } else {
                    format!("{} – {}", kind.value(summary.low), kind.value(summary.high))
                },
            ))
            .child(stat(
                "Based on",
                match summary.samples {
                    1 => "1 sample".to_owned(),
                    n => format!("{n} samples over {}", span_label(summary.span())),
                },
            ));

        // The keyboard reads real readings only, never the line drawn across a gap, and skips a reading repeated at
        // the same time and value (one refresh can record the value another just stored), so each key press moves.
        let mut readings: Vec<ChartPoint> = selection
            .series
            .iter()
            .copied()
            .filter(|point| point.measured)
            .collect();
        let mut seen = std::collections::HashSet::new();
        readings.retain(|point| seen.insert((point.at.timestamp() / 60, point.value.to_bits())));
        let len = readings.len();
        let cursor = self.cursor.unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1));
        let readout = readings.get(cursor).map(|point| {
            format!(
                "{} · {}",
                crate::locale::style().full(point.at.with_timezone(&Local).naive_local()),
                kind.value(point.value)
            )
        });
        let description = describe(summary, kind);

        let color = cx.theme().chart_1;
        let chart: gpui_kit::AnyElement = if summary.samples < 2 {
            // One sample can't draw a line; show it as a value instead of a lone dot.
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .child(div().text_3xl().font_semibold().child(kind.value(summary.latest)))
                .child(div().text_sm().text_color(muted).child("Only one sample so far"))
                .into_any_element()
        } else {
            let point_count = selection.series.len();
            let data: Vec<(SharedString, f64)> = selection
                .series
                .iter()
                .map(|point| (SharedString::from(point_label(point, range)), point.value))
                .collect();
            let (domain_low, domain_high) = match kind {
                ValueKind::Percent => (0.0, 1.0),
                // Balances start at zero so a drop reads as a drop; a flat series still gets visible headroom.
                // A negative (overdrawn) balance extends the axis below zero instead of being clipped.
                ValueKind::Money(_) | ValueKind::Credits | ValueKind::Count => {
                    (summary.low.min(0.0) * 1.15, (summary.high * 1.15).max(1.0))
                }
            };
            AreaChart::new(data)
                .id("history-chart")
                .x(|(label, _)| label.clone())
                .y(|(_, value)| *value)
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.35), 1.),
                    linear_color_stop(color.opacity(0.), 0.),
                ))
                .name(selection.metric.label().to_owned())
                .linear()
                .y_domain(domain_low, domain_high)
                .y_padding(0., 0.)
                .y_axis(true)
                .y_tick_count(5)
                .point_count(point_count)
                .tick_margin((point_count / 6).max(1))
                .y_tick_format(move |value| kind.value(value))
                .into_any_element()
        };

        let ring = cx.theme().ring;
        v_flex()
            .gap_4()
            .child(stats)
            .when_some(summary.longest_gap, |this, gap| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(muted)
                        .child(Icon::new(IconName::Info).small())
                        .child(format!(
                            "Longest gap without data: {}. The line crosses gaps straight from one reading to the next.",
                            span_label(gap)
                        )),
                )
            })
            .when_some(summary.stale_for, |this, stale| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(format!(
                            "No new data for {}. Latest reads as of {}.",
                            span_label(stale),
                            crate::locale::style().full(summary.latest_at.with_timezone(&Local).naive_local())
                        )),
                )
            })
            .child(
                v_flex()
                    .id("history-chart-region")
                    .role(Role::Figure)
                    .test_support()
                    .aria_label(format!(
                        "{} history, last {}. {description}",
                        selection.metric.label(),
                        range.label()
                    ))
                    .track_focus(&self.chart_focus)
                    .on_key_down(
                        cx.listener(move |this, event: &KeyDownEvent, _, cx| this.on_chart_key(event, len, cx)),
                    )
                    .h(rems(20.))
                    .p_4()
                    .gap_2()
                    .bg(cx.theme().group_box)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius_lg)
                    .focus_visible(move |style| style.border_color(ring))
                    .child(div().flex_1().min_h_0().child(chart))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .text_sm()
                            .text_color(muted)
                            .child(
                                div()
                                    .id("history-readout")
                                    .role(Role::Status)
                                    .test_support()
                                    .aria_label(readout.clone().unwrap_or_default())
                                    .text_color(cx.theme().foreground)
                                    .child(readout.unwrap_or_default()),
                            )
                            .child("Focus the chart and use ← → Home End to read each point"),
                    ),
            )
    }
}

fn empty_state(
    icon: IconName,
    title: impl Into<SharedString>,
    detail: Option<&'static str>,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .id("history-empty")
        .role(Role::Status)
        .test_support()
        .flex_1()
        .min_h(rems(20.))
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(icon).large())
        .child(
            div()
                .font_semibold()
                .text_color(cx.theme().foreground)
                .child(title.into()),
        )
        .children(detail.map(|detail| div().text_sm().child(detail)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use codexbar_store::summary::Point;

    fn summary(values: &[(i64, f64)]) -> HistorySummary {
        let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let points: Vec<Point> = values
            .iter()
            .map(|&(minutes, value)| Point::new(start + Duration::minutes(minutes), value))
            .collect();
        summarize(
            &points,
            points.last().unwrap().at,
            codexbar_store::summary::GAP_THRESHOLD,
        )
        .unwrap()
    }

    #[test]
    fn money_values_use_the_metric_currency() {
        let euros = Metric::Balance {
            label: "Credits".into(),
            remaining: codexbar_core::Money::new(1842, Currency::Eur),
            burn_per_day: None,
        };
        let kind = ValueKind::of(&euros);
        assert_eq!(kind, ValueKind::Money(Currency::Eur));
        assert_eq!(kind.value(18.42), "\u{20ac}18.42");
        assert_eq!(ValueKind::Money(Currency::Jpy).value(1200.0), "\u{a5}1200");
        assert_eq!(ValueKind::Money(Currency::Jpy).change(0.3), "no change");
        assert_eq!(ValueKind::Money(Currency::Jpy).change(-40.0), "\u{2212}\u{a5}40");
    }

    #[test]
    fn value_kind_formats_values_and_changes() {
        assert_eq!(ValueKind::Percent.value(0.623), "62%");
        assert_eq!(ValueKind::Percent.change(0.18), "+18 pts");
        assert_eq!(ValueKind::Percent.change(-0.04), "\u{2212}4 pts");
        assert_eq!(ValueKind::Percent.change(0.001), "no change");
        assert_eq!(ValueKind::Money(Currency::Usd).value(18.42), "$18.42");
        assert_eq!(ValueKind::Money(Currency::Usd).value(-9.3), "\u{2212}$9.30");
        assert_eq!(ValueKind::Money(Currency::Usd).change(-9.3), "\u{2212}$9.30");
        assert_eq!(ValueKind::Money(Currency::Usd).change(0.0), "no change");
        assert_eq!(ValueKind::Credits.value(18_400.0), "18,400");
        assert_eq!(
            ValueKind::Credits.value(29_614.1),
            "29,614",
            "whole credits on axes and summaries"
        );
        assert_eq!(ValueKind::Credits.change(-1_700.0), "\u{2212}1,700");
        assert_eq!(ValueKind::Credits.change_words(2_000.0), "up 2,000 credits");
        assert_eq!(ValueKind::Count.value(37.0), "37");
        assert_eq!(ValueKind::Count.change(3.0), "+3");
        assert_eq!(ValueKind::Count.change(0.2), "no change");
        assert_eq!(ValueKind::Count.change_words(-2.0), "down 2");
        assert_eq!(
            ValueKind::of(&Metric::Activity {
                label: "Completed reviews".into(),
                count: 1
            }),
            ValueKind::Count
        );
    }

    #[test]
    fn span_label_picks_two_units() {
        assert_eq!(span_label(Duration::minutes(12)), "12m");
        assert_eq!(span_label(Duration::minutes(310)), "5h 10m");
        assert_eq!(span_label(Duration::minutes(7 * 24 * 60 - 60)), "6d 23h");
    }

    #[test]
    fn describe_covers_latest_change_range_and_window() {
        let text = describe(&summary(&[(0, 0.44), (60, 0.71), (120, 0.62)]), ValueKind::Percent);
        assert_eq!(
            text,
            "Latest 62%, up 18 points over 2h 0m. Low 44%, high 71%. 3 samples."
        );
    }

    #[test]
    fn describe_handles_single_samples_and_gaps() {
        assert_eq!(
            describe(&summary(&[(0, 12.6)]), ValueKind::Money(Currency::Usd)),
            "Latest $12.60. Only one sample so far."
        );
        let gappy = describe(&summary(&[(0, 0.1), (300, 0.1)]), ValueKind::Percent);
        assert!(
            gappy.ends_with("unchanged over 5h 0m. Low 10%, high 10%. 2 samples. Longest gap without data: 5h 0m.")
        );
    }
}
