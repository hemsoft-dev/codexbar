//! The urgency-ordered account table on the Usage view.

use std::collections::HashMap;

use crate::usage_bar::{Projection, usage_bar};
use chrono::{DateTime, Utc};
use codexbar_core::{AccountSnapshot, Metric, format};
use codexbar_store::summary::HistorySummary;
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex};
use gpui_kit::{
    App, AppContext as _, Context, Div, DragMoveEvent, ElementId, EntityId, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, Render, Role, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, rems,
};

use crate::history_view::{ValueKind, describe};
use crate::prefs_hub::PrefsHub;
use gpui_kit::TestSupportExt as _;

/// A row being dragged to a new place (#178): the preview under the pointer, and what the drop needs.
#[derive(Clone)]
struct DragRow {
    /// The table the row came from, so another table's rows can't be dropped here.
    table: EntityId,
    account: String,
    label: SharedString,
    /// Where in the row the pointer grabbed it. GPUI draws the preview at the pointer less this offset, so the
    /// preview is pushed back by it to sit under the pointer wherever the row was grabbed.
    grab: Point<Pixels>,
}

impl Render for DragRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().pl(self.grab.x - px(12.)).pt(self.grab.y - px(14.)).child(
            div()
                .px_3()
                .py_1()
                .rounded(cx.theme().radius)
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .text_sm()
                .border_1()
                .border_color(cx.theme().border)
                .shadow_md()
                .child(self.label.clone()),
        )
    }
}

use crate::status::{severity_color, severity_dot_color, severity_tag};
use crate::zoom::scaled;

#[derive(Clone, Copy, PartialEq)]
enum Col {
    Account,
    Limit,
    Used,
    Percent,
    Progress,
    /// The standard windows side by side (#195), each as a percentage and a bar; blank for a row without one.
    Window(WindowKind, WindowCell),
    Resets,
    Trend,
}

/// The two windows most subscriptions share, shown side by side when the table is wide enough (#195).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowKind {
    FiveHour,
    Weekly,
}

impl WindowKind {
    /// Which standard window a metric is, by its label: "5-hour window", "Gemini Apps five-hour", "Weekly",
    /// "Weekly usage", "Weekly SuperGrok". A scoped weekly limit ("Weekly Opus") is weekly too; the first weekly
    /// metric of an account is the one shown, which providers list before the scoped ones.
    pub fn of(label: &str) -> Option<Self> {
        let label = label.to_ascii_lowercase();
        if label.contains("5-hour") || label.contains("five-hour") || label.contains("5 hour") {
            Some(Self::FiveHour)
        } else if label.contains("week") {
            Some(Self::Weekly)
        } else {
            None
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::FiveHour => "5-hour",
            Self::Weekly => "Weekly",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::FiveHour => "5h",
            Self::Weekly => "weekly",
        }
    }

    /// The account's window of this kind, when it reports one.
    pub fn in_account(self, account: &AccountSnapshot) -> Option<&Metric> {
        account
            .metrics()
            .iter()
            .find(|metric| matches!(metric, Metric::Window { .. }) && Self::of(metric.label()) == Some(self))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WindowCell {
    Percent,
    Bar,
}

/// The columns at the usual width: one metric per row, its primary one.
const NARROW: [Col; 7] = [
    Col::Account,
    Col::Limit,
    Col::Used,
    Col::Percent,
    Col::Progress,
    Col::Resets,
    Col::Trend,
];

/// The columns when there is room (#195): the 5-hour and weekly windows get a percentage and a bar each, and the
/// primary metric's own percentage and bar show only when it is neither (credits, premium requests, activity).
const WIDE: [Col; 11] = [
    Col::Account,
    Col::Limit,
    Col::Used,
    Col::Percent,
    Col::Progress,
    Col::Window(WindowKind::FiveHour, WindowCell::Percent),
    Col::Window(WindowKind::FiveHour, WindowCell::Bar),
    Col::Window(WindowKind::Weekly, WindowCell::Percent),
    Col::Window(WindowKind::Weekly, WindowCell::Bar),
    Col::Resets,
    Col::Trend,
];

/// The width the wide columns need, in unzoomed pixels: what the table must have to show both windows.
pub const WIDE_TABLE_WIDTH: f32 = 320. + 150. + 110. + 64. + 200. + 2. * (72. + 140.) + 110. + 200.;

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
    /// Rows can be dragged to a new place in their group (#178); off while the preferences file is read-only.
    draggable: bool,
    /// While a row is dragged over the table: the gap it would drop into, as the index of the row below the gap.
    drop_gap: Option<usize>,
    /// Both standard windows side by side (#195); the dashboard sets it from the window's width.
    wide: bool,
}

impl AccountTable {
    pub fn new(rows: Vec<AccountSnapshot>, now: DateTime<Utc>, compact: Compact, states: States) -> Self {
        Self {
            rows,
            now,
            compact,
            states,
            groups: HashMap::new(),
            draggable: false,
            drop_gap: None,
            wide: false,
        }
    }

