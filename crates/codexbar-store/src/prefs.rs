//! Dashboard preferences that only the Rust app uses: which accounts show history (#86), alert settings and the
//! alerts currently active (#87), account groups with the manual card order (#89), the widget builder's tiles and
//! refresh choice (#95), and each account's remembered starting credit balance (#150).
//!
//! They live in `dashboard.json`, not `settings.json`: the WPF app reads `settings.json` into typed settings and writes
//! them back, so a key it doesn't know would be dropped on its next save. Keys this version doesn't know are kept, and
//! a file from a newer version is never overwritten.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use codexbar_core::alerts::AlertSettings;
use codexbar_core::layout::{Group, Layout, OrderMode};
use serde_json::{Map, Value, json};

use crate::widgets::WidgetBuilder;

pub const PREFS_FILE: &str = "dashboard.json";
const LOCK_FILE: &str = "dashboard.write.lock";
const VERSION: u64 = 1;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardPrefs {
    hidden_history: BTreeSet<String>,
    /// Changes not yet saved, by account: true shows history, false hides it. A save applies only these to the
    /// file's current contents, so another process's changes made meanwhile are kept.
    pending: BTreeMap<String, bool>,
    alerts: AlertSettings,
    /// Alert conditions already notified and not yet recovered, so a restart doesn't notify them again.
    active_alerts: BTreeSet<String>,
    /// Alert-setting fields changed and not yet saved, by JSON name. A save merges these into the file's `alerts`,
    /// so two processes editing different fields don't undo each other.
    pending_alert_fields: BTreeMap<&'static str, Value>,
    /// System, Light or Dark (#92), as its key; `None` until chosen, which means System.
    appearance: Option<String>,
    /// The appearance changed and isn't saved yet.
    appearance_dirty: bool,
    /// Active-alert changes not yet saved: true adds a key, false removes it. Merged like `pending`.
    pending_active: BTreeMap<String, bool>,
    /// Account groups and the manual order (#89).
    layout: Layout,
    /// The layout changed and isn't saved yet. A save replaces the file's layout with this one as a whole: group
    /// edits depend on each other (a membership needs its group), and only one CodexBar runs per user.
    layout_dirty: bool,
    /// The widget builder's choices (#95). Saved as a whole when changed, like the layout.
    widgets: WidgetBuilder,
    widgets_dirty: bool,
    /// Each account's starting credit balance (#150): the highest balance seen since the last top-up. A count of
    /// credits, never a secret.
    credit_starts: BTreeMap<String, f64>,
    /// Starting balances changed and not yet saved, by account: a new start, or `None` to forget it. Merged by
    /// account like `pending`.
    pending_credit_starts: BTreeMap<String, Option<f64>>,
    /// The file came from a newer version: read what we understand, never write it.
    read_only: bool,
}

