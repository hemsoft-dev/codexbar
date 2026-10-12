//! The urgency-ordered account table on the Usage view.

use std::collections::HashMap;

use chrono::{DateTime, Local, Utc};
use codexbar_core::{AccountSnapshot, Metric, format};
use codexbar_store::summary::HistorySummary;
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex};
use gpui_kit::{
    Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, linear_color_stop, linear_gradient, rems,
};

use crate::history_view::{ValueKind, describe};
use crate::prefs_hub::PrefsHub;
use gpui_kit::TestSupportExt as _;

use crate::status::{severity_color, severity_dot_color, severity_tag};
use crate::zoom::scaled;

#[derive(Clone, Copy)]
enum Col {
    Account,
    Limit,
    Used,
    Percent,
    Progress,
    Resets,
    Trend,
}

const COLUMNS: [Col; 7] = [
    Col::Account,
    Col::Limit,
    Col::Used,
    Col::Percent,
    Col::Progress,
    Col::Resets,
    Col::Trend,
];

/// Each account's stored history summary for its primary metric, by account id.
pub type Compact = HashMap<String, (HistorySummary, ValueKind)>;

/// Where an account's shown usage comes from (#76).
#[derive(Clone, Debug, PartialEq)]
pub enum AccountState {
    /// Saved from an earlier run, waiting for this run's first fetch.
    Restored,
    /// Configured, fetch in progress, nothing known yet.
    Loading,
    /// From the latest fetch.
    Fresh,
    /// The latest fetch failed; the usage shown is the last good one. Holds the error.
    Failed(String),
    /// The first fetch failed: there is no usage to show yet. Holds the error.
    Unavailable(String),
}

/// Refresh state by account id; accounts not listed are fresh.
pub type States = HashMap<String, AccountState>;

/// Supplies rows for the gpui-kit `DataTable`. Rows arrive already arranged by group and order.
pub struct AccountTable {
    rows: Vec<AccountSnapshot>,
    now: DateTime<Utc>,
    compact: Compact,
    states: States,
    /// Group names by account id (#89); rows arrive grouped, and the name shows beside the account.
    groups: HashMap<String, String>,
}

impl AccountTable {
    pub fn new(rows: Vec<AccountSnapshot>, now: DateTime<Utc>, compact: Compact, states: States) -> Self {
        Self {
            rows,
            now,
            compact,
            states,
            groups: HashMap::new(),
        }
    }

    pub fn with_groups(mut self, groups: HashMap<String, String>) -> Self {
        self.groups = groups;
        self
    }

    pub fn set_compact(&mut self, compact: Compact) {
        self.compact = compact;
    }

    /// Advances the clock that freshness labels ("Saved 3m ago") read.
    pub fn set_now(&mut self, now: DateTime<Utc>) {
        self.now = now;
    }

    pub fn row(&self, ix: usize) -> Option<&AccountSnapshot> {
        self.rows.get(ix)
    }
}

impl TableDelegate for AccountTable {
    fn columns_count(&self, _: &gpui_kit::App) -> usize {
        COLUMNS.len()
    }

