//! The Spend view (#215): every account that reports money, with this period's spend totalled per currency, how
//! close capped spend is to its limit, how long prepaid balances last, and the summed spend over stored history.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Local, Utc};
use codexbar_core::spend::{BalanceRow, SpendRow, SpendSheet};
use codexbar_core::{AccountSnapshot, Currency, Metric, Money, Provider, format};
use codexbar_store::HistoryStore;
use codexbar_store::summary::{ChartPoint, HistorySummary, chart_series, summarize};
use gpui_kit::TestSupportExt as _;
use gpui_kit::base::ObservedElement;
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, rems,
};

use crate::account_table::{AccountState, States};
use crate::history_view::{ValueKind, describe};
use crate::prefs_hub::PrefsHub;
use crate::status::severity_dot_color;
use crate::usage_bar::{Projection, usage_bar};

/// Days the spend trend covers; the history's retention.
const TREND_DAYS: usize = 30;
/// The most points the trend draws.
const MAX_CHART_POINTS: usize = 120;
/// The trend's points are one per local day, timed at that day's last reading, so elapsed time says nothing about
/// missing days; `summarize` gets a threshold no stretch inside the window reaches, and missing days are counted from
/// the calendar instead (`missing_days`).
const NO_GAP: Duration = Duration::days(TREND_DAYS as i64 + 1);

/// The providers whose accounts report spend or a prepaid balance, for the empty state.
const MONEY_PROVIDERS: [Provider; 8] = [
    Provider::Claude,
    Provider::Cursor,
    Provider::GitHubBilling,
    Provider::OpenRouter,
    Provider::Moonshot,
    Provider::OpenCode,
    Provider::Grok,
    Provider::XSubscription,
];

/// What the Spend view asks the dashboard to show: Settings on the Accounts page from the empty state, or the
/// Usage view, where a failed account's error and retry are.
pub enum SpendRequest {
    OpenAccounts,
    OpenUsage,
}

pub struct SpendView {
    history: Arc<Mutex<HistoryStore>>,
    accounts: Vec<AccountSnapshot>,
    /// Refresh state by account id, as the Usage table has it: amounts from a failed or not yet repeated fetch are
    /// labelled as the last known ones.
    states: States,
    /// The currency the trend shows when spend comes in more than one; the first one until picked.
    currency: Option<Currency>,
    /// The trend point the keyboard is on; `None` reads the latest.
    cursor: Option<usize>,
    chart_focus: FocusHandle,
    /// One focus stop per row, spend rows first, so the list can be read with Tab.
    row_focus: Vec<FocusHandle>,
}

impl EventEmitter<SpendRequest> for SpendView {}

impl SpendView {
    pub fn new(history: Arc<Mutex<HistoryStore>>, cx: &mut Context<Self>) -> Self {
        Self {
            history,
            accounts: Vec::new(),
            states: States::new(),
            currency: None,
            cursor: None,
            chart_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            row_focus: Vec::new(),
        }
    }

    /// New accounts after a refresh, with their refresh states.
    pub fn set_accounts(&mut self, accounts: Vec<AccountSnapshot>, states: States, cx: &mut Context<Self>) {
        self.accounts = accounts;
        self.states = states;
        let sheet = SpendSheet::of(&self.accounts);
        let rows = sheet.spend.len() + sheet.balances.len();
        while self.row_focus.len() < rows {
            self.row_focus.push(cx.focus_handle().tab_index(0).tab_stop(true));
        }
        self.cursor = None;
        cx.notify();
    }

    /// The currency the trend shows: the picked one while spend still comes in it, else the first.
    fn trend_currency(&self, currencies: &[Currency]) -> Option<Currency> {
        self.currency
            .filter(|currency| currencies.contains(currency))
            .or_else(|| currencies.first().copied())
    }

