//! The app-wide Rust-only preferences in `dashboard.json`: which accounts show history (#86), alert settings and
//! the alerts already notified (#87), account groups and the manual order (#89). Changes apply at once and save
//! under the shared lock.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use codexbar_core::AccountSnapshot;
use codexbar_core::alerts::AlertSettings;
use codexbar_core::layout::{Layout, LayoutError};
use codexbar_store::prefs::DashboardPrefs;
use gpui_kit::{App, BorrowAppContext as _, Global, SharedString};

pub struct PrefsHub {
    prefs: DashboardPrefs,
    /// Where `dashboard.json` lives; `None` keeps everything in memory (the demo, so trying alert settings there never
    /// changes the real app's).
    dir: Option<PathBuf>,
    error: Option<SharedString>,
}

impl Global for PrefsHub {}

impl PrefsHub {
    pub fn init(cx: &mut App, dir: &Path) {
        cx.set_global(Self {
            prefs: DashboardPrefs::load(dir),
            dir: Some(dir.to_owned()),
            error: None,
        });
    }

    /// Preferences that are never saved, starting from the defaults (the demo).
    pub fn init_in_memory(cx: &mut App) {
        cx.set_global(Self {
            prefs: DashboardPrefs::default(),
            dir: None,
            error: None,
        });
    }

    pub fn shows(cx: &App, account: &str) -> bool {
        cx.try_global::<Self>()
            .is_none_or(|hub| hub.prefs.shows_history(account))
    }

    /// Shows or hides an account's history and saves at once. Hiding never deletes stored samples.
    pub fn set(cx: &mut App, account: &str, show: bool) {
        Self::edit(cx, |prefs| prefs.set_shows_history(account, show));
        cx.refresh_windows();
    }

    /// Moves preferences saved under old account ids to the new ones, saving once if anything moved.
    pub fn rename_accounts(cx: &mut App, renames: &[(&str, String)]) {
        Self::edit(cx, |prefs| {
            for (from, to) in renames {
                prefs.rename_account(from, to);
            }
        });
    }

    /// Forgets removed accounts' preferences, held alerts and layout (#85), saving once if anything changed.
    pub fn forget_accounts(cx: &mut App, accounts: &[String]) {
        Self::edit(cx, |prefs| {
            for account in accounts {
                prefs.forget_account(account);
            }
        });
    }

    /// System, Light or Dark (#92); System until chosen or when the stored value is unknown.
    pub fn appearance(cx: &App) -> crate::theme::Appearance {
        cx.try_global::<Self>()
            .and_then(|hub| hub.prefs.appearance())
            .and_then(crate::theme::Appearance::from_key)
            .unwrap_or_default()
    }

    /// Saves the appearance and applies it at once.
    pub fn set_appearance(cx: &mut App, appearance: crate::theme::Appearance) {
        Self::edit(cx, |prefs| prefs.set_appearance(appearance.key()));
        crate::theme::apply(cx, appearance);
        cx.refresh_windows();
    }

    /// Where the dashboard window was last (#194), when a placement was saved.
    pub fn window_placement(cx: &App) -> Option<codexbar_store::prefs::WindowPlacement> {
        cx.try_global::<Self>()
            .and_then(|hub| hub.prefs.window_placement().cloned())
    }

    /// Remembers where the dashboard window is, saved at once.
    pub fn set_window_placement(cx: &mut App, placement: codexbar_store::prefs::WindowPlacement) {
        Self::edit(cx, |prefs| prefs.set_window_placement(placement));
    }

    pub fn alert_settings(cx: &App) -> AlertSettings {
        cx.try_global::<Self>()
            .map(|hub| hub.prefs.alert_settings().clone())
            .unwrap_or_default()
    }

