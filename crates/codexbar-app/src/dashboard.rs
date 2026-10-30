//! The borderless dashboard window: title bar, view rail, urgency table, focus cards and status line.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, Local, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider, demo::demo_accounts, format};
use codexbar_providers::{AccountOutcome, ProviderError, UsageProvider};
use codexbar_store::summary::{GAP_THRESHOLD, summarize};
use codexbar_store::{HistoryStore, TREND_DAYS, demo_history};

use crate::settings_hub::SettingsHub;
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size, StyledExt as _, TitleBar, button::Button,
    button::ButtonVariants as _, h_flex, v_flex,
};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window, canvas, div, px, rems,
};

use crate::account_table::{AccountState, AccountTable, Compact, States};
use crate::focus_cards::focus_cards;
use crate::history_view::{HistoryView, ValueKind};
use crate::spend_view::{SpendRequest, SpendView};
use crate::status::{severity_dot_color, severity_tag};

/// The dashboard views. A tray click restores whichever one was open last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashboardView {
    Usage,
    Spend,
    History,
    Settings,
}

impl DashboardView {
    const ALL: [Self; 3] = [Self::Usage, Self::Spend, Self::History];

    fn title(self) -> &'static str {
        match self {
            Self::Usage => "Usage",
            Self::Spend => "Spend",
            Self::History => "History",
            Self::Settings => "Settings",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Usage => IconName::LayoutDashboard,
            Self::Spend => IconName::ChartPie,
            Self::History => IconName::Calendar,
            Self::Settings => IconName::Settings,
        }
    }
}

/// Row height of a large `DataTable` (header and body rows).
/// Row height of the account table at 100% zoom; DataTable rows are pixels, so they're scaled explicitly.
const TABLE_ROW_HEIGHT: f32 = 40.;
/// What the window's width holds besides the table, in unzoomed pixels: the 13.5rem rail, the content padding and
/// the table's borders (#195).
const RAIL_AND_PADDING_WIDTH: f32 = 13.5 * 16. + 48.;
/// A tray click this soon after the window lost focus means "hide it": the click itself took the focus away.
const TOGGLE_GRACE: std::time::Duration = std::time::Duration::from_millis(400);

/// Where accounts come from.
/// Builds a refresh's providers from the current settings: the real adapters in the app, fakes in tests.
pub type ProviderFactory = Arc<dyn Fn(&SettingsHub) -> Vec<Arc<dyn UsageProvider>> + Send + Sync>;

pub enum DataSource {
    /// Real provider adapters, fetched off the UI thread, with their history.
    /// Providers are rebuilt from the account settings on every refresh, so edits apply at once.
    Live {
        history: Arc<Mutex<HistoryStore>>,
        providers: ProviderFactory,
    },
    /// Synthetic accounts for design work (`CODEXBAR_DEMO=1`).
    Demo,
}

/// Moves history and Show history preferences saved under a single OpenRouter or Moonshot account's legacy id to its
/// configured id. With unreadable settings (newer schema, invalid accounts) the hub holds defaults rather than the
/// real accounts, so ownership is unknown and nothing moves until the file is readable.
pub fn migrate_legacy_ids(history: &Mutex<HistoryStore>, cx: &mut gpui_kit::App) {
    let hub = SettingsHub::global(cx);
    if hub.is_read_only() {
        return;
    }
    let renames = crate::providers::legacy_ids(hub);
    {
        let mut store = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for (from, to) in &renames {
            // A failed rewrite keeps the samples in memory under the new id; the next prune retries it.
            let _ = store.rename_account(from, to);
        }
    }
    crate::prefs_hub::PrefsHub::rename_accounts(cx, &renames);
}

// gpui-kit's DataTable takes Tab and Shift+Tab to move between columns, which traps keyboard focus in the table
// (#92). The arrow keys already move between rows and columns, so Tab and Shift+Tab leave the table instead.
gpui_kit::actions!(codexbar_table, [LeaveTableForward, LeaveTableBackward]);

/// Placeholder rows for configured providers that haven't returned anything yet.
pub(crate) const PLACEHOLDER_PREFIX: &str = "pending:";

/// One adapter's outcome: its provider name, the configured account it serves (if just one) and that account's
/// label, and the result.
type FetchResult = (
    &'static str,
    Option<String>,
    Option<String>,
    Result<Vec<AccountOutcome>, ProviderError>,
);

/// A provider whose last fetch failed. Its last good accounts stay on screen.
struct Failure {
    provider: &'static str,
    /// The configured account, when the failed adapter serves just one (one of several OpenRouter accounts).
    account: Option<String>,
    /// That account's label, to tell sibling failures apart.
    label: Option<String>,
    message: String,
    /// The sign-in or key is the problem, so the row offers Settings beside Retry (#188).
    sign_in: bool,
}

impl Failure {
    /// "OpenRouter · Team" for one labelled account of several, else the provider name.
    fn name(&self) -> String {
        match &self.label {
            Some(label) => format!("{} · {label}", self.provider),
            None => self.provider.to_owned(),
        }
    }

    /// What tells this row's buttons apart: several users of one provider can fail in the same refresh.
    fn element_suffix(&self) -> String {
        match (&self.account, &self.label) {
            (Some(account), _) => format!("{}-{account}", self.provider),
            (None, Some(label)) => format!("{}-{label}", self.provider),
            (None, None) => self.provider.to_owned(),
        }
    }
}

/// Where Settings opens for a failure (#188): the failed provider's group on the Accounts page (its index in the
/// catalog) and the account record to edit, when one is known. An adapter for one configured account names it,
/// by the record's id or by the dashboard account the record owns (a Codex or Claude account CodexBar signed in
/// reports under its identity, a Copilot one under its username); `owned` maps record ids to those. A
/// provider-wide failure (OpenCode's cookie adapter, which covers Go and Zen) goes to its first enabled record,
/// then any record, then the provider's first group with none.
fn settings_target(
    failure: &Failure,
    owned: &HashMap<String, String>,
    cx: &gpui_kit::App,
) -> Option<(usize, Option<codexbar_store::settings::AccountRecord>)> {
    let providers = &crate::catalog::PROVIDERS;
    let group_of = |provider: &str| providers.iter().position(|info| info.id.eq_ignore_ascii_case(provider));
    let hub = SettingsHub::global(cx);
    let records = hub.settings().accounts();
    let named = failure.account.as_ref().and_then(|id| {
        records.iter().find(|record| &record.id == id).or_else(|| {
            // A Copilot organization card is owned under "<record id>#org".
            let owner = owned
                .iter()
                .find(|(_, account)| *account == id)
                .and_then(|(key, _)| key.split('#').next())?;
            records.iter().find(|record| record.id == owner)
        })
    });
    if let Some(record) = named {
        return group_of(&record.provider).map(|group| (group, Some(record.clone())));
    }
    let kind = Provider::from_display_name(failure.provider)?;
    let of_kind = |provider: &str| {
        group_of(provider).is_some_and(|group| crate::brand::from_settings_name(providers[group].id) == Some(kind))
    };
    let siblings: Vec<&codexbar_store::settings::AccountRecord> =
        records.iter().filter(|record| of_kind(&record.provider)).collect();
    match siblings.iter().find(|record| record.enabled).or(siblings.first()) {
        Some(record) => group_of(&record.provider).map(|group| (group, Some((*record).clone()))),
        None => providers
            .iter()
            .position(|info| crate::brand::from_settings_name(info.id) == Some(kind))
            .map(|group| (group, None)),
    }
}