    /// The spend rows whose history is shown, each with every currency it has spend in: its current one and any its
    /// stored history was recorded in, so readings from before a currency change keep a chart of their own.
    fn trend_rows<'r>(&self, sheet: &'r SpendSheet<'_>, cx: &App) -> Vec<(&'r SpendRow<'r>, Vec<Currency>)> {
        let history = self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        sheet
            .spend
            .iter()
            .filter(|row| PrefsHub::shows(cx, row.account.id().as_str()))
            .map(|row| {
                let mut currencies: Vec<Currency> = history
                    .currencies(
                        row.account.id().as_str(),
                        &row.counted.key(),
                        TREND_DAYS,
                        &Local,
                        Utc::now(),
                    )
                    .iter()
                    .filter_map(|code| Currency::from_code(code))
                    .chain(std::iter::once(row.spent().currency()))
                    .collect();
                currencies.sort_unstable();
                currencies.dedup();
                (row, currencies)
            })
            .collect()
    }

    /// The summed daily spend in `currency` over the trend's days, from the rows that have spend in it.
    fn trend(
        &self,
        rows: &[(&SpendRow<'_>, Vec<Currency>)],
        currency: Currency,
    ) -> Vec<codexbar_store::summary::Point> {
        let keys: Vec<(String, String, &'static str)> = rows
            .iter()
            .filter(|(_, currencies)| currencies.contains(&currency))
            .map(|(row, _)| {
                (
                    row.account.id().as_str().to_owned(),
                    row.counted.key(),
                    row.spent().currency().code(),
                )
            })
            .collect();
        let series: Vec<(&str, &str, &str)> = keys.iter().map(|(a, m, c)| (a.as_str(), m.as_str(), *c)).collect();
        let history = self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        history.daily_total(&series, currency.code(), TREND_DAYS, &Local, Utc::now())
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

/// "Resets in 12d", or why there is no date.
fn reset_text(metric: &Metric, now: DateTime<Utc>) -> String {
    match metric.resets_at() {
        Some(at) => format!("Resets {}", format::time_left_label(at, now)),
        None => "No reset date".to_owned(),
    }
}

/// What sits where an uncapped spend's bar would: "No limit", or that a cap in another currency can't be compared
/// with the amount, so neither a bar nor a severity is drawn from it.
fn cap_text(metric: &Metric) -> String {
    match metric {
        Metric::Spend {
            spent,
            limit: Some(limit),
            ..
        } if limit.currency() != spent.currency() => {
            format!(
                "Limit of {} can't be compared with {}",
                limit.display(),
                spent.currency().code()
            )
        }
        _ => "No limit".to_owned(),
    }
}

/// "$12.40 of $50.00" for capped spend, "$23.89" otherwise.
fn amount_text(metric: &Metric) -> String {
    match metric {
        Metric::Spend {
            spent,
            limit: Some(limit),
            ..
        } => format!("{} of {}", spent.display(), limit.display()),
        Metric::Spend { spent, .. } => spent.display(),
        other => other.used_display(),
    }
}

/// "Gross charges $605.52 · Discounts $581.63".
fn breakdown_text(row: &SpendRow<'_>) -> Option<String> {
    let parts: Vec<String> = row
        .breakdown
        .iter()
        .filter_map(|metric| {
            codexbar_core::spend::spent(metric)?;
            Some(match metric.used_fraction() {
                Some(used) => format!("{} {}, {:.0}% used", metric.label(), amount_text(metric), used * 100.0),
                None => format!("{} {}", metric.label(), amount_text(metric)),
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// How long a balance lasts: "Empty", "About 6 days left, until Oct 25", or that no rate is known yet.
fn runway_text(row: &BalanceRow<'_>, now: DateTime<Utc>) -> String {
    if row.is_empty() {
        return "Empty".to_owned();
    }
    // The days left count down to the date fixed at the last fetch, so they shrink while the reading ages.
    match row.runs_out() {
        Some(at) => {
            let until = crate::locale::style().month_day(at.with_timezone(&Local).date_naive());
            match (at - now).num_seconds() as f64 / 86_400.0 {
                d if d <= 0.0 => format!("Expected to have run out by {until}"),
                d if d < 1.0 => format!("Less than a day left, until {until}"),
                d if d < 1.5 => format!("About 1 day left, until {until}"),
                d => format!("About {:.0} days left, until {until}", d),
            }
        }
        None => "No spending rate known yet".to_owned(),
    }
}

/// How an account's amounts are dated when they aren't from this refresh, as the Usage table's tags say it.
fn stale_note(state: Option<&AccountState>, account: &AccountSnapshot, now: DateTime<Utc>) -> Option<String> {
    let age = format::age_label(account.fetched_at(), now);
    match state? {
        AccountState::Restored => Some(format!("Last known, saved {age}")),
        AccountState::Failed(_) => Some(format!("Stale, from {age}")),
        AccountState::Fresh | AccountState::Loading | AccountState::Unavailable(_) => None,
    }
}

/// The severity a spend row shows: the worst of its capped metrics, the counted one and any capped breakdown (a
/// team pool beside an individual cap).
fn row_severity(row: &SpendRow<'_>, now: DateTime<Utc>) -> Option<codexbar_core::Severity> {
    std::iter::once(row.counted)
        .chain(row.breakdown.iter().copied())
        .filter(|metric| metric.used_fraction().is_some())
        .map(|metric| codexbar_core::assess(metric, now).severity())
        .max()
}

/// Money accounts with nothing to show yet, by display name.
#[derive(Default)]
struct Pending {
    loading: Vec<String>,
    unavailable: Vec<String>,
}

impl Pending {
    /// "Couldn't fetch Claude · personal; see Usage for the error and a retry. Still loading: Cursor."
    fn sentence(&self) -> String {
        let mut parts = Vec::new();
        if !self.unavailable.is_empty() {
            parts.push(format!(
                "Couldn't fetch {}; see Usage for the error and a retry.",
                self.unavailable.join(", ")
            ));
        }
        if !self.loading.is_empty() {
            parts.push(format!("Still loading: {}.", self.loading.join(", ")));
        }
        parts.join(" ")
    }
}

/// The note above the lists while some money accounts are still loading or failed their first fetch: they are
/// missing from the lists and the totals.
fn pending_note(pending: &Pending, cx: &App) -> AnyElement {
    let text = format!("Not in the totals yet. {}", pending.sentence());
    let color = if pending.unavailable.is_empty() {
        cx.theme().muted_foreground
    } else {
        cx.theme().warning
    };
    h_flex()
        .id("spend-pending-note")
        .role(Role::Status)
        .test_support()
        .aria_label(text.clone())
        .gap_2()
        .items_center()
        .text_sm()
        .text_color(color)
        .child(Icon::new(IconName::Info).small())
        .child(text)
        .into_any_element()
}

/// Days inside the trend with no reading at all: between points, and since the last one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Missing {
    between: i64,
    since_last: i64,
}

impl Missing {
    fn text(self) -> Option<String> {
        let days = |n: i64| {
            if n == 1 {
                "1 day".to_owned()
            } else {
                format!("{n} days")
            }
        };
        match (self.between, self.since_last) {
            (0, 0) => None,
            (between, 0) => Some(format!(
                "{} without data; the line crosses them straight.",
                days(between)
            )),
            (0, since) => Some(format!("No data for the last {}.", days(since))),
            (between, since) => Some(format!(
                "{} without data, and none for the last {}.",
                days(between),
                days(since)
            )),
        }
    }
}

/// Counts the local days with no point: gaps between consecutive points' days, and the days after the last point up
/// to today.
fn missing_days(points: &[codexbar_store::summary::Point], now: DateTime<Utc>) -> Missing {
    let day = |at: DateTime<Utc>| at.with_timezone(&Local).date_naive();
    let between = points
        .windows(2)
        .map(|pair| ((day(pair[1].at) - day(pair[0].at)).num_days() - 1).max(0))
        .sum();
    let since_last = points
        .last()
        .map_or(0, |last| (day(now) - day(last.at)).num_days().max(0));
    Missing { between, since_last }
}

/// What a total reads as, with its currency named only when there is more than one.
fn total_label(label: &str, amount: Money, several: bool) -> String {
    if several {
        format!("{label} · {}", amount.currency().code())
    } else {
        label.to_owned()
    }
}

impl Render for SpendView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self.accounts.clone();
        let sheet = SpendSheet::of(&accounts);
        if sheet.is_empty() {
            return match self.pending() {
                Some(pending) => self.render_pending(&pending, cx).into_any_element(),
                None => self.render_empty(cx).into_any_element(),
            };
        }
        let now = Utc::now();
        let mut focus = self.row_focus.iter();
        let spend_rows: Vec<AnyElement> = sheet
            .spend
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let stale = stale_note(self.states.get(row.account.id().as_str()), row.account, now);
                spend_row(ix, row, stale, focus.next(), now, cx)
            })
            .collect();
        let balance_rows: Vec<AnyElement> = sheet
            .balances
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let stale = stale_note(self.states.get(row.account.id().as_str()), row.account, now);
                balance_row(ix, row, stale, focus.next(), now, cx)
            })
            .collect();
        v_flex()
            .id("spend-view")
            .w_full()
            .gap_6()
            .child(self.render_totals(&sheet, cx))
            .children(self.render_stale_note(&sheet, now, cx))
            .children(self.pending().map(|pending| pending_note(&pending, cx)))
            .when(!spend_rows.is_empty(), |this| {
                this.child(section("spend-list", "Spend this period", spend_rows, cx))
            })
            .when(!balance_rows.is_empty(), |this| {
                this.child(section("balance-list", "Prepaid balances", balance_rows, cx))
            })
            .when(!sheet.spend.is_empty(), |this| {
                this.child(self.render_trend(&sheet, cx))
            })
            .into_any_element()
    }
}

impl SpendView {
    /// Money accounts that are configured but have nothing to show yet: still on their first fetch, or failed it.
    fn pending(&self) -> Option<Pending> {
        let mut pending = Pending::default();
        for account in &self.accounts {
            if !MONEY_PROVIDERS.contains(&account.provider()) {
                continue;
            }
            match self.states.get(account.id().as_str()) {
                Some(AccountState::Loading) => pending.loading.push(account.display_name()),
                Some(AccountState::Unavailable(_)) => pending.unavailable.push(account.display_name()),
                _ => {}
            }
        }
        (!pending.loading.is_empty() || !pending.unavailable.is_empty()).then_some(pending)
    }

    /// The Spend view while its only money accounts are loading or failed: what is happening, and where the error
    /// and retry are, instead of the "no account" empty state.
    fn render_pending(&self, pending: &Pending, cx: &mut Context<Self>) -> impl IntoElement {
        let failed = !pending.unavailable.is_empty();
        let (icon, title) = if failed {
            (IconName::TriangleAlert, "Couldn't fetch spend yet")
        } else {
            (IconName::Loader, "Fetching spend and balances")
        };
        let detail = pending.sentence();
        v_flex()
            .id("spend-pending")
            .role(Role::Status)
            .test_support()
            .aria_label(format!("{title}. {detail}"))
            .flex_1()
            .min_h(rems(20.))
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(icon).large())
            .child(div().font_semibold().text_color(cx.theme().foreground).child(title))
            .child(div().text_sm().max_w(rems(36.)).text_center().child(detail))
            .when(failed, |this| {
                this.child(
                    Button::new("spend-open-usage")
                        .outline()
                        .small()
                        .label("Show the error on Usage")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SpendRequest::OpenUsage))),
                )
            })
    }

    /// Says when the totals include amounts that weren't refreshed, so last known money doesn't read as current.
    fn render_stale_note(&self, sheet: &SpendSheet<'_>, now: DateTime<Utc>, cx: &App) -> Option<impl IntoElement> {
        let mut stale: Vec<&str> = sheet
            .spend
            .iter()
            .map(|row| row.account)
            .chain(sheet.balances.iter().map(|row| row.account))
            .filter(|account| stale_note(self.states.get(account.id().as_str()), account, now).is_some())
            .map(|account| account.id().as_str())
            .collect();
        stale.sort_unstable();
        stale.dedup();
        let text = match stale.len() {
            0 => return None,
            1 => "1 account wasn't refreshed: its amounts are the last known, and the totals include them.".to_owned(),
            n => format!(
                "{n} accounts weren't refreshed: their amounts are the last known, and the totals include them."
            ),
        };
        Some(
            h_flex()
                .id("spend-stale-note")
                .role(Role::Status)
                .test_support()
                .aria_label(text.clone())
                .gap_2()
                .items_center()
                .text_sm()
                .text_color(cx.theme().warning)
                .child(Icon::new(IconName::TriangleAlert).small())
                .child(text),
        )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let names: Vec<&str> = MONEY_PROVIDERS.iter().map(|provider| provider.display_name()).collect();
        v_flex()
            .id("spend-empty")
            .role(Role::Status)
            .test_support()
            .aria_label("No account reports spend or a prepaid balance")
            .flex_1()
            .min_h(rems(20.))
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::ChartPie).large())
            .child(
                div()
                    .font_semibold()
                    .text_color(cx.theme().foreground)
                    .child("No account reports spend or a prepaid balance"),
            )
            .child(
                div()
                    .text_sm()
                    .max_w(rems(36.))
                    .text_center()
                    .child(format!("Spend and balances come from {}.", names.join(", "))),
            )
            .child(
                Button::new("spend-open-accounts")
                    .outline()
                    .small()
                    .label("Add an account in Settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SpendRequest::OpenAccounts))),
            )
    }

    fn render_totals(&self, sheet: &SpendSheet<'_>, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let stat = |id: String, label: String, value: String, note: Option<String>| {
            v_flex()
                .id(SharedString::from(id))
                .test_support()
                .aria_label(format!(
                    "{label}: {value}{}",
                    note.as_ref().map(|note| format!(", {note}")).unwrap_or_default()
                ))
                .gap_0p5()
                .child(div().text_sm().text_color(muted).child(label))
                .child(div().text_2xl().font_semibold().child(value))
                .children(note.map(|note| div().text_sm().text_color(muted).child(note)))
        };
        let spent = sheet.spent();
        let left = sheet.left();
        let accounts = |n: usize| match n {
            1 => "1 account".to_owned(),
            n => format!("{n} accounts"),
        };
        let spent_count = |currency: Currency| {
            sheet
                .spend
                .iter()
                .filter(|row| row.spent().currency() == currency)
                .count()
        };
        let left_count = |currency: Currency| {
            sheet
                .balances
                .iter()
                .filter(|row| row.remaining().currency() == currency)
                .count()
        };
        h_flex()
            .id("spend-totals")
            .gap_10()
            .flex_wrap()
            .children(spent.iter().map(|amount| {
                stat(
                    format!("spend-total-{}", amount.currency().code()),
                    total_label("Spent this period", *amount, spent.len() > 1),
                    amount.display(),
                    Some(accounts(spent_count(amount.currency()))),
                )
            }))
            .children(left.iter().map(|amount| {
                stat(
                    format!("balance-total-{}", amount.currency().code()),
                    total_label("Prepaid left", *amount, left.len() > 1),
                    amount.display(),
                    Some(accounts(left_count(amount.currency()))),
                )
            }))
            .children(sheet.first_to_run_out().map(|(row, at)| {
                stat(
                    "spend-runs-out-first".to_owned(),
                    "Runs out first".to_owned(),
                    row.account.display_name(),
                    Some(crate::locale::style().month_day(at.with_timezone(&Local).date_naive())),
                )
            }))
    }

    fn render_trend(&self, sheet: &SpendSheet<'_>, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.trend_rows(sheet, cx);
        let mut currencies: Vec<Currency> = rows
            .iter()
            .flat_map(|(_, currencies)| currencies.iter().copied())
            .collect();
        currencies.sort_unstable();
        currencies.dedup();
        let currency = self.trend_currency(&currencies).unwrap_or_default();
        let points = self.trend(&rows, currency);
        let now = Utc::now();
        let summary = summarize(&points, now, NO_GAP);
        let missing = missing_days(&points, now);
        let series = chart_series(&points, MAX_CHART_POINTS);
        let kind = ValueKind::Money(currency);
        let muted = cx.theme().muted_foreground;

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().text_lg().font_semibold().child("Spend over 30 days"))
            .when(currencies.len() > 1, |this| {
                let picked = currencies.iter().position(|c| *c == currency).unwrap_or(0);
                let choices = currencies.clone();
                this.child(
                    ButtonGroup::new("spend-currency")
                        .outline()
                        .small()
                        .children(currencies.iter().enumerate().map(|(ix, currency)| {
                            Button::new(("spend-currency", ix))
                                .label(currency.code())
                                .selected(ix == picked)
                        }))
                        .on_click(cx.listener(move |this, clicks: &Vec<usize>, _, cx| {
                            if let Some(currency) = clicks.first().and_then(|ix| choices.get(*ix)) {
                                this.currency = Some(*currency);
                                this.cursor = None;
                                cx.notify();
                            }
                        })),
                )
            });

        let body: AnyElement = match &summary {
            None => v_flex()
                .id("spend-trend-empty")
                .role(Role::Status)
                .test_support()
                .h(rems(12.))
                .items_center()
                .justify_center()
                .gap_1()
                .text_color(muted)
                .child(
                    div()
                        .font_semibold()
                        .text_color(cx.theme().foreground)
                        .child("No spend history yet"),
                )
                .child(div().text_sm().child("CodexBar records spend on every refresh."))
                .into_any_element(),
            Some(summary) => self
                .render_chart(&series, summary, missing, kind, cx)
                .into_any_element(),
        };
        v_flex()
            .gap_3()
            .child(header)
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Each day's total is where every account's spend stood when the day ended."),
            )
            .child(body)
    }

    fn render_chart(
        &self,
        series: &[ChartPoint],
        summary: &HistorySummary,
        missing: Missing,
        kind: ValueKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let readings: Vec<ChartPoint> = series.iter().copied().filter(|point| point.measured).collect();
        let len = readings.len();
        let cursor = self.cursor.unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1));
        let readout = readings.get(cursor).map(|point| {
            format!(
                "{} · {}",
                crate::locale::style().month_day(point.at.with_timezone(&Local).date_naive()),
                kind.value(point.value)
            )
        });
        let description = [Some(describe(summary, kind)), missing.text()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let color = cx.theme().chart_1;
        let chart: AnyElement = if summary.samples < 2 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .child(div().text_3xl().font_semibold().child(kind.value(summary.latest)))
                .child(div().text_sm().text_color(muted).child("Only one day recorded so far"))
                .into_any_element()
        } else {
            let point_count = series.len();
            let data: Vec<(SharedString, f64)> = series
                .iter()
                .map(|point| {
                    let day = crate::locale::style().month_day(point.at.with_timezone(&Local).date_naive());
                    (SharedString::from(day), point.value)
                })
                .collect();
            let (low, high) = (summary.low.min(0.0) * 1.15, (summary.high * 1.15).max(1.0));
            AreaChart::new(data)
                .id("spend-chart")
                .x(|(label, _)| label.clone())
                .y(|(_, value)| *value)
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.35), 1.),
                    linear_color_stop(color.opacity(0.), 0.),
                ))
                .name("Spend")
                .linear()
                .y_domain(low, high)
                .y_padding(0., 0.)
                .y_axis(true)
                .y_tick_count(5)
                .point_count(point_count)
                .tick_margin((point_count / 6).max(1))
                .y_tick_format(move |value| kind.value(value))
                .tooltip_value(move |_, _, value| kind.value(value).into())
                .into_any_element()
        };
        let ring = cx.theme().ring;
        v_flex()
            .gap_3()
            .when_some(missing.text(), |this, text| {
                this.child(
                    h_flex()
                        .id("spend-missing-days")
                        .role(Role::Status)
                        .test_support()
                        .aria_label(text.clone())
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(muted)
                        .child(Icon::new(IconName::Info).small())
                        .child(text),
                )
            })
            .child(
                v_flex()
                    .id("spend-chart-region")
                    .role(Role::Figure)
                    .test_support()
                    .aria_label(format!("Spend over 30 days. {description}"))
                    .track_focus(&self.chart_focus)
                    .on_key_down(
                        cx.listener(move |this, event: &KeyDownEvent, _, cx| this.on_chart_key(event, len, cx)),
                    )
                    .h(rems(18.))
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
                            .flex_wrap()
                            .text_sm()
                            .text_color(muted)
                            .child(
                                div()
                                    .id("spend-readout")
                                    .role(Role::Status)
                                    .test_support()
                                    .aria_label(readout.clone().unwrap_or_default())
                                    .text_color(cx.theme().foreground)
                                    .child(readout.unwrap_or_default()),
                            )
                            .child("Focus the chart and use ← → Home End to read each day"),
                    ),
            )
    }
}