    /// Changes alert settings with `change` and saves.
    /// Changes alert settings and saves. A changed usage or balance threshold clears that kind's active alerts, so
    /// they are judged against the new threshold rather than held by the old one.
    pub fn update_alert_settings(cx: &mut App, change: impl FnOnce(&mut AlertSettings)) {
        Self::edit(cx, |prefs| {
            let old = prefs.alert_settings().clone();
            let mut settings = old.clone();
            change(&mut settings);
            let mut stale = Vec::new();
            if settings.usage_threshold != old.usage_threshold {
                stale.push("|usage");
            }
            if settings.balance_threshold != old.balance_threshold {
                stale.push("|balance");
            }
            if !stale.is_empty() {
                let active = prefs
                    .active_alerts()
                    .iter()
                    .filter(|key| !stale.iter().any(|kind| key.ends_with(kind)))
                    .cloned()
                    .collect();
                prefs.set_active_alerts(active);
            }
            prefs.set_alert_settings(settings);
        });
        cx.refresh_windows();
    }

    pub fn active_alerts(cx: &App) -> BTreeSet<String> {
        cx.try_global::<Self>()
            .map(|hub| hub.prefs.active_alerts().clone())
            .unwrap_or_default()
    }

    /// Records the active alerts and saves. Also retries an earlier save that failed (the lock was busy), even
    /// when the set itself hasn't changed since.
    pub fn set_active_alerts(cx: &mut App, active: BTreeSet<String>) {
        let dirty = cx.try_global::<Self>().is_some_and(|hub| hub.prefs.is_dirty());
        if dirty || Self::active_alerts(cx) != active {
            Self::edit(cx, |prefs| prefs.set_active_alerts(active));
        }
    }

    /// Account groups and the manual order (#89).
    pub fn layout(cx: &App) -> Layout {
        cx.try_global::<Self>()
            .map(|hub| hub.prefs.layout().clone())
            .unwrap_or_default()
    }

    /// Changes the layout and saves when it changed; returns `change`'s result (a refused change saves nothing).
    pub fn update_layout<T>(
        cx: &mut App,
        change: impl FnOnce(&mut Layout) -> Result<T, LayoutError>,
    ) -> Result<T, LayoutError> {
        let mut result = Err(LayoutError::UnknownGroup);
        Self::edit(cx, |prefs| result = prefs.update_layout(change));
        cx.refresh_windows();
        result
    }

    /// The widget builder's tiles and refresh choice (#95).
    #[cfg(windows)]
    pub fn widget_builder(cx: &App) -> codexbar_store::widgets::WidgetBuilder {
        cx.try_global::<Self>()
            .map(|hub| hub.prefs.widget_builder().clone())
            .unwrap_or_default()
    }

    /// Changes the widget builder's choices and saves when they changed.
    #[cfg(windows)]
    pub fn update_widget_builder(cx: &mut App, change: impl FnOnce(&mut codexbar_store::widgets::WidgetBuilder)) {
        Self::edit(cx, |prefs| prefs.update_widget_builder(change));
        cx.refresh_windows();
    }

    /// Gives a fresh account's credits their starting balance (#150): remembers the balance just read (a first one,
    /// or a top-up, becomes the start) and saves when the start moved.
    pub fn with_credit_start(cx: &mut App, account: AccountSnapshot) -> AccountSnapshot {
        let Some(balance) = account.credit_balance() else {
            return account;
        };
        let mut start = balance;
        Self::edit(cx, |prefs| {
            start = prefs.remember_credit_balance(account.id().as_str(), balance);
        });
        account.with_credit_start(start)
    }

    /// True when `dashboard.json` is from a newer CodexBar: settings are read, but nothing can be saved.
    pub fn is_read_only(cx: &App) -> bool {
        cx.try_global::<Self>()
            .is_some_and(|hub| hub.dir.is_some() && hub.prefs.is_read_only())
    }

    /// The last save problem, for the Settings and History views.
    pub fn error(cx: &App) -> Option<SharedString> {
        cx.try_global::<Self>().and_then(|hub| hub.error.clone())
    }

    /// Applies a change in memory at once; a failed save keeps it pending for the next save and says so.
    fn edit(cx: &mut App, change: impl FnOnce(&mut DashboardPrefs)) {
        if cx.try_global::<Self>().is_none() {
            return;
        }
        cx.update_global(|hub: &mut Self, _| {
            change(&mut hub.prefs);
            if !hub.prefs.is_dirty() {
                return;
            }
            let Some(dir) = hub.dir.clone() else {
                return;
            };
            hub.error = hub
                .prefs
                .save(&dir)
                .err()
                .map(|err| format!("Setting not saved: {err}").into());
        });
    }
}