    /// Whether both standard windows are shown side by side (#195).
    pub fn is_wide(&self) -> bool {
        self.wide
    }

    /// Shows both standard windows side by side, or one metric per row; the caller refreshes the table's columns.
    pub fn set_wide(&mut self, wide: bool) {
        self.wide = wide;
    }

    fn columns(&self) -> &'static [Col] {
        if self.wide { &WIDE } else { &NARROW }
    }

    pub fn with_groups(mut self, groups: HashMap<String, String>) -> Self {
        self.groups = groups;
        self
    }

    /// Lets rows be dragged to a new place in their group (#178).
    pub fn draggable(mut self, draggable: bool) -> Self {
        self.draggable = draggable;
        self
    }

    /// Whether two accounts share a group: both in the same one, or both ungrouped.
    fn same_group(&self, a: &str, b: &str) -> bool {
        self.groups.get(a) == self.groups.get(b)
    }

    /// What dropping `account` into `gap` means: `Some(Some(id))` to go before `id`, `Some(None)` to go last in its
    /// group, `None` when the gap lies inside another group. The gap sits between rows `gap - 1` and `gap`: the row
    /// below it decides when it shares the group, otherwise the row above does at the end of the group.
    fn drop_target(&self, account: &str, gap: usize) -> Option<Option<String>> {
        let id = |ix: usize| self.rows.get(ix).map(|row| row.id().as_str().to_owned());
        if let Some(below) = id(gap)
            && self.same_group(account, &below)
        {
            return Some(Some(below));
        }
        if let Some(above) = gap.checked_sub(1).and_then(id)
            && self.same_group(account, &above)
        {
            return Some(None);
        }
        None
    }

    /// True for a `pending:` row, which takes its real id once the first fetch succeeds: it is never dragged, never a
    /// drop target, and never written into the saved order.
    fn is_placeholder(id: &str) -> bool {
        id.starts_with(crate::dashboard::PLACEHOLDER_PREFIX)
    }

    /// Applies a drop of `account` into `gap` (#178): the manual order takes it, starting from the order shown, and
    /// Smart order gives way to Manual so the change shows. A gap inside another group is refused.
    fn drop(&self, account: &str, gap: usize, cx: &mut App) {
        use codexbar_core::layout::{LayoutError, OrderMode};
        let Some(before) = self.drop_target(account, gap) else {
            return;
        };
        if before.as_deref().is_some_and(Self::is_placeholder) {
            return;
        }
        let ids: Vec<String> = self
            .rows
            .iter()
            .map(|row| row.id().as_str().to_owned())
            .filter(|id| !Self::is_placeholder(id))
            .collect();
        let account = account.to_owned();
        let _ = PrefsHub::update_layout(cx, move |layout| {
            layout.set_mode(OrderMode::Manual);
            Ok::<_, LayoutError>(layout.place_account(&account, before.as_deref(), &ids))
        });
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
        self.columns().len()
    }