/// A titled list of rows.
fn section(id: &'static str, title: &'static str, rows: Vec<AnyElement>, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_2()
        .child(div().text_lg().font_semibold().child(title))
        .child(
            v_flex()
                .id(id)
                .role(Role::List)
                .aria_label(title)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius_lg)
                .overflow_hidden()
                .children(rows),
        )
}

/// What a row's first column and its focus stop say.
struct RowHead {
    id: String,
    first: bool,
    /// Everything the row shows, for screen readers and the headless tests.
    spoken: String,
    dot: Hsla,
    name: String,
    detail: Option<String>,
    /// "Stale, from 2h ago" when the amounts aren't from this refresh.
    stale: Option<String>,
}

/// A row's frame: a focus stop that names everything the row shows, with the account in the first column. The caller
/// adds the other columns.
fn row_frame(
    head: RowHead,
    focus: Option<&FocusHandle>,
    cx: &App,
) -> ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    let RowHead {
        id,
        first,
        spoken,
        dot,
        name,
        detail,
        stale,
    } = head;
    let ring = cx.theme().ring;
    let warning = cx.theme().warning;
    let muted = cx.theme().muted_foreground;
    let row = h_flex()
        .id(SharedString::from(id))
        .role(Role::ListItem)
        // Registered before the focus binding, so the headless tests can see it.
        .test_support()
        .aria_label(spoken)
        .w_full()
        .min_h(rems(3.5))
        .px_4()
        .py_2()
        .gap_4()
        .items_center()
        .when(!first, |this| this.border_t_1().border_color(cx.theme().border))
        .focus_visible(move |style| style.bg(ring.opacity(0.12)))
        .child(
            // The name gives way first when zoomed, so the middle column keeps room for its bar or breakdown.
            h_flex()
                .flex_basis(rems(18.))
                .flex_shrink(1.)
                .min_w(rems(10.))
                .gap_2()
                .items_center()
                .child(div().size_2().flex_shrink_0().rounded_full().bg(dot))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().truncate().child(name))
                        .children(detail.map(|detail| div().text_sm().text_color(muted).truncate().child(detail)))
                        .children(stale.map(|stale| div().text_sm().text_color(warning).truncate().child(stale))),
                ),
        );
    match focus {
        Some(focus) => row.track_focus(focus),
        None => row,
    }
}