impl DashboardPrefs {
    /// Loads the preferences in `dir`. A missing or unreadable file gives the defaults (every account shows history).
    pub fn load(dir: &Path) -> Self {
        let Some(doc) = read(dir) else {
            return Self::default();
        };
        let version = doc.get("version").and_then(Value::as_u64).unwrap_or(VERSION);
        let hidden_history = doc
            .get("hiddenHistory")
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        Self {
            hidden_history,
            pending: BTreeMap::new(),
            alerts: doc.get("alerts").map(alerts_from_json).unwrap_or_default(),
            active_alerts: doc
                .get("activeAlerts")
                .and_then(Value::as_array)
                .map(|keys| keys.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            pending_alert_fields: BTreeMap::new(),
            appearance: doc.get("appearance").and_then(Value::as_str).map(str::to_owned),
            appearance_dirty: false,
            pending_active: BTreeMap::new(),
            layout: layout_from_json(&doc),
            layout_dirty: false,
            widgets: doc.get("widgets").map(WidgetBuilder::from_json).unwrap_or_default(),
            widgets_dirty: false,
            credit_starts: credit_starts_from_json(doc.get("creditStarts")),
            pending_credit_starts: BTreeMap::new(),
            read_only: version > VERSION,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Changes the layout; returns `change`'s result. A change that leaves the layout as it was isn't saved.
    pub fn update_layout<T>(&mut self, change: impl FnOnce(&mut Layout) -> T) -> T {
        let before = self.layout.clone();
        let result = change(&mut self.layout);
        if self.layout != before {
            self.layout_dirty = true;
        }
        result
    }

    pub fn widget_builder(&self) -> &WidgetBuilder {
        &self.widgets
    }

    /// Changes the widget builder's choices; returns `change`'s result. An unchanged builder isn't saved.
    pub fn update_widget_builder<T>(&mut self, change: impl FnOnce(&mut WidgetBuilder) -> T) -> T {
        let before = self.widgets.clone();
        let result = change(&mut self.widgets);
        if self.widgets != before {
            self.widgets_dirty = true;
        }
        result
    }

    /// The starting credit balance remembered for `account`, if any.
    pub fn credit_start(&self, account: &str) -> Option<f64> {
        self.credit_starts.get(account).copied()
    }

    /// Records a credit balance just read for `account` and returns the start its bar runs down from (#150). The
    /// first balance seen is the start; a higher one is a top-up and becomes the new start.
    pub fn remember_credit_balance(&mut self, account: &str, balance: f64) -> f64 {
        if !balance.is_finite() || balance < 0.0 {
            return self.credit_start(account).unwrap_or(0.0);
        }
        match self.credit_start(account) {
            Some(start) if start >= balance => start,
            _ => {
                self.credit_starts.insert(account.to_owned(), balance);
                self.pending_credit_starts.insert(account.to_owned(), Some(balance));
                balance
            }
        }
    }

    fn set_credit_start(&mut self, account: &str, start: Option<f64>) {
        match start {
            Some(start) => self.credit_starts.insert(account.to_owned(), start),
            None => self.credit_starts.remove(account),
        };
        self.pending_credit_starts.insert(account.to_owned(), start);
    }

    /// The appearance choice's key ("system", "light", "dark"), when one was made.
    pub fn appearance(&self) -> Option<&str> {
        self.appearance.as_deref()
    }

    pub fn set_appearance(&mut self, key: &str) {
        if self.appearance.as_deref() != Some(key) {
            self.appearance = Some(key.to_owned());
            self.appearance_dirty = true;
        }
    }

    pub fn alert_settings(&self) -> &AlertSettings {
        &self.alerts
    }

    pub fn set_alert_settings(&mut self, alerts: AlertSettings) {
        let (old, new) = (alerts_to_json(&self.alerts), alerts_to_json(&alerts));
        for name in ALERT_FIELDS {
            if old[name] != new[name] {
                self.pending_alert_fields.insert(name, new[name].clone());
            }
        }
        self.alerts = alerts;
    }

    pub fn active_alerts(&self) -> &BTreeSet<String> {
        &self.active_alerts
    }

    pub fn set_active_alerts(&mut self, active: BTreeSet<String>) {
        for removed in self.active_alerts.difference(&active) {
            self.pending_active.insert(removed.clone(), false);
        }
        for added in active.difference(&self.active_alerts) {
            self.pending_active.insert(added.clone(), true);
        }
        self.active_alerts = active;
    }

    /// True when there are changes to save.
    pub fn is_dirty(&self) -> bool {
        !self.pending.is_empty()
            || !self.pending_alert_fields.is_empty()
            || !self.pending_active.is_empty()
            || self.layout_dirty
            || self.appearance_dirty
            || self.widgets_dirty
            || !self.pending_credit_starts.is_empty()
    }

    pub fn shows_history(&self, account: &str) -> bool {
        !self.hidden_history.contains(account)
    }

    pub fn set_shows_history(&mut self, account: &str, show: bool) {
        apply(&mut self.hidden_history, account, show);
        self.pending.insert(account.to_owned(), show);
    }

    /// Forgets a removed account (#85): its hidden-history choice, its held alerts, and its group and place in the
    /// order. Returns true when something changed.
    pub fn forget_account(&mut self, account: &str) -> bool {
        let mut changed = false;
        if self.hidden_history.contains(account) {
            self.set_shows_history(account, true);
            changed = true;
        }
        let kept: BTreeSet<String> = self
            .active_alerts
            .iter()
            .filter(|key| {
                let mut parts = key.rsplitn(3, '|');
                let (_, _, owner) = (parts.next(), parts.next(), parts.next());
                owner != Some(account)
            })
            .cloned()
            .collect();
        if kept != self.active_alerts {
            self.set_active_alerts(kept);
            changed = true;
        }
        if self.update_layout(|layout| layout.forget_account(account)) {
            changed = true;
        }
        if self.update_widget_builder(|builder| builder.forget_account(account)) {
            changed = true;
        }
        if self.credit_starts.contains_key(account) {
            self.set_credit_start(account, None);
            changed = true;
        }
        changed
    }

    /// Carries a preference from an account's old id to its new one. Returns true when something changed.
    pub fn rename_account(&mut self, from: &str, to: &str) -> bool {
        let mut changed = false;
        if self.hidden_history.contains(from) {
            self.set_shows_history(from, true);
            self.set_shows_history(to, false);
            changed = true;
        }
        // Active alerts are keyed `account|metric|kind`; they follow the account so a held condition doesn't
        // notify again under the new id.
        // The account is everything before the last two parts, so ids containing `|` are compared whole and a key
        // already under `to` (whose id may start with `from|`) is never rewritten again.
        let moved: BTreeSet<String> = self
            .active_alerts
            .iter()
            .map(|key| {
                let mut parts = key.rsplitn(3, '|');
                let (kind, metric, account) = (parts.next(), parts.next(), parts.next());
                match (account, metric, kind) {
                    (Some(account), Some(metric), Some(kind)) if account == from => format!("{to}|{metric}|{kind}"),
                    _ => key.clone(),
                }
            })
            .collect();
        if moved != self.active_alerts {
            self.set_active_alerts(moved);
            changed = true;
        }
        if self.update_layout(|layout| layout.rename_account(from, to)) {
            changed = true;
        }
        if self.update_widget_builder(|builder| builder.rename_account(from, to)) {
            changed = true;
        }
        if let Some(start) = self.credit_start(from) {
            self.set_credit_start(from, None);
            self.set_credit_start(to, Some(start));
            changed = true;
        }
        changed
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Saves the unsaved changes. Under the shared `dashboard.write.lock` it rereads the file, applies only this
    /// instance's changes, keeps keys this version doesn't know, and swaps in a temp file, so concurrent writers
    /// never lose each other's changes and a crash never leaves a torn file. Afterwards this instance holds the
    /// merged result. On failure the changes stay pending for the next save.
    pub fn save(&mut self, dir: &Path) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "dashboard.json is from a newer CodexBar and was not changed",
            ));
        }
        let _lock = crate::lock::FileLock::acquire(&dir.join(LOCK_FILE))?;
        let mut doc = read(dir).unwrap_or_default();
        // Another (newer) CodexBar may have upgraded the file since it was loaded; never downgrade it.
        if doc
            .get("version")
            .and_then(Value::as_u64)
            .is_some_and(|version| version > VERSION)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "dashboard.json was upgraded by a newer CodexBar and was not changed",
            ));
        }
        let mut hidden: BTreeSet<String> = doc
            .get("hiddenHistory")
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        for (account, show) in &self.pending {
            apply(&mut hidden, account, *show);
        }
        let mut alerts = match doc.get("alerts") {
            Some(Value::Object(fields)) => fields.clone(),
            _ => Map::new(),
        };
        for (name, value) in &self.pending_alert_fields {
            alerts.insert((*name).into(), value.clone());
        }
        let mut active: BTreeSet<String> = string_set(doc.get("activeAlerts"));
        for (key, add) in &self.pending_active {
            apply(&mut active, key, !add);
        }
        doc.insert("version".into(), json!(VERSION));
        doc.insert("hiddenHistory".into(), json!(hidden));
        doc.insert("alerts".into(), Value::Object(alerts.clone()));
        doc.insert("activeAlerts".into(), json!(active));
        let mut credit_starts = credit_starts_from_json(doc.get("creditStarts"));
        for (account, start) in &self.pending_credit_starts {
            match start {
                // Starts only ever rise between removals, so the higher of two writers' starts is the true one.
                Some(start) => {
                    let merged = credit_starts.get(account).map_or(*start, |saved| saved.max(*start));
                    credit_starts.insert(account.clone(), merged)
                }
                None => credit_starts.remove(account),
            };
        }
        doc.insert("creditStarts".into(), json!(credit_starts));
        if self.layout_dirty {
            layout_to_json(&self.layout, &mut doc);
        }
        if self.widgets_dirty {
            doc.insert(
                "widgets".into(),
                serde_json::to_value(&self.widgets).map_err(io::Error::other)?,
            );
        }
        if self.appearance_dirty
            && let Some(appearance) = &self.appearance
        {
            doc.insert("appearance".into(), json!(appearance));
        }
        let layout = layout_from_json(&doc);
        let widgets = doc.get("widgets").map(WidgetBuilder::from_json).unwrap_or_default();
        let doc_appearance = doc.get("appearance").and_then(Value::as_str).map(str::to_owned);
        let text = serde_json::to_string_pretty(&Value::Object(doc)).map_err(io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let path = dir.join(PREFS_FILE);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        // This instance now holds the merged result, including other writers' changes.
        self.hidden_history = hidden;
        self.alerts = alerts_from_json(&Value::Object(alerts));
        self.active_alerts = active;
        self.layout = layout;
        self.layout_dirty = false;
        self.widgets = widgets;
        self.widgets_dirty = false;
        self.appearance_dirty = false;
        self.appearance = doc_appearance;
        self.pending.clear();
        self.pending_alert_fields.clear();
        self.pending_active.clear();
        self.credit_starts = credit_starts;
        self.pending_credit_starts.clear();
        Ok(())
    }
}