/// Whether one of `adapters` serves `account`: an adapter for one configured account by that account's id, a
/// provider-wide adapter by the provider's name (the same rule `apply_results` uses to tell what a fetch reported).
fn covered_by(adapters: &[(&'static str, Option<String>)], account: &AccountSnapshot) -> bool {
    adapters.iter().any(|(name, id)| match id {
        Some(id) => account.id().as_str() == id,
        None => account.provider().display_name() == *name,
    })
}

pub struct Dashboard {
    source: DataSource,
    /// Stored history: the history file for live data, generated in memory for the demo.
    history: Arc<Mutex<HistoryStore>>,
    history_view: Entity<HistoryView>,
    spend_view: Entity<SpendView>,
    /// The gauge ring's size slider (#219), its focus stop, and the saved size it last took, so a reset from the
    /// gauge card's menu moves it while a drag in progress isn't undone by the size saved before it.
    ring_slider: Entity<gpui_kit::component::slider::SliderState>,
    ring_focus: FocusHandle,
    ring_size_seen: Option<f32>,
    accounts: Vec<AccountSnapshot>,
    failures: Vec<Failure>,
    loading: bool,
    last_refresh: Option<DateTime<Utc>>,
    table: Entity<TableState<AccountTable>>,
    selected: Option<AccountId>,
    view: DashboardView,
    now: DateTime<Utc>,
    deactivated_at: Option<Instant>,
    /// The zoom the table's column widths were last laid out for.
    table_zoom: f64,
    /// A refresh was requested while one was running; it runs when that one finishes.
    refresh_queued: bool,
    /// Where each shown account's usage comes from: restored, loading, fresh or failed (#76).
    states: States,
    /// The running fetch is a single-provider retry rather than a full refresh.
    fetch_partial: bool,
    /// Ids of rows standing in for configured accounts with no result yet (Loading, or Unavailable after a failed
    /// first fetch). Tracked explicitly: a real account can have no metrics too (Copilot with unlimited quotas).
    placeholders: std::collections::HashSet<String>,
    /// The tray text last sent, so the once-a-second clock only republishes changes.
    published_tooltip: Option<String>,
    /// The settings revision the running live fetch started under.
    refresh_revision: u64,
    /// The last refresh request from Settings that was acted on.
    refresh_requests: u64,
    /// The settings revision the shown accounts were fetched under, or `None` when they came from a fetch that
    /// settings changed during; failed notifications are only resent from accounts that are current.
    accounts_revision: Option<u64>,
    /// The minute the table's compact history was computed for; it is recomputed as the 14-day window moves.
    compact_minute: i64,
    /// One focus handle per view tab. Only the selected tab is a Tab-key stop (a roving tab stop); the arrow keys
    /// move focus between tabs, and Enter or Space activates the focused one.
    view_tab_focus: Vec<FocusHandle>,
    /// The groups and manual order the table was arranged with (#89); a change rearranges it.
    layout: codexbar_core::layout::Layout,
    /// The alert settings and held alerts the Smart order was ranked with; a change re-ranks it.
    ranked_with: (codexbar_core::alerts::AlertSettings, std::collections::BTreeSet<String>),
    /// The dashboard account each configured record owns, by record id (#85); a record that disappears takes its
    /// account's history, snapshot and preferences with it.
    owned_ids: HashMap<String, String>,
    /// Removed accounts, with whether their history is deleted on disk yet. A refresh that started before the removal
    /// can still write them back, and a failed rewrite needs another try, so each refresh checks them again.
    forgotten: HashMap<String, bool>,
    /// When `widgets.json` was last written (#94); it is rewritten at least every few minutes so the widgets can
    /// tell a running CodexBar from one that quit. Only Windows has widgets.
    #[cfg(windows)]
    widgets_written: Option<DateTime<Utc>>,
    /// A widget tile's account (#95) that wasn't loaded yet when it was tapped; shown once it arrives.
    pending_focus: Option<(crate::handoff::FocusRequest, Instant)>,
    /// Where Settings opens next (#188): a failure row's Open settings button points it at the failed account's
    /// group. Taken by the first Settings frame; later frames keep whatever the user navigates to.
    settings_start: Option<gpui_kit::component::setting::SelectIndex>,
    /// Bumped with every such jump, so a Settings panel already on screen starts over at the new place.
    settings_generation: u32,
    /// The adapters the settings produced when they last changed, as (provider name, account id) pairs (#192): a
    /// settings change that adds one fetches it at once, and one that drops one takes its rows off the screen.
    configured: Vec<(&'static str, Option<String>)>,
    /// The window placement last saved (#194), and one seen since that is saved once it stays the same for a tick,
    /// so a drag or resize is written once it ends rather than at every step.
    placement: Option<codexbar_store::prefs::WindowPlacement>,
    placement_pending: Option<codexbar_store::prefs::WindowPlacement>,
    /// The widget builder's choices the last snapshot carried (#95).
    #[cfg(windows)]
    widget_builder: codexbar_store::widgets::WidgetBuilder,
    _clock: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// True when the table's column `ix` is no longer the width the table gave it (#245): the column was dragged.
fn column_was_dragged(delegate: &AccountTable, ix: usize, width: gpui_kit::Pixels, cx: &gpui_kit::App) -> bool {
    use gpui_kit::component::table::TableDelegate as _;
    ix < delegate.columns_count(cx) && (delegate.column(ix, cx).width - width).abs() > px(0.5)
}

/// How long a widget tap waits for an account that hasn't loaded yet (#95).
const PENDING_FOCUS_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

impl Dashboard {
    pub fn new(source: DataSource, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The taskbar, Alt+Tab, window managers and the macOS Window menu name the window by its title, which carries
        // the version as the title bar does (#244).
        window.set_window_title(&crate::version::title());
        let now = Utc::now();
        let table = cx.new(|cx| {
            TableState::new(
                AccountTable::new(Vec::new(), now, Compact::default(), States::new()),
                window,
                cx,
            )
            .row_selectable(true)
            .col_selectable(false)
            .col_movable(false)
            // Column borders drag to resize (#245); the widths are remembered in dashboard.json.
            .col_resizable(true)
            .sortable(false)
        });

        let selection = cx.subscribe(&table, |this, table, event: &TableEvent, cx| match event {
            TableEvent::SelectRow(row) => {
                this.selected = table.read(cx).delegate().row(*row).map(|account| account.id().clone());
                let selected = this.selected.clone();
                this.history_view
                    .update(cx, |view, cx| view.follow(selected.as_ref(), cx));
                cx.notify();
            }
            // A drag ended (#245): remember each column's width, unzoomed, so it comes back at any zoom. Preferences
            // from a newer CodexBar can't be saved, so the borders don't drag then (see `refresh`).
            TableEvent::ColumnWidthsChanged(widths) if !crate::prefs_hub::PrefsHub::is_read_only(cx) => {
                let zoom = crate::zoom::level(cx) as f32;
                let delegate = table.read(cx).delegate();
                // Only the columns that moved: one never dragged keeps following its default, and one dragged back to
                // its default forgets its saved width.
                let widths: Vec<(String, Option<f32>)> = widths
                    .iter()
                    .enumerate()
                    .filter(|(ix, width)| column_was_dragged(delegate, *ix, **width, cx))
                    .filter_map(|(ix, width)| {
                        let width = f32::from(*width) / zoom;
                        let default = delegate.column_default_width(ix)?;
                        Some((
                            delegate.column_key(ix)?,
                            ((width - default).abs() >= 0.5).then_some(width),
                        ))
                    })
                    .collect();
                crate::prefs_hub::PrefsHub::set_column_widths(cx, &widths);
            }
            _ => {}
        });
        let activation = cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() {
                this.deactivated_at = Some(Instant::now());
            }
        });

        // Once a second: advance the clock that ages and countdowns read, and refresh when due.
        // Read now, before the first refresh below can start Keychain lookups: the task only starts later.
        let mut keychain_changes = codexbar_providers::claude::keychain_changes();
        let window_handle = window.window_handle();
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                // Where the window is now (#194); read on the window, outside the entity update.
                let placement = cx
                    .update_window(window_handle, |_, window, cx| crate::placement::capture(window, cx))
                    .ok();
                let alive = this.update(cx, |this, cx| {
                    this.now = crate::sus::now(cx);
                    if let Some(placement) = placement {
                        this.note_placement(placement, cx);
                    }
                    // A Claude sign-in looked up in the Keychain in the background came out differently from what the
                    // accounts were last sorted by: fetch again with the new answer.
                    let changes = codexbar_providers::claude::keychain_changes();
                    if changes != keychain_changes {
                        keychain_changes = changes;
                        SettingsHub::request_refresh(cx);
                    }
                    // Windows' animation setting, system appearance and high contrast can change while CodexBar
                    // runs (#92); gpui-kit reads the animation setting only at start.
                    gpui_kit::base::apply_system_reduce_motion(cx);
                    let appearance = crate::prefs_hub::PrefsHub::appearance(cx);
                    crate::theme::apply(cx, appearance);
                    let interval = SettingsHub::global(cx).settings().refresh_interval_secs();
                    let due = match (this.last_refresh, interval) {
                        (None, _) => true,
                        (Some(_), None) => false,
                        (Some(at), Some(secs)) => this.now - at >= Duration::seconds(secs as i64),
                    };
                    if due && !this.loading {
                        this.refresh(cx);
                    } else if this.now.timestamp() / 60 != this.compact_minute {
                        this.update_compact_history(cx);
                        this.rerank_if_stale(cx);
                    }
                    let now = this.now;
                    this.table.update(cx, |table, _| table.delegate_mut().set_now(now));
                    for account in &this.accounts {
                        if account.observation_stale(this.now)
                            && matches!(this.states.get(account.id().as_str()), None | Some(AccountState::Fresh))
                        {
                            this.states.insert(
                                account.id().as_str().to_owned(),
                                AccountState::Failed("Muse allowance observation is over 7 minutes old".into()),
                            );
                        }
                    }
                    // The tooltip's "last known" age advances too; it is only republished when its text changes.
                    this.publish_tooltip(cx);
                    #[cfg(windows)]
                    if this
                        .widgets_written
                        .is_none_or(|at| this.now - at >= crate::widget_feed::HEARTBEAT)
                    {
                        this.save_widget_snapshot(cx);
                    }
                    // Resend against the last refresh's accounts while settings still match it; otherwise refresh,
                    // so the failed alert is judged under the current settings rather than dropped.
                    if this.accounts_revision == Some(SettingsHub::revision(cx)) {
                        // Only fresh accounts (unlisted ones, as in the demo, are fresh): a failed account's
                        // last-good snapshot neither clears nor repeats alerts.
                        let accounts: Vec<AccountSnapshot> = this
                            .accounts
                            .iter()
                            .filter(|account| {
                                matches!(this.states.get(account.id().as_str()), None | Some(AccountState::Fresh))
                            })
                            .cloned()
                            .collect();
                        crate::notifications::retry_failed(cx, &accounts, this.now);
                    } else if crate::notifications::has_failed(cx) {
                        this.refresh(cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let history = match &source {
            DataSource::Live { history, .. } => {
                migrate_legacy_ids(history, cx);
                history.clone()
            }
            DataSource::Demo => Arc::new(Mutex::new(demo_history(&demo_accounts(now, &Local), now))),
        };
        let history_view = cx.new(|cx| HistoryView::new(history.clone(), cx));
        let spend_view = cx.new(|cx| SpendView::new(history.clone(), cx));
        let ring_size_seen = crate::prefs_hub::PrefsHub::ring_size(cx);
        let ring_slider = cx.new(|_| {
            gpui_kit::component::slider::SliderState::new()
                .min(0.)
                .max(100.)
                .step(crate::ring_size::STEP)
                .default_value(ring_size_seen.unwrap_or(crate::ring_size::DEFAULT))
        });
        // A drag redraws the ring as it goes and saves the size when it is let go.
        let ring_redraws = cx.observe(&ring_slider, |_, _, cx| cx.notify());
        let ring_saves = cx.subscribe(
            &ring_slider,
            |this, _, event: &gpui_kit::component::slider::SliderEvent, cx| {
                if let gpui_kit::component::slider::SliderEvent::Release(value) = event {
                    let size = Some(value.end());
                    this.ring_size_seen = size;
                    crate::prefs_hub::PrefsHub::set_ring_size(cx, size);
                }
            },
        );
        // The Spend view's empty state opens Settings on the Accounts page.
        let spend_requests = cx.subscribe(&spend_view, |this, _, request: &SpendRequest, cx| match request {
            SpendRequest::OpenAccounts => this.open_accounts(cx),
            SpendRequest::OpenUsage => this.show_view(DashboardView::Usage, cx),
        });

        // Groups and the manual order can change in Settings or from the focused account; rearrange the table then.
        // Smart order also follows alert settings and the held alerts, which the demo keeps in `Notifications`.
        let layout_changes = cx.observe_global::<crate::prefs_hub::PrefsHub>(|this, cx| {
            this.rearrange_if_changed(cx);
            // The widget builder's tiles (#95) go to the widgets with the next snapshot; write it now.
            #[cfg(windows)]
            if crate::prefs_hub::PrefsHub::widget_builder(cx) != this.widget_builder {
                this.save_widget_snapshot(cx);
            }
        });
        let alert_changes =
            cx.observe_global::<crate::notifications::Notifications>(|this, cx| this.rearrange_if_changed(cx));
        let settings_changes = cx.observe_global::<SettingsHub>(|this, cx| {
            this.forget_removed_accounts(cx);
            this.follow_configured_adapters(cx);
            let requests = SettingsHub::refresh_requests(cx);
            if requests != this.refresh_requests {
                this.refresh_requests = requests;
                this.refresh(cx);
            }
        });
        let mut dashboard = Self {
            source,
            history,
            history_view,
            spend_view,
            ring_slider,
            ring_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            ring_size_seen,
            accounts: Vec::new(),
            failures: Vec::new(),
            loading: false,
            last_refresh: None,
            table,
            selected: None,
            view: DashboardView::Usage,
            now,
            deactivated_at: None,
            table_zoom: crate::zoom::level(cx),
            refresh_queued: false,
            states: States::new(),
            fetch_partial: false,
            placeholders: Default::default(),
            published_tooltip: None,
            refresh_revision: 0,
            refresh_requests: 0,
            accounts_revision: None,
            compact_minute: 0,
            view_tab_focus: DashboardView::ALL.iter().map(|_| cx.focus_handle()).collect(),
            layout: crate::prefs_hub::PrefsHub::layout(cx),
            ranked_with: Default::default(),
            owned_ids: crate::providers::owned_account_ids(SettingsHub::global(cx)),
            forgotten: HashMap::new(),
            #[cfg(windows)]
            widgets_written: None,
            pending_focus: None,
            settings_start: None,
            settings_generation: 0,
            configured: Vec::new(),
            placement: crate::prefs_hub::PrefsHub::window_placement(cx),
            placement_pending: None,
            #[cfg(windows)]
            widget_builder: Default::default(),
            _clock: clock,
            _subscriptions: vec![
                selection,
                activation,
                layout_changes,
                alert_changes,
                settings_changes,
                spend_requests,
                ring_redraws,
                ring_saves,
            ],
        };
        dashboard.configured = dashboard.configured_adapters(cx).unwrap_or_default();
        if matches!(dashboard.source, DataSource::Live { .. }) {
            dashboard.restore_snapshots(cx);
        }
        dashboard.refresh(cx);
        // Later bindings win, so these take Tab back from the table's own bindings.
        cx.bind_keys([
            gpui_kit::KeyBinding::new("tab", LeaveTableForward, Some("DataTable")),
            gpui_kit::KeyBinding::new("shift-tab", LeaveTableBackward, Some("DataTable")),
        ]);
        // Keyboard users start on the selected view tab, so the first Tab or arrow key does something (#92).
        let selected = DashboardView::ALL
            .iter()
            .position(|view| *view == dashboard.view)
            .unwrap_or(0);
        window.focus(&dashboard.view_tab_focus[selected], cx);
        dashboard
    }

    /// True when a tray click should hide rather than raise: the window is up and was focused until the click.
    pub fn should_hide_on_toggle(&self, window: &Window) -> bool {
        crate::tray::is_shown(window)
            && (window.is_window_active() || self.deactivated_at.is_some_and(|at| at.elapsed() < TOGGLE_GRACE))
    }

    /// The history store, for the headless UI tests.
    #[cfg(test)]
    pub fn history(&self) -> Arc<Mutex<HistoryStore>> {
        self.history.clone()
    }

    /// A shown account, for the headless UI tests.
    #[cfg(test)]
    pub fn account(&self, id: &str) -> Option<&AccountSnapshot> {
        self.accounts.iter().find(|account| account.id().as_str() == id)
    }

    /// Shown account ids in table order, for the headless UI tests.
    #[cfg(test)]
    pub fn account_ids(&self) -> Vec<String> {
        self.accounts
            .iter()
            .map(|account| account.id().as_str().to_owned())
            .collect()
    }

    /// An account's refresh state (fresh when unlisted), for the headless UI tests.
    #[cfg(test)]
    pub fn state(&self, id: &str) -> AccountState {
        self.states.get(id).cloned().unwrap_or(AccountState::Fresh)
    }

    /// Shown account names, for the headless UI tests.
    #[cfg(test)]
    pub fn display_names_for_test(&self) -> Vec<String> {
        self.accounts.iter().map(AccountSnapshot::display_name).collect()
    }

    /// When the last full refresh finished, for the headless UI tests.
    #[cfg(test)]
    pub fn last_refresh_for_test(&self) -> Option<DateTime<Utc>> {
        self.last_refresh
    }

    /// The providers listed as failed, for the headless UI tests.
    #[cfg(test)]
    pub fn failed_providers(&self) -> Vec<&'static str> {
        self.failures.iter().map(|failure| failure.provider).collect()
    }

    /// The visible view, for the headless UI tests.
    #[cfg(test)]
    pub fn view(&self) -> DashboardView {
        self.view
    }

    /// The History view and the shared history store, for the headless UI tests.
    #[cfg(test)]
    pub fn history_parts(&self) -> (Entity<HistoryView>, Arc<Mutex<HistoryStore>>) {
        (self.history_view.clone(), self.history.clone())
    }

    /// The id of the account in table row `ix`, for the headless UI tests.
    #[cfg(test)]
    pub fn account_id(&self, ix: usize) -> Option<String> {
        self.accounts.get(ix).map(|account| account.id().as_str().to_owned())
    }

    /// The usage table, for the headless UI tests.
    #[cfg(test)]
    pub fn table(&self) -> &Entity<TableState<AccountTable>> {
        &self.table
    }

    /// Selects a table row the way a click does (the table emits its selection event), for the headless UI tests.
    #[cfg(test)]
    pub fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| table.set_selected_row(ix, cx));
    }

    /// Shows an account and metric, from a widget tile (#95): the Usage view with the account selected, and History
    /// set to the metric. An account that isn't loaded yet is shown when it arrives; a removed one just opens Usage.
    pub fn focus_account(&mut self, request: &crate::handoff::FocusRequest, cx: &mut Context<Self>) {
        self.view = DashboardView::Usage;
        let Some(ix) = self
            .accounts
            .iter()
            .position(|account| account.id().as_str() == request.account)
        else {
            self.pending_focus = Some((request.clone(), Instant::now()));
            cx.notify();
            return;
        };
        self.pending_focus = None;
        let id = self.accounts[ix].id().clone();
        self.selected = Some(id.clone());
        self.table.update(cx, |table, cx| table.set_selected_row(ix, cx));
        let metric = request.metric.clone();
        self.history_view
            .update(cx, |view, cx| view.focus(&id, metric.as_deref(), cx));
        cx.notify();
    }

    /// The gauge ring's size slider focus stop (#219), for the headless UI tests.
    #[cfg(test)]
    pub fn ring_focus(&self) -> FocusHandle {
        self.ring_focus.clone()
    }

    /// The selected account's id, for the headless UI tests.
    #[cfg(test)]
    pub fn selected_id(&self) -> Option<String> {
        self.selected.as_ref().map(|id| id.as_str().to_owned())
    }

    /// The History view's account and metric, for the headless UI tests.
    #[cfg(test)]
    pub fn history_shown(&self, cx: &gpui_kit::App) -> Option<(String, String)> {
        self.history_view.read(cx).shown()
    }

    /// Switches the visible view (the tray's Settings… item, the `--settings` flag).
    pub fn show_view(&mut self, view: DashboardView, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
    }

    /// Fetches every provider off the UI thread. A failed provider keeps its last good accounts.
    /// Fetches every enabled provider.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.fetch(None, cx);
    }

    /// Fetches one failed adapter again (a provider, or one configured account of it), keeping every other account
    /// as it is.
    pub fn retry(&mut self, provider: &'static str, account: Option<String>, cx: &mut Context<Self>) {
        self.fetch(Some((provider, account)), cx);
    }

    /// The record a failure row's Open settings would edit, for the headless UI tests.
    #[cfg(test)]
    pub fn settings_target_record(
        &self,
        provider: &'static str,
        account: Option<String>,
        cx: &gpui_kit::App,
    ) -> Option<String> {
        self.failures
            .iter()
            .find(|failure| failure.provider == provider && failure.account == account)
            .and_then(|failure| settings_target(failure, &self.owned_ids, cx))
            .and_then(|(_, record)| record.map(|record| record.id))
    }

    /// Shows Settings on the Accounts page, where accounts are added (the Spend view's empty state, #215).
    fn open_accounts(&mut self, cx: &mut Context<Self>) {
        self.settings_start = Some(gpui_kit::component::setting::SelectIndex {
            page_ix: crate::settings_view::ACCOUNTS_PAGE,
            group_ix: None,
        });
        self.settings_generation = self.settings_generation.wrapping_add(1);
        self.view = DashboardView::Settings;
        cx.notify();
    }

    /// A failure row's Open settings (#188): shows Settings on the Accounts page at the failed provider's group and,
    /// for an account whose key or cookie is entered there, opens its dialog (Add account… when the provider has no
    /// record yet). Accounts CodexBar signs in itself land on their row, where Sign in… is.
    pub fn open_settings_for(
        &mut self,
        provider: &'static str,
        account: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self
            .failures
            .iter()
            .find(|failure| failure.provider == provider && failure.account == account)
            .and_then(|failure| settings_target(failure, &self.owned_ids, cx));
        self.settings_start = Some(gpui_kit::component::setting::SelectIndex {
            page_ix: crate::settings_view::ACCOUNTS_PAGE,
            group_ix: target.as_ref().map(|(group, _)| *group),
        });
        self.settings_generation = self.settings_generation.wrapping_add(1);
        self.view = DashboardView::Settings;
        cx.notify();
        let Some((group, record)) = target else {
            return;
        };
        let info = &crate::catalog::PROVIDERS[group];
        let managed = record
            .as_ref()
            .is_some_and(|record| crate::managed::Managed::of(record).is_some());
        if info.secret.is_some() && !managed {
            crate::settings_view::open_account_dialog(info.id, record, window, cx);
        }
    }

    fn fetch(&mut self, only: Option<(&'static str, Option<String>)>, cx: &mut Context<Self>) {
        // One refresh at a time: a tray Refresh during a fetch would otherwise race it, and the older result could
        // land last, replacing newer accounts and recording history out of order. A request made meanwhile runs
        // right after, with the settings as they are then.
        if self.loading {
            self.refresh_queued = true;
            return;
        }
        let providers = match &self.source {
            DataSource::Demo => {
                let now = Utc::now();
                self.last_refresh = Some(now);
                let accounts = demo_accounts(now, &Local);
                self.accounts_revision = Some(SettingsHub::revision(cx));
                crate::notifications::process(cx, &accounts, now);
                // Demo history keeps up with the demo accounts, as live history does.
                let _ = self
                    .history
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .record(&accounts, now);
                self.set_accounts(accounts, cx);
                return;
            }
            DataSource::Live { history, providers } => (history.clone(), providers.clone()),
        };
        let (history, factory) = providers;
        // Account settings may have changed since the last refresh (a first configured account added).
        migrate_legacy_ids(&history, cx);
        let mut providers = factory(SettingsHub::global(cx));
        if let Some((name, account)) = &only {
            providers.retain(|provider| {
                provider.name() == *name && (account.is_none() || provider.account_id() == account.as_deref())
            });
        }
        self.refresh_revision = SettingsHub::revision(cx);
        self.loading = true;
        self.fetch_partial = only.is_some();
        self.show_placeholders(&providers, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move {
                    let now = Utc::now();
                    providers
                        .iter()
                        .map(|provider| {
                            // Lock only after the network fetch: the History view reads the store on the UI thread.
                            let result = provider.fetch_outcomes(now).map(|outcomes| {
                                let mut history = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                                let fresh: Vec<AccountSnapshot> = outcomes
                                    .iter()
                                    .filter_map(|outcome| match outcome {
                                        AccountOutcome::Fresh(account) => Some(account.clone()),
                                        AccountOutcome::Failed { .. } => None,
                                    })
                                    .collect();
                                // A history write failure must not hide fresh usage; the next refresh retries.
                                let _ = history.record(&fresh, now);
                                outcomes
                                    .into_iter()
                                    .map(|outcome| match outcome {
                                        AccountOutcome::Fresh(account) => {
                                            AccountOutcome::Fresh(codexbar_store::enrich(
                                                &history,
                                                account,
                                                &Local,
                                                crate::locale::style(),
                                                now,
                                            ))
                                        }
                                        failed => failed,
                                    })
                                    .collect()
                            });
                            (
                                provider.name(),
                                provider.account_id().map(str::to_owned),
                                provider.account_label().map(str::to_owned),
                                result,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| this.apply_results(results, cx)).ok();
        })
        .detach();
    }

    /// Configured providers with nothing shown yet (first run, newly added) get a placeholder row, so every account
    /// is visible before its first result arrives.
    fn show_placeholders(&mut self, providers: &[Arc<dyn UsageProvider>], cx: &mut Context<Self>) {
        let mut accounts = self.accounts.clone();
        let mut added = false;
        for provider in providers {
            let Some(kind) = Provider::from_display_name(provider.name()) else {
                continue;
            };
            // An adapter for one configured account gets a placeholder under that account's id, which its first
            // result then replaces; others get one per provider.
            let shown = match provider.account_id() {
                Some(id) => accounts.iter().any(|account| account.id().as_str() == id),
                None => accounts
                    .iter()
                    .any(|account| account.provider().display_name() == provider.name()),
            };
            if !shown {
                let id = AccountId::new(
                    provider
                        .account_id()
                        .map_or_else(|| format!("{PLACEHOLDER_PREFIX}{}", kind.key()), str::to_owned),
                );
                self.states.insert(id.as_str().to_owned(), AccountState::Loading);
                self.placeholders.insert(id.as_str().to_owned());
                let placeholder = AccountSnapshot::new(id, kind, Vec::new(), Utc::now());
                // Labelled like the account it stands for, so sibling accounts' rows can be told apart.
                accounts.push(match provider.account_label() {
                    Some(label) => placeholder.with_label(label),
                    None => placeholder,
                });
                added = true;
            }
        }
        if added {
            self.set_accounts(accounts, cx);
        }
    }

    fn apply_results(&mut self, results: Vec<FetchResult>, cx: &mut Context<Self>) {
        // Which adapters reported: a provider name, narrowed to one account when the adapter serves just one.
        let adapters: Vec<(&'static str, Option<String>)> = results
            .iter()
            .map(|(provider, account, _, _)| (*provider, account.clone()))
            .collect();
        let reported = |account: &AccountSnapshot| {
            adapters.iter().any(|(name, id)| match id {
                Some(id) => account.id().as_str() == id,
                None => account.provider().display_name() == *name,
            })
        };
        // A retry replaces only what it fetched; a full refresh replaces everything, so providers switched off since
        // disappear.
        let mut accounts: Vec<AccountSnapshot> = if self.fetch_partial {
            self.accounts
                .iter()
                .filter(|account| !reported(account))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if self.fetch_partial {
            self.failures.retain(|failure| {
                !adapters
                    .iter()
                    .any(|(name, id)| failure.provider == *name && failure.account == *id)
            });
        } else {
            self.failures.clear();
            self.states
                .retain(|id, _| accounts.iter().any(|account| account.id().as_str() == id));
        }
        // Only accounts that refreshed are checked for alerts: a failed provider's alerts neither clear nor repeat.
        let mut refreshed = Vec::new();
        for (provider, account_id, account_label, result) in results {
            match result {
                Ok(outcomes) => {
                    for outcome in outcomes {
                        match outcome {
                            AccountOutcome::Fresh(account) => {
                                let account = crate::prefs_hub::PrefsHub::with_credit_start(cx, account);
                                let state = AccountState::fresh(&account, self.now);
                                self.states.insert(account.id().as_str().to_owned(), state);
                                self.placeholders.remove(account.id().as_str());
                                refreshed.push(account.clone());
                                accounts.push(account);
                            }
                            // One account of the provider failed while others succeeded: keep its last good usage
                            // (or show it unavailable) and list it as its own failure.
                            AccountOutcome::Failed { account, label, error } => {
                                let message = crate::locale::describe(&error);
                                let known = self.accounts.iter().find(|shown| shown.id() == &account).cloned();
                                let row = known.unwrap_or_else(|| {
                                    self.placeholders.insert(account.as_str().to_owned());
                                    let kind = Provider::from_display_name(provider).unwrap_or(Provider::Copilot);
                                    let placeholder =
                                        AccountSnapshot::new(account.clone(), kind, Vec::new(), Utc::now());
                                    match &label {
                                        Some(label) => placeholder.with_label(label.clone()),
                                        None => placeholder,
                                    }
                                });
                                let state = if self.placeholders.contains(account.as_str()) {
                                    AccountState::Unavailable(message.clone())
                                } else {
                                    AccountState::Failed(message.clone())
                                };
                                self.states.insert(account.as_str().to_owned(), state);
                                accounts.push(row);
                                self.failures.push(Failure {
                                    provider,
                                    account: account_id.clone(),
                                    label,
                                    message,
                                    sign_in: error.needs_sign_in(),
                                });
                            }
                        }
                    }
                }
                Err(error) => {
                    // Keep the failed adapter's last good snapshots (or its placeholder) visible, marked stale. An
                    // adapter for one configured account keeps only that account; its siblings report separately.
                    let message = crate::locale::describe(&error);
                    let belongs = |a: &&AccountSnapshot| match &account_id {
                        Some(id) => a.id().as_str() == id,
                        None => a.provider().display_name() == provider,
                    };
                    for account in self.accounts.iter().filter(belongs) {
                        self.states.insert(
                            account.id().as_str().to_owned(),
                            if self.placeholders.contains(account.id().as_str()) {
                                AccountState::Unavailable(message.clone())
                            } else {
                                AccountState::Failed(message.clone())
                            },
                        );
                        accounts.push(account.clone());
                    }
                    self.failures.push(Failure {
                        provider,
                        account: account_id.clone(),
                        label: account_label.clone(),
                        message,
                        sign_in: error.needs_sign_in(),
                    });
                }
            }
        }
        self.loading = false;
        // A retry of one provider doesn't count as a refresh: the next full refresh keeps its schedule.
        if !self.fetch_partial {
            self.last_refresh = Some(Utc::now());
        }
        // Placeholders that a result replaced (or that left with their provider) are gone.
        self.placeholders
            .retain(|id| accounts.iter().any(|account| account.id().as_str() == id));
        // Settings saved while the fetch ran (an account switched off) make its results stale for alerting; the next
        // refresh judges everything against the current settings.
        if SettingsHub::revision(cx) == self.refresh_revision {
            self.accounts_revision = Some(self.refresh_revision);
            crate::notifications::process(cx, &refreshed, Utc::now());
        } else {
            self.accounts_revision = None;
            // An account switched off (or a provider, or a record removed) while the fetch ran must not come back
            // with the fetch's results, not even until the next refresh replaces them (#192).
            if let Some(adapters) = self.configured_adapters(cx) {
                accounts.retain(|account| covered_by(&adapters, account));
                self.failures
                    .retain(|failure| adapters.contains(&(failure.provider, failure.account.clone())));
                self.states
                    .retain(|id, _| accounts.iter().any(|account| account.id().as_str() == id));
                self.placeholders
                    .retain(|id| accounts.iter().any(|account| account.id().as_str() == id));
            }
            // Run again under the current settings, so a crossed condition is still noticed even with automatic
            // refresh off.
            self.refresh_queued = true;
        }
        // A fetch that started before an account was removed still returns it, and recorded its history: take it out
        // again (#85).
        if !self.forgotten.is_empty() {
            let returned: Vec<String> = accounts
                .iter()
                .map(|account| account.id().as_str().to_owned())
                .filter(|id| self.forgotten.contains_key(id))
                .collect();
            for id in &returned {
                self.forgotten.insert(id.clone(), false);
            }
            accounts.retain(|account| !self.forgotten.contains_key(account.id().as_str()));
            self.delete_forgotten_history(cx);
        }
        self.set_accounts(accounts, cx);
        self.save_snapshots(cx);
        if std::mem::take(&mut self.refresh_queued) {
            self.refresh(cx);
        }
    }

    /// Saves real accounts' usage for the next start. Placeholders aren't saved; a write failure only costs the
    /// restored view next time.
    fn save_snapshots(&self, cx: &mut Context<Self>) {
        if !matches!(self.source, DataSource::Live { .. }) {
            return;
        }
        let real: Vec<AccountSnapshot> = self
            .accounts
            .iter()
            .filter(|account| !self.placeholders.contains(account.id().as_str()))
            .cloned()
            .collect();
        let _ = codexbar_store::snapshots::save_snapshots(SettingsHub::global(cx).dir(), &real);
    }

    /// Shows the last-good snapshots from the previous run until the first fetch returns.
    fn restore_snapshots(&mut self, cx: &mut Context<Self>) {
        let DataSource::Live { providers, .. } = &self.source else {
            return;
        };
        // Only accounts still switched on: one removed or disabled since the last run doesn't reappear.
        let hub = SettingsHub::global(cx);
        let adapters = providers(hub);
        let enabled: Vec<&'static str> = adapters.iter().map(|provider| provider.name()).collect();
        // A provider signed in to another account since the last run (Cursor, #81) doesn't show the old account's
        // saved usage, even briefly.
        let signed_in: Vec<(&'static str, AccountId)> = adapters
            .iter()
            .filter_map(|provider| Some((provider.name(), provider.signed_in_account()?)))
            .collect();
        let disabled: Vec<String> = hub
            .settings()
            .accounts()
            .iter()
            .filter(|account| !account.enabled)
            // A switched-off Codex or Claude account CodexBar signed in reports under the identity it holds (#78, #80).
            .flat_map(|account| {
                let identity = (crate::codex_sign_in::is_managed(account)
                    || crate::claude_sign_in::is_managed(account)
                    || crate::cursor_sign_in::is_managed(account))
                .then(|| account.external_id.clone())
                .flatten();
                std::iter::once(account.id.clone()).chain(identity)
            })
            .collect();
        let restored: Vec<AccountSnapshot> = codexbar_store::snapshots::load_snapshots(hub.dir())
            .into_iter()
            .filter(|account| enabled.contains(&account.provider().display_name()))
            .filter(|account| !disabled.iter().any(|id| id == account.id().as_str()))
            .filter(|account| {
                // With several adapters for one provider (Codex accounts, #78), any of them may hold the account.
                let mut current = signed_in
                    .iter()
                    .filter(|(name, _)| *name == account.provider().display_name())
                    .peekable();
                current.peek().is_none() || current.any(|(_, id)| id == account.id())
            })
            .collect();
        if restored.is_empty() {
            return;
        }
        let now = Utc::now();
        let restored: Vec<AccountSnapshot> = {
            let history = self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            restored
                .into_iter()
                .map(|account| codexbar_store::enrich(&history, account, &Local, crate::locale::style(), now))
                .collect()
        };
        for account in &restored {
            self.states
                .insert(account.id().as_str().to_owned(), AccountState::Restored);
        }
        self.set_accounts(restored, cx);
    }

    /// What Smart order ranks each account by (#90): the status the heading shows, the strongest alert that holds,
    /// a projected run-out, and whether its usage is current.
    fn urgency_of(
        &self,
        accounts: &[AccountSnapshot],
        cx: &gpui_kit::App,
    ) -> HashMap<String, codexbar_core::layout::Urgency> {
        use codexbar_core::alerts::AlertKind;
        use codexbar_core::layout::{Health, Urgency};
        let settings = crate::prefs_hub::PrefsHub::alert_settings(cx);
        let active = crate::notifications::Notifications::active(cx);
        accounts
            .iter()
            .map(|account| {
                let id = account.id().as_str();
                let assessment = account.assess(self.now);
                let details = codexbar_core::alerts::account_alerts(&settings, &active, account, self.now);
                let alert = details.first().map_or(0, |detail| match detail.kind {
                    AlertKind::Critical => 3,
                    AlertKind::Warning => 2,
                    AlertKind::Usage | AlertKind::Balance => 1,
                });
                let severity = if details.is_empty() {
                    assessment.severity()
                } else {
                    assessment.severity().max(codexbar_core::Severity::Watch)
                };
                let health = match self.states.get(id) {
                    None | Some(AccountState::Fresh) => Health::Fresh,
                    Some(AccountState::Restored | AccountState::Failed(_)) => Health::Stale,
                    Some(AccountState::Loading | AccountState::Unavailable(_)) => Health::Unavailable,
                };
                let urgency = Urgency {
                    severity,
                    alert,
                    // A used-up window that credits carry projects nothing (#150).
                    projected: account
                        .metrics()
                        .iter()
                        .filter(|metric| !account.credits_cover(metric))
                        .any(|metric| {
                            account
                                .evaluation_time(metric, self.now)
                                .is_some_and(|at| metric.exhausts_before_reset(at))
                        }),
                    health,
                    pressure: assessment.pressure(),
                };
                (id.to_owned(), urgency)
            })
            .collect()
    }

    /// Remembers where the window is (#194), once a placement has stayed the same for a tick. A minimized window
    /// can't say whether it is also maximized, so the state last seen while it was up is kept: the sample still
    /// pending when there is one, else the saved one. The demo saves nothing.
    fn note_placement(&mut self, mut seen: codexbar_store::prefs::WindowPlacement, cx: &mut Context<Self>) {
        if !matches!(self.source, DataSource::Live { .. }) {
            return;
        }
        // A sample not yet saved (a maximize a moment before the hide) is the latest word on that.
        if seen.minimized
            && let Some(last) = self.placement_pending.as_ref().or(self.placement.as_ref())
        {
            seen.maximized = last.maximized;
        }
        if self.placement.as_ref() == Some(&seen) {
            self.placement_pending = None;
            return;
        }
        if self.placement_pending.as_ref() == Some(&seen) {
            // A failed save (a busy lock) leaves the placement pending, so the next tick tries again.
            if crate::prefs_hub::PrefsHub::set_window_placement(cx, seen.clone()) {
                self.placement = Some(seen);
                self.placement_pending = None;
            }
        } else {
            self.placement_pending = Some(seen);
        }
    }

    /// The adapters the current settings produce, as (provider name, account id) pairs: what a shown account or failure
    /// row must be served by once the settings change (#192). `None` in the demo, which has no adapters.
    fn configured_adapters(&self, cx: &gpui_kit::App) -> Option<Vec<(&'static str, Option<String>)>> {
        let DataSource::Live { providers, .. } = &self.source else {
            return None;
        };
        Some(
            providers(SettingsHub::global(cx))
                .iter()
                .map(|provider| (provider.name(), provider.account_id().map(str::to_owned)))
                .collect(),
        )
    }

    /// Follows a settings change to the adapters it produces (#192): rows and failure banners no adapter serves any
    /// more are dropped (an account switched off, its provider turned off, a record removed), and an adapter that
    /// didn't exist before (an account switched on or added) is fetched at once, whatever the automatic refresh
    /// setting. A refresh that is still running is handled when its results land, in `apply_results`.
    fn follow_configured_adapters(&mut self, cx: &mut Context<Self>) {
        let Some(adapters) = self.configured_adapters(cx) else {
            return;
        };
        let added = adapters.iter().any(|adapter| !self.configured.contains(adapter));
        self.configured = adapters.clone();
        self.drop_unconfigured_accounts(&adapters, cx);
        if added {
            self.refresh(cx);
        }
    }

    /// Drops the rows and failure banners none of `adapters` serves.
    fn drop_unconfigured_accounts(&mut self, adapters: &[(&'static str, Option<String>)], cx: &mut Context<Self>) {
        let before = self.accounts.len() + self.failures.len();
        self.failures
            .retain(|failure| adapters.contains(&(failure.provider, failure.account.clone())));
        let accounts: Vec<AccountSnapshot> = self
            .accounts
            .iter()
            .filter(|account| covered_by(adapters, account))
            .cloned()
            .collect();
        if before == accounts.len() + self.failures.len() {
            return;
        }
        self.states
            .retain(|id, _| accounts.iter().any(|account| account.id().as_str() == id));
        self.placeholders
            .retain(|id| accounts.iter().any(|account| account.id().as_str() == id));
        self.set_accounts(accounts, cx);
        self.save_snapshots(cx);
    }

    /// When accounts are removed in Settings (one at a time or by Reset), deletes what was kept for them: stored history,
    /// the last-good snapshot, preferences, held alerts and their place in groups (#85). Accounts that keep showing
    /// through a provider's own sign-in own no id here, so their history stays. Nothing is deleted while the settings
    /// are read-only, or in the demo.
    fn forget_removed_accounts(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.source, DataSource::Live { .. }) {
            return;
        }
        let hub = SettingsHub::global(cx);
        if hub.is_read_only() {
            return;
        }
        let owned = crate::providers::owned_account_ids(hub);
        // A Copilot account still shows through discovery when Copilot isn't limited to usernames.
        let discovers_all = crate::providers::copilot_discovers_all(hub);
        let removed: Vec<String> = self
            .owned_ids
            .values()
            .filter(|id| !owned.values().any(|still| still == *id))
            .filter(|id| !(discovers_all && id.starts_with("copilot-")))
            .cloned()
            .collect();
        // An account configured again is no longer forgotten.
        self.forgotten.retain(|id, _| !owned.values().any(|still| still == id));
        self.owned_ids = owned;
        if removed.is_empty() {
            return;
        }
        self.forgotten.extend(removed.iter().map(|id| (id.clone(), false)));
        self.delete_forgotten_history(cx);
        crate::prefs_hub::PrefsHub::forget_accounts(cx, &removed);
        for id in &removed {
            self.states.remove(id);
            self.placeholders.remove(id);
        }
        // Its failure row would otherwise stay, with a Retry that finds no adapter.
        self.failures.retain(|failure| {
            failure
                .account
                .as_ref()
                .is_none_or(|account| !removed.contains(account))
        });
        let accounts: Vec<AccountSnapshot> = self
            .accounts
            .iter()
            .filter(|account| !removed.iter().any(|id| id == account.id().as_str()))
            .cloned()
            .collect();
        self.set_accounts(accounts, cx);
        self.save_snapshots(cx);
    }

    /// Deletes removed accounts' history that isn't deleted on disk yet (a failed rewrite) or that a refresh started
    /// before the removal wrote back. A failure is shown in Settings and retried on the next refresh.
    fn delete_forgotten_history(&mut self, cx: &mut Context<Self>) {
        let DataSource::Live { history, .. } = &self.source else {
            return;
        };
        let mut failed = None;
        {
            let mut store = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for (id, deleted) in self.forgotten.iter_mut().filter(|(_, deleted)| !**deleted) {
                match store.remove_account(id) {
                    Ok(()) => *deleted = true,
                    Err(err) => failed = Some(err),
                }
            }
        }
        if let Some(err) = failed {
            SettingsHub::set_error(
                cx,
                Some(
                    format!("A removed account's usage history couldn't be deleted yet: {err}. CodexBar tries again.")
                        .into(),
                ),
            );
        }
    }

    /// Smart order ranks by time-dependent signals (projections, resets), so it is rechecked each minute; the table is
    /// replaced only when the order actually changed.
    fn rerank_if_stale(&mut self, cx: &mut Context<Self>) {
        if self.layout.mode() != codexbar_core::layout::OrderMode::Smart {
            return;
        }
        let urgency = self.urgency_of(&self.accounts, cx);
        let ranked = arrange(self.accounts.clone(), &self.layout, |id| {
            urgency.get(id).copied().unwrap_or_default()
        });
        if ranked
            .iter()
            .map(AccountSnapshot::id)
            .ne(self.accounts.iter().map(AccountSnapshot::id))
        {
            self.set_accounts(ranked, cx);
        }
    }

    /// Rearranges the table when the layout changed, or, in Smart order, when what it ranks by changed.
    fn rearrange_if_changed(&mut self, cx: &mut Context<Self>) {
        let layout = crate::prefs_hub::PrefsHub::layout(cx);
        let inputs = alert_inputs(cx);
        let smart = layout.mode() == codexbar_core::layout::OrderMode::Smart;
        if layout != self.layout || (smart && inputs != self.ranked_with) {
            self.layout = layout;
            let accounts = self.accounts.clone();
            self.set_accounts(accounts, cx);
        }
    }

    /// Synthetic plan transitions for screenshot qualification; unavailable in normal builds and real accounts.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_demo_plan(&mut self, account_id: &str, plan: &str, cx: &mut Context<Self>) {
        if !crate::is_demo() {
            return;
        }
        let accounts = self
            .accounts
            .iter()
            .map(|account| {
                if account.id().as_str() == account_id {
                    account.clone().with_plan(plan)
                } else {
                    account.clone()
                }
            })
            .collect();
        self.set_accounts(accounts, cx);
    }

    /// Exercises first-fetch status on a synthetic account for rendered plan-note qualification.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_demo_plan_state(&mut self, state: AccountState, cx: &mut Context<Self>) {
        if !crate::is_demo() {
            return;
        }
        let id = "cursor";
        let mut accounts = self.accounts.clone();
        let Some(account) = accounts.iter_mut().find(|account| account.id().as_str() == id) else {
            return;
        };
        if matches!(state, AccountState::Fresh) {
            *account = codexbar_core::demo::demo_accounts(self.now, &chrono::Local)
                .into_iter()
                .find(|account| account.id().as_str() == id)
                .unwrap();
            self.placeholders.remove(id);
        } else {
            *account = AccountSnapshot::new(account.id().clone(), account.provider(), Vec::new(), self.now);
            self.placeholders.insert(id.to_owned());
        }
        self.states.insert(id.to_owned(), state);
        self.set_accounts(accounts, cx);
    }

    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn capture_sus_phase(&mut self, phase: u8, base: DateTime<Utc>, cx: &mut Context<Self>) {
        assert!(matches!(self.source, DataSource::Demo));
        let now = if phase == 1 { base + Duration::seconds(2) } else { base };
        crate::sus::capture_time(now, cx);
        self.states.clear();
        self.placeholders.clear();
        self.selected = Some(codexbar_core::AccountId::new("gemini-personal"));
        self.set_accounts(crate::sus::fixture(base, phase), cx);
        self.now = now;
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_now(now);
            table.refresh(cx);
        });
        self.selected = Some(codexbar_core::AccountId::new("gemini-personal"));
        self.view = DashboardView::Usage;
        cx.notify();
    }

    /// A synthetic Claude usage payload through the real parser, `age` into a week that is 8% used, for the early
    /// and firm SUS states without live accounts.
    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn capture_claude_weekly(&mut self, age: Duration, cx: &mut Context<Self>) {
        use codexbar_core::{AccountId, Provider};
        assert!(matches!(self.source, DataSource::Demo));
        let start: DateTime<Utc> = "2026-10-26T09:00:00Z".parse().unwrap();
        let now = start + age;
        crate::sus::capture_time(now, cx);
        let session = (now + Duration::hours(3)).to_rfc3339();
        let week = (start + Duration::days(7)).to_rfc3339();
        let payload = format!(
            r#"{{"five_hour":{{"utilization":12,"resets_at":"{session}"}},"seven_day":{{"utilization":8,"resets_at":"{week}"}}}}"#
        );
        let parsed = codexbar_providers::claude::parse_usage(&payload, None, now).unwrap();
        let account = AccountSnapshot::new(
            AccountId::new("claude"),
            Provider::Claude,
            parsed.metrics().to_vec(),
            now,
        )
        .with_plan("Max 20x")
        .with_trend(vec![0.02, 0.05, 0.08]);
        self.states.clear();
        self.placeholders.clear();
        self.set_accounts(vec![account], cx);
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "claude", |choice| {
            choice.rings = vec!["weekly".into()];
        });
        self.focus_account(
            &crate::handoff::FocusRequest {
                account: "claude".into(),
                metric: Some("weekly".into()),
            },
            cx,
        );
        self.view = DashboardView::Usage;
        cx.notify();
    }

    /// Synthetic CLI payload through the real parser, for age-boundary rendering without live accounts.
    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn capture_gemini_weekly(&mut self, mature: bool, cx: &mut Context<Self>) {
        use codexbar_core::{AccountDetail, AccountId, LimitSeries, Metric, Provider};
        assert!(matches!(self.source, DataSource::Demo));
        let base: DateTime<Utc> = "2026-10-23T16:38:00Z".parse().unwrap();
        let now = if mature { base } else { base - Duration::seconds(1) };
        crate::sus::capture_time(now, cx);
        let mut metrics = vec![
            Metric::Window {
                label: "Gemini Apps five-hour".into(),
                used: 0.18,
                resets_at: base + Duration::hours(3),
                pace: Some(codexbar_core::Pace::per_hour(0.09)),
                length: Some(Duration::hours(5)),
            },
            Metric::Window {
                label: "Gemini Apps weekly".into(),
                used: 0.65,
                resets_at: base + Duration::days(2),
                pace: None,
                length: Some(Duration::days(7)),
            },
        ];
        metrics.extend(
            codexbar_providers::gemini_coding::parse(
                include_str!("../../codexbar-providers/fixtures/antigravity-weekly-age.json"),
                now,
            )
            .unwrap()
            .metrics,
        );
        let account = AccountSnapshot::new(AccountId::new("gemini"), Provider::Gemini, metrics, now)
            .with_plan("Google AI Pro")
            .with_trend(vec![0.10, 0.18])
            .with_detail(AccountDetail::default().with_limit_series(
                "gemini-models-weekly",
                LimitSeries {
                    curve: None,
                    trend: vec![0.01, 0.02, 0.03],
                },
            ));
        self.states.clear();
        self.placeholders.clear();
        self.set_accounts(vec![account], cx);
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "gemini", |choice| {
            choice.rings = vec!["gemini-models-weekly".into()];
        });
        self.focus_account(
            &crate::handoff::FocusRequest {
                account: "gemini".into(),
                metric: Some("gemini-models-weekly".into()),
            },
            cx,
        );
        self.view = DashboardView::Usage;
        cx.notify();
    }

    /// Read the same sanitized saved quota shape seen on home, without live provider access or user state.
    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn capture_gemini_status(&mut self, present: bool, cx: &mut Context<Self>) {
        use codexbar_core::{AccountDetail, LimitSeries};
        assert!(matches!(self.source, DataSource::Demo));
        let mut fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/gemini-home-snapshot-20261023.json")).unwrap();
        if !present {
            fixture["accounts"][0]["metrics"]
                .as_array_mut()
                .unwrap()
                .retain(|m| m["label"] == "Argon weekly");
        }
        let dir = SettingsHub::global(cx).dir().join("gemini-status-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("snapshots.json"), serde_json::to_vec(&fixture).unwrap()).unwrap();
        let account = codexbar_store::snapshots::load_snapshots(&dir).remove(0);
        let now = account.fetched_at();
        let account = account
            .with_detail(AccountDetail::default().with_limit_series(
                "gemini-models-weekly",
                LimitSeries {
                    curve: None,
                    trend: vec![0.01, 0.02, 0.027857840061187633],
                },
            ))
            .with_trend(vec![0.3, 0.3131752014160156]);
        crate::sus::capture_time(now, cx);
        self.states.clear();
        self.placeholders.clear();
        self.set_accounts(vec![account], cx);
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "gemini", |choice| {
            choice.rings = vec![
                if present {
                    "gemini-models-weekly"
                } else {
                    "argon-weekly"
                }
                .into(),
            ];
        });
        self.focus_account(
            &crate::handoff::FocusRequest {
                account: "gemini".into(),
                metric: None,
            },
            cx,
        );
        self.view = DashboardView::Usage;
        cx.notify();
    }

    /// The production Muse adapter with a synthetic command runner. No live provider is constructed.
    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn capture_muse(&mut self, state: &str, cx: &mut Context<Self>) {
        use codexbar_providers::UsageProvider as _;
        assert!(matches!(self.source, DataSource::Demo));
        #[cfg(any(windows, feature = "capture"))]
        crate::prefs_hub::PrefsHub::update_widget_builder(cx, |builder| {
            builder.tiles = vec![
                codexbar_store::widgets::WidgetTile {
                    account: "muse-demo".into(),
                    metric: "5-hour-window".into(),
                    mode: codexbar_store::widgets::TileMode::Automatic,
                },
                codexbar_store::widgets::WidgetTile {
                    account: "muse-demo".into(),
                    metric: "weekly".into(),
                    mode: codexbar_store::widgets::TileMode::Automatic,
                },
            ];
        });
        if state == "cached-alert" {
            crate::prefs_hub::PrefsHub::update_alert_settings(cx, |settings| settings.enabled = true);
        }
        let now = crate::muse_fixture::now(state);
        self.last_refresh = Some(now);
        crate::sus::capture_time(now, cx);
        let result = if state == "cached-alert" {
            Ok(vec![crate::muse_fixture::cached_alert()])
        } else if state == "reset-after" {
            crate::muse_fixture::provider("reset-before").fetch(crate::muse_fixture::now("reset-before"))
        } else {
            crate::muse_fixture::provider(state).fetch(now)
        };
        self.states.clear();
        self.placeholders.clear();
        let account = match result {
            Ok(mut accounts) => {
                let account = accounts.remove(0);
                if account.observation_stale(now) {
                    self.states.insert(
                        "muse-demo".into(),
                        AccountState::Failed("Muse source observation is stale".into()),
                    );
                }
                account
            }
            Err(error) if state == "timeout" => {
                self.states
                    .insert("muse-demo".into(), AccountState::Failed(error.to_string()));
                crate::muse_fixture::provider("measured")
                    .fetch(crate::muse_fixture::now("measured"))
                    .unwrap()
                    .remove(0)
            }
            Err(error) => {
                self.states
                    .insert("muse-demo".into(), AccountState::Unavailable(error.to_string()));
                self.placeholders.insert("muse-demo".into());
                AccountSnapshot::new(
                    codexbar_core::AccountId::new("muse-demo"),
                    Provider::MuseCode,
                    Vec::new(),
                    now,
                )
                .with_label("Demo")
            }
        };
        self.set_accounts(vec![account], cx);
        #[cfg(any(windows, feature = "capture"))]
        {
            let snapshot = crate::widget_feed::build(&self.accounts, &self.states, &self.layout, now)
                .with_builder(crate::prefs_hub::PrefsHub::widget_builder(cx));
            cx.set_global(crate::widget_builder::WidgetFeed(snapshot));
        }

        self.focus_account(
            &crate::handoff::FocusRequest {
                account: "muse-demo".into(),
                metric: None,
            },
            cx,
        );
        self.view = DashboardView::Usage;
        cx.notify();
    }

    /// Keep geometry fixtures independent of the real weekly reset day.
    #[cfg(test)]
    pub(crate) fn set_alignment_demo(&mut self, cx: &mut Context<Self>) {
        let now = "2026-10-27T15:00:00Z".parse().unwrap();
        crate::sus::capture_time(now, cx);
        self.set_accounts(demo_accounts(now, &Utc), cx);
        cx.notify();
    }

    fn set_accounts(&mut self, mut accounts: Vec<AccountSnapshot>, cx: &mut Context<Self>) {
        self.ranked_with = alert_inputs(cx);
        self.now = crate::sus::now(cx);
        let urgency = self.urgency_of(&accounts, cx);
        accounts = arrange(accounts, &self.layout, |id| {
            urgency.get(id).copied().unwrap_or_default()
        });
        let selected_ix = self
            .selected
            .as_ref()
            .and_then(|id| accounts.iter().position(|a| a.id() == id))
            .unwrap_or(0);
        self.selected = accounts.get(selected_ix).map(|a| a.id().clone());
        self.accounts = accounts.clone();
        let now = self.now;
        self.compact_minute = now.timestamp() / 60;
        let compact = self.compact_history(&accounts);
        let states = self.states.clone();
        let groups: HashMap<String, String> = accounts
            .iter()
            .filter_map(|account| {
                let group = self.layout.group_of(account.id().as_str())?;
                Some((account.id().as_str().to_owned(), group.name.clone()))
            })
            .collect();
        let preferred = self.selected.clone();
        self.history_view.update(cx, |view, cx| {
            view.set_accounts(accounts.clone(), preferred.as_ref(), cx);
        });
        self.spend_view
            .update(cx, |view, cx| view.set_accounts(accounts.clone(), states.clone(), cx));
        // Rows drag to a new place (#178), and column borders resize (#245), unless the preferences can't be saved.
        let draggable = !crate::prefs_hub::PrefsHub::is_read_only(cx);
        self.table.update(cx, |table, cx| {
            table.col_resizable = draggable;
            *table.delegate_mut() = AccountTable::new(accounts, now, compact, states)
                .with_groups(groups)
                .draggable(draggable);
            table.refresh(cx);
            if table.delegate().row(selected_ix).is_some() {
                table.set_selected_row(selected_ix, cx);
            }
        });
        self.publish_tooltip(cx);
        #[cfg(windows)]
        self.save_widget_snapshot(cx);
        // A tap waits for its account only briefly, so it never takes over after the user has moved on.
        if self
            .pending_focus
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > PENDING_FOCUS_WAIT)
        {
            self.pending_focus = None;
        }
        if let Some((request, _)) = self.pending_focus.clone()
            && self
                .accounts
                .iter()
                .any(|account| account.id().as_str() == request.account)
        {
            self.focus_account(&request, cx);
        }
        cx.notify();
    }

    /// Writes what the Windows widgets show (#94), for live data only. A failed write only leaves the widgets on the
    /// previous snapshot until the next one.
    #[cfg(windows)]
    fn save_widget_snapshot(&mut self, cx: &mut Context<Self>) {
        self.widget_builder = crate::prefs_hub::PrefsHub::widget_builder(cx);
        let snapshot = crate::widget_feed::build(&self.accounts, &self.states, &self.layout, self.now)
            .with_builder(self.widget_builder.clone());
        self.widgets_written = Some(self.now);
        // The builder's preview reads it (#95), in the demo too; only live data goes to the widgets.
        if matches!(self.source, DataSource::Live { .. }) {
            let _ = codexbar_store::widgets::save_widget_snapshot(SettingsHub::global(cx).dir(), &snapshot);
        }
        cx.set_global(crate::widget_builder::WidgetFeed(snapshot));
    }

    /// Recomputes the table's compact history for the current minute, keeping its rows and selection.
    fn update_compact_history(&mut self, cx: &mut Context<Self>) {
        self.compact_minute = self.now.timestamp() / 60;
        let compact = self.compact_history(&self.accounts);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_compact(compact);
            cx.notify();
        });
    }

    /// The compact history for the metric each account's table row shows (#246) over the trend's 14 days (#86).
    fn compact_history(&self, accounts: &[AccountSnapshot]) -> Compact {
        let since = self.now - Duration::days(TREND_DAYS as i64);
        let history = self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        accounts
            .iter()
            .filter_map(|account| {
                let metric = crate::account_table::row_metric_at(account, self.now)?;
                let points = history.points(account.id().as_str(), &metric.key(), since, self.now);
                let summary = summarize(&points, self.now, GAP_THRESHOLD)?;
                Some((account.id().as_str().to_owned(), (summary, ValueKind::of(metric))))
            })
            .collect()
    }

    /// Sends the tray hover text when it changed. Also called once the tray icon exists, since restored accounts are
    /// shown before it is created.
    pub fn publish_tooltip(&mut self, cx: &mut Context<Self>) {
        let text = self.tooltip();
        if self.published_tooltip.as_deref() != Some(text.as_str()) || !crate::tray::has_tooltip_target(cx) {
            crate::tray::set_tooltip(cx, &text);
            self.published_tooltip = crate::tray::has_tooltip_target(cx).then_some(text);
        }
    }

    /// The tray hover text: the most urgent account, or the reason there is none.
    fn tooltip(&self) -> String {
        // The most urgent account, whatever the table's manual order.
        let mut ranked = self.accounts.clone();
        codexbar_core::sort_by_urgency(&mut ranked, self.now);
        let Some(top) = ranked.first() else {
            return match self.failures.first() {
                Some(failure) => format!("CodexBar: {} - {}", failure.name(), failure.message),
                None => "CodexBar".to_owned(),
            };
        };
        let metric = top.primary();
        let used = metric
            .and_then(Metric::used_fraction)
            .map(|u| format!(" {:.0}%", u * 100.0))
            .unwrap_or_default();
        let reset = metric
            .and_then(Metric::resets_at)
            .map(|at| format!(", resets {}", format::time_left_label(at, self.now)))
            .unwrap_or_default();
        // Restored or stale usage says so, so an old figure isn't read as current.
        let freshness = match self.states.get(top.id().as_str()) {
            Some(AccountState::Restored | AccountState::Failed(_)) => {
                format!(" (last known, {})", format::age_label(top.fetched_at(), self.now))
            }
            Some(AccountState::Unavailable(_)) => " (unavailable)".to_owned(),
            _ => String::new(),
        };
        let scores = top
            .status_metrics()
            .filter_map(|m| codexbar_core::sus::text(m, self.now))
            .collect::<Vec<_>>()
            .join(", ");
        let score = if scores.is_empty() {
            String::new()
        } else {
            format!(", {scores}")
        };
        format!("CodexBar - {}{used}{reset}{score}{freshness}", top.display_name())
    }

    fn focused(&self) -> Option<&AccountSnapshot> {
        let id = self.selected.as_ref()?;
        self.accounts.iter().find(|a| a.id() == id)
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let updated = match (self.loading, self.last_refresh) {
            (true, None) => "Loading…".to_owned(),
            (_, Some(at)) => format!("Updated {}", format::age_label(at, self.now)),
            (false, None) => String::new(),
        };
        let selected = DashboardView::ALL.iter().position(|v| *v == self.view).unwrap_or(0);

        TitleBar::new()
            // The title bar is a fixed 34px; let it grow with zoom so the tabs aren't clipped (#116).
            .h(crate::zoom::scaled(34., cx).max(px(34.)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::LayoutDashboard).text_color(cx.theme().primary))
                    .child(div().font_semibold().child("CodexBar"))
                    // Which build is running, at a glance (#244).
                    .child(
                        div()
                            .id("app-version")
                            .test_support()
                            .aria_label(crate::version::title())
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::version::current()),
                    )
                    // The view tabs switch dashboard views; Settings has its own navigation.
                    .when(self.view != DashboardView::Settings, |this| {
                        this.child(
                            div()
                                .pl_6()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                // gpui-kit's TabBar heights stop at 44px, which clips labels past ~250% zoom, so these
                                // underline tabs are sized in rems and follow the zoom continuously (#116).
                                .child(
                                    h_flex()
                                        .id("view-tabs")
                                        .role(Role::TabList)
                                        .gap(rems(1.25))
                                        .text_sm()
                                        .children(DashboardView::ALL.into_iter().enumerate().map(|(ix, view)| {
                                            let active = ix == selected;
                                            let ring = cx.theme().ring;
                                            div()
                                                .id(("view-tab", ix))
                                                // Same semantics as gpui-kit's Tab: announced as a selected or unselected
                                                // tab. Tab reaches the selected tab only (a roving tab stop), arrow keys move
                                                // between tabs, and a focused div turns Enter/Space into a click.
                                                .role(Role::Tab)
                                                // Lets headless UI tests find each tab (inert outside tests).
                                                .test_support()
                                                .aria_selected(active)
                                                // A tracked handle carries its own tab stop; the div's `tab_index` and
                                                // `tab_stop` only apply to handles the div creates itself.
                                                .track_focus(
                                                    &self.view_tab_focus[ix].clone().tab_index(0).tab_stop(active),
                                                )
                                                .on_key_down(cx.listener(
                                                    move |this, event: &KeyDownEvent, window, cx| {
                                                        // Modified arrows (Ctrl, Alt, Shift, Win) keep their own meaning.
                                                        if event.keystroke.modifiers.modified() {
                                                            return;
                                                        }
                                                        let count = DashboardView::ALL.len();
                                                        let next = match event.keystroke.key.as_str() {
                                                            "right" => (ix + 1) % count,
                                                            "left" => (ix + count - 1) % count,
                                                            "home" => 0,
                                                            "end" => count - 1,
                                                            _ => return,
                                                        };
                                                        cx.stop_propagation();
                                                        this.view = DashboardView::ALL[next];
                                                        window.focus(&this.view_tab_focus[next], cx);
                                                        cx.notify();
                                                    },
                                                ))
                                                .focus_visible(move |style| style.text_color(ring).border_color(ring))
                                                .py(rems(0.125))
                                                .border_b_2()
                                                .border_color(if active {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().transparent
                                                })
                                                .text_color(if active {
                                                    cx.theme().tab_active_foreground
                                                } else {
                                                    cx.theme().tab_foreground
                                                })
                                                .cursor_pointer()
                                                .hover(|style| style.text_color(cx.theme().tab_active_foreground))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.view = view;
                                                    cx.notify();
                                                }))
                                                .child(view.title())
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .pr_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child({
                        let fleet = crate::sus::fleet(&self.accounts, &self.states, &self.owned_ids, self.now, cx);
                        gpui_kit::component::h_flex()
                            .gap_1()
                            .child(div().text_xs().child("Fleet"))
                            .child(crate::sus::badge(
                                fleet.score.map(codexbar_core::sus::Reading::Firm),
                                "fleet-sus",
                                "Fleet SUS",
                                cx,
                            ))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                                if fleet.subscriptions == 0 {
                                    "no eligible subscriptions".to_owned()
                                } else {
                                    format!(
                                        "{} · {} subscriptions",
                                        if fleet.equal_weights {
                                            "equal weights"
                                        } else {
                                            "price weighted"
                                        },
                                        fleet.subscriptions
                                    )
                                },
                            ))
                    })
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(updated))
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip("Refresh now (F5)")
                            .loading(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = DashboardView::ALL.map(|view| {
            SidebarMenuItem::new(view.title())
                .icon(view.icon())
                .active(self.view == view)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.view = view;
                    cx.notify();
                }))
        });
        Sidebar::new("rail")
            .w(rems(13.5))
            .header(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .border_1()
                    .border_color(cx.theme().input)
                    .rounded(cx.theme().radius)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::Search).small())
                    .child(div().flex_1().child("Search accounts…"))
                    .child(
                        div()
                            .text_xs()
                            .child(if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl+K" }),
                    ),
            )
            .child(
                SidebarMenu::new().children(items).child(
                    SidebarMenuItem::new("Settings")
                        .icon(IconName::Settings)
                        .active(self.view == DashboardView::Settings)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.view = DashboardView::Settings;
                            cx.notify();
                        })),
                ),
            )
    }

    /// The focused account's group and its place in the manual order (#89): a group menu, and Move up / Move down
    /// within the group.
    fn group_controls(&self, account: &AccountSnapshot, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::Disableable as _;
        use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
        let id = account.id().as_str().to_owned();
        let ids: Vec<String> = self.accounts.iter().map(|a| a.id().as_str().to_owned()).collect();
        let section = self
            .layout
            .arrange(&ids)
            .into_iter()
            .find(|section| section.accounts.contains(&id));
        let (ix, len) = section
            .as_ref()
            .map(|section| {
                let ix = section.accounts.iter().position(|a| *a == id).unwrap_or_default();
                (ix, section.accounts.len())
            })
            .unwrap_or((0, 1));
        let current = self.layout.group_of(&id).map(|group| group.name.clone());
        let groups = self.layout.groups().to_vec();
        // A `pending:` placeholder takes the account's real id once its first fetch succeeds, so layout edits made
        // under it would be lost; they wait for the real account.
        let read_only = crate::prefs_hub::PrefsHub::is_read_only(cx) || id.starts_with(PLACEHOLDER_PREFIX);
        let error = crate::prefs_hub::PrefsHub::error(cx);
        let menu_id = id.clone();
        let smart = self.layout.mode() == codexbar_core::layout::OrderMode::Smart;
        let mover = |delta: isize, label: &'static str, disabled: bool| {
            let id = id.clone();
            let ids = ids.clone();
            Button::new(SharedString::from(if delta < 0 { "move-up" } else { "move-down" }))
                .small()
                .ghost()
                .label(label)
                // Smart order ranks by urgency, so a manual move wouldn't show; the manual order waits for Manual.
                .disabled(disabled || read_only || smart)
                .on_click(move |_, _, cx| {
                    let _ = crate::prefs_hub::PrefsHub::update_layout(cx, |layout| {
                        Ok::<_, codexbar_core::layout::LayoutError>(layout.move_account(&id, delta, &ids))
                    });
                })
        };
        h_flex()
            .gap_1()
            .items_center()
            .text_sm()
            .child(
                Button::new("group-menu")
                    .small()
                    .outline()
                    .label(format!(
                        "Group: {}",
                        current
                            .clone()
                            .unwrap_or_else(|| codexbar_core::layout::UNGROUPED.into())
                    ))
                    .disabled(read_only)
                    .dropdown_menu(move |menu, _, _| {
                        let assign = |group: Option<String>| {
                            let account = menu_id.clone();
                            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                                let _ = crate::prefs_hub::PrefsHub::update_layout(cx, |layout| {
                                    layout.assign(&account, group.as_deref())
                                });
                            }
                        };
                        let mut menu = menu.item(
                            PopupMenuItem::new(codexbar_core::layout::UNGROUPED)
                                .checked(current.is_none())
                                .on_click(assign(None)),
                        );
                        for group in &groups {
                            menu = menu.item(
                                PopupMenuItem::new(group.name.clone())
                                    .checked(current.as_deref() == Some(group.name.as_str()))
                                    .on_click(assign(Some(group.id.clone()))),
                            );
                        }
                        if groups.is_empty() {
                            menu = menu.item(PopupMenuItem::label("Create groups in Settings → Groups"));
                        }
                        menu
                    }),
            )
            .child(mover(-1, "Move up", ix == 0))
            .child(mover(1, "Move down", ix + 1 >= len))
            .children(error.map(|error| {
                div()
                    .id("layout-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error.clone())
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }

    /// Smart or Manual order for the table (#90). Saved with the layout; the manual order is kept either way.
    fn order_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use codexbar_core::layout::OrderMode;
        use gpui_kit::component::button::ButtonGroup;
        use gpui_kit::component::{Disableable as _, Selectable as _};
        const MODES: [OrderMode; 2] = [OrderMode::Smart, OrderMode::Manual];
        let mode = self.layout.mode();
        let read_only = crate::prefs_hub::PrefsHub::is_read_only(cx);
        h_flex()
            .gap_2()
            .items_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Order")
            .child(
                ButtonGroup::new("order-mode")
                    .outline()
                    .small()
                    // A newer CodexBar's dashboard.json can't be saved; like the group controls, wait for it.
                    .children(MODES.iter().enumerate().map(|(ix, option)| {
                        Button::new(("order-mode", ix))
                            .label(option.label())
                            .selected(*option == mode)
                            .disabled(read_only)
                    }))
                    .on_click(cx.listener(|_, clicks: &Vec<usize>, _, cx| {
                        if let Some(mode) = clicks.first().and_then(|ix| MODES.get(*ix)) {
                            let mode = *mode;
                            let _ = crate::prefs_hub::PrefsHub::update_layout(cx, |layout| {
                                layout.set_mode(mode);
                                Ok::<_, codexbar_core::layout::LayoutError>(())
                            });
                        }
                    })),
            )
            .child(match mode {
                OrderMode::Smart => "Most urgent first within each group; drag a row to switch to your own order",
                OrderMode::Manual => "Your order; drag a row, or use Move up and Move down on an account",
            })
    }

    fn render_usage(&self, ring: &crate::ring_size::RingControl, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let focused = self.focused();
        let cards = focused
            .map(|account| focus_cards(account, self.now, ring, cx))
            .unwrap_or_default();
        let heading = focused.map(|account| {
            // Alerts that hold now (#88), from the current settings and the delivered set, so they stay visible
            // after their notification was sent or deduplicated.
            let details = codexbar_core::alerts::account_alerts(
                &crate::prefs_hub::PrefsHub::alert_settings(cx),
                &crate::notifications::Notifications::active(cx),
                account,
                self.now,
            );
            // A held alert (say a 50% usage alert) is at least worth watching, so the status agrees with the block.
            let assessment = account.assess(self.now);
            let severity = assessment.severity();
            let severity = if details.is_empty() {
                severity
            } else {
                severity.max(codexbar_core::Severity::Watch)
            };
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(crate::brand::badge(account.provider(), "focused", cx))
                        .child(div().text_xl().font_semibold().child(account.display_name()))
                        .child(div().size_2().rounded_full().bg(severity_dot_color(severity, cx)))
                        .children(severity_tag(severity).filter(|_| !assessment.is_exhausted()))
                        .children(crate::alert_details::projected_tag(account, self.now)),
                )
                .child(self.group_controls(account, cx))
                .children(crate::alert_details::alert_details(&details, self.now, cx))
                .children(crate::focus_cards::subscription_note(account, self.now, cx))
                .children(
                    (account.provider() == Provider::MuseCode)
                        .then(|| self.states.get(account.id().as_str()))
                        .flatten()
                        .and_then(|state| match state {
                            AccountState::Failed(error) => Some(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().warning)
                                    .child(format!("Last known allowance. {error}")),
                            ),
                            AccountState::Unavailable(error) => {
                                Some(div().text_sm().text_color(cx.theme().warning).child(error.clone()))
                            }
                            _ => None,
                        }),
                )
                .children(crate::focus_cards::metric_list(account, self.now, cx))
                // What the provider said besides numbers (#75), kept with last-good usage.
                .children(
                    (account.plan().is_none() && !self.placeholders.contains(account.id().as_str())).then(|| {
                        div()
                            .id("plan-unavailable")
                            .test_support()
                            .aria_label("The connected usage source does not report a subscription plan.")
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("The connected usage source does not report a subscription plan.")
                    }),
                )
                .children(account.messages().iter().enumerate().map(|(ix, message)| {
                    div()
                        .id(("provider-message", ix))
                        .role(Role::Note)
                        .test_support()
                        .aria_label(message.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone())
                }))
        });

        let failures = self.failures.iter().map(|failure| {
            h_flex()
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .small()
                        .text_color(cx.theme().warning),
                )
                .child(div().font_semibold().child(failure.name()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(failure.message.clone()),
                )
                .when(failure.sign_in, |this| {
                    let provider = failure.provider;
                    let account = failure.account.clone();
                    this.child(
                        Button::new(SharedString::from(format!("fix-{}", failure.element_suffix())))
                            .label("Open settings")
                            .accessibility_label(format!("Open settings for {}", failure.name()))
                            .small()
                            .outline()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_settings_for(provider, account.clone(), window, cx)
                            })),
                    )
                })
                .child({
                    let provider = failure.provider;
                    let account = failure.account.clone();
                    Button::new(SharedString::from(format!("retry-{}", failure.element_suffix())))
                        .label("Retry")
                        .accessibility_label(format!("Retry {}", failure.name()))
                        .small()
                        .outline()
                        .loading(self.loading)
                        .on_click(cx.listener(move |this, _, _, cx| this.retry(provider, account.clone(), cx)))
                })
        });
        if self.accounts.is_empty() {
            return v_flex()
                .flex_1()
                .gap_3()
                .children(failures)
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(IconName::Inbox).large())
                        .child(if self.loading {
                            "Fetching usage…"
                        } else {
                            "No accounts connected yet"
                        }),
                )
                .into_any_element();
        }

        v_flex()
            .min_h_full()
            .gap_4()
            .children(failures)
            .child(self.order_toggle(cx))
            .child(
                div()
                    .h(crate::zoom::scaled(TABLE_ROW_HEIGHT * (self.accounts.len() + 1) as f32, cx) + px(2.))
                    .flex_shrink_0()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(
                        DataTable::new(&self.table)
                            .with_size(Size::Size(crate::zoom::scaled(TABLE_ROW_HEIGHT, cx)))
                            .bordered(false)
                            .stripe(false)
                            // Zoomed in, the columns can outgrow the window; keep them reachable.
                            .scrollbar_visible(false, true),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h(rems(20.))
                    .gap_3()
                    .p_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .children(heading)
                    .child(h_flex().flex_1().min_h_0().items_stretch().gap_4().children(cards)),
            )
            .into_any_element()
    }

    fn render_status_line(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let attention = self
            .accounts
            .iter()
            .filter(|a| a.assess(self.now).severity().needs_attention())
            .count();
        let interval = SettingsHub::global(cx).settings().refresh_interval_secs();
        let next_refresh = match (self.last_refresh, interval) {
            (Some(at), Some(secs)) => {
                format!(
                    "next refresh {}",
                    format::clock_countdown(at + Duration::seconds(secs as i64) - self.now)
                )
            }
            (_, None) => "auto refresh off".to_owned(),
            (None, Some(_)) => "refreshing".to_owned(),
        };
        h_flex()
            .gap_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(match self.accounts.len() {
                1 => "1 account".to_owned(),
                n => format!("{n} accounts"),
            })
            .child("·")
            .child(
                div()
                    .when(attention > 0, |this| this.text_color(cx.theme().warning))
                    .child(format!("{attention} need attention")),
            )
            .child("·")
            .child(next_refresh)
    }
}