fn spend_row(
    ix: usize,
    row: &SpendRow<'_>,
    stale: Option<String>,
    focus: Option<&FocusHandle>,
    now: DateTime<Utc>,
    cx: &App,
) -> AnyElement {
    let metric = row.counted;
    let capped = metric.used_fraction();
    let name = row.account.display_name();
    let breakdown = breakdown_text(row);
    let detail = Some(metric.label().to_owned());
    let reset = reset_text(metric, now);
    let projection = capped.map(|used| Projection::of(metric, used, now));
    let spoken = [
        Some(format!("{name}, {}: {}", metric.label(), amount_text(metric))),
        stale.clone(),
        projection.as_ref().map(|projection| projection.label.clone()),
        projection.is_none().then(|| cap_text(metric)),
        Some(reset.clone()),
        breakdown.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");
    let dot = match row_severity(row, now) {
        Some(worst) => severity_dot_color(worst, cx),
        None => cx.theme().muted_foreground,
    };
    let main: AnyElement = match &projection {
        Some(projection) => usage_bar(
            &format!("spend-bar-{ix}"),
            projection,
            cx.theme().chart_1,
            crate::usage_bar::small(),
            name.clone(),
            cx,
        ),
        // Uncapped spend has room here for what the counted amount is made of.
        // One line each, so a narrow column wraps between items rather than inside one.
        None => h_flex()
            .flex_wrap()
            .gap_x_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(cap_text(metric))
            .children(
                row.breakdown
                    .iter()
                    .filter(|metric| metric.used_fraction().is_none())
                    .filter_map(|metric| {
                        let spent = codexbar_core::spend::spent(metric)?;
                        Some(
                            div()
                                .whitespace_nowrap()
                                .child(format!("{} {}", metric.label(), spent.display())),
                        )
                    }),
            )
            .into_any_element(),
    };
    // A capped breakdown, such as a team pool beside the individual cap, gets a bar of its own: it can run out first.
    let capped_breakdown = row
        .breakdown
        .iter()
        .enumerate()
        .filter_map(|(j, metric)| {
            let used = metric.used_fraction()?;
            let projection = Projection::of(metric, used, now);
            Some(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!("{} {}", metric.label(), amount_text(metric))),
                    )
                    .child(div().flex_1().min_w(rems(4.)).child(usage_bar(
                        &format!("spend-bar-{ix}-{j}"),
                        &projection,
                        cx.theme().chart_1,
                        crate::usage_bar::small(),
                        format!("{name}, {}", metric.label()),
                        cx,
                    ))),
            )
        })
        .collect::<Vec<_>>();
    let bar = v_flex().gap_1().child(main).children(capped_breakdown);
    let head = RowHead {
        id: format!("spend-row-{ix}"),
        first: ix == 0,
        spoken,
        dot,
        name,
        detail,
        stale,
    };
    row_frame(head, focus, cx)
        .child(
            div()
                .w(rems(11.))
                .flex_shrink_0()
                .font_semibold()
                .child(amount_text(metric)),
        )
        .child(div().flex_1().min_w(rems(8.)).child(bar))
        .child(
            div()
                .w(rems(9.))
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(reset),
        )
        .into_any_element()
}