/// `creditStarts`: account id to a finite, non-negative credit count. Anything else is skipped.
fn credit_starts_from_json(value: Option<&Value>) -> BTreeMap<String, f64> {
    value
        .and_then(Value::as_object)
        .map(|starts| {
            starts
                .iter()
                .filter_map(|(account, start)| {
                    let start = start.as_f64().filter(|start| start.is_finite() && *start >= 0.0)?;
                    Some((account.clone(), start))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The JSON names of the alert settings, as `alerts_to_json` writes them.
const ALERT_FIELDS: [&str; 5] = ["enabled", "usageThreshold", "balanceThreshold", "warning", "critical"];

fn string_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn alerts_from_json(value: &Value) -> AlertSettings {
    let defaults = AlertSettings::default();
    let flag = |name: &str, default: bool| value.get(name).and_then(Value::as_bool).unwrap_or(default);
    let number = |name: &str, default: f64| {
        value
            .get(name)
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite() && *number >= 0.0)
            .unwrap_or(default)
    };
    AlertSettings {
        enabled: flag("enabled", defaults.enabled),
        usage_threshold: number("usageThreshold", defaults.usage_threshold).clamp(0.05, 1.0),
        balance_threshold: number("balanceThreshold", defaults.balance_threshold),
        warning: flag("warning", defaults.warning),
        critical: flag("critical", defaults.critical),
    }
}

/// Reads `groups` ([{id, name}] in order), `groupMembers` ({account: group}) and `manualOrder` ([account]). Missing
/// keys (files from before groups) give no groups and the default order.
fn layout_from_json(doc: &Map<String, Value>) -> Layout {
    let groups = doc
        .get("groups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| {
                    Some(Group {
                        id: group.get("id")?.as_str()?.to_owned(),
                        name: group.get("name")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let members = doc
        .get("groupMembers")
        .and_then(Value::as_object)
        .map(|members| {
            members
                .iter()
                .filter_map(|(account, group)| Some((account.clone(), group.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let order = doc
        .get("manualOrder")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let mut layout = Layout::new(groups, members, order);
    // Files from before #90 have no mode and get the default (Smart).
    if let Some(mode) = doc
        .get("orderMode")
        .and_then(Value::as_str)
        .and_then(OrderMode::from_key)
    {
        layout.set_mode(mode);
    }
    layout
}

fn layout_to_json(layout: &Layout, doc: &mut Map<String, Value>) {
    let groups: Vec<Value> = layout
        .groups()
        .iter()
        .map(|group| json!({ "id": group.id, "name": group.name }))
        .collect();
    // Memberships of deleted groups are dropped on save; ones read from the file are already resolved.
    let members: Map<String, Value> = layout
        .members()
        .iter()
        .filter(|(_, group)| layout.group(group).is_some())
        .map(|(account, group)| (account.clone(), json!(group)))
        .collect();
    doc.insert("groups".into(), Value::Array(groups));
    doc.insert("groupMembers".into(), Value::Object(members));
    doc.insert("manualOrder".into(), json!(layout.order()));
    doc.insert("orderMode".into(), json!(layout.mode().key()));
}

fn alerts_to_json(alerts: &AlertSettings) -> Value {
    json!({
        "enabled": alerts.enabled,
        "usageThreshold": alerts.usage_threshold,
        "balanceThreshold": alerts.balance_threshold,
        "warning": alerts.warning,
        "critical": alerts.critical,
    })
}

fn apply(hidden: &mut BTreeSet<String>, account: &str, show: bool) {
    if show {
        hidden.remove(account);
    } else {
        hidden.insert(account.to_owned());
    }
}

fn read(dir: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(dir.join(PREFS_FILE)).ok()?;
    match serde_json::from_str(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("codexbar-prefs-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, text: &str) {
            std::fs::write(self.0.join(PREFS_FILE), text).unwrap();
        }

        fn read(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.0.join(PREFS_FILE)).unwrap()).unwrap()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn credit_start_tracks_first_value_spend_top_up_and_restart() {
        let dir = Dir::new("credit-start");
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert_eq!(
            prefs.remember_credit_balance("codex-a", 500.0),
            500.0,
            "the first balance is the start"
        );
        assert_eq!(
            prefs.remember_credit_balance("codex-a", 320.0),
            500.0,
            "spending keeps the start"
        );
        prefs.save(&dir.0).unwrap();
        assert!(!prefs.is_dirty());
        assert_eq!(prefs.remember_credit_balance("codex-a", 120.0), 500.0);
        assert!(!prefs.is_dirty(), "an unchanged start isn't saved again");

        let mut restarted = DashboardPrefs::load(&dir.0);
        assert_eq!(
            restarted.credit_start("codex-a"),
            Some(500.0),
            "the start survives a restart"
        );
        assert_eq!(
            restarted.remember_credit_balance("codex-a", 900.0),
            900.0,
            "a top-up resets the start"
        );
        restarted.save(&dir.0).unwrap();
        assert_eq!(DashboardPrefs::load(&dir.0).credit_start("codex-a"), Some(900.0));
    }

    #[test]
    fn credit_starts_follow_renamed_and_removed_accounts() {
        let dir = Dir::new("credit-start-moves");
        let mut prefs = DashboardPrefs::load(&dir.0);
        prefs.remember_credit_balance("old", 40.0);
        prefs.remember_credit_balance("other", 7.0);
        prefs.save(&dir.0).unwrap();
        assert!(prefs.rename_account("old", "new"));
        assert!(prefs.forget_account("other"));
        prefs.save(&dir.0).unwrap();
        let loaded = DashboardPrefs::load(&dir.0);
        assert_eq!(loaded.credit_start("old"), None);
        assert_eq!(loaded.credit_start("new"), Some(40.0));
        assert_eq!(loaded.credit_start("other"), None);
    }

    #[test]
    fn credit_starts_skip_bad_values_and_merge_with_other_writers() {
        let dir = Dir::new("credit-start-merge");
        dir.write(r#"{"version":1,"creditStarts":{"a":10,"bad":-1,"text":"5","nan":null}}"#);
        let mut mine = DashboardPrefs::load(&dir.0);
        assert_eq!(mine.credit_start("a"), Some(10.0));
        assert_eq!(mine.credit_start("bad"), None);
        assert_eq!(mine.credit_start("text"), None);
        let mut theirs = DashboardPrefs::load(&dir.0);
        theirs.remember_credit_balance("b", 3.0);
        theirs.save(&dir.0).unwrap();
        mine.remember_credit_balance("c", 8.0);
        mine.save(&dir.0).unwrap();
        assert_eq!(mine.credit_start("b"), Some(3.0), "the other writer's start is kept");
        assert_eq!(mine.credit_start("c"), Some(8.0));
        assert_eq!(
            mine.remember_credit_balance("c", f64::NAN),
            8.0,
            "a bad balance changes nothing"
        );
        // Two writers see different top-ups from the same old start: the higher survives either save order.
        let mut low = DashboardPrefs::load(&dir.0);
        let mut high = DashboardPrefs::load(&dir.0);
        high.remember_credit_balance("a", 500.0);
        high.save(&dir.0).unwrap();
        low.remember_credit_balance("a", 400.0);
        low.save(&dir.0).unwrap();
        assert_eq!(DashboardPrefs::load(&dir.0).credit_start("a"), Some(500.0));
    }

    #[test]
    fn load_missing_or_unreadable_file_shows_every_account() {
        let dir = Dir::new("missing");
        assert!(DashboardPrefs::load(&dir.0).shows_history("any"));
        dir.write("{ not json");
        assert!(DashboardPrefs::load(&dir.0).shows_history("any"));
    }

    #[test]
    fn save_round_trips_hidden_accounts() {
        let dir = Dir::new("round-trip");
        let mut prefs = DashboardPrefs::load(&dir.0);
        prefs.set_shows_history("cursor", false);
        prefs.set_shows_history("moonshot", false);
        prefs.set_shows_history("moonshot", true);
        prefs.save(&dir.0).unwrap();

        let loaded = DashboardPrefs::load(&dir.0);
        assert!(!loaded.shows_history("cursor"));
        assert!(loaded.shows_history("moonshot"));
        assert_eq!(dir.read()["hiddenHistory"], json!(["cursor"]));
    }

    #[test]
    fn rename_account_moves_a_hidden_preference() {
        let mut prefs = DashboardPrefs::default();
        prefs.set_shows_history("openrouter", false);
        assert!(prefs.rename_account("openrouter", "a1b2"));
        assert!(prefs.shows_history("openrouter"));
        assert!(!prefs.shows_history("a1b2"));
        assert!(!prefs.rename_account("openrouter", "a1b2"), "nothing left to move");
    }

    #[test]
    fn save_merges_with_another_writers_changes() {
        let dir = Dir::new("merge");
        let mut first = DashboardPrefs::load(&dir.0);
        let mut second = DashboardPrefs::load(&dir.0);
        first.set_shows_history("cursor", false);
        first.save(&dir.0).unwrap();
        // The second instance loaded before the first saved; its save must not undo the first's change.
        second.set_shows_history("moonshot", false);
        second.save(&dir.0).unwrap();
        assert_eq!(dir.read()["hiddenHistory"], json!(["cursor", "moonshot"]));
        assert!(!second.shows_history("cursor"), "the saver now holds the merged result");
    }

    #[test]
    fn save_while_another_writer_holds_the_lock_is_busy_and_keeps_the_change() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let dir = Dir::new("busy");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(dir.0.join(LOCK_FILE))
            .unwrap();
        let mut prefs = DashboardPrefs::load(&dir.0);
        prefs.set_shows_history("cursor", false);
        assert_eq!(prefs.save(&dir.0).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        drop(lock);
        prefs.save(&dir.0).unwrap();
        assert_eq!(
            dir.read()["hiddenHistory"],
            json!(["cursor"]),
            "the pending change was kept and saved"
        );
    }

    #[test]
    fn alert_settings_and_active_alerts_round_trip() {
        let dir = Dir::new("alerts");
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert_eq!(prefs.alert_settings(), &AlertSettings::default());
        let settings = AlertSettings {
            enabled: true,
            usage_threshold: 0.9,
            balance_threshold: 2.0,
            warning: false,
            critical: true,
        };
        prefs.set_alert_settings(settings.clone());
        prefs.set_active_alerts(["a|weekly|usage".to_owned()].into());
        assert!(prefs.is_dirty());
        prefs.save(&dir.0).unwrap();
        assert!(!prefs.is_dirty());

        let loaded = DashboardPrefs::load(&dir.0);
        assert_eq!(loaded.alert_settings(), &settings);
        assert!(loaded.active_alerts().contains("a|weekly|usage"));
        assert_eq!(dir.read()["alerts"]["usageThreshold"], json!(0.9));
    }

    #[test]
    fn concurrent_alert_edits_merge_by_field_and_key() {
        let dir = Dir::new("alerts-merge");
        let mut first = DashboardPrefs::load(&dir.0);
        let mut second = DashboardPrefs::load(&dir.0);
        let mut enabled = first.alert_settings().clone();
        enabled.enabled = true;
        first.set_alert_settings(enabled);
        first.set_active_alerts(["a|weekly|usage".to_owned()].into());
        first.save(&dir.0).unwrap();

        // The second process loaded before; it changes another field and another key.
        let mut balance = second.alert_settings().clone();
        balance.balance_threshold = 20.0;
        second.set_alert_settings(balance);
        second.set_active_alerts(["b|credits|balance".to_owned()].into());
        second.save(&dir.0).unwrap();

        let merged = DashboardPrefs::load(&dir.0);
        assert!(merged.alert_settings().enabled, "the first process's change survives");
        assert_eq!(merged.alert_settings().balance_threshold, 20.0);
        assert_eq!(merged.active_alerts().len(), 2);
        assert_eq!(
            second.alert_settings(),
            merged.alert_settings(),
            "the saver holds the merged result"
        );
        assert_eq!(second.active_alerts(), merged.active_alerts());
    }

    #[test]
    fn rename_account_compares_whole_ids_and_runs_once() {
        let mut prefs = DashboardPrefs::default();
        prefs.set_active_alerts(["openrouter|credits|balance".to_owned()].into());
        assert!(prefs.rename_account("openrouter", "openrouter|team"));
        // The migrated key starts with `openrouter|`, but its account is `openrouter|team`: leave it alone.
        assert!(!prefs.rename_account("openrouter", "openrouter|team"));
        let keys: Vec<&str> = prefs.active_alerts().iter().map(String::as_str).collect();
        assert_eq!(keys, vec!["openrouter|team|credits|balance"]);
    }

    #[test]
    fn rename_account_moves_active_alert_keys() {
        let mut prefs = DashboardPrefs::default();
        prefs.set_active_alerts(["openrouter|credits|balance".to_owned(), "c|weekly|usage".to_owned()].into());
        assert!(prefs.rename_account("openrouter", "or-1"));
        let keys: Vec<&str> = prefs.active_alerts().iter().map(String::as_str).collect();
        assert_eq!(keys, vec!["c|weekly|usage", "or-1|credits|balance"]);
    }

    #[test]
    fn unreadable_alert_values_fall_back_to_defaults() {
        let dir = Dir::new("alerts-bad");
        dir.write(r#"{ "version": 1, "alerts": { "enabled": "yes", "usageThreshold": -3, "balanceThreshold": "x" } }"#);
        assert_eq!(DashboardPrefs::load(&dir.0).alert_settings(), &AlertSettings::default());
    }

    #[test]
    fn groups_and_manual_order_survive_a_restart() {
        let dir = Dir::new("layout");
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert!(prefs.layout().groups().is_empty(), "files from before groups have none");
        let accounts: Vec<String> = ["codex", "claude", "cursor"].map(str::to_owned).to_vec();
        prefs.update_layout(|layout| {
            let work = layout.create_group("Work").unwrap();
            layout.assign("claude", Some(&work)).unwrap();
            layout.assign("codex", Some(&work)).unwrap();
            layout.move_account("claude", -1, &accounts);
        });
        assert!(prefs.is_dirty());
        prefs.save(&dir.0).unwrap();
        let loaded = DashboardPrefs::load(&dir.0);
        assert_eq!(loaded.layout(), prefs.layout());
        let sections = loaded.layout().arrange(&accounts);
        assert_eq!(sections[0].accounts, ["claude", "codex"]);
        assert_eq!(sections[1].accounts, ["cursor"]);
    }

    #[test]
    fn the_order_mode_is_saved_and_defaults_to_smart() {
        let dir = Dir::new("order-mode");
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert_eq!(prefs.layout().mode(), OrderMode::Smart);
        prefs.update_layout(|layout| layout.set_mode(OrderMode::Manual));
        prefs.save(&dir.0).unwrap();
        assert_eq!(dir.read()["orderMode"], json!("manual"));
        assert_eq!(DashboardPrefs::load(&dir.0).layout().mode(), OrderMode::Manual);
        dir.write(r#"{ "version": 1, "orderMode": "sideways" }"#);
        assert_eq!(
            DashboardPrefs::load(&dir.0).layout().mode(),
            OrderMode::Smart,
            "unknown modes read as Smart"
        );
    }

    #[test]
    fn a_layout_change_that_changes_nothing_is_not_saved() {
        let dir = Dir::new("layout-noop");
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert!(prefs.update_layout(|layout| layout.delete_group("g1")).is_err());
        assert!(!prefs.is_dirty());
    }

    #[test]
    fn stale_memberships_read_as_ungrouped_and_are_dropped_on_save() {
        let dir = Dir::new("layout-stale");
        dir.write(
            r#"{ "version": 1, "groups": [ { "id": "g1", "name": "Work" } ],
                "groupMembers": { "claude": "g1", "codex": "g7" }, "manualOrder": [ "codex", "claude" ] }"#,
        );
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert_eq!(prefs.layout().group_of("codex"), None);
        prefs
            .update_layout(|layout| layout.create_group("Home").map(|_| ()))
            .unwrap();
        prefs.save(&dir.0).unwrap();
        assert_eq!(dir.read()["groupMembers"], json!({ "claude": "g1" }));
        assert_eq!(dir.read()["manualOrder"], json!(["codex", "claude"]));
    }

    #[test]
    fn forgetting_an_account_drops_its_preferences_alerts_and_layout() {
        let dir = Dir::new("forget");
        let mut prefs = DashboardPrefs::load(&dir.0);
        prefs.set_shows_history("gone", false);
        prefs.set_active_alerts(["gone|weekly@1|usage".to_owned(), "kept|weekly@1|usage".to_owned()].into());
        prefs.update_layout(|layout| {
            let work = layout.create_group("Work").unwrap();
            layout.assign("gone", Some(&work)).unwrap();
        });
        assert!(prefs.forget_account("gone"));
        prefs.save(&dir.0).unwrap();
        let loaded = DashboardPrefs::load(&dir.0);
        assert!(loaded.shows_history("gone"));
        assert_eq!(
            loaded.active_alerts(),
            &BTreeSet::from(["kept|weekly@1|usage".to_owned()])
        );
        assert_eq!(loaded.layout().group_of("gone"), None);
        assert!(!prefs.forget_account("gone"));
    }

    #[test]
    fn the_appearance_choice_is_saved() {
        let dir = Dir::new("appearance");
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert_eq!(prefs.appearance(), None, "System until chosen");
        prefs.set_appearance("light");
        assert!(prefs.is_dirty());
        prefs.save(&dir.0).unwrap();
        assert_eq!(DashboardPrefs::load(&dir.0).appearance(), Some("light"));
        prefs.set_appearance("light");
        assert!(!prefs.is_dirty(), "the same choice again changes nothing");
    }

    #[test]
    fn widget_builder_choices_save_and_follow_account_changes() {
        use crate::widgets::{TileMode, WidgetRefresh, WidgetTile};
        let dir = Dir::new("widgets");
        let mut prefs = DashboardPrefs::load(&dir.0);
        prefs.update_widget_builder(|builder| {
            builder.add(WidgetTile {
                account: "a".into(),
                metric: "weekly".into(),
                mode: TileMode::Status,
            });
            builder.add(WidgetTile {
                account: "b".into(),
                metric: "credits".into(),
                mode: TileMode::Balance,
            });
            builder.refresh = WidgetRefresh::FiveMinutes;
        });
        assert!(prefs.is_dirty());
        prefs.save(&dir.0).unwrap();
        let loaded = DashboardPrefs::load(&dir.0);
        assert_eq!(loaded.widget_builder(), prefs.widget_builder());
        assert_eq!(loaded.widget_builder().refresh, WidgetRefresh::FiveMinutes);

        let mut prefs = loaded;
        assert!(prefs.rename_account("a", "a2"));
        assert!(prefs.forget_account("b"));
        prefs.save(&dir.0).unwrap();
        let tiles = DashboardPrefs::load(&dir.0).widget_builder().tiles.clone();
        assert_eq!(tiles.len(), 1);
        assert_eq!((tiles[0].account.as_str(), tiles[0].mode), ("a2", TileMode::Status));
    }

    #[test]
    fn save_keeps_unknown_keys() {
        let dir = Dir::new("unknown");
        dir.write(r#"{ "version": 1, "hiddenHistory": [], "lastView": "history" }"#);
        let mut prefs = DashboardPrefs::load(&dir.0);
        prefs.set_shows_history("cursor", false);
        prefs.save(&dir.0).unwrap();
        assert_eq!(dir.read()["lastView"], json!("history"));
    }

    #[test]
    fn save_refuses_a_file_upgraded_after_load() {
        let dir = Dir::new("upgraded");
        let mut prefs = DashboardPrefs::load(&dir.0);
        let newer = r#"{ "version": 2, "hiddenHistory": [] }"#;
        dir.write(newer);
        prefs.set_shows_history("cursor", false);
        assert!(prefs.save(&dir.0).is_err());
        assert_eq!(std::fs::read_to_string(dir.0.join(PREFS_FILE)).unwrap(), newer);
    }

    #[test]
    fn newer_version_is_read_but_never_overwritten() {
        let dir = Dir::new("newer");
        let newer = r#"{ "version": 2, "hiddenHistory": ["cursor"] }"#;
        dir.write(newer);
        let mut prefs = DashboardPrefs::load(&dir.0);
        assert!(prefs.is_read_only());
        assert!(!prefs.shows_history("cursor"));
        prefs.set_shows_history("cursor", true);
        assert!(prefs.save(&dir.0).is_err());
        assert_eq!(std::fs::read_to_string(dir.0.join(PREFS_FILE)).unwrap(), newer);
    }
}