    /// The row itself: draggable to a new place in its group (#178), with a line in the gap a drop would take.
    fn render_tr(&mut self, row_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> Stateful<Div> {
        let tr = div().id(("row", row_ix));
        if !cx.has_active_drag() {
            self.drop_gap = None;
        }
        let Some(row) = self.rows.get(row_ix).filter(|_| self.draggable) else {
            return tr;
        };
        let account = row.id().as_str().to_owned();
        if Self::is_placeholder(&account) {
            return tr;
        }
        let last = row_ix + 1 == self.rows.len();
        let indicator = match self.drop_gap {
            Some(gap) if gap == row_ix => Some(false),
            Some(gap) if last && gap == row_ix + 1 => Some(true),
            _ => None,
        };
        let drag = DragRow {
            table: cx.entity_id(),
            account,
            label: row.display_name().into(),
            grab: Point::default(),
        };
        let accent = cx.theme().primary;
        tr.relative()
            .cursor_grab()
            .on_drag(drag, |drag, grab, _, cx| {
                cx.stop_propagation();
                let drag = DragRow { grab, ..drag.clone() };
                cx.new(|_| drag)
            })
            .on_drag_move(cx.listener(move |table, event: &DragMoveEvent<DragRow>, _, cx| {
                let drag = event.drag(cx);
                if drag.table != cx.entity_id() || !event.bounds.contains(&event.event.position) {
                    return;
                }
                let top_half = event.event.position.y < event.bounds.origin.y + event.bounds.size.height / 2.;
                let gap = if top_half { row_ix } else { row_ix + 1 };
                // Only a gap the drop would take gets the line; a gap inside another group shows nothing.
                let gap = Some(gap).filter(|gap| {
                    table
                        .delegate()
                        .drop_target(&drag.account, *gap)
                        .is_some_and(|before| !before.as_deref().is_some_and(Self::is_placeholder))
                });
                if table.delegate().drop_gap != gap {
                    table.delegate_mut().drop_gap = gap;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|table, drag: &DragRow, _, cx| {
                let gap = table.delegate_mut().drop_gap.take();
                if drag.table != cx.entity_id() {
                    return;
                }
                if let Some(gap) = gap {
                    table.delegate().drop(&drag.account, gap, cx);
                }
                cx.notify();
            }))
            .when_some(indicator, |this, bottom| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(accent)
                        .map(|this| if bottom { this.bottom_0() } else { this.top_0() }),
                )
            })
    }

    fn rows_count(&self, _: &gpui_kit::App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, cx: &gpui_kit::App) -> Column {
        match self.columns()[col_ix] {
            Col::Window(kind, WindowCell::Percent) => {
                Column::new(SharedString::from(format!("{}-percent", kind.key())), kind.title())
                    .width(scaled(72., cx))
                    .text_right()
            }
            Col::Window(kind, WindowCell::Bar) => {
                Column::new(SharedString::from(format!("{}-bar", kind.key())), "").width(scaled(140., cx))
            }
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
        // Wide, a standard window has its own columns; the primary pair then shows only what is neither (#195).
        let own_pair = if self.wide {
            primary.filter(|metric| WindowKind::of(metric.label()).is_none())
        } else {
            primary
        };

        match self.columns()[col_ix] {
            Col::Window(kind, cell) => {
                let metric = kind.in_account(row);
                let id = format!("window-{}-{}", kind.key(), row.id().as_str());
                let name = metric.map_or(kind.title(), Metric::label);
                match cell {
                    WindowCell::Percent => percent_cell(metric, name, &id, muted),
                    WindowCell::Bar => match metric {
                        Some(metric) => {
                            bar_cell(metric, row, severity, &assessment, self.now, &format!("{id}-bar"), cx)
                        }
                        None => blank_cell(&format!("{id}-bar"), kind.title(), muted),
                    },
                }
            }
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
            Col::Percent => {
                let id = format!("percent-{}", row.id().as_str());
                let name = own_pair.map_or("Primary metric", Metric::label);
                percent_cell(own_pair, name, &id, muted)
            }
            Col::Progress => {
                let id = format!("progress-cell-{}", row.id().as_str());
                match own_pair {
                    Some(metric) => bar_cell(metric, row, severity, &assessment, self.now, &id, cx),
                    // Wide, the row's window sits in its own columns; narrow, a row always has a primary metric.
                    None if self.wide && primary.is_some() => blank_cell(&id, "Primary metric", muted),
                    None => bar_cell_without_metric(&id, &assessment, severity, row, cx),
                }
            }
            // Always how long is left (#196), never a weekday, time or date; the clock refreshes it every second.
            Col::Resets => {
                let label = primary
                    .and_then(Metric::resets_at)
                    .map(|at| format::time_left_label(at, self.now))
                    .unwrap_or_else(|| "—".into());
                div()
                    .id(ElementId::Name(format!("resets-{}", row.id().as_str()).into()))
                    .test_support()
                    .aria_label(format!("Resets: {label}"))
                    .text_color(muted)
                    .child(label)
                    .into_any_element()
            }
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
/// A metric's share used, as "42%", or a dash with nothing to show. `name` names the metric for screen readers.
fn percent_cell(metric: Option<&Metric>, name: &str, id: &str, muted: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    let percent = metric.and_then(Metric::used_fraction);
    let text = percent.map_or_else(|| "—".to_owned(), |used| format!("{:.0}%", used * 100.0));
    let label = match percent {
        Some(_) => format!("{name}: {text}"),
        None => format!("{name}: not applicable"),
    };
    div()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label(label)
        .w_full()
        .text_right()
        .when(percent.is_none(), |this| this.text_color(muted))
        .child(text)
        .into_any_element()
}

/// A dash where a row has no metric for a column, named for screen readers so the gap reads as deliberate.
fn blank_cell(id: &str, name: &str, muted: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    div()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label(format!("{name}: not applicable"))
        .text_color(muted)
        .child("—")
        .into_any_element()
}

/// The bar for `metric`: its share used, or for credits the spend from the remembered start (#150), with the share
/// projected by the reset faded past it and a tooltip saying both (#197); an activity count or credits without a
/// start have no bar (#103).
fn bar_cell(
    metric: &Metric,
    row: &AccountSnapshot,
    severity: codexbar_core::Severity,
    assessment: &codexbar_core::Assessment,
    now: DateTime<Utc>,
    id: &str,
    cx: &gpui_kit::App,
) -> gpui_kit::AnyElement {
    let muted = cx.theme().muted_foreground;
    let used = match metric {
        Metric::Activity { .. } => return no_bar(id, muted),
        Metric::Credits { .. } => match metric.progress() {
            Some(progress) => progress,
            None => return no_bar(id, muted),
        },
        _ => metric.used_fraction().unwrap_or_else(|| assessment.pressure()),
    };
    bar(id, &Projection::of(metric, used, now), severity, row, cx)
}

/// The bar for a row with no primary metric: the account's pressure, with nothing to project.
fn bar_cell_without_metric(
    id: &str,
    assessment: &codexbar_core::Assessment,
    severity: codexbar_core::Severity,
    row: &AccountSnapshot,
    cx: &gpui_kit::App,
) -> gpui_kit::AnyElement {
    bar(id, &Projection::bare(assessment.pressure()), severity, row, cx)
}

fn no_bar(id: &str, muted: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    div()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label("No bar")
        .text_color(muted)
        .child("—")
        .into_any_element()
}

fn bar(
    id: &str,
    projection: &Projection,
    severity: codexbar_core::Severity,
    row: &AccountSnapshot,
    cx: &gpui_kit::App,
) -> gpui_kit::AnyElement {
    usage_bar(
        id,
        projection,
        severity_color(severity, cx),
        crate::usage_bar::small(),
        row.display_name(),
        cx,
    )
}

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
        // A count with no limit (#103): the column names what is counted.
        Metric::Activity { label, .. } => label.clone(),
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
