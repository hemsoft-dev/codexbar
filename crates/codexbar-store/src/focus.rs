//! What each of the selected account's focus cards shows (#218), by account id, in `dashboard.json`. An account
//! without a choice, or a card without one, keeps the automatic selection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The most rings the gauge card shows side by side; more would shrink each below the gauge's smallest ring.
pub const MAX_RINGS: usize = 3;

/// The usage card: the current window against the previous one, or the 14-day trend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageView {
    Window,
    Trend,
}

/// The activity card: requests by hour, or usage by day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityView {
    Hours,
    Days,
}

/// How a card draws a limit chosen for it (#230): over time, as a progress bar, or as a gauge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitView {
    Graph,
    Bar,
    Gauge,
}

/// One limit, by metric key, and how a card draws it (#230).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitChoice {
    pub key: String,
    pub view: LimitView,
}

/// The number of focus cards.
pub const CARDS: usize = 3;

/// One account's choices. Every field left empty means "automatic".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageView>,
    /// The limits the gauge card shows a ring for, by metric key, in the order chosen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ActivityView>,
    /// A limit chosen for each card, left to right (#230). A card with one shows that limit in its view instead of
    /// what the fields above choose for it.
    #[serde(default, skip_serializing_if = "no_limits")]
    pub limits: [Option<LimitChoice>; CARDS],
}

fn no_limits(limits: &[Option<LimitChoice>; CARDS]) -> bool {
    limits.iter().all(Option::is_none)
}

impl FocusChoice {
    /// Nothing chosen: every card is automatic.
    pub fn is_automatic(&self) -> bool {
        self == &Self::default()
    }

    /// Adds or removes `key`'s ring among the limits the account reports now (`available`). Chosen limits it no
    /// longer reports are dropped first, so they can't come back later, and a ring is added only while fewer than
    /// [`MAX_RINGS`] are chosen.
    pub fn toggle_ring_among(&mut self, key: &str, available: &[String]) {
        self.rings.retain(|ring| available.contains(ring));
        if self.rings.iter().any(|ring| ring == key) || self.rings.len() < MAX_RINGS {
            self.toggle_ring(key);
        }
    }

    /// Adds `key` to the rings, or removes it when it is already there.
    pub fn toggle_ring(&mut self, key: &str) {
        match self.rings.iter().position(|ring| ring == key) {
            Some(ix) => {
                self.rings.remove(ix);
            }
            None => self.rings.push(key.to_owned()),
        }
    }
}

/// Every account's choices. Saved as a whole when changed, like the layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FocusChoices(BTreeMap<String, FocusChoice>);

impl FocusChoices {
    /// Reads the saved map; anything unreadable gives no choices rather than an error.
    pub fn from_json(value: &serde_json::Value) -> Self {
        let mut choices: Self = serde_json::from_value(value.clone()).unwrap_or_default();
        choices.0.retain(|_, choice| !choice.is_automatic());
        choices
    }

    /// `account`'s choices; automatic when none are saved.
    pub fn of(&self, account: &str) -> FocusChoice {
        self.0.get(account).cloned().unwrap_or_default()
    }

    /// Changes `account`'s choices. An account whose cards are all automatic again is dropped from the map.
    pub fn update(&mut self, account: &str, change: impl FnOnce(&mut FocusChoice)) {
        let mut choice = self.of(account);
        change(&mut choice);
        if choice.is_automatic() {
            self.0.remove(account);
        } else {
            self.0.insert(account.to_owned(), choice);
        }
    }

    /// Carries an account's choices to its new id.
    pub fn rename_account(&mut self, from: &str, to: &str) -> bool {
        match self.0.remove(from) {
            Some(choice) => {
                self.0.insert(to.to_owned(), choice);
                true
            }
            None => false,
        }
    }

