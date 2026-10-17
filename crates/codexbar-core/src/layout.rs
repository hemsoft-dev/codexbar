//! Account groups and the manual card order (#89). Pure: the dashboard keeps a `Layout`, the store persists it.
//!
//! Groups have a stable id and a unique name. Accounts belong to at most one group; the rest are Ungrouped, which
//! always comes last. The manual order is one list of account ids; each group shows its accounts in that order, and
//! accounts the order doesn't know yet follow in the order they are given (provider order), so new accounts land
//! deterministically. Memberships that point at a group that no longer exists read as Ungrouped.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use crate::Severity;

/// The longest group name, in characters.
pub const MAX_GROUP_NAME: usize = 40;
/// The name of the section for accounts without a group; no real group may take it.
pub const UNGROUPED: &str = "Ungrouped";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
}

/// Why a group change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    EmptyName,
    NameTooLong,
    DuplicateName,
    ReservedName,
    UnknownGroup,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyName => "Enter a group name.",
            Self::NameTooLong => "Group names can be up to 40 characters.",
            Self::DuplicateName => "A group with that name already exists.",
            Self::ReservedName => "“Ungrouped” is reserved for accounts without a group.",
            Self::UnknownGroup => "That group no longer exists.",
        })
    }
}

impl std::error::Error for LayoutError {}

/// One group's accounts in display order. `group` is `None` for Ungrouped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub group: Option<Group>,
    pub accounts: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    groups: Vec<Group>,
    /// Account id to group id.
    members: BTreeMap<String, String>,
    /// The manual order, account ids first to last.
    order: Vec<String>,
    mode: OrderMode,
}

/// How accounts are ordered within each group (#90). Groups keep their own order either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrderMode {
    /// Most urgent first, with the manual order breaking ties.
    #[default]
    Smart,
    /// The user's manual order.
    Manual,
}

impl OrderMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Manual => "manual",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Smart, Self::Manual].into_iter().find(|mode| mode.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Smart => "Smart",
            Self::Manual => "Manual",
        }
    }
}

/// Whether an account's shown usage is current (#76).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    #[default]
    Fresh,
    /// Last known usage: restored at startup, or kept after a failed refresh.
    Stale,
    /// No usage to show: loading, or the first fetch failed.
    Unavailable,
}

/// What Smart order ranks an account by, most important first: effective severity, the strongest alert that holds,
/// projected exhaustion, then health (current usage before last known, last known before none), then pressure.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Urgency {
    pub severity: Severity,
    /// 0 for none, 1 for a threshold alert, 2 for At risk, 3 for Limit soon.
    pub alert: u8,
    /// A limit is on pace to run out before it resets.
    pub projected: bool,
    pub health: Health,
    /// 0..=1, how close the account is to blocking.
    pub pressure: f64,
}

impl Urgency {
    /// `Less` when `self` is more urgent, so sorting ascending puts the most urgent first.
    pub fn rank(&self, other: &Self) -> Ordering {
        other
            .severity
            .cmp(&self.severity)
            .then(other.alert.cmp(&self.alert))
            .then(other.projected.cmp(&self.projected))
            .then(self.health.cmp(&other.health))
            .then(other.pressure.total_cmp(&self.pressure))
    }
}