    fn rows_count(&self, _: &gpui_kit::App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, cx: &gpui_kit::App) -> Column {
        match COLUMNS[col_ix] {
            // DataTable widths are pixels, so they follow the interface zoom explicitly (#116).
            Col::Account => Column::new("account", "Account")
                .width(scaled(320., cx))
                .min_width(scaled(220., cx)),
            Col::Limit => Column::new("limit", "Limit").width(scaled(150., cx)),
            Col::Used => Column::new("used", "Used").width(scaled(110., cx)).text_right(),
            Col::Percent => Column::new("percent", "%").width(scaled(64., cx)).text_right(),
            Col::Progress => Column::new("progress", "Progress").width(scaled(200., cx)),
            Col::Resets => Column::new("resets", "Resets").width(scaled(110., cx)),
            Col::Trend => Column::new("trend", "14-day trend").width(scaled(200., cx)),
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let assessment = row.assess(self.now);
        let severity = assessment.severity();
        let primary = row.primary();
        let muted = cx.theme().muted_foreground;

        match COLUMNS[col_ix] {
            Col::Account => h_flex()
                .gap_2()
                .items_center()
                .min_w_0()
                .child(
                    div()
                        .size_2()
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(severity_dot_color(severity, cx)),
                )
                .child(crate::brand::badge(row.provider(), row.id().as_str().to_owned(), cx))
                .child(div().truncate().child(row.display_name()))
                .children(self.groups.get(row.id().as_str()).map(|group| {
                    div()
                        .flex_shrink_0()
                        .child(Tag::secondary().small().child(group.clone()))
                }))
                // A fully used limit has no pill (#150): its 100%, bar and dot already say so.
                .children(
                    severity_tag(severity)
                        .filter(|_| !assessment.is_exhausted())
                        .map(|tag| {
                            div()
                                .id(ElementId::Name(format!("severity-tag-{}", row.id().as_str()).into()))
                                .test_support()
                                .aria_label(severity.label().unwrap_or_default())
                                .flex_shrink_0()
                                .child(tag)
                        }),
                )
                .children(self.state_tag(row, cx))
                .into_any_element(),
            Col::Limit => div()
                .text_color(muted)
                .truncate()
                .child(primary.map(limit_label).unwrap_or_default())
                .into_any_element(),
            Col::Used => {
                let used = primary.map(Metric::used_display).unwrap_or_default();
                div()
                    .id(ElementId::Name(format!("used-{}", row.id().as_str()).into()))
                    .test_support()
                    .aria_label(format!("{}: {used}", primary.map(limit_label).unwrap_or_default()))
                    .w_full()
                    .text_right()
                    .child(used)
                    .into_any_element()
            }
            Col::Percent => div()
                .w_full()
                .text_right()
                .child(
                    primary
                        .and_then(Metric::used_fraction)
                        .map(|used| format!("{:.0}%", used * 100.0))
                        .unwrap_or_else(|| "—".into()),
                )
                .into_any_element(),
            Col::Progress => {
                // Credits draw their spend from the remembered start, and nothing without one (#150).
                let value = match primary {
                    Some(metric @ Metric::Credits { .. }) => match metric.progress() {
                        Some(progress) => progress,
                        None => {
                            return div()
                                .id(ElementId::Name(format!("progress-cell-{}", row.id().as_str()).into()))
                                .test_support()
                                .aria_label("No bar")
                                .text_color(muted)
                                .child("—")
                                .into_any_element();
                        }
                    },
                    _ => primary
                        .and_then(Metric::used_fraction)
                        .unwrap_or_else(|| assessment.pressure()),
                };
                h_flex()
                    .id(ElementId::Name(format!("progress-cell-{}", row.id().as_str()).into()))
                    .test_support()
                    .aria_label(format!("{:.0}% used", value * 100.0))
                    .size_full()
                    .items_center()
                    .child(
                        Progress::new(ElementId::Name(format!("progress-{}", row.id().as_str()).into()))
                            .small()
                            .value(value as f32 * 100.0)
                            .color(severity_color(severity, cx))
                            .accessibility_label(format!("{} used", row.display_name())),
                    )
                    .into_any_element()
            }
            Col::Resets => div()
                .text_color(muted)
                .child(
                    primary
                        .and_then(Metric::resets_at)
                        .map(|at| format::reset_label_in(at, self.now, &Local, crate::locale::style()))
                        .unwrap_or_else(|| "—".into()),
                )
                .into_any_element(),
            Col::Trend => self.trend_cell(row, cx),
        }
    }
}

impl AccountTable {
    /// "Loading", "Last known" or "Stale" beside an account that isn't fresh; stale names its error on hover and to
    /// screen readers.
    fn state_tag(&self, row: &AccountSnapshot, cx: &Context<TableState<AccountTable>>) -> Option<gpui_kit::AnyElement> {
        let id = row.id().as_str();
        let (tag, label, detail) = match self.states.get(id)? {
            AccountState::Fresh => return None,
            AccountState::Loading => (
                Tag::secondary(),
                "Loading",
                "Fetching usage for the first time.".to_owned(),
            ),
            AccountState::Restored => (
                Tag::secondary(),
                "Last known",
                format!(
                    "Saved {}; refreshing now.",
                    format::age_label(row.fetched_at(), self.now)
                ),
            ),
            // A failed first fetch has no usage to be stale; say it is unavailable instead.
            AccountState::Unavailable(error) => {
                (Tag::danger(), "Unavailable", format!("Couldn't fetch usage: {error}."))
            }
            AccountState::Failed(error) => (
                Tag::warning(),
                "Stale",
                format!(
                    "Refresh failed: {error}. Showing usage from {}.",
                    format::age_label(row.fetched_at(), self.now)
                ),
            ),
        };
        let _ = cx;
        let detail: SharedString = detail.into();
        Some(
            div()
                .id(ElementId::Name(format!("state-{id}").into()))
                .role(Role::Status)
                .test_support()
                .aria_label(format!("{label}. {detail}"))
                .flex_shrink_0()
                .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
                .child(tag.outline().child(label))
                .into_any_element(),
        )
    }

