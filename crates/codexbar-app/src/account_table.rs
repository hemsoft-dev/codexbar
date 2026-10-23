//! The urgency-ordered account table on the Usage view.

use std::collections::HashMap;

use crate::usage_bar::{Forecast, Projection, usage_bar};
use chrono::{DateTime, Utc};
use codexbar_core::{AccountSnapshot, Metric, Provider, format};
use codexbar_store::summary::HistorySummary;
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex};
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

use crate::status::{severity_dot_color, severity_tag};
use crate::zoom::scaled;

#[derive(Clone, Copy, PartialEq)]
enum Col {
    Account,
    Limit,
    Used,
    Percent,
    Progress,
    /// Where the primary metric's pace lands by the reset and when its limit runs out (#234), as the Pi dashboard's
    /// "At reset" column says it: "112% · runs out in 5d 4h".
    AtReset,
    /// The standard windows side by side (#195), each as a percentage, a bar and its own reset (#204); blank for a
    /// row without one.
    Window(WindowKind, WindowCell),
    Resets,
    Trend,
}

impl Col {
    /// The column's key: its id in the table and its name in the saved widths (#245).
    fn key(self) -> String {
        match self {
            Self::Account => "account".into(),
            Self::Limit => "limit".into(),
            Self::Used => "used".into(),
            Self::Percent => "percent".into(),
            Self::Progress => "progress".into(),
            Self::AtReset => "at-reset".into(),
            Self::Resets => "resets".into(),
            Self::Trend => "trend".into(),
            Self::Window(kind, WindowCell::Percent) => format!("{}-percent", kind.key()),
            Self::Window(kind, WindowCell::Bar) => format!("{}-bar", kind.key()),
            Self::Window(kind, WindowCell::AtReset) => format!("{}-at-reset", kind.key()),
            Self::Window(kind, WindowCell::Resets) => format!("{}-resets", kind.key()),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Account => "Account",
            Self::Limit => "Limit",
            Self::Used => "Used",
            Self::Percent => "%",
            Self::Progress => "Progress",
            Self::AtReset | Self::Window(_, WindowCell::AtReset) => "At reset",
            Self::Resets | Self::Window(_, WindowCell::Resets) => "Resets",
            Self::Trend => "14-day trend",
            Self::Window(kind, WindowCell::Percent) => kind.title(),
            Self::Window(_, WindowCell::Bar) => "",
        }
    }

    /// The default width and the narrowest a drag can make it (#245), in unzoomed pixels. The minimum keeps the
    /// header and a typical value readable.
    fn widths(self) -> (f32, f32) {
        match self {
            Self::Account => (ACCOUNT_WIDTH, 220.),
            Self::Limit => (LIMIT_WIDTH, 80.),
            Self::Used => (USED_WIDTH, 64.),
            Self::Percent => (PERCENT_WIDTH, 48.),
            Self::Progress => (PROGRESS_WIDTH, 80.),
            Self::AtReset | Self::Window(_, WindowCell::AtReset) => (AT_RESET_WIDTH, 100.),
            Self::Resets | Self::Window(_, WindowCell::Resets) => (RESETS_WIDTH, 64.),
            Self::Trend => (TREND_WIDTH, 100.),
            Self::Window(_, WindowCell::Percent) => (WINDOW_PERCENT_WIDTH, 56.),
            Self::Window(_, WindowCell::Bar) => (WINDOW_BAR_WIDTH, 64.),
        }
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::Used | Self::Percent | Self::Window(_, WindowCell::Percent))
    }
}

/// How far past its default a column can be dragged (#245), as a multiple of it.
const MAX_WIDTH_FACTOR: f32 = 4.;

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

