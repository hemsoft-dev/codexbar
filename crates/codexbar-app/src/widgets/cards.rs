//! What a widget shows (#94): Adaptive Cards built from the dashboard's `widgets.json` snapshot, one per widget
//! configuration and size. Pure, so every state is tested without the widget host.

use chrono::{DateTime, Utc};
use codexbar_core::format::{age_label, countdown};
use codexbar_store::widgets::{LoadError, MAX_TILES, TileMode, WidgetAccount, WidgetHealth, WidgetSnapshot};
use serde_json::{Value, json};

/// A snapshot older than this means CodexBar isn't running: it rewrites the file at least every five minutes.
pub const NOT_RUNNING_AFTER: chrono::Duration = chrono::Duration::minutes(15);

/// Which accounts a widget shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    /// Every account, in dashboard order.
    #[default]
    All,
    /// One provider's accounts, by provider key.
    Provider(String),
    /// One group's accounts, by group id.
    Group(String),
    /// The tiles chosen in CodexBar's Settings › Widgets (#95).
    Custom,
}

impl Focus {
    pub fn key(&self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Custom => "custom".into(),
            Self::Provider(key) => format!("provider:{key}"),
            Self::Group(id) => format!("group:{id}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.split_once(':') {
            None if key == "all" => Some(Self::All),
            None if key == "custom" => Some(Self::Custom),
            Some(("provider", key)) if !key.is_empty() => Some(Self::Provider(key.to_owned())),
            Some(("group", id)) if !id.is_empty() => Some(Self::Group(id.to_owned())),
            _ => None,
        }
    }
}

/// How many accounts a widget shows as tiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tiles {
    /// By widget size: one on small, two on medium, four on large.
    #[default]
    Automatic,
    One,
    Two,
    Four,
}

impl Tiles {
    pub const ALL: [Self; 4] = [Self::Automatic, Self::One, Self::Two, Self::Four];

    pub fn key(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::One => "1",
            Self::Two => "2",
            Self::Four => "4",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tiles| tiles.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic (by widget size)",
            Self::One => "One tile",
            Self::Two => "Two tiles",
            Self::Four => "Four tiles",
        }
    }

    /// How many tiles show. Automatic fills the size: one on small, two on medium, four on large, or all six of
    /// the builder's tiles on large.
    pub fn count(self, size: Size, custom: bool) -> usize {
        match (self, size) {
            (Self::Automatic, Size::Small) | (Self::One, _) => 1,
            (Self::Automatic, Size::Medium) | (Self::Two, _) => 2,
            (Self::Automatic, Size::Large) if custom => MAX_TILES,
            (Self::Automatic, Size::Large) | (Self::Four, _) => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    Small,
    #[default]
    Medium,
    Large,
}

/// A widget's settings, kept by the widget host as the widget's custom state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub focus: Focus,
    pub tiles: Tiles,
}

impl Config {
    /// The custom state text. JSON, so later versions can add settings.
    pub fn to_state(&self) -> String {
        json!({ "focus": self.focus.key(), "tiles": self.tiles.key() }).to_string()
    }

    /// Settings from custom state; anything missing or unreadable takes its default.
    pub fn from_state(state: &str) -> Self {
        let value: Value = serde_json::from_str(state).unwrap_or(Value::Null);
        Self {
            focus: value
                .get("focus")
                .and_then(Value::as_str)
                .and_then(Focus::from_key)
                .unwrap_or_default(),
            tiles: value
                .get("tiles")
                .and_then(Value::as_str)
                .and_then(Tiles::from_key)
                .unwrap_or_default(),
        }
    }
}

/// The action verbs cards send back.
pub mod verbs {
    pub const OPEN: &str = "open";
    pub const SAVE: &str = "save";
    pub const CANCEL: &str = "cancel";
}

fn card(body: Vec<Value>, actions: Vec<Value>) -> Value {
    let mut card = json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": "1.5",
        "body": body,
    });
    if !actions.is_empty() {
        card["actions"] = Value::Array(actions);
    }
    card
}

fn text(text: impl Into<String>) -> Value {
    json!({ "type": "TextBlock", "text": text.into(), "wrap": true })
}

fn subtle(value: impl Into<String>) -> Value {
    let mut block = text(value);
    block["isSubtle"] = json!(true);
    block["size"] = json!("Small");
    block
}