impl Layout {
    /// A layout from stored parts. Duplicate group ids keep their first entry, duplicate names are made unique, and
    /// repeated ids in the order keep their first place, so a hand-edited file can't break the invariants.
    pub fn new(groups: Vec<Group>, members: BTreeMap<String, String>, order: Vec<String>) -> Self {
        let mut layout = Self::default();
        for group in groups {
            if group.id.is_empty() || layout.group(&group.id).is_some() {
                continue;
            }
            let mut name = clean(&group.name)
                .ok()
                .filter(|name| !is_reserved(name))
                .unwrap_or_else(|| "Unnamed group".to_owned());
            let base = name.clone();
            let mut n = 2;
            while layout.name_taken(&name, None) {
                name = format!("{base} ({n})");
                n += 1;
            }
            layout.groups.push(Group { id: group.id, name });
        }
        layout.members = members;
        for id in order {
            if !layout.order.contains(&id) {
                layout.order.push(id);
            }
        }
        layout
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Stored memberships, including stale ones (whose group was deleted elsewhere).
    pub fn members(&self) -> &BTreeMap<String, String> {
        &self.members
    }

    pub fn order(&self) -> &[String] {
        &self.order
    }

    pub fn mode(&self) -> OrderMode {
        self.mode
    }

    /// Switches between Smart and Manual. The manual order is kept either way, so switching back restores it.
    pub fn set_mode(&mut self, mode: OrderMode) {
        self.mode = mode;
    }

    /// Like `arrange`, but in Smart mode each section is ranked by `urgency`, most urgent first. The sort is stable,
    /// so accounts that rank the same keep their manual order.
    pub fn arrange_by(&self, accounts: &[String], urgency: impl Fn(&str) -> Urgency) -> Vec<Section> {
        let mut sections = self.arrange(accounts);
        if self.mode == OrderMode::Smart {
            for section in &mut sections {
                let mut ranked: Vec<(Urgency, String)> =
                    section.accounts.drain(..).map(|id| (urgency(&id), id)).collect();
                ranked.sort_by(|(a, _), (b, _)| a.rank(b));
                section.accounts = ranked.into_iter().map(|(_, id)| id).collect();
            }
        }
        sections
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// The account's group, or `None` for Ungrouped (also when its group no longer exists).
    pub fn group_of(&self, account: &str) -> Option<&Group> {
        self.members.get(account).and_then(|id| self.group(id))
    }

    /// Creates a group and returns its id.
    pub fn create_group(&mut self, name: &str) -> Result<String, LayoutError> {
        let name = clean(name)?;
        if self.name_taken(&name, None) {
            return Err(LayoutError::DuplicateName);
        }
        // Ids still named by a membership are taken too, so a stale membership never joins a new group.
        let next = self
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .chain(self.members.values().map(String::as_str))
            .filter_map(|id| id.strip_prefix('g')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("g{next}");
        self.groups.push(Group { id: id.clone(), name });
        Ok(id)
    }

    pub fn rename_group(&mut self, id: &str, name: &str) -> Result<(), LayoutError> {
        let name = clean(name)?;
        if self.group(id).is_none() {
            return Err(LayoutError::UnknownGroup);
        }
        if self.name_taken(&name, Some(id)) {
            return Err(LayoutError::DuplicateName);
        }
        if let Some(group) = self.groups.iter_mut().find(|group| group.id == id) {
            group.name = name;
        }
        Ok(())
    }

    /// Deletes a group. Its accounts become Ungrouped; no account is removed.
    pub fn delete_group(&mut self, id: &str) -> Result<(), LayoutError> {
        let before = self.groups.len();
        self.groups.retain(|group| group.id != id);
        if self.groups.len() == before {
            return Err(LayoutError::UnknownGroup);
        }
        self.members.retain(|_, group| group != id);
        Ok(())
    }

    /// Moves a group one place earlier (`-1`) or later (`1`). Returns true when it moved.
    pub fn move_group(&mut self, id: &str, delta: isize) -> bool {
        let Some(ix) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        let Some(target) = ix
            .checked_add_signed(delta)
            .filter(|target| *target < self.groups.len())
        else {
            return false;
        };
        self.groups.swap(ix, target);
        true
    }

    /// Puts an account in a group, or Ungrouped with `None`.
    pub fn assign(&mut self, account: &str, group: Option<&str>) -> Result<(), LayoutError> {
        match group {
            Some(id) if self.group(id).is_none() => Err(LayoutError::UnknownGroup),
            Some(id) => {
                self.members.insert(account.to_owned(), id.to_owned());
                Ok(())
            }
            None => {
                self.members.remove(account);
                Ok(())
            }
        }
    }

    /// Carries an account's group and place from an old id to a new one. Returns true when something changed.
    /// Drops a removed account's group membership and place in the order (#85). Returns true when something changed.
    pub fn forget_account(&mut self, account: &str) -> bool {
        let member = self.members.remove(account).is_some();
        let before = self.order.len();
        self.order.retain(|id| id != account);
        member || self.order.len() != before
    }

    pub fn rename_account(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        let mut changed = false;
        if let Some(group) = self.members.remove(from) {
            self.members.insert(to.to_owned(), group);
            changed = true;
        }
        if let Some(ix) = self.order.iter().position(|id| id == from) {
            self.order.retain(|id| id != to);
            let ix = self.order.iter().position(|id| id == from).unwrap_or(ix);
            self.order[ix] = to.to_owned();
            changed = true;
        }
        changed
    }

    /// Arranges `accounts` (in their default order) into sections: each group in its order, then Ungrouped. Every
    /// group gets a section, empty or not; every account appears exactly once.
    pub fn arrange(&self, accounts: &[String]) -> Vec<Section> {
        let rank = |id: &String| self.order.iter().position(|known| known == id).unwrap_or(usize::MAX);
        let mut sorted: Vec<(usize, usize, &String)> = accounts
            .iter()
            .enumerate()
            .map(|(given, id)| (rank(id), given, id))
            .collect();
        sorted.sort();
        let ordered = || sorted.iter().map(|(_, _, id)| *id);
        let mut sections: Vec<Section> = self
            .groups
            .iter()
            .map(|group| Section {
                group: Some(group.clone()),
                accounts: ordered()
                    .filter(|id| self.group_of(id).is_some_and(|of| of.id == group.id))
                    .cloned()
                    .collect(),
            })
            .collect();
        sections.push(Section {
            group: None,
            accounts: ordered().filter(|id| self.group_of(id).is_none()).cloned().collect(),
        });
        sections
    }

    /// Moves an account one place earlier (`-1`) or later (`1`) within its group, among `accounts` (the accounts
    /// shown, in default order). Accounts the order didn't know yet are written into it, so the result is stable.
    /// Returns true when it moved.
    pub fn move_account(&mut self, account: &str, delta: isize, accounts: &[String]) -> bool {
        let mut sections = self.arrange(accounts);
        let Some(section) = sections
            .iter_mut()
            .find(|section| section.accounts.iter().any(|id| id == account))
        else {
            return false;
        };
        let ix = section.accounts.iter().position(|id| id == account).unwrap_or_default();
        let Some(target) = ix
            .checked_add_signed(delta)
            .filter(|target| *target < section.accounts.len())
        else {
            return false;
        };
        section.accounts.swap(ix, target);
        let moved = section.accounts.clone();
        self.rewrite_section(sections, &moved);
        true
    }

    /// Puts `account` before `before` within its group, or last in its group when `before` is `None` (#178): a
    /// dropped row. `accounts` is the order the rows are shown in, which the group's new order starts from: under
    /// Smart order that differs from the manual order, and the drop means what the screen showed. Nothing changes,
    /// and false comes back, when either account is unknown, `before` is in another group, or the place is where the
    /// account already is.
    pub fn place_account(&mut self, account: &str, before: Option<&str>, accounts: &[String]) -> bool {
        let mut sections = self.arrange(accounts);
        let Some(section) = sections
            .iter_mut()
            .find(|section| section.accounts.iter().any(|id| id == account))
        else {
            return false;
        };
        section
            .accounts
            .sort_by_key(|id| accounts.iter().position(|shown| shown == id));
        let Some(from) = section.accounts.iter().position(|id| id == account) else {
            return false;
        };
        let target = match before {
            Some(before) => match section.accounts.iter().position(|id| id == before) {
                Some(ix) => ix,
                None => return false,
            },
            None => section.accounts.len(),
        };
        if target == from || target == from + 1 {
            return false;
        }
        let id = section.accounts.remove(from);
        let target = if target > from { target - 1 } else { target };
        section.accounts.insert(target, id);
        let moved = section.accounts.clone();
        self.rewrite_section(sections, &moved);
        true
    }

    /// Writes one section's new sequence (`moved`) into the manual order. Shown accounts the order didn't know join
    /// its end, in the order they are shown. Then only that section's accounts are rewritten, in their new sequence,
    /// within the slots they already held; every other account, shown or hidden, in this group or another, keeps its
    /// slot.
    fn rewrite_section(&mut self, sections: Vec<Section>, moved: &[String]) {
        let shown: Vec<String> = sections.into_iter().flat_map(|section| section.accounts).collect();
        let mut order = self.order.clone();
        order.extend(shown.iter().filter(|id| !self.order.contains(id)).cloned());
        let mut next = moved.iter();
        for slot in order.iter_mut() {
            if moved.contains(slot)
                && let Some(id) = next.next()
            {
                slot.clone_from(id);
            }
        }
        self.order = order;
    }

    fn name_taken(&self, name: &str, except: Option<&str>) -> bool {
        self.groups
            .iter()
            .any(|group| Some(group.id.as_str()) != except && group.name.to_lowercase() == name.to_lowercase())
    }
}

fn is_reserved(name: &str) -> bool {
    name.to_lowercase() == UNGROUPED.to_lowercase()
}

/// A trimmed, valid group name.
fn clean(name: &str) -> Result<String, LayoutError> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        Err(LayoutError::EmptyName)
    } else if name.chars().count() > MAX_GROUP_NAME {
        Err(LayoutError::NameTooLong)
    } else if is_reserved(&name) {
        Err(LayoutError::ReservedName)
    } else {
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|id| (*id).to_owned()).collect()
    }

