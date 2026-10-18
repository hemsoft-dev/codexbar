//! The widget provider's state (#94): the widgets the host has pinned, their settings and size, which are on screen,
//! and which are being customized. Each call returns what to send to the widget host. Pure; `super` wires it to COM.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use codexbar_store::widgets::{LoadError, WidgetSnapshot};
use serde_json::Value;

use super::cards::{self, Config, Size, verbs};

/// The widget definition in `packaging/AppxManifest.xml`.
pub const DEFINITION: &str = "CodexBar_Usage";

/// What to send the widget host for one widget.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub id: String,
    pub card: Value,
    pub custom_state: String,
}

/// What an action asks the provider to do besides updating the widget.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Nothing,
    Update(Update),
    /// Start CodexBar, focused on an account and metric when a tile was tapped (#95).
    Open(Option<Target>),
}

/// The account (and metric) a tapped tile asks CodexBar to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub account: String,
    pub metric: Option<String>,
}

impl Target {
    /// The target in an Open action's data (`{"account": ..., "metric": ...}`), if it names an account.
    pub fn from_data(data: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(data).ok()?;
        let account = value.get("account")?.as_str().filter(|account| !account.is_empty())?;
        Some(Self {
            account: account.to_owned(),
            metric: value
                .get("metric")
                .and_then(Value::as_str)
                .filter(|metric| !metric.is_empty())
                .map(str::to_owned),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Widget {
    size: Size,
    config: Config,
    active: bool,
    customizing: bool,
}

#[derive(Default)]
pub struct Host {
    widgets: BTreeMap<String, Widget>,
}

impl Host {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.widgets.is_empty()
    }

    pub fn any_active(&self) -> bool {
        self.widgets.values().any(|widget| widget.active)
    }

    fn update(id: &str, widget: &Widget, snapshot: &Result<WidgetSnapshot, LoadError>, now: DateTime<Utc>) -> Update {
        let card = if widget.customizing {
            cards::customize(snapshot, &widget.config)
        } else {
            cards::render(snapshot, &widget.config, widget.size, now)
        };
        Update {
            id: id.to_owned(),
            card,
            custom_state: widget.config.to_state(),
        }
    }

    /// A widget the host already has, after the provider restarted: its settings come back from the custom state.
    pub fn restore(&mut self, id: &str, definition: &str, size: Size, custom_state: &str, active: bool) {
        if definition != DEFINITION {
            return;
        }
        self.widgets.insert(
            id.to_owned(),
            Widget {
                size,
                config: Config::from_state(custom_state),
                active,
                customizing: false,
            },
        );
    }

    /// A newly pinned widget. It starts with the default settings and is sent its first card at once.
    pub fn create(
        &mut self,
        id: &str,
        definition: &str,
        size: Size,
        snapshot: &Result<WidgetSnapshot, LoadError>,
        now: DateTime<Utc>,
    ) -> Option<Update> {
        if definition != DEFINITION {
            return None;
        }
        let widget = Widget {
            size,
            config: Config::default(),
            active: true,
            customizing: false,
        };
        let update = Self::update(id, &widget, snapshot, now);
        self.widgets.insert(id.to_owned(), widget);
        Some(update)
    }

    pub fn delete(&mut self, id: &str) {
        self.widgets.remove(id);
    }

    pub fn activate(
        &mut self,
        id: &str,
        size: Size,
        snapshot: &Result<WidgetSnapshot, LoadError>,
        now: DateTime<Utc>,
    ) -> Option<Update> {
        let widget = self.widgets.get_mut(id)?;
        widget.active = true;
        widget.size = size;
        Some(Self::update(id, widget, snapshot, now))
    }

    pub fn deactivate(&mut self, id: &str) {
        if let Some(widget) = self.widgets.get_mut(id) {
            widget.active = false;
        }
    }

    pub fn resize(
        &mut self,
        id: &str,
        size: Size,
        snapshot: &Result<WidgetSnapshot, LoadError>,
        now: DateTime<Utc>,
    ) -> Option<Update> {
        let widget = self.widgets.get_mut(id)?;
        widget.size = size;
        Some(Self::update(id, widget, snapshot, now))
    }

    pub fn customize(
        &mut self,
        id: &str,
        snapshot: &Result<WidgetSnapshot, LoadError>,
        now: DateTime<Utc>,
    ) -> Option<Update> {
        let widget = self.widgets.get_mut(id)?;
        widget.customizing = true;
        Some(Self::update(id, widget, snapshot, now))
    }

    /// A button or tap on a widget. `data` is the card's inputs as JSON.
    pub fn action(
        &mut self,
        id: &str,
        verb: &str,
        data: &str,
        snapshot: &Result<WidgetSnapshot, LoadError>,
        now: DateTime<Utc>,
    ) -> Outcome {
        if verb == verbs::OPEN {
            return Outcome::Open(Target::from_data(data));
        }
        let Some(widget) = self.widgets.get_mut(id) else {
            return Outcome::Nothing;
        };
        match verb {
            verbs::SAVE if widget.customizing => {
                widget.config = cards::saved(&widget.config, data);
                widget.customizing = false;
            }
            verbs::CANCEL if widget.customizing => widget.customizing = false,
            _ => return Outcome::Nothing,
        }
        Outcome::Update(Self::update(id, widget, snapshot, now))
    }

    /// New cards for every widget on screen, after the snapshot changed or time moved on.
    pub fn refresh(&self, snapshot: &Result<WidgetSnapshot, LoadError>, now: DateTime<Utc>) -> Vec<Update> {
        self.widgets
            .iter()
            .filter(|(_, widget)| widget.active && !widget.customizing)
            .map(|(id, widget)| Self::update(id, widget, snapshot, now))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::cards::{Focus, Tiles};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn snapshot() -> Result<WidgetSnapshot, LoadError> {
        Ok(WidgetSnapshot::new(now(), vec![], vec![]))
    }

    #[test]
    fn a_pinned_widget_gets_its_card_and_default_settings() {
        let mut host = Host::default();
        let update = host.create("w1", DEFINITION, Size::Medium, &snapshot(), now()).unwrap();
        assert_eq!(update.id, "w1");
        assert_eq!(update.card["type"], "AdaptiveCard");
        assert_eq!(Config::from_state(&update.custom_state), Config::default());
        assert!(host.any_active());
        assert_eq!(
            host.create("w2", "Someone_Else", Size::Medium, &snapshot(), now()),
            None
        );
    }

    #[test]
    fn only_widgets_on_screen_refresh() {
        let mut host = Host::default();
        host.create("w1", DEFINITION, Size::Small, &snapshot(), now());
        host.create("w2", DEFINITION, Size::Large, &snapshot(), now());
        host.deactivate("w2");
        let ids: Vec<String> = host
            .refresh(&snapshot(), now())
            .into_iter()
            .map(|update| update.id)
            .collect();
        assert_eq!(ids, ["w1"]);
        assert!(host.activate("w2", Size::Large, &snapshot(), now()).is_some());
        assert_eq!(host.refresh(&snapshot(), now()).len(), 2);
        host.delete("w1");
        host.delete("w2");
        assert!(host.is_empty());
        assert!(
            host.activate("w1", Size::Small, &snapshot(), now()).is_none(),
            "deleted widgets stay deleted"
        );
    }

    #[test]
    fn customize_save_keeps_the_settings_in_custom_state() {
        let mut host = Host::default();
        host.create("w1", DEFINITION, Size::Medium, &snapshot(), now());
        let update = host.customize("w1", &snapshot(), now()).unwrap();
        assert_eq!(update.card["body"][0]["text"], "Customize CodexBar");
        assert!(
            host.refresh(&snapshot(), now()).is_empty(),
            "a refresh doesn't replace the customize card"
        );
        let Outcome::Update(update) = host.action(
            "w1",
            verbs::SAVE,
            r#"{"focus":"provider:claude","tiles":"2"}"#,
            &snapshot(),
            now(),
        ) else {
            panic!("save updates the widget");
        };
        assert_eq!(
            Config::from_state(&update.custom_state),
            Config {
                focus: Focus::Provider("claude".into()),
                tiles: Tiles::Two
            }
        );
        assert_ne!(update.card["body"][0]["text"], "Customize CodexBar");
        assert_eq!(
            host.action("w1", verbs::SAVE, "{}", &snapshot(), now()),
            Outcome::Nothing,
            "not customizing"
        );
    }

    #[test]
    fn cancel_leaves_the_settings_alone() {
        let mut host = Host::default();
        host.restore(
            "w1",
            DEFINITION,
            Size::Medium,
            r#"{"focus":"group:g1","tiles":"4"}"#,
            true,
        );
        host.customize("w1", &snapshot(), now());
        let Outcome::Update(update) = host.action("w1", verbs::CANCEL, "", &snapshot(), now()) else {
            panic!("cancel returns to the widget");
        };
        assert_eq!(
            Config::from_state(&update.custom_state),
            Config {
                focus: Focus::Group("g1".into()),
                tiles: Tiles::Four
            }
        );
    }

    #[test]
    fn open_works_from_any_widget_and_unknown_verbs_do_nothing() {
        let mut host = Host::default();
        assert_eq!(
            host.action("unknown", verbs::OPEN, "", &snapshot(), now()),
            Outcome::Open(None)
        );
        host.create("w1", DEFINITION, Size::Medium, &snapshot(), now());
        assert_eq!(host.action("w1", "dance", "", &snapshot(), now()), Outcome::Nothing);
    }

    #[test]
    fn a_tapped_tile_opens_its_account_and_metric() {
        let mut host = Host::default();
        let data = r#"{"account":"claude-1","metric":"weekly"}"#;
        assert_eq!(
            host.action("w1", verbs::OPEN, data, &snapshot(), now()),
            Outcome::Open(Some(Target {
                account: "claude-1".into(),
                metric: Some("weekly".into())
            }))
        );
        let data = r#"{"account":"claude-1","metric":null}"#;
        assert_eq!(
            host.action("w1", verbs::OPEN, data, &snapshot(), now()),
            Outcome::Open(Some(Target {
                account: "claude-1".into(),
                metric: None
            }))
        );
        assert_eq!(
            host.action("w1", verbs::OPEN, r#"{"account":""}"#, &snapshot(), now()),
            Outcome::Open(None)
        );
    }

    #[test]
    fn resizing_redraws_for_the_new_size() {
        let mut host = Host::default();
        host.create("w1", DEFINITION, Size::Small, &snapshot(), now());
        assert!(host.resize("w1", Size::Large, &snapshot(), now()).is_some());
        assert!(host.resize("nope", Size::Large, &snapshot(), now()).is_none());
    }

    #[test]
    fn restored_widgets_of_other_definitions_are_ignored() {
        let mut host = Host::default();
        host.restore("w1", "Other", Size::Small, "", true);
        assert!(host.is_empty());
    }
}