/// The metric an account's row shows: its primary one, except that a Claude row shows the weekly window when the
/// primary is the 5-hour one (#246). The week is what paces a Claude subscription; the 5-hour window resets several
/// times a day. Only the all-model "Weekly" window counts: a model-scoped one ("Weekly Opus") doesn't pace the whole
/// subscription. Everything else (focus cards, the tooltip, alerts, widgets) keeps the primary metric.
pub fn row_metric(account: &AccountSnapshot) -> Option<&Metric> {
    let primary = account.primary()?;
    if account.provider() == Provider::Claude
        && WindowKind::of(primary.label()) == Some(WindowKind::FiveHour)
        && matches!(primary, Metric::Window { .. })
        && let Some(weekly) = account
            .metrics()
            .iter()
            .find(|metric| matches!(metric, Metric::Window { .. }) && metric.label().eq_ignore_ascii_case("weekly"))
    {
        return Some(weekly);
    }
    Some(primary)
}

/// The daily trend of the metric an account's row shows (#246): the account's own trend for its primary metric, else
/// that limit's stored series.
fn row_trend(account: &AccountSnapshot) -> &[f64] {
    match (row_metric(account), account.primary()) {
        (Some(metric), Some(primary)) if metric.key() != primary.key() => account
            .detail()
            .limit_series(&metric.key())
            .map_or(&[], |series| series.trend.as_slice()),
        _ => account.trend(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WindowCell {
    Percent,
    Bar,
    AtReset,
    Resets,
}

/// The columns at the usual width: one metric per row, the one `row_metric` picks.
const NARROW: [Col; 8] = [
    Col::Account,
    Col::Limit,
    Col::Used,
    Col::Percent,
    Col::Progress,
    Col::AtReset,
    Col::Resets,
    Col::Trend,
];

/// The columns when there is room (#195): the 5-hour and weekly windows get a percentage, a bar, the share at the
/// reset (#234) and their own reset each (#204), and the primary metric's own follow them (#207), filled only when it
/// is neither window (credits, premium requests, activity).
const WIDE: [Col; 16] = [
    Col::Account,
    Col::Limit,
    Col::Used,
    Col::Window(WindowKind::FiveHour, WindowCell::Percent),
    Col::Window(WindowKind::FiveHour, WindowCell::Bar),
    Col::Window(WindowKind::FiveHour, WindowCell::AtReset),
    Col::Window(WindowKind::FiveHour, WindowCell::Resets),
    Col::Window(WindowKind::Weekly, WindowCell::Percent),
    Col::Window(WindowKind::Weekly, WindowCell::Bar),
    Col::Window(WindowKind::Weekly, WindowCell::AtReset),
    Col::Window(WindowKind::Weekly, WindowCell::Resets),
    Col::Percent,
    Col::Progress,
    Col::AtReset,
    Col::Resets,
    Col::Trend,
];

/// The width the wide columns need, in unzoomed pixels: what the table must have to show both windows.
pub const WIDE_TABLE_WIDTH: f32 = ACCOUNT_WIDTH
    + LIMIT_WIDTH
    + USED_WIDTH
    + 2. * (WINDOW_PERCENT_WIDTH + WINDOW_BAR_WIDTH + AT_RESET_WIDTH + RESETS_WIDTH)
    + PERCENT_WIDTH
    + PROGRESS_WIDTH
    + AT_RESET_WIDTH
    + RESETS_WIDTH
    + TREND_WIDTH;

/// Default column widths in unzoomed pixels; a column the user dragged keeps the width it was given (#245). The "At
/// reset" column (#234) took its room from the account, limit, used, progress, resets and trend columns, so the
/// narrow table still fits the default 1440-pixel window.
const ACCOUNT_WIDTH: f32 = 275.;
const LIMIT_WIDTH: f32 = 145.;
const USED_WIDTH: f32 = 100.;
const PERCENT_WIDTH: f32 = 64.;
const PROGRESS_WIDTH: f32 = 160.;
const AT_RESET_WIDTH: f32 = 210.;
const RESETS_WIDTH: f32 = 90.;
const TREND_WIDTH: f32 = 140.;
const WINDOW_PERCENT_WIDTH: f32 = 72.;
const WINDOW_BAR_WIDTH: f32 = 140.;

/// Each account's stored history summary for the metric its row shows (#246), by account id.
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

    /// The key of the column at `ix`, as its width is saved under (#245).
    pub fn column_key(&self, ix: usize) -> Option<String> {
        self.columns().get(ix).map(|col| col.key())
    }

    /// The default width of the column at `ix`, unzoomed (#245).
    pub fn column_default_width(&self, ix: usize) -> Option<f32> {
        self.columns().get(ix).map(|col| col.widths().0)
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
        let col = self.columns()[col_ix];
        let (default, min) = col.widths();
        let max = default * MAX_WIDTH_FACTOR;
        let key = col.key();
        // Users drag the borders to resize (#245); a saved width comes back, kept within the column's limits.
        let width = crate::prefs_hub::PrefsHub::column_width(cx, &key)
            .unwrap_or(default)
            .clamp(min, max);
        // DataTable widths are pixels, so they follow the interface zoom explicitly (#116).
        let column = Column::new(SharedString::from(key), col.title())
            .width(scaled(width, cx))
            .min_width(scaled(min, cx))
            .max_width(scaled(max, cx));
        if col.right_aligned() {
            column.text_right()
        } else {
            column
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
        let primary = row_metric(row);
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
                        Some(metric) => bar_cell(metric, row, &assessment, self.now, &format!("{id}-bar"), cx),
                        None => blank_cell(&format!("{id}-bar"), kind.title(), muted),
                    },
                    WindowCell::AtReset => match metric {
                        Some(metric) => at_reset_cell(metric, row, self.now, &format!("{id}-at-reset"), name, cx),
                        None => blank_cell(&format!("{id}-at-reset"), &format!("{} at reset", kind.title()), muted),
                    },
                    // The window's own reset (#204), as time left (#196), beside its bar.
                    WindowCell::Resets => match metric.and_then(Metric::resets_at) {
                        Some(at) => resets_cell(
                            &format!("{id}-resets"),
                            &format!("{name} resets"),
                            &format::time_left_label(at, self.now),
                            muted,
                        ),
                        None => blank_cell(&format!("{id}-resets"), &format!("{name} resets"), muted),
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
                .child(
                    div()
                        .id(ElementId::Name(format!("account-name-{}", row.id().as_str()).into()))
                        .test_support()
                        .aria_label(row.account_name())
                        .truncate()
                        .child(row.account_name()),
                )
                .children(row.plan().map(|plan| {
                    let full: SharedString = plan.to_owned().into();
                    div()
                        .id(ElementId::Name(format!("plan-badge-{}", row.id().as_str()).into()))
                        .test_support()
                        .aria_label(plan.to_owned())
                        .flex_shrink_0()
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                        .child(
                            Tag::secondary()
                                .small()
                                .child(div().max_w(px(56.)).truncate().child(plan.to_owned())),
                        )
                }))
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
                    Some(metric) => bar_cell(metric, row, &assessment, self.now, &id, cx),
                    // Wide, the row's window sits in its own columns; narrow, a row always has a primary metric.
                    None if self.wide && primary.is_some() => blank_cell(&id, "Primary metric", muted),
                    None => bar_cell_without_metric(&id, &assessment, severity, row, cx),
                }
            }
            Col::AtReset => {
                let id = format!("at-reset-{}", row.id().as_str());
                match own_pair {
                    Some(metric) => at_reset_cell(metric, row, self.now, &id, metric.label(), cx),
                    None => blank_cell(&id, "At reset", muted),
                }
            }
            // Always how long is left (#196), never a weekday, time or date; the clock refreshes it every second.
            // Wide, a window's reset sits in its own column (#204), so this one is blank for it.
            Col::Resets => {
                let id = format!("resets-{}", row.id().as_str());
                match own_pair.and_then(Metric::resets_at) {
                    Some(at) => resets_cell(&id, "Resets", &format::time_left_label(at, self.now), muted),
                    None if self.wide && primary.is_some() => blank_cell(&id, "Resets", muted),
                    None => resets_cell(&id, "Resets", "—", muted),
                }
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

/// A reset as time left (#196), named for its window so screen readers tell the columns apart (#204).
fn resets_cell(id: &str, name: &str, text: &str, muted: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    div()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label(format!("{name}: {text}"))
        .text_color(muted)
        .child(text.to_owned())
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

/// A metric's own bar (#203), graded by its own runway (#234) rather than the account's worst, so a weekly window at
/// 53% stays calm while the 5-hour window beside it is about to run out. A used-up window the account's credits carry
/// it past (#150) stays calm too, as it does for the account.
fn bar_projection(metric: &Metric, used: f64, row: &AccountSnapshot, now: DateTime<Utc>) -> Projection {
    Projection::of_account(metric, used, row, now)
}

/// The share at the reset and what it means (#234), as the Pi dashboard's "At reset" column says it: the share in the
/// runway's color, then "runs out in 5d 4h", "near limit" or "limit reached" muted; "no forecast" for a limit too young
/// to project, a dash for what has none. `name` names the metric for screen readers, which hear "at reset" too.
fn at_reset_cell(
    metric: &Metric,
    row: &AccountSnapshot,
    now: DateTime<Utc>,
    id: &str,
    name: &str,
    cx: &gpui_kit::App,
) -> gpui_kit::AnyElement {
    let muted = cx.theme().muted_foreground;
    let plain = |text: &str, spoken: String, color: gpui_kit::Hsla| {
        div()
            .id(ElementId::Name(id.to_owned().into()))
            .test_support()
            .aria_label(spoken)
            .text_color(color)
            .truncate()
            .child(text.to_owned())
            .into_any_element()
    };
    // A used-up window the account's credits carry (#150) neither runs out nor is reached: the credits pay for it.
    if row.credits_cover(metric) {
        return plain(
            crate::usage_bar::COVERED,
            format!("{name} at reset: {}", crate::usage_bar::COVERED),
            muted,
        );
    }
    let Some(forecast) = Forecast::of(metric, now) else {
        // Only a limit with a reset ahead can be forecast; one without (a money-only spend cap) is not applicable.
        let limited = (matches!(metric, Metric::Window { .. } | Metric::Quota { .. })
            || matches!(metric, Metric::Spend { limit: Some(_), .. }))
            && metric.resets_at().is_some();
        // A used-up limit with no pace left to project still says so, as the Pi dashboard does.
        let used_up = metric.used_fraction().is_some_and(|used| used >= 1.0);
        let (text, spoken) = if used_up {
            ("limit reached", format!("{name} at reset: limit reached"))
        } else if limited {
            ("no forecast", format!("{name} at reset: no forecast"))
        } else {
            ("—", format!("{name} at reset: not applicable"))
        };
        return plain(text, spoken, if used_up { cx.theme().danger } else { muted });
    };
    let runway = forecast.runway();
    let tint = match runway {
        crate::usage_bar::Runway::Ok => cx.theme().foreground,
        crate::usage_bar::Runway::Tight => cx.theme().warning,
        crate::usage_bar::Runway::Risk | crate::usage_bar::Runway::Out => cx.theme().danger,
    };
    let phrase = forecast.phrase();
    h_flex()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label(format!("{name}: {}", forecast.text()))
        .gap_2()
        .min_w_0()
        .items_baseline()
        .child(
            div()
                .flex_shrink_0()
                .font_semibold()
                .text_color(tint)
                .child(forecast.share()),
        )
        .children(phrase.map(|phrase| {
            let color = if runway >= crate::usage_bar::Runway::Risk {
                tint
            } else {
                muted
            };
            div().truncate().text_color(color).child(phrase)
        }))
        .into_any_element()
}

/// The bar for `metric`: its share used, or for credits the spend from the remembered start (#150), with the share
/// projected by the reset faded past it and a tooltip saying both (#197); an activity count or credits without a
/// start have no bar (#103). Colored by the metric's own severity (#203).
fn bar_cell(
    metric: &Metric,
    row: &AccountSnapshot,
    assessment: &codexbar_core::Assessment,
    now: DateTime<Utc>,
    id: &str,
    cx: &gpui_kit::App,
) -> gpui_kit::AnyElement {
    let muted = cx.theme().muted_foreground;
    let used = match metric {
        Metric::Activity { .. } => {
            return gpui_kit::component::h_flex()
                .gap_2()
                .child(no_bar(id, muted))
                .children(crate::sus::metric(metric, now, &format!("{id}-sus"), cx))
                .into_any_element();
        }
        Metric::Credits { .. } => match metric.progress() {
            Some(progress) => progress,
            None => return no_bar(id, muted),
        },
        _ => metric.used_fraction().unwrap_or_else(|| assessment.pressure()),
    };
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(bar(id, &bar_projection(metric, used, row, now), row, cx)),
        )
        .children(crate::sus::metric(metric, now, &format!("{id}-sus"), cx))
        .into_any_element()
}

/// The bar for a row with no primary metric: the account's pressure, with nothing to project.
fn bar_cell_without_metric(
    id: &str,
    assessment: &codexbar_core::Assessment,
    severity: codexbar_core::Severity,
    row: &AccountSnapshot,
    cx: &gpui_kit::App,
) -> gpui_kit::AnyElement {
    if matches!(row.provider(), Provider::Grok | Provider::XSubscription) {
        return h_flex()
            .gap_2()
            .items_center()
            .child(no_bar(id, cx.theme().muted_foreground))
            .child(crate::sus::badge(None, &format!("{id}-sus"), "Quota unavailable", cx))
            .into_any_element();
    }
    bar(id, &Projection::bare(assessment.pressure()).graded(severity), row, cx)
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

fn bar(id: &str, projection: &Projection, row: &AccountSnapshot, cx: &gpui_kit::App) -> gpui_kit::AnyElement {
    usage_bar(
        id,
        projection,
        cx.theme().chart_1,
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
    let trend = row_trend(row);
    if trend.len() < 2 {
        // Real providers have no history until #85 stores it.
        return div()
            .text_color(cx.theme().muted_foreground)
            .child("—")
            .into_any_element();
    }
    let color = cx.theme().chart_1;
    let points: Vec<(SharedString, f64)> = trend
        .iter()
        .enumerate()
        .map(|(ix, value)| (SharedString::from(ix.to_string()), *value))
        .collect();
    let (low, high) = trend.iter().fold((f64::MAX, f64::MIN), |(low, high), value| {
        (low.min(*value), high.max(*value))
    });
    let pad = ((high - low) * 0.15).max(0.01);
    div()
        .id(ElementId::Name(format!("sparkline-{}", row.id().as_str()).into()))
        .test_support()
        .aria_label(format!(
            "14-day sparkline: {} samples, latest {:.0}%",
            trend.len(),
            trend[trend.len() - 1] * 100.0
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_bar::Runway;
    use chrono::Duration;
    use codexbar_core::{AccountId, Pace, Provider, Severity};

    fn now() -> DateTime<Utc> {
        "2026-10-19T08:00:00Z".parse().unwrap()
    }

    fn window(label: &str, used: f64, resets_in: Duration, pace: Pace) -> Metric {
        Metric::Window {
            label: label.into(),
            used,
            resets_at: now() + resets_in,
            pace: Some(pace),
            length: None,
        }
    }

    fn account(metrics: Vec<Metric>) -> AccountSnapshot {
        AccountSnapshot::new(AccountId::new("codex-1"), Provider::Codex, metrics, now())
    }

    fn claude(metrics: Vec<Metric>) -> AccountSnapshot {
        AccountSnapshot::new(AccountId::new("claude-1"), Provider::Claude, metrics, now())
    }

    /// #246: Claude lists its 5-hour window first; its row shows the weekly one, which paces the subscription.
    #[test]
    fn a_claude_row_shows_the_weekly_window_not_the_five_hour_one() {
        let five_hour = window("5-hour window", 0.64, Duration::minutes(72), Pace::per_hour(0.12));
        let weekly = window("Weekly", 0.82, Duration::days(2), Pace::per_day(0.05));
        let opus = window("Weekly Opus", 0.3, Duration::days(2), Pace::per_day(0.02));
        let row = claude(vec![five_hour.clone(), weekly.clone(), opus]);
        assert_eq!(row_metric(&row), Some(&weekly));
        assert_eq!(
            row.primary(),
            Some(&five_hour),
            "everything else keeps the 5-hour window"
        );
    }

    #[test]
    fn a_claude_row_without_a_weekly_window_shows_its_first_metric() {
        let five_hour = window("5-hour window", 0.64, Duration::minutes(72), Pace::per_hour(0.12));
        let row = claude(vec![five_hour.clone()]);
        assert_eq!(row_metric(&row), Some(&five_hour));
        assert_eq!(row_metric(&claude(Vec::new())), None);
        // A model-scoped weekly limit isn't the subscription's week.
        let opus = window("Weekly Opus", 0.3, Duration::days(2), Pace::per_day(0.02));
        assert_eq!(row_metric(&claude(vec![five_hour.clone(), opus])), Some(&five_hour));
    }

    /// Credits lead when a window is used up and they can still be spent (#150); the row keeps showing them.
    #[test]
    fn a_claude_row_led_by_its_credits_keeps_showing_them() {
        let credits = Metric::Credits {
            label: "Extra usage".into(),
            balance: Some(12.0),
            start: None,
        };
        let row = claude(vec![
            credits.clone(),
            window("5-hour window", 1.0, Duration::minutes(30), Pace::per_hour(0.2)),
            window("Weekly", 0.82, Duration::days(2), Pace::per_day(0.05)),
        ]);
        assert_eq!(row_metric(&row), Some(&credits));
    }

    /// Other providers' rows keep their primary metric, even with the same two windows.
    #[test]
    fn other_providers_rows_keep_their_primary_metric() {
        let five_hour = window("5-hour window", 0.91, Duration::minutes(38), Pace::per_hour(0.216));
        let weekly = window("Weekly", 0.53, Duration::days(6), Pace::per_hour(0.00007));
        let row = account(vec![five_hour.clone(), weekly]);
        assert_eq!(row_metric(&row), Some(&five_hour));
    }

    /// The row's sparkline follows the row's metric: Weekly's stored series for Claude, the account's own trend else.
    #[test]
    fn a_claude_row_trend_follows_the_weekly_window() {
        use codexbar_core::{AccountDetail, LimitSeries};
        let five_hour = window("5-hour window", 0.64, Duration::minutes(72), Pace::per_hour(0.12));
        let weekly = window("Weekly", 0.82, Duration::days(2), Pace::per_day(0.05));
        let row = claude(vec![five_hour, weekly])
            .with_trend(vec![0.9, 0.5, 0.95])
            .with_detail(AccountDetail::default().with_limit_series(
                "weekly",
                LimitSeries {
                    curve: None,
                    trend: vec![0.7, 0.76, 0.82],
                },
            ));
        assert_eq!(row_trend(&row), [0.7, 0.76, 0.82]);
        let codex = account(vec![window(
            "5-hour window",
            0.2,
            Duration::hours(1),
            Pace::per_hour(0.1),
        )])
        .with_trend(vec![0.1, 0.2]);
        assert_eq!(row_trend(&codex), [0.1, 0.2]);
        let no_series = claude(vec![
            window("5-hour window", 0.64, Duration::minutes(72), Pace::per_hour(0.12)),
            window("Weekly", 0.82, Duration::days(2), Pace::per_day(0.05)),
        ])
        .with_trend(vec![0.9, 0.5]);
        assert!(
            row_trend(&no_series).is_empty(),
            "never the 5-hour window's trend under the weekly window"
        );
    }

    /// The screenshot on #203: the 5-hour window about to run out, the weekly window on pace for 54%.
    #[test]
    fn a_calm_weekly_bar_stays_calm_beside_a_five_hour_window_about_to_run_out() {
        let five_hour = window("5-hour window", 0.91, Duration::minutes(38), Pace::per_hour(0.216));
        let weekly = window("Weekly", 0.53, Duration::days(6), Pace::per_hour(0.00007));
        let row = account(vec![five_hour.clone(), weekly.clone()]);
        assert_eq!(
            row.assess(now()).severity(),
            Severity::LimitSoon,
            "the account is in trouble"
        );
        assert_eq!(bar_projection(&five_hour, 0.91, &row, now()).runway, Runway::Risk);
        assert_eq!(bar_projection(&weekly, 0.53, &row, now()).runway, Runway::Ok);
    }

    /// The other way round: a weekly window projected past its limit colors its own bar, not the 5-hour one.
    #[test]
    fn a_weekly_window_projected_past_its_limit_colors_only_its_own_bar() {
        let five_hour = window("5-hour window", 0.12, Duration::hours(4), Pace::per_hour(0.03));
        let weekly = window("Weekly", 0.70, Duration::days(3), Pace::per_day(0.15));
        let row = account(vec![five_hour.clone(), weekly.clone()]);
        assert_eq!(bar_projection(&weekly, 0.70, &row, now()).runway, Runway::Risk);
        assert_eq!(bar_projection(&five_hour, 0.12, &row, now()).runway, Runway::Ok);
    }

    /// The wide layout (#204, #207): the windows first, each with its reset, then the primary pair and its reset.
    #[test]
    fn the_wide_columns_put_the_windows_before_the_primary_pair() {
        let order: Vec<&str> = WIDE
            .iter()
            .map(|col| match col {
                Col::Account => "account",
                Col::Limit => "limit",
                Col::Used => "used",
                Col::Window(WindowKind::FiveHour, WindowCell::Percent) => "5h%",
                Col::Window(WindowKind::FiveHour, WindowCell::Bar) => "5h-bar",
                Col::Window(WindowKind::FiveHour, WindowCell::AtReset) => "5h-at-reset",
                Col::Window(WindowKind::FiveHour, WindowCell::Resets) => "5h-resets",
                Col::Window(WindowKind::Weekly, WindowCell::Percent) => "weekly%",
                Col::Window(WindowKind::Weekly, WindowCell::Bar) => "weekly-bar",
                Col::Window(WindowKind::Weekly, WindowCell::AtReset) => "weekly-at-reset",
                Col::Window(WindowKind::Weekly, WindowCell::Resets) => "weekly-resets",
                Col::Percent => "%",
                Col::Progress => "progress",
                Col::AtReset => "at-reset",
                Col::Resets => "resets",
                Col::Trend => "trend",
            })
            .collect();
        assert_eq!(
            order,
            [
                "account",
                "limit",
                "used",
                "5h%",
                "5h-bar",
                "5h-at-reset",
                "5h-resets",
                "weekly%",
                "weekly-bar",
                "weekly-at-reset",
                "weekly-resets",
                "%",
                "progress",
                "at-reset",
                "resets",
                "trend"
            ]
        );
        assert!(
            NARROW.iter().all(|col| !matches!(col, Col::Window(..))),
            "narrow has no window columns"
        );
    }

    /// A used-up window the account's credits carry it past (#150) keeps a calm bar, as the account does.
    #[test]
    fn a_used_up_window_covered_by_credits_keeps_a_calm_bar() {
        let five_hour = window("5-hour window", 1.0, Duration::hours(2), Pace::per_hour(0.2));
        let credits = Metric::Credits {
            label: "Credits".into(),
            balance: Some(12.0),
            start: None,
        };
        let row = account(vec![five_hour.clone(), credits]);
        assert!(row.credits_cover(&five_hour));
        assert_eq!(bar_projection(&five_hour, 1.0, &row, now()).runway, Runway::Ok);
    }
}