    fn names(layout: &Layout) -> Vec<&str> {
        layout.groups().iter().map(|group| group.name.as_str()).collect()
    }

    fn shape(sections: &[Section]) -> Vec<(Option<&str>, Vec<&str>)> {
        sections
            .iter()
            .map(|section| {
                (
                    section.group.as_ref().map(|group| group.name.as_str()),
                    section.accounts.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn no_layout_keeps_every_account_ungrouped_in_default_order() {
        let sections = Layout::default().arrange(&ids(&["codex", "claude", "cursor"]));
        assert_eq!(shape(&sections), vec![(None, vec!["codex", "claude", "cursor"])]);
    }

    #[test]
    fn groups_are_created_renamed_and_named_uniquely() {
        let mut layout = Layout::default();
        let work = layout.create_group("  Work  ").unwrap();
        assert_eq!(work, "g1");
        assert_eq!(
            layout.create_group("work"),
            Err(LayoutError::DuplicateName),
            "case-insensitive"
        );
        assert_eq!(layout.create_group("   "), Err(LayoutError::EmptyName));
        assert_eq!(layout.create_group(&"x".repeat(41)), Err(LayoutError::NameTooLong));
        let home = layout.create_group("Home").unwrap();
        assert_eq!(layout.rename_group(&home, "WORK"), Err(LayoutError::DuplicateName));
        assert_eq!(layout.rename_group(&work, "Day   job"), Ok(()));
        assert_eq!(
            layout.rename_group(&work, "day job"),
            Ok(()),
            "renaming to itself in another case"
        );
        assert_eq!(layout.rename_group("g9", "Other"), Err(LayoutError::UnknownGroup));
        assert_eq!(names(&layout), vec!["day job", "Home"]);
    }

    #[test]
    fn deleting_a_group_moves_its_accounts_to_ungrouped() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        layout.assign("claude", Some(&work)).unwrap();
        layout.assign("codex", Some(&work)).unwrap();
        layout.delete_group(&work).unwrap();
        assert!(layout.members().is_empty());
        assert_eq!(
            shape(&layout.arrange(&ids(&["codex", "claude"]))),
            vec![(None, vec!["codex", "claude"])]
        );
        assert_eq!(layout.delete_group(&work), Err(LayoutError::UnknownGroup));
        // A new group never reuses an id still in use.
        let a = layout.create_group("A").unwrap();
        let b = layout.create_group("B").unwrap();
        layout.delete_group(&a).unwrap();
        assert_ne!(layout.create_group("C").unwrap(), b);
    }

    #[test]
    fn sections_follow_group_order_then_ungrouped() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        let home = layout.create_group("Home").unwrap();
        layout.assign("claude", Some(&home)).unwrap();
        layout.assign("codex", Some(&work)).unwrap();
        assert_eq!(layout.assign("cursor", Some("g9")), Err(LayoutError::UnknownGroup));
        let accounts = ids(&["codex", "claude", "cursor"]);
        assert_eq!(
            shape(&layout.arrange(&accounts)),
            vec![
                (Some("Work"), vec!["codex"]),
                (Some("Home"), vec!["claude"]),
                (None, vec!["cursor"]),
            ]
        );
        assert!(layout.move_group(&home, -1));
        assert!(!layout.move_group(&home, -1), "already first");
        assert_eq!(names(&layout), vec!["Home", "Work"]);
        layout.assign("codex", None).unwrap();
        assert_eq!(layout.group_of("codex"), None);
    }

    #[test]
    fn a_dropped_account_goes_before_its_target_or_last_in_its_group() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        for id in ["a", "b", "c"] {
            layout.assign(id, Some(&work)).unwrap();
        }
        let accounts = ids(&["a", "b", "c", "x", "y"]);
        // Under Manual order the rows are shown as the layout arranges them.
        let shown = |layout: &Layout| -> Vec<String> {
            layout
                .arrange(&accounts)
                .into_iter()
                .flat_map(|section| section.accounts)
                .collect()
        };
        assert!(layout.place_account("c", Some("a"), &shown(&layout)));
        assert_eq!(
            shape(&layout.arrange(&accounts)),
            vec![(Some("Work"), vec!["c", "a", "b"]), (None, vec!["x", "y"])]
        );
        assert!(
            layout.place_account("c", None, &shown(&layout)),
            "to the end of its group"
        );
        let settled = vec![(Some("Work"), vec!["a", "b", "c"]), (None, vec!["x", "y"])];
        assert_eq!(shape(&layout.arrange(&accounts)), settled);
        // Drops that would change nothing, or cross into another group, are refused and leave the order alone.
        let rows = shown(&layout);
        assert!(!layout.place_account("c", None, &rows), "already last");
        assert!(!layout.place_account("a", Some("b"), &rows), "already right before b");
        assert!(!layout.place_account("a", Some("a"), &rows), "onto itself");
        assert!(!layout.place_account("a", Some("x"), &rows), "x is in another group");
        assert!(!layout.place_account("a", Some("nope"), &rows), "unknown target");
        assert!(!layout.place_account("nope", None, &rows), "unknown account");
        assert_eq!(shape(&layout.arrange(&accounts)), settled);
        // The drop means what the screen showed: with Smart order showing Work as c, b, a, dropping a before b gives
        // c, a, b, not a move within the stale manual order a, b, c.
        assert!(layout.place_account("a", Some("b"), &ids(&["c", "b", "a", "x", "y"])));
        assert_eq!(
            shape(&layout.arrange(&accounts)),
            vec![(Some("Work"), vec!["c", "a", "b"]), (None, vec!["x", "y"])]
        );
        assert!(layout.place_account("c", None, &shown(&layout)));
        assert_eq!(shape(&layout.arrange(&accounts)), settled);
        // Ungrouped accounts drop the same way, and accounts the order didn't know keep following (#89).
        assert!(layout.place_account("y", Some("x"), &shown(&layout)));
        let more = ids(&["new", "a", "b", "c", "x", "y"]);
        assert_eq!(
            shape(&layout.arrange(&more)),
            vec![(Some("Work"), vec!["a", "b", "c"]), (None, vec!["y", "x", "new"])]
        );
    }

    #[test]
    fn manual_order_is_group_local_and_new_accounts_follow() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        for id in ["a", "b", "c"] {
            layout.assign(id, Some(&work)).unwrap();
        }
        let accounts = ids(&["a", "b", "c", "x", "y"]);
        assert!(layout.move_account("c", -1, &accounts));
        assert!(layout.move_account("c", -1, &accounts));
        assert!(!layout.move_account("c", -1, &accounts), "already first in its group");
        assert!(layout.move_account("y", -1, &accounts));
        assert_eq!(
            shape(&layout.arrange(&accounts)),
            vec![(Some("Work"), vec!["c", "a", "b"]), (None, vec!["y", "x"])]
        );
        // A new account joins the end of its section; known ones keep their places.
        let more = ids(&["new", "a", "b", "c", "x", "y"]);
        assert_eq!(
            shape(&layout.arrange(&more)),
            vec![(Some("Work"), vec!["c", "a", "b"]), (None, vec!["y", "x", "new"])]
        );
        // An account not shown right now keeps its place for when it comes back.
        assert!(layout.move_account("a", 1, &ids(&["a", "b", "c"])));
        assert!(layout.order().contains(&"y".to_owned()));
    }