fn open_action() -> Value {
    json!({ "type": "Action.Execute", "title": "Open CodexBar", "verb": verbs::OPEN })
}

/// A message card: a title line, an explanation and an Open action.
fn message(title: &str, body: &str) -> Value {
    let mut heading = text(title);
    heading["weight"] = json!("Bolder");
    card(vec![heading, subtle(body)], vec![open_action()])
}

/// The focus's display name, if it still exists in the snapshot.
fn focus_name(focus: &Focus, snapshot: &WidgetSnapshot) -> Option<String> {
    match focus {
        Focus::All => Some("All accounts".into()),
        Focus::Custom => Some("CodexBar".into()),
        Focus::Provider(key) => snapshot
            .accounts
            .iter()
            .find(|account| &account.provider == key)
            .map(|account| account.provider_name.clone()),
        Focus::Group(id) => snapshot
            .groups
            .iter()
            .find(|group| &group.id == id)
            .map(|group| group.name.clone()),
    }
}

fn focused<'a>(focus: &Focus, snapshot: &'a WidgetSnapshot) -> Vec<&'a WidgetAccount> {
    snapshot
        .accounts
        .iter()
        .filter(|account| match focus {
            Focus::All | Focus::Custom => true,
            Focus::Provider(key) => &account.provider == key,
            Focus::Group(id) => account.group.as_ref() == Some(id),
        })
        .collect()
}

/// What one tile shows, worked out once for the widget card and the in-app preview (#95).
#[derive(Clone, Debug, PartialEq)]
pub struct TileView {
    /// The account and metric a tap opens; none when the account is gone.
    pub account: Option<String>,
    pub metric: Option<String>,
    pub title: String,
    pub mode: TileMode,
    /// "Weekly: 42% used".
    pub line: Option<String>,
    /// The large figure of the compact, balance and status modes.
    pub big: Option<String>,
    /// 0 to 100, for a bar.
    pub percent: Option<f64>,
    /// "Watch", "At risk", "Limit soon"; none for normal accounts.
    pub status: Option<String>,
    /// Freshness or what's wrong: "Last known, 5m ago", "Unavailable", "Resets in 2d".
    pub note: Option<String>,
}

impl TileView {
    fn message(account: Option<&str>, title: impl Into<String>, mode: TileMode, note: &str) -> Self {
        Self {
            account: account.map(str::to_owned),
            metric: None,
            title: title.into(),
            mode,
            line: None,
            big: None,
            percent: None,
            status: None,
            note: Some(note.to_owned()),
        }
    }

    /// The tile for `account`'s `metric` (its primary one when `None`), shown as `mode`.
    fn of(account: &WidgetAccount, metric: Option<&str>, mode: TileMode, now: DateTime<Utc>) -> Self {
        if account.health == WidgetHealth::Unavailable {
            // The tap still opens the tile's metric, for when the account recovers.
            let mut view = Self::message(Some(&account.id), account.name.clone(), mode, "Unavailable");
            view.metric = metric.map(str::to_owned);
            return view;
        }
        let found = match metric {
            Some(key) => account.metrics.iter().find(|m| m.key == key),
            None => account.metrics.first(),
        };
        let Some(found) = found else {
            let note = if metric.is_some() {
                "This limit isn't reported any more. Choose another in CodexBar's Settings › Widgets."
            } else {
                "No limits to show"
            };
            return Self::message(Some(&account.id), account.name.clone(), mode, note);
        };
        let freshness = match (account.health, account.updated_at) {
            (WidgetHealth::Stale, Some(at)) => Some(format!("Last known, {}", age_label(at, now))),
            (WidgetHealth::Stale, None) => Some("Last known".to_owned()),
            _ => None,
        };
        // The tile's own metric decides its status, not the account's most urgent one.
        let status = found.status.clone();
        let line = Some(format!("{}: {}", found.label, found.value));
        let percent_text = found.used_percent.map(|p| format!("{p:.0}%"));
        let resets = found
            .resets_at
            .filter(|at| *at > now)
            .map(|at| format!("Resets in {}", countdown(at - now)));
        let (line, big, percent, note) = match mode {
            // A bar for limits; money (no percentage) shows its amount large.
            TileMode::Automatic if found.used_percent.is_none() => (
                Some(found.label.clone()),
                Some(found.value.clone()),
                None,
                freshness.or(status.clone()),
            ),
            TileMode::Automatic => (line, None, found.used_percent, freshness.or(status.clone())),
            TileMode::Percent => (
                Some(found.label.clone()),
                percent_text.or(Some(found.value.clone())),
                None,
                freshness,
            ),
            TileMode::Bar => (line, None, found.used_percent, freshness.or(resets).or(status.clone())),
            TileMode::Balance => (Some(found.label.clone()), Some(found.value.clone()), None, freshness),
            TileMode::Status => (
                line,
                Some(status.clone().unwrap_or_else(|| "OK".to_owned())),
                None,
                freshness,
            ),
        };
        Self {
            account: Some(account.id.clone()),
            metric: Some(found.key.clone()),
            title: account.name.clone(),
            mode,
            line,
            big,
            percent,
            status,
            note,
        }
    }
}

