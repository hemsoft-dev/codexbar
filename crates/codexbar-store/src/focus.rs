//! What each of the selected account's focus cards shows (#218), by account id, in `dashboard.json`. An account
//! without a choice, or a card without one, keeps the automatic selection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
}

impl FocusChoice {
    /// Nothing chosen: every card is automatic.
    pub fn is_automatic(&self) -> bool {
        self == &Self::default()
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
}