    #[test]
    fn new_groups_never_take_an_id_a_stale_membership_names() {
        let members = BTreeMap::from([("claude".to_owned(), "g2".to_owned())]);
        let mut layout = Layout::new(
            vec![Group {
                id: "g1".into(),
                name: "Work".into(),
            }],
            members,
            vec![],
        );
        let new = layout.create_group("Home").unwrap();
        assert_eq!(new, "g3");
        assert_eq!(layout.group_of("claude"), None, "still Ungrouped");
    }

    #[test]
    fn hidden_accounts_keep_their_place_when_others_move() {
        let mut layout = Layout::new(vec![], BTreeMap::new(), ids(&["b", "a", "c"]));
        // b is hidden (say switched off); c moves above a.
        assert!(layout.move_account("c", -1, &ids(&["a", "c"])));
        assert_eq!(layout.order(), ids(&["b", "c", "a"]).as_slice());
    }

    #[test]
    fn moving_in_one_group_leaves_other_groups_order_alone() {
        let mut layout = Layout::new(
            vec![
                Group {
                    id: "g1".into(),
                    name: "One".into(),
                },
                Group {
                    id: "g2".into(),
                    name: "Two".into(),
                },
            ],
            BTreeMap::from([
                ("a".to_owned(), "g1".to_owned()),
                ("b".to_owned(), "g1".to_owned()),
                ("x".to_owned(), "g2".to_owned()),
                ("y".to_owned(), "g2".to_owned()),
            ]),
            ids(&["a", "y", "x", "b"]),
        );
        // x is hidden; b moves above a.
        assert!(layout.move_account("b", -1, &ids(&["a", "b", "y"])));
        assert_eq!(layout.order(), ids(&["b", "y", "x", "a"]).as_slice());
    }