/// The tiles a widget shows: the builder's tiles for the Custom focus, otherwise each focused account's primary
/// metric. `None` when the focus is gone.
pub fn tile_views(snapshot: &WidgetSnapshot, config: &Config, size: Size, now: DateTime<Utc>) -> Option<Vec<TileView>> {
    focus_name(&config.focus, snapshot)?;
    let custom = config.focus == Focus::Custom;
    let count = config.tiles.count(size, custom);
    let views = if custom {
        snapshot
            .builder
            .tiles
            .iter()
            .take(count)
            .map(
                |tile| match snapshot.accounts.iter().find(|account| account.id == tile.account) {
                    Some(account) => TileView::of(account, Some(&tile.metric), tile.mode, now),
                    None => TileView::message(
                        None,
                        "Removed account",
                        tile.mode,
                        "This account is gone. Choose another tile in CodexBar's Settings › Widgets.",
                    ),
                },
            )
            .collect()
    } else {
        focused(&config.focus, snapshot)
            .into_iter()
            .take(count)
            .map(|account| TileView::of(account, None, TileMode::Automatic, now))
            .collect()
    };
    Some(views)
}

/// A usage bar: two weighted columns, used and left. The color follows the account's status.
fn bar(percent: f64, status: Option<&str>) -> Value {
    let style = match status {
        Some("At risk" | "Limit soon") => "attention",
        Some(_) => "warning",
        None => "good",
    };
    let used = percent.round().clamp(0.0, 100.0) as u32;
    let mut columns = Vec::new();
    if used > 0 {
        columns.push(
            json!({ "type": "Column", "width": used.to_string(), "style": style, "minHeight": "6px", "items": [] }),
        );
    }
    if used < 100 {
        columns.push(json!({ "type": "Column", "width": (100 - used).to_string(), "style": "emphasis", "minHeight": "6px", "items": [] }));
    }
    json!({ "type": "ColumnSet", "spacing": "Small", "columns": columns })
}

/// The Adaptive Cards color for a status.
fn status_color(status: Option<&str>) -> &'static str {
    match status {
        Some("At risk" | "Limit soon") => "Attention",
        Some(_) => "Warning",
        None => "Good",
    }
}

/// One tile as a card container. Tapping it opens CodexBar on its account and metric.
fn tile(view: &TileView, compact: bool) -> Value {
    let mut items = Vec::new();
    let mut name = text(view.title.clone());
    name["weight"] = json!("Bolder");
    name["size"] = json!("Small");
    name["wrap"] = json!(false);
    items.push(name);
    if let Some(big) = &view.big {
        let mut figure = text(big.clone());
        figure["size"] = json!(if compact { "Medium" } else { "Large" });
        figure["weight"] = json!("Bolder");
        figure["spacing"] = json!("None");
        if view.mode == TileMode::Status {
            figure["color"] = json!(status_color(view.status.as_deref()));
        }
        items.push(figure);
    }
    if let Some(line) = view.line.as_ref().filter(|_| !compact || view.big.is_none()) {
        let mut block = text(line.clone());
        block["size"] = json!("Small");
        block["spacing"] = json!("None");
        items.push(block);
    }
    if let Some(percent) = view.percent {
        items.push(bar(percent, view.status.as_deref()));
    }
    if let Some(note) = view
        .note
        .as_ref()
        .filter(|note| !compact || note.starts_with("Last known") || view.line.is_none())
    {
        items.push(subtle(note.clone()));
    }
    let mut container = json!({ "type": "Container", "items": items });
    if let Some(account) = &view.account {
        container["selectAction"] = json!({
            "type": "Action.Execute",
            "verb": verbs::OPEN,
            "data": { "account": account, "metric": view.metric },
        });
    }
    container
}