    /// The 14-day sparkline with the change over it. Hover (or a screen reader) gives the full compact summary:
    /// latest value, change, range and sample window. Hidden accounts say so instead.
    fn trend_cell(&self, row: &AccountSnapshot, cx: &Context<TableState<AccountTable>>) -> gpui_kit::AnyElement {
        let id = row.id().as_str();
        let muted = cx.theme().muted_foreground;
        if !PrefsHub::shows(cx, id) {
            return h_flex()
                .gap_1p5()
                .items_center()
                .text_sm()
                .text_color(muted)
                .child(Icon::new(IconName::EyeOff).small())
                .child("Hidden")
                .into_any_element();
        }
        let Some((summary, kind)) = self.compact.get(id) else {
            return sparkline(row, cx);
        };
        let text: SharedString = format!("Last 14 days: {}", describe(summary, *kind)).into();
        h_flex()
            .id(ElementId::Name(format!("trend-cell-{id}").into()))
            .w_full()
            .gap_2()
            .items_center()
            .role(Role::Group)
            .test_support()
            .aria_label(format!("{} trend. {text}", row.display_name()))
            .tooltip({
                let text = text.clone();
                move |window, cx| Tooltip::new(text.clone()).build(window, cx)
            })
            .child(div().flex_1().min_w_0().child(sparkline(row, cx)))
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(kind.change(summary.change)),
            )
            .into_any_element()
    }
}

/// What the primary metric limits: "5-hour window", "1,500 requests", "$ credits".
fn limit_label(metric: &Metric) -> String {
    match metric {
        Metric::Window { label, .. } => label.clone(),
        Metric::Quota { limit, label, .. } => {
            // The unit is the label's last word: "Premium requests" -> requests, "Chat messages" -> messages.
            let unit = label.rsplit(' ').next().unwrap_or("requests").to_lowercase();
            format!("{} {unit}", codexbar_core::group_thousands(*limit))
        }
        Metric::Spend { limit: Some(limit), .. } => format!("{} limit", limit.display()),
        Metric::Spend { label, .. } => label.clone(),
        Metric::Balance { label, remaining, .. } => {
            format!("{} {}", remaining.currency().symbol(), label.to_lowercase())
        }
        Metric::Credits { start: Some(start), .. } => format!("{} credits", codexbar_core::format_credits(*start)),
        Metric::Credits { label, .. } => label.clone(),
    }
}

/// A 14-day pressure trend. A sparkline shows shape, not level (the progress column shows level),
/// so each row fits its own range.
fn sparkline(row: &AccountSnapshot, cx: &Context<TableState<AccountTable>>) -> gpui_kit::AnyElement {
    if row.trend().len() < 2 {
        // Real providers have no history until #85 stores it.
        return div()
            .text_color(cx.theme().muted_foreground)
            .child("—")
            .into_any_element();
    }
    let color = cx.theme().chart_1;
    let points: Vec<(SharedString, f64)> = row
        .trend()
        .iter()
        .enumerate()
        .map(|(ix, value)| (SharedString::from(ix.to_string()), *value))
        .collect();
    let (low, high) = row.trend().iter().fold((f64::MAX, f64::MIN), |(low, high), value| {
        (low.min(*value), high.max(*value))
    });
    let pad = ((high - low) * 0.15).max(0.01);
    div()
        .w_full()
        .h(rems(1.625))
        .child(
            AreaChart::new(points)
                .id(ElementId::Name(format!("trend-{}", row.id().as_str()).into()))
                .x(|(label, _)| label.clone())
                .y(|(_, value)| *value)
                .y_domain((low - pad).max(0.0), high + pad)
                .linear()
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.35), 1.),
                    linear_color_stop(color.opacity(0.), 0.),
                ))
                .x_axis(false)
                .grid(false)
                .y_axis(false)
                .interactive(false)
                .appear(false),
        )
        .into_any_element()
}