    #[test]
    fn renaming_an_account_to_itself_changes_nothing() {
        let mut layout = Layout::new(vec![], BTreeMap::new(), ids(&["a", "openrouter"]));
        assert!(!layout.rename_account("openrouter", "openrouter"));
        assert_eq!(layout.order(), ids(&["a", "openrouter"]).as_slice());
    }

    #[test]
    fn ungrouped_is_a_reserved_name() {
        let mut layout = Layout::default();
        assert_eq!(layout.create_group("ungrouped"), Err(LayoutError::ReservedName));
        let work = layout.create_group("Work").unwrap();
        assert_eq!(
            layout.rename_group(&work, " Ungrouped "),
            Err(LayoutError::ReservedName)
        );
        let stored = Layout::new(
            vec![Group {
                id: "g4".into(),
                name: "Ungrouped".into(),
            }],
            BTreeMap::new(),
            vec![],
        );
        assert_eq!(
            names(&stored),
            vec!["Unnamed group"],
            "a stored group can't take it either"
        );
        // Nor through the repair fallback, whatever the stored id.
        let by_id = Layout::new(
            vec![Group {
                id: "Ungrouped".into(),
                name: String::new(),
            }],
            BTreeMap::new(),
            vec![],
        );
        assert_eq!(names(&by_id), vec!["Unnamed group"]);
    }