/// The widget's card for the current snapshot (or why there is none).
pub fn render(snapshot: &Result<WidgetSnapshot, LoadError>, config: &Config, size: Size, now: DateTime<Utc>) -> Value {
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(LoadError::Missing) => {
            return message("CodexBar", "Open CodexBar to see your AI coding usage here.");
        }
        Err(LoadError::Newer) => {
            return message(
                "Update CodexBar",
                "This widget is older than CodexBar's usage data. Install the latest CodexBar package.",
            );
        }
        Err(LoadError::Invalid) => {
            return message(
                "CodexBar",
                "CodexBar's usage data couldn't be read. Open CodexBar to refresh it.",
            );
        }
    };
    let (Some(title), Some(views)) = (
        focus_name(&config.focus, snapshot),
        tile_views(snapshot, config, size, now),
    ) else {
        return message(
            "Nothing to show",
            "The provider or group this widget showed is gone. Choose another in Customize widget.",
        );
    };
    if views.is_empty() {
        return message(
            &title,
            if config.focus == Focus::Custom {
                "No tiles yet. Choose them in CodexBar's Settings › Widgets."
            } else if snapshot.accounts.is_empty() {
                "No accounts yet. Add one in CodexBar's Settings."
            } else {
                "No accounts here yet."
            },
        );
    }

    let count = views.len();
    let compact = size == Size::Small && count > 1;
    let tiles: Vec<Value> = views.iter().map(|view| tile(view, compact)).collect();
    let mut body = Vec::new();
    let mut heading = text(title);
    heading["weight"] = json!("Bolder");
    heading["size"] = json!("Small");
    heading["isSubtle"] = json!(true);
    body.push(heading);
    // Four or more tiles sit two by two; fewer stack, unless a wide layout has room for two side by side.
    if count >= 3 || (count == 2 && size == Size::Large) {
        for pair in tiles.chunks(2) {
            let columns: Vec<Value> = pair
                .iter()
                .map(|tile| json!({ "type": "Column", "width": "stretch", "items": [tile] }))
                .collect();
            body.push(json!({ "type": "ColumnSet", "columns": columns }));
        }
    } else {
        body.extend(tiles);
    }
    let age = now - snapshot.generated_at;
    let footer = if age > NOT_RUNNING_AFTER {
        format!(
            "CodexBar isn't running. Updated {}.",
            age_label(snapshot.generated_at, now)
        )
    } else {
        format!("Updated {}", age_label(snapshot.generated_at, now))
    };
    body.push(subtle(footer));
    let actions = if age > NOT_RUNNING_AFTER {
        vec![open_action()]
    } else {
        Vec::new()
    };
    let mut card = card(body, actions);
    // Tapping the widget outside a tile opens CodexBar.
    card["selectAction"] = json!({ "type": "Action.Execute", "verb": verbs::OPEN });
    card
}

/// The Customize widget card: which accounts, and how many.
pub fn customize(snapshot: &Result<WidgetSnapshot, LoadError>, config: &Config) -> Value {
    let mut choices = vec![
        json!({ "title": "All accounts", "value": Focus::All.key() }),
        json!({ "title": "My tiles (Settings › Widgets)", "value": Focus::Custom.key() }),
    ];
    if let Ok(snapshot) = snapshot {
        let mut providers: Vec<(&str, &str)> = Vec::new();
        for account in &snapshot.accounts {
            if !providers.iter().any(|(key, _)| *key == account.provider) {
                providers.push((&account.provider, &account.provider_name));
            }
        }
        for (key, name) in providers {
            choices.push(json!({ "title": name, "value": Focus::Provider(key.to_owned()).key() }));
        }
        for group in &snapshot.groups {
            choices.push(
                json!({ "title": format!("Group: {}", group.name), "value": Focus::Group(group.id.clone()).key() }),
            );
        }
    }
    let tiles: Vec<Value> = Tiles::ALL
        .iter()
        .map(|tiles| json!({ "title": tiles.label(), "value": tiles.key() }))
        .collect();
    let mut heading = text("Customize CodexBar");
    heading["weight"] = json!("Bolder");
    card(
        vec![
            heading,
            json!({ "type": "Input.ChoiceSet", "id": "focus", "label": "Show", "style": "compact",
                    "value": config.focus.key(), "choices": choices }),
            json!({ "type": "Input.ChoiceSet", "id": "tiles", "label": "Layout", "style": "compact",
                    "value": config.tiles.key(), "choices": tiles }),
        ],
        vec![
            json!({ "type": "Action.Execute", "title": "Save", "verb": verbs::SAVE }),
            json!({ "type": "Action.Execute", "title": "Cancel", "verb": verbs::CANCEL }),
        ],
    )
}