use gpui_kit::prelude::FluentBuilder as _;

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // DataTable caches column widths; refresh them when the zoom changes.
        let zoom = crate::zoom::level(cx);
        if (zoom - self.table_zoom).abs() > f64::EPSILON {
            self.table_zoom = zoom;
            self.table.update(cx, |table, cx| table.refresh(cx));
        }
        // Both standard windows side by side when the table has room for them (#195): the window's width less the
        // rail and the content padding, against the wide columns' total, all following the zoom.
        let table_width = window.viewport_size().width - crate::zoom::scaled(RAIL_AND_PADDING_WIDTH, cx);
        let wide = table_width >= crate::zoom::scaled(crate::account_table::WIDE_TABLE_WIDTH, cx);
        if self.table.read(cx).delegate().is_wide() != wide {
            self.table.update(cx, |table, cx| {
                table.delegate_mut().set_wide(wide);
                table.refresh(cx);
            });
        }
        // The ring size follows a change saved elsewhere, such as Default ring size in the gauge card's menu (#219).
        let saved_ring_size = crate::prefs_hub::PrefsHub::ring_size(cx);
        if saved_ring_size != self.ring_size_seen {
            self.ring_size_seen = saved_ring_size;
            let size = saved_ring_size.unwrap_or(crate::ring_size::DEFAULT);
            self.ring_slider
                .update(cx, |slider, cx| slider.set_value(size, window, cx));
        }
        let ring = crate::ring_size::RingControl {
            state: self.ring_slider.clone(),
            focus: self.ring_focus.clone(),
            revealed: self.ring_focus.is_focused(window),
        };
        let body = match self.view {
            DashboardView::Usage => self.render_usage(&ring, cx),
            DashboardView::Settings => {
                crate::settings_view::render(self.settings_start.take(), self.settings_generation, window, cx)
                    .into_any_element()
            }
            DashboardView::History => self.history_view.clone().into_any_element(),
            DashboardView::Spend => self.spend_view.clone().into_any_element(),
        };
        v_flex()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|_, _: &LeaveTableForward, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &LeaveTableBackward, window, cx| window.focus_prev(cx)))
            // Ctrl+wheel zoom is caught in the capture phase, before the table or settings list can scroll (#116).
            .child(
                canvas(|_, _, _| {}, |_, _, window, _| crate::zoom::capture_wheel(window))
                    .absolute()
                    .size_full(),
            )
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.render_rail(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .p_4()
                            .gap_4()
                            // Zoomed in, the dashboard scrolls instead of squeezing its cards (#116).
                            .child(
                                div()
                                    .id("main-scroll")
                                    // Registered so headless captures can scroll the zoomed dashboard (#200).
                                    .test_support()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(body),
                            )
                            .child(self.render_status_line(cx)),
                    ),
            )
    }
}