    fn urgency(severity: Severity) -> Urgency {
        Urgency {
            severity,
            ..Urgency::default()
        }
    }

    #[test]
    fn smart_order_ranks_within_groups_and_keeps_the_manual_order() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        for id in ["calm", "critical"] {
            layout.assign(id, Some(&work)).unwrap();
        }
        let accounts = ids(&["calm", "critical", "watch", "normal"]);
        let signals = |id: &str| match id {
            "critical" => urgency(Severity::LimitSoon),
            "watch" => urgency(Severity::Watch),
            _ => urgency(Severity::Normal),
        };
        assert_eq!(layout.mode(), OrderMode::Smart, "Smart by default");
        assert_eq!(
            shape(&layout.arrange_by(&accounts, signals)),
            vec![
                (Some("Work"), vec!["critical", "calm"]),
                (None, vec!["watch", "normal"])
            ],
            "groups stay intact"
        );
        layout.set_mode(OrderMode::Manual);
        assert_eq!(
            shape(&layout.arrange_by(&accounts, signals)),
            vec![
                (Some("Work"), vec!["calm", "critical"]),
                (None, vec!["watch", "normal"])
            ],
            "Manual ignores urgency"
        );
        assert!(layout.order().is_empty(), "Smart never rewrote the manual order");
    }

    #[test]
    fn alerts_projection_and_health_break_severity_ties_in_order() {
        let at = |alert: u8, projected: bool, health: Health, pressure: f64| Urgency {
            severity: Severity::Watch,
            alert,
            projected,
            health,
            pressure,
        };
        let layout = Layout::default();
        let accounts = ids(&[
            "plain",
            "stale",
            "failed",
            "projected",
            "alerted",
            "busier",
            "tie-a",
            "tie-b",
        ]);
        let signals = |id: &str| match id {
            "alerted" => at(2, false, Health::Fresh, 0.5),
            "projected" => at(1, true, Health::Fresh, 0.5),
            "busier" => at(1, false, Health::Fresh, 0.9),
            "stale" => at(1, false, Health::Stale, 0.9),
            "failed" => at(1, false, Health::Unavailable, 0.9),
            _ => at(1, false, Health::Fresh, 0.5),
        };
        let sections = layout.arrange_by(&accounts, signals);
        assert_eq!(
            sections[0].accounts,
            ids(&[
                "alerted",
                "projected",
                "busier",
                "plain",
                "tie-a",
                "tie-b",
                "stale",
                "failed"
            ]),
        );
    }

    #[test]
    fn a_forgotten_account_leaves_its_group_and_the_order() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        layout.assign("gone", Some(&work)).unwrap();
        layout.assign("kept", Some(&work)).unwrap();
        assert!(layout.move_account("gone", 1, &ids(&["gone", "kept"])));
        assert!(layout.forget_account("gone"));
        assert_eq!(layout.group_of("gone"), None);
        assert_eq!(layout.order(), ids(&["kept"]).as_slice());
        assert!(!layout.forget_account("gone"), "nothing left to forget");
    }

    #[test]
    fn stale_group_ids_read_as_ungrouped() {
        let members = BTreeMap::from([("claude".to_owned(), "g7".to_owned())]);
        let layout = Layout::new(vec![], members, vec![]);
        assert_eq!(layout.group_of("claude"), None);
        assert_eq!(shape(&layout.arrange(&ids(&["claude"]))), vec![(None, vec!["claude"])]);
    }

    #[test]
    fn stored_layouts_are_repaired_not_trusted() {
        let group = |id: &str, name: &str| Group {
            id: id.into(),
            name: name.into(),
        };
        let layout = Layout::new(
            vec![
                group("g1", "Work"),
                group("g1", "Dup id"),
                group("g2", "work"),
                group("", "No id"),
                group("g3", "  "),
            ],
            BTreeMap::new(),
            ids(&["a", "b", "a"]),
        );
        assert_eq!(names(&layout), vec!["Work", "work (2)", "Unnamed group"]);
        assert_eq!(layout.order(), ids(&["a", "b"]).as_slice());
    }

    #[test]
    fn renaming_an_account_keeps_its_group_and_place() {
        let mut layout = Layout::default();
        let work = layout.create_group("Work").unwrap();
        layout.assign("old", Some(&work)).unwrap();
        let accounts = ids(&["x", "old"]);
        layout.assign("x", Some(&work)).unwrap();
        assert!(layout.move_account("old", -1, &accounts));
        assert!(layout.rename_account("old", "new"));
        assert_eq!(layout.group_of("new").map(|g| g.name.as_str()), Some("Work"));
        assert_eq!(
            shape(&layout.arrange(&ids(&["x", "new"]))),
            vec![(Some("Work"), vec!["new", "x"]), (None, vec![])]
        );
        assert!(!layout.rename_account("missing", "other"));
    }
}