fn balance_row(
    ix: usize,
    row: &BalanceRow<'_>,
    stale: Option<String>,
    focus: Option<&FocusHandle>,
    now: DateTime<Utc>,
    cx: &App,
) -> AnyElement {
    let severity = codexbar_core::assess(row.metric, now).severity();
    let name = row.account.display_name();
    let runway = runway_text(row, now);
    let remaining = row.remaining().display();
    let spoken = match &stale {
        Some(stale) => format!("{name}, {}: {remaining} left, {stale}, {runway}", row.metric.label()),
        None => format!("{name}, {}: {remaining} left, {runway}", row.metric.label()),
    };
    let dot = if row.is_empty() {
        cx.theme().danger
    } else {
        severity_dot_color(severity, cx)
    };
    let runway_color = if row.is_empty() {
        cx.theme().danger
    } else {
        cx.theme().muted_foreground
    };
    let head = RowHead {
        id: format!("balance-row-{ix}"),
        first: ix == 0,
        spoken,
        dot,
        name,
        detail: Some(row.metric.label().to_owned()),
        stale,
    };
    row_frame(head, focus, cx)
        .child(
            div()
                .w(rems(11.))
                .flex_shrink_0()
                .font_semibold()
                .child(format!("{remaining} left")),
        )
        .child(
            div()
                .flex_1()
                .min_w(rems(6.))
                .text_sm()
                .text_color(runway_color)
                .child(runway),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use codexbar_core::{AccountId, Pace, Provider};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-19T12:00:00Z").unwrap().to_utc()
    }

    fn balance(remaining: i64, burn: Option<i64>) -> AccountSnapshot {
        AccountSnapshot::new(
            AccountId::new("openrouter"),
            Provider::OpenRouter,
            vec![Metric::Balance {
                label: "Credits".into(),
                remaining: Money::from_cents(remaining),
                burn_per_day: burn.map(Money::from_cents),
            }],
            now(),
        )
    }

    fn runway(account: &AccountSnapshot) -> String {
        let accounts = [account.clone()];
        let sheet = SpendSheet::of(&accounts);
        runway_text(&sheet.balances[0], now())
    }

    #[test]
    fn a_balance_says_how_long_it_lasts() {
        assert_eq!(runway(&balance(1842, Some(310))), "About 6 days left, until Oct 25");
        assert_eq!(runway(&balance(300, Some(310))), "Less than a day left, until Oct 20");
        assert_eq!(runway(&balance(1842, None)), "No spending rate known yet");
        assert_eq!(runway(&balance(0, Some(310))), "Empty");
    }

    #[test]
    fn a_balance_keeps_its_run_out_date_while_the_reading_ages() {
        let accounts = [balance(1842, Some(310))];
        let sheet = SpendSheet::of(&accounts);
        let row = &sheet.balances[0];
        assert_eq!(
            runway_text(row, now() + Duration::days(2)),
            "About 4 days left, until Oct 25"
        );
        assert_eq!(
            runway_text(row, now() + Duration::days(7)),
            "Expected to have run out by Oct 25"
        );
    }

    #[test]
    fn capped_and_uncapped_spend_read_differently() {
        let capped = Metric::Spend {
            label: "On-demand".into(),
            spent: Money::from_cents(1240),
            limit: Some(Money::from_cents(5000)),
            resets_at: Some(now() + Duration::days(14)),
            pace: Some(Pace::per_day(0.02)),
        };
        let uncapped = Metric::Spend {
            label: "Net spend".into(),
            spent: Money::from_cents(2389),
            limit: None,
            resets_at: None,
            pace: None,
        };
        assert_eq!(amount_text(&capped), "$12.40 of $50.00");
        assert_eq!(reset_text(&capped, now()), "Resets in 14d");
        assert_eq!(amount_text(&uncapped), "$23.89");
        assert_eq!(reset_text(&uncapped, now()), "No reset date");
    }

    #[test]
    fn a_nearly_spent_team_pool_colors_the_row_and_is_read_out() {
        let cap = |label: &str, spent: i64, limit: i64| Metric::Spend {
            label: label.into(),
            spent: Money::from_cents(spent),
            limit: Some(Money::from_cents(limit)),
            resets_at: Some(now() + Duration::days(14)),
            pace: None,
        };
        let accounts = [AccountSnapshot::new(
            AccountId::new("cursor"),
            Provider::Cursor,
            vec![cap("On-demand", 500, 5000), cap("Team on-demand", 48_500, 50_000)],
            now(),
        )];
        let sheet = SpendSheet::of(&accounts);
        let row = &sheet.spend[0];
        assert_eq!(row.counted.label(), "On-demand");
        assert_eq!(row_severity(row, now()), Some(codexbar_core::Severity::LimitSoon));
        assert_eq!(
            breakdown_text(row).as_deref(),
            Some("Team on-demand $485.00 of $500.00, 97% used")
        );
    }

    #[test]
    fn stale_amounts_say_how_old_they_are() {
        let account = balance(1842, Some(310));
        let later = now() + Duration::hours(2);
        assert_eq!(
            stale_note(Some(&AccountState::Failed("offline".into())), &account, later).as_deref(),
            Some("Stale, from 2h ago")
        );
        assert_eq!(
            stale_note(Some(&AccountState::Restored), &account, later).as_deref(),
            Some("Last known, saved 2h ago")
        );
        assert_eq!(stale_note(Some(&AccountState::Fresh), &account, later), None);
        assert_eq!(stale_note(None, &account, later), None);
    }

    #[test]
    fn missing_days_count_calendar_days_not_elapsed_hours() {
        use codexbar_store::summary::Point;
        // 09:00 local, then 09:00 two days later: 48 hours apart, one whole day missing.
        let day = |n: i64| {
            Local
                .from_local_datetime(
                    &chrono::NaiveDate::from_ymd_opt(2026, 10, n as u32)
                        .unwrap()
                        .and_hms_opt(9, 0, 0)
                        .unwrap(),
                )
                .unwrap()
                .to_utc()
        };
        let points = [
            Point::new(day(10), 1.0),
            Point::new(day(12), 2.0),
            Point::new(day(13), 3.0),
        ];
        let missing = missing_days(&points, day(13));
        assert_eq!(
            missing,
            Missing {
                between: 1,
                since_last: 0
            }
        );
        assert_eq!(
            missing.text().as_deref(),
            Some("1 day without data; the line crosses them straight.")
        );
        assert_eq!(missing_days(&points, day(16)).since_last, 3);
        assert_eq!(missing_days(&points[1..], day(13)).text(), None);
    }

    #[test]
    fn a_cap_in_another_currency_is_not_called_no_limit() {
        let crossed = Metric::Spend {
            label: "Extra usage".into(),
            spent: Money::new(1240, Currency::Eur),
            limit: Some(Money::from_cents(5000)),
            resets_at: None,
            pace: None,
        };
        assert_eq!(crossed.used_fraction(), None);
        assert_eq!(cap_text(&crossed), "Limit of $50.00 can't be compared with EUR");
        let uncapped = Metric::Spend {
            label: "Net spend".into(),
            spent: Money::from_cents(2389),
            limit: None,
            resets_at: None,
            pace: None,
        };
        assert_eq!(cap_text(&uncapped), "No limit");
    }

    #[test]
    fn totals_name_their_currency_only_beside_another() {
        let euros = Money::new(420, Currency::Eur);
        assert_eq!(total_label("Spent this period", euros, false), "Spent this period");
        assert_eq!(total_label("Spent this period", euros, true), "Spent this period · EUR");
    }
}