fn alert_inputs(cx: &gpui_kit::App) -> (codexbar_core::alerts::AlertSettings, std::collections::BTreeSet<String>) {
    (
        crate::prefs_hub::PrefsHub::alert_settings(cx),
        crate::notifications::Notifications::active(cx),
    )
}

/// The default order: provider order, then account id, so it doesn't depend on which fetch finished first.
fn default_order(accounts: &mut [AccountSnapshot]) {
    accounts.sort_by_key(|account| {
        let provider = Provider::ALL
            .iter()
            .position(|kind| *kind == account.provider())
            .unwrap_or(usize::MAX);
        (provider, account.id().as_str().to_owned())
    });
}

/// Orders accounts by group (#89): each group in its order, then Ungrouped, each in the manual order.
fn arrange(
    mut accounts: Vec<AccountSnapshot>,
    layout: &codexbar_core::layout::Layout,
    urgency: impl Fn(&str) -> codexbar_core::layout::Urgency,
) -> Vec<AccountSnapshot> {
    default_order(&mut accounts);
    let ids: Vec<String> = accounts
        .iter()
        .map(|account| account.id().as_str().to_owned())
        .collect();
    let mut by_id: HashMap<String, AccountSnapshot> = accounts
        .into_iter()
        .map(|account| (account.id().as_str().to_owned(), account))
        .collect();
    layout
        .arrange_by(&ids, urgency)
        .into_iter()
        .flat_map(|section| section.accounts)
        .filter_map(|id| by_id.remove(&id))
        .collect()
}