    /// Drops a removed account's choices.
    pub fn forget_account(&mut self, account: &str) -> bool {
        self.0.remove(account).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn choices_round_trip_and_automatic_accounts_are_not_stored() {
        let mut choices = FocusChoices::default();
        choices.update("claude-1", |choice| {
            choice.toggle_ring("5-hour-window");
            choice.toggle_ring("weekly");
            choice.usage = Some(UsageView::Trend);
        });
        choices.update("codex-1", |choice| choice.activity = Some(ActivityView::Days));
        let value = serde_json::to_value(&choices).unwrap();
        assert_eq!(
            value,
            json!({
                "claude-1": {"usage": "trend", "rings": ["5-hour-window", "weekly"]},
                "codex-1": {"activity": "days"}
            })
        );
        assert_eq!(FocusChoices::from_json(&value), choices);

        // Back to automatic: the account leaves the map.
        choices.update("codex-1", |choice| choice.activity = None);
        assert_eq!(choices.of("codex-1"), FocusChoice::default());
        assert_eq!(serde_json::to_value(&choices).unwrap().as_object().unwrap().len(), 1);
    }

    #[test]
    fn a_ring_toggles_and_keeps_the_chosen_order() {
        let mut choice = FocusChoice::default();
        choice.toggle_ring("weekly");
        choice.toggle_ring("5-hour-window");
        assert_eq!(choice.rings, ["weekly", "5-hour-window"]);
        choice.toggle_ring("weekly");
        assert_eq!(choice.rings, ["5-hour-window"]);
    }

    #[test]
    fn rings_drop_unreported_limits_and_stop_at_the_cap() {
        let available: Vec<String> = ["a", "b", "c", "d"].map(str::to_owned).to_vec();
        let mut choice = FocusChoice {
            rings: vec!["gone".into(), "a".into()],
            ..FocusChoice::default()
        };
        choice.toggle_ring_among("b", &available);
        assert_eq!(choice.rings, ["a", "b"], "the unreported limit went");
        choice.toggle_ring_among("c", &available);
        choice.toggle_ring_among("d", &available);
        assert_eq!(choice.rings, ["a", "b", "c"], "no fourth ring");
        choice.toggle_ring_among("a", &available);
        assert_eq!(choice.rings, ["b", "c"], "a chosen ring still comes off at the cap");
    }

    #[test]
    fn choices_follow_renamed_and_removed_accounts() {
        let mut choices = FocusChoices::default();
        choices.update("old", |choice| choice.usage = Some(UsageView::Window));
        assert!(choices.rename_account("old", "new"));
        assert_eq!(choices.of("new").usage, Some(UsageView::Window));
        assert!(choices.of("old").is_automatic());
        assert!(!choices.rename_account("old", "other"));
        assert!(choices.forget_account("new"));
        assert!(!choices.forget_account("new"));
    }

    #[test]
    fn an_unreadable_value_gives_no_choices() {
        assert_eq!(FocusChoices::from_json(&json!("nope")), FocusChoices::default());
        assert_eq!(
            FocusChoices::from_json(&json!({"a": {"usage": "sideways"}})),
            FocusChoices::default()
        );
        // Empty entries an older writer left are dropped.
        assert_eq!(FocusChoices::from_json(&json!({"a": {}})), FocusChoices::default());
    }

    #[test]
    fn a_chosen_limit_per_card_round_trips_and_older_files_still_read() {
        // Claude: the 5-hour window as a graph on the first card, Weekly as a gauge on the second (#230).
        let mut choices = FocusChoices::default();
        choices.update("claude-1", |choice| {
            choice.limits[0] = Some(LimitChoice {
                key: "5-hour-window".into(),
                view: LimitView::Graph,
            });
            choice.limits[1] = Some(LimitChoice {
                key: "weekly".into(),
                view: LimitView::Gauge,
            });
        });
        let value = serde_json::to_value(&choices).unwrap();
        assert_eq!(
            value,
            json!({"claude-1": {"limits": [
                {"key": "5-hour-window", "view": "graph"},
                {"key": "weekly", "view": "gauge"},
                null
            ]}})
        );
        assert_eq!(FocusChoices::from_json(&value), choices);

        // A file from before #230 has no limits: every card keeps its view.
        let older = FocusChoices::from_json(&json!({"codex-1": {"usage": "trend"}}));
        assert_eq!(older.of("codex-1").limits, [None, None, None]);
        assert_eq!(older.of("codex-1").usage, Some(UsageView::Trend));

        // Clearing the limits makes the account automatic again, and it leaves the map.
        choices.update("claude-1", |choice| choice.limits = Default::default());
        assert_eq!(choices, FocusChoices::default());
    }
}