/// The settings a Save action carries (`{"focus": ..., "tiles": ...}`), applied over `config`.
pub fn saved(config: &Config, data: &str) -> Config {
    let value: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    Config {
        focus: value
            .get("focus")
            .and_then(Value::as_str)
            .and_then(Focus::from_key)
            .unwrap_or_else(|| config.focus.clone()),
        tiles: value
            .get("tiles")
            .and_then(Value::as_str)
            .and_then(Tiles::from_key)
            .unwrap_or(config.tiles),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexbar_store::widgets::{WidgetGroup, WidgetMetric};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn account(id: &str, provider: &str, group: Option<&str>, percent: f64) -> WidgetAccount {
        WidgetAccount {
            id: id.into(),
            provider: provider.into(),
            provider_name: provider.to_uppercase(),
            name: format!("{provider} {id}"),
            group: group.map(str::to_owned),
            health: WidgetHealth::Fresh,
            updated_at: Some(now()),
            status: None,
            metrics: vec![WidgetMetric {
                key: "weekly".into(),
                label: "Weekly".into(),
                value: format!("{percent:.0}% used"),
                used_percent: Some(percent),
                resets_at: None,
                status: None,
            }],
        }
    }

    fn snapshot() -> Result<WidgetSnapshot, LoadError> {
        Ok(WidgetSnapshot::new(
            now() - chrono::Duration::minutes(2),
            vec![WidgetGroup {
                id: "g1".into(),
                name: "Work".into(),
            }],
            vec![
                account("a", "claude", Some("g1"), 82.0),
                account("b", "codex", None, 10.0),
                account("c", "claude", None, 0.0),
                account("d", "copilot", Some("g1"), 100.0),
                account("e", "cursor", None, 50.0),
            ],
        ))
    }

    fn all_text(card: &Value) -> String {
        let mut out = String::new();
        fn walk(value: &Value, out: &mut String) {
            match value {
                Value::Object(map) => {
                    if let Some(Value::String(text)) = map.get("text") {
                        out.push_str(text);
                        out.push('\n');
                    }
                    map.values().for_each(|v| walk(v, out));
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        walk(card, &mut out);
        out
    }

    fn names(card: &Value) -> Vec<String> {
        all_text(card)
            .lines()
            .filter(|line| {
                line.contains(' ')
                    && ["claude", "codex", "copilot", "cursor"]
                        .iter()
                        .any(|p| line.starts_with(p))
            })
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn automatic_tiles_follow_the_widget_size() {
        let config = Config::default();
        for (size, expected) in [(Size::Small, 1), (Size::Medium, 2), (Size::Large, 4)] {
            let card = render(&snapshot(), &config, size, now());
            assert_eq!(names(&card).len(), expected, "{size:?}");
        }
        assert_eq!(
            names(&render(&snapshot(), &config, Size::Large, now()))[0],
            "claude a",
            "dashboard order"
        );
    }

    #[test]
    fn explicit_tile_counts_win_over_size() {
        for (tiles, expected) in [(Tiles::One, 1), (Tiles::Two, 2), (Tiles::Four, 4)] {
            let config = Config {
                focus: Focus::All,
                tiles,
            };
            assert_eq!(
                names(&render(&snapshot(), &config, Size::Medium, now())).len(),
                expected,
                "{tiles:?}"
            );
        }
    }

    #[test]
    fn provider_and_group_focus_filter_in_dashboard_order() {
        let provider = Config {
            focus: Focus::Provider("claude".into()),
            tiles: Tiles::Four,
        };
        let card = render(&snapshot(), &provider, Size::Large, now());
        assert_eq!(names(&card), ["claude a", "claude c"]);
        assert!(
            all_text(&card).starts_with("CLAUDE\n"),
            "the provider's name heads the card"
        );
        let group = Config {
            focus: Focus::Group("g1".into()),
            tiles: Tiles::Four,
        };
        let card = render(&snapshot(), &group, Size::Large, now());
        assert_eq!(names(&card), ["claude a", "copilot d"]);
        assert!(all_text(&card).starts_with("Work\n"));
    }

    #[test]
    fn a_removed_group_or_provider_degrades_to_a_message() {
        let gone = Config {
            focus: Focus::Group("deleted".into()),
            tiles: Tiles::Automatic,
        };
        let text = all_text(&render(&snapshot(), &gone, Size::Medium, now()));
        assert!(text.contains("is gone") && text.contains("Customize widget"), "{text}");
    }

    #[test]
    fn loading_newer_and_broken_snapshots_say_so() {
        let config = Config::default();
        assert!(all_text(&render(&Err(LoadError::Missing), &config, Size::Medium, now())).contains("Open CodexBar"));
        assert!(all_text(&render(&Err(LoadError::Newer), &config, Size::Medium, now())).contains("Update CodexBar"));
        assert!(all_text(&render(&Err(LoadError::Invalid), &config, Size::Medium, now())).contains("couldn't be read"));
        let empty = Ok(WidgetSnapshot::new(now(), vec![], vec![]));
        assert!(all_text(&render(&empty, &config, Size::Medium, now())).contains("No accounts yet"));
    }

    #[test]
    fn stale_and_unavailable_accounts_and_a_stopped_app_are_marked() {
        let mut snapshot = snapshot().unwrap();
        snapshot.accounts[0].health = WidgetHealth::Stale;
        snapshot.accounts[0].updated_at = Some(now() - chrono::Duration::minutes(30));
        snapshot.accounts[1].health = WidgetHealth::Unavailable;
        let card = render(&Ok(snapshot.clone()), &Config::default(), Size::Medium, now());
        let text = all_text(&card);
        assert!(text.contains("Last known, 30m ago"), "{text}");
        assert!(text.contains("Unavailable"));
        assert!(text.contains("Updated 2m ago"));
        assert!(card.get("actions").is_none(), "a running app needs no Open button");

        snapshot.generated_at = now() - chrono::Duration::hours(3);
        let card = render(&Ok(snapshot), &Config::default(), Size::Medium, now());
        assert!(all_text(&card).contains("CodexBar isn't running"));
        assert_eq!(card["actions"][0]["verb"], verbs::OPEN);
    }

    #[test]
    fn bars_split_used_and_left_and_color_by_status() {
        let full = bar(100.0, Some("Limit soon"));
        assert_eq!(full["columns"].as_array().unwrap().len(), 1);
        assert_eq!(full["columns"][0]["style"], "attention");
        let empty = bar(0.0, None);
        assert_eq!(empty["columns"].as_array().unwrap().len(), 1);
        assert_eq!(empty["columns"][0]["style"], "emphasis");
        let watch = bar(42.4, Some("Watch"));
        assert_eq!(watch["columns"][0]["width"], "42");
        assert_eq!(watch["columns"][0]["style"], "warning");
        assert_eq!(watch["columns"][1]["width"], "58");
    }

    #[test]
    fn four_tiles_sit_two_by_two() {
        let config = Config {
            focus: Focus::All,
            tiles: Tiles::Four,
        };
        let card = render(&snapshot(), &config, Size::Large, now());
        let rows: Vec<&Value> = card["body"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "ColumnSet")
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row["columns"].as_array().unwrap().len() == 2));
    }

    #[test]
    fn config_round_trips_through_custom_state_and_tolerates_junk() {
        let config = Config {
            focus: Focus::Group("g1".into()),
            tiles: Tiles::Two,
        };
        assert_eq!(Config::from_state(&config.to_state()), config);
        assert_eq!(Config::from_state(""), Config::default());
        assert_eq!(
            Config::from_state(r#"{"focus":"provider:","tiles":"9"}"#),
            Config::default()
        );
    }

    fn custom(tiles: &[(&str, &str, TileMode)]) -> Result<WidgetSnapshot, LoadError> {
        use codexbar_store::widgets::{WidgetBuilder, WidgetTile};
        let mut snapshot = snapshot().unwrap();
        snapshot.accounts[0].metrics.push(WidgetMetric {
            key: "credits".into(),
            label: "Credits".into(),
            value: "$12.30 left".into(),
            used_percent: None,
            resets_at: None,
            status: None,
        });
        snapshot.accounts[0].metrics[0].resets_at = Some(now() + chrono::Duration::hours(50));
        snapshot.accounts[0].status = Some("At risk".into());
        snapshot.accounts[0].metrics[0].status = Some("At risk".into());
        let builder = WidgetBuilder {
            tiles: tiles
                .iter()
                .map(|(account, metric, mode)| WidgetTile {
                    account: (*account).into(),
                    metric: (*metric).into(),
                    mode: *mode,
                })
                .collect(),
            ..WidgetBuilder::default()
        };
        Ok(snapshot.with_builder(builder))
    }

    fn views(snapshot: &Result<WidgetSnapshot, LoadError>, tiles: Tiles, size: Size) -> Vec<TileView> {
        let config = Config {
            focus: Focus::Custom,
            tiles,
        };
        tile_views(snapshot.as_ref().unwrap(), &config, size, now()).unwrap()
    }

    #[test]
    fn custom_tiles_show_their_metric_in_each_display_mode() {
        let snapshot = custom(&[
            ("a", "weekly", TileMode::Automatic),
            ("a", "weekly", TileMode::Percent),
            ("a", "weekly", TileMode::Bar),
            ("a", "credits", TileMode::Balance),
            ("a", "weekly", TileMode::Status),
            ("b", "weekly", TileMode::Status),
        ]);
        let views = views(&snapshot, Tiles::Automatic, Size::Large);
        assert_eq!(views.len(), 6, "automatic on large shows all six");
        let [auto, percent, bar, balance, status, ok] = views.as_slice() else {
            panic!()
        };
        assert_eq!((auto.percent, auto.big.as_deref()), (Some(82.0), None));
        assert_eq!(auto.line.as_deref(), Some("Weekly: 82% used"));
        assert_eq!((percent.big.as_deref(), percent.percent), (Some("82%"), None));
        assert_eq!(
            bar.note.as_deref(),
            Some("Resets in 2d 2h"),
            "the full bar adds the reset"
        );
        assert_eq!(bar.percent, Some(82.0));
        assert_eq!(balance.big.as_deref(), Some("$12.30 left"));
        assert_eq!(status.big.as_deref(), Some("At risk"));
        assert_eq!(ok.big.as_deref(), Some("OK"), "a normal account reads OK");
        // Credits are fine although the account's weekly limit is at risk: the tile follows its own metric.
        let credits = TileView::of(
            &snapshot.as_ref().unwrap().accounts[0],
            Some("credits"),
            TileMode::Status,
            now(),
        );
        assert_eq!(credits.big.as_deref(), Some("OK"));
        assert!(views.iter().all(|view| view.account.is_some() && view.metric.is_some()));
    }

    #[test]
    fn automatic_money_tiles_show_the_amount_large_and_unavailable_tiles_keep_their_metric() {
        let mut snapshot = custom(&[("a", "credits", TileMode::Automatic), ("b", "weekly", TileMode::Bar)]);
        snapshot.as_mut().unwrap().accounts[1].health = WidgetHealth::Unavailable;
        let views = views(&snapshot, Tiles::Two, Size::Large);
        assert_eq!(views[0].big.as_deref(), Some("$12.30 left"));
        assert_eq!(views[0].line.as_deref(), Some("Credits"));
        assert_eq!(views[0].percent, None);
        assert_eq!(views[1].note.as_deref(), Some("Unavailable"));
        assert_eq!(
            views[1].metric.as_deref(),
            Some("weekly"),
            "the tap opens the tile's metric"
        );
    }

    #[test]
    fn layouts_limit_how_many_custom_tiles_show() {
        let snapshot = custom(&[
            ("a", "weekly", TileMode::Automatic),
            ("b", "weekly", TileMode::Automatic),
            ("c", "weekly", TileMode::Automatic),
            ("d", "weekly", TileMode::Automatic),
            ("e", "weekly", TileMode::Automatic),
        ]);
        assert_eq!(views(&snapshot, Tiles::One, Size::Large).len(), 1);
        assert_eq!(views(&snapshot, Tiles::Two, Size::Large).len(), 2);
        assert_eq!(views(&snapshot, Tiles::Four, Size::Small).len(), 4);
        assert_eq!(views(&snapshot, Tiles::Automatic, Size::Medium).len(), 2);
        assert_eq!(views(&snapshot, Tiles::Automatic, Size::Large).len(), 5);
    }

    #[test]
    fn removed_accounts_and_metrics_degrade_to_a_hint() {
        let snapshot = custom(&[
            ("gone", "weekly", TileMode::Bar),
            ("a", "monthly", TileMode::Percent),
            ("b", "weekly", TileMode::Automatic),
        ]);
        let views = views(&snapshot, Tiles::Four, Size::Large);
        assert_eq!(views[0].title, "Removed account");
        assert_eq!(views[0].account, None, "nothing to open");
        assert!(views[0].note.as_deref().unwrap().contains("Settings › Widgets"));
        assert_eq!(views[1].title, "claude a", "the account's current name");
        assert!(views[1].note.as_deref().unwrap().contains("isn't reported any more"));
        assert_eq!(views[2].line.as_deref(), Some("Weekly: 10% used"));
    }

    #[test]
    fn renamed_accounts_show_their_new_name() {
        let mut snapshot = custom(&[("a", "weekly", TileMode::Automatic)]).unwrap();
        snapshot.accounts[0].name = "Claude · Personal".into();
        let config = Config {
            focus: Focus::Custom,
            tiles: Tiles::One,
        };
        assert_eq!(
            tile_views(&snapshot, &config, Size::Medium, now()).unwrap()[0].title,
            "Claude · Personal"
        );
    }

    #[test]
    fn tapping_a_tile_opens_its_account_and_metric() {
        let snapshot = custom(&[("a", "credits", TileMode::Balance)]);
        let card = render(
            &snapshot,
            &Config {
                focus: Focus::Custom,
                tiles: Tiles::One,
            },
            Size::Medium,
            now(),
        );
        let tile = &card["body"][1];
        assert_eq!(tile["selectAction"]["verb"], verbs::OPEN);
        assert_eq!(
            tile["selectAction"]["data"],
            serde_json::json!({ "account": "a", "metric": "credits" })
        );
    }

    #[test]
    fn an_empty_builder_says_where_to_add_tiles() {
        let card = render(
            &custom(&[]),
            &Config {
                focus: Focus::Custom,
                tiles: Tiles::Automatic,
            },
            Size::Medium,
            now(),
        );
        assert!(all_text(&card).contains("Settings › Widgets"));
    }

    #[test]
    fn customize_offers_providers_once_and_groups_and_save_applies_it() {
        let card = customize(&snapshot(), &Config::default());
        let choices: Vec<&str> = card["body"][1]["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|choice| choice["value"].as_str().unwrap())
            .collect();
        assert_eq!(
            choices,
            [
                "all",
                "custom",
                "provider:claude",
                "provider:codex",
                "provider:copilot",
                "provider:cursor",
                "group:g1"
            ]
        );
        assert_eq!(card["body"][2]["choices"].as_array().unwrap().len(), 4);
        let saved = saved(&Config::default(), r#"{"focus":"group:g1","tiles":"4"}"#);
        assert_eq!(
            saved,
            Config {
                focus: Focus::Group("g1".into()),
                tiles: Tiles::Four
            }
        );
        assert_eq!(
            super::saved(&saved, "{}"),
            saved,
            "missing inputs keep the current settings"
        );
    }

    #[test]
    fn cards_are_adaptive_cards() {
        for card in [
            render(&snapshot(), &Config::default(), Size::Large, now()),
            render(&Err(LoadError::Missing), &Config::default(), Size::Small, now()),
            customize(&snapshot(), &Config::default()),
        ] {
            assert_eq!(card["type"], "AdaptiveCard");
            assert_eq!(card["version"], "1.5");
            assert!(card["body"].as_array().is_some_and(|body| !body.is_empty()));
        }
    }
}
