//! Pure measurement policy. This module never resolves ownership or changes qualification.
//! Raw unresolved tuples are intentionally unrepresentable in the serialized anonymous categories.
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CAP: usize = 64;

#[derive(Clone, Copy)]
pub struct Event {
    pub hwnd: u64,
    pub thread: u32,
    pub kind: u32,
    pub generated_ms: u32,
    pub received_ms: u64,
    pub callback_sequence: u64,
}

/// Supplied only AFTER independent window queries match a retained owned process identity.
#[derive(Clone, Copy)]
pub struct OwnedObservation {
    pub hwnd: u64,
    pub pid: u32,
    pub birth: u64,
    pub creator_thread: u32,
    pub received_ms: u64,
    pub evidence_event_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    WindowOwner,
    ThreadOpen,
    ThreadOwner,
    CacheFallback,
    OwnedFilter,
    Metadata,
    Identity,
    EventCap,
    Accepted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    Unresolved,
    OutsideOwned,
    NotTopLevel,
    QueryFailed,
    MissingMetadata,
    IdentityMismatch,
    CapReached,
    Accepted,
}

#[derive(Clone, Copy)]
pub struct Decision {
    pub stage: Stage,
    pub reason: Reason,
    pub unresolved_before: usize,
    pub unresolved_after: usize,
    pub cached_creator_thread_match: Option<bool>,
    pub observation_sequence_before_resolution: u64,
    /// Fresh record() result for THIS decision, never a token obtained just by cached HWND/time matching.
    pub independent_observation: Option<u64>,
}

#[derive(Clone, Copy)]
struct History {
    observation: OwnedObservation,
    epoch: u64,
    first_sequence: u64,
    last_sequence: u64,
    first_ms: u64,
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Category {
    kind: &'static str,
    cache: &'static str,
    thread_match: Option<bool>,
    order: &'static str,
    stage: Stage,
    reason: Reason,
}

pub struct Diagnostics {
    history: BTreeMap<u64, History>,
    categories: BTreeMap<Category, u64>,
    validated_events: Vec<Value>,
    sequence: u64,
    epoch: u64,
    decisions: u64,
    unresolved_decisions: u64,
    last_unresolved_after: usize,
    counter_order_valid: bool,
    truncated: bool,
    invalid_observations: u64,
}
impl Diagnostics {
    pub fn new() -> Self {
        Self {
            history: BTreeMap::new(),
            categories: BTreeMap::new(),
            validated_events: Vec::new(),
            sequence: 0,
            epoch: 0,
            decisions: 0,
            unresolved_decisions: 0,
            last_unresolved_after: 0,
            counter_order_valid: true,
            truncated: false,
            invalid_observations: 0,
        }
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn observe_owned(&mut self, observation: OwnedObservation) {
        if observation.hwnd == 0 || observation.pid == 0 || observation.birth == 0 || observation.creator_thread == 0 {
            self.invalid_observations += 1;
            return;
        }
        self.sequence += 1;
        if !self.history.contains_key(&observation.hwnd) && self.history.len() == CAP {
            self.truncated = true;
            return;
        }
        let previous = self.history.get(&observation.hwnd);
        let same_identity = previous.is_some_and(|p| {
            p.observation.pid == observation.pid
                && p.observation.birth == observation.birth
                && p.observation.creator_thread == observation.creator_thread
        });
        if !same_identity {
            self.epoch += 1;
        }
        let (epoch, first_sequence, first_ms) = previous
            .filter(|_| same_identity)
            .map(|p| (p.epoch, p.first_sequence, p.first_ms))
            .unwrap_or((self.epoch, self.sequence, observation.received_ms));
        self.history.insert(
            observation.hwnd,
            History {
                observation,
                epoch,
                first_sequence,
                last_sequence: self.sequence,
                first_ms,
            },
        );
    }
    pub fn record(&mut self, event: Event, decision: Decision) {
        self.decisions += 1;
        if decision.reason == Reason::Unresolved {
            self.unresolved_decisions += 1;
            self.counter_order_valid &= decision.unresolved_after == decision.unresolved_before.saturating_add(1)
                && decision.unresolved_before == self.last_unresolved_after;
            self.last_unresolved_after = decision.unresolved_after;
        } else {
            self.counter_order_valid &= decision.unresolved_after == decision.unresolved_before;
        }
        let history = self.history.get(&event.hwnd);
        let category = Category {
            kind: match event.kind {
                0x8000 => "create",
                0x8001 => "destroy",
                0x8002 => "show",
                0x8003 => "hide",
                _ => "other",
            },
            cache: match (decision.cached_creator_thread_match, history) {
                (None, _) => "miss",
                (Some(_), Some(_)) => "independently-validated-owned-history",
                (Some(_), None) => "owned-history-unavailable-at-diagnostic-cap",
            },
            thread_match: decision.cached_creator_thread_match,
            order: history.map_or("no-owned-history", |h| order(event, h)),
            stage: decision.stage,
            reason: decision.reason,
        };
        if let Some(count) = self.categories.get_mut(&category) {
            *count += 1;
        } else if self.categories.len() < CAP {
            self.categories.insert(category, 1);
        } else {
            self.truncated = true;
        }

        // Unknown, stale, cache-only, foreign and mismatched events NEVER serialize raw tuple fields.
        // Even a matching HWND/thread/time cannot mint this fresh independent-observation token.
        if decision.reason == Reason::Accepted
            && let Some(h) = history
            && decision.cached_creator_thread_match == Some(true)
            && decision.independent_observation == Some(h.last_sequence)
            && h.last_sequence > decision.observation_sequence_before_resolution
            && h.first_sequence <= decision.observation_sequence_before_resolution
            && h.observation.creator_thread == event.thread
        {
            if self.validated_events.len() == CAP {
                self.truncated = true;
                return;
            }
            self.validated_events.push(json!({
                "cache_validation": "fresh-independent-owned-window-observation-at-event-processing",
                "event_ownership_proof": false, "hwnd": h.observation.hwnd,
                "retained_pid": h.observation.pid, "retained_process_birth": h.observation.birth,
                "creator_thread": h.observation.creator_thread, "event_thread": event.thread,
                "event_kind": event.kind, "event_generated_ms": event.generated_ms,
                "event_received_ms": event.received_ms, "callback_sequence": event.callback_sequence,
                "observed_identity_epoch": h.epoch, "observation_sequence": h.last_sequence,
                "evidence_event_index": h.observation.evidence_event_index,
                "complete_hwnd_generation_proof": false,
            }));
        }
    }
    pub fn report(&self) -> Value {
        let anonymous: Vec<_> = self.categories.iter().map(|(key, count)| json!({
            "event_kind": key.kind, "cache": key.cache, "cached_creator_thread_match": key.thread_match,
            "event_ownership_proof": false,
            "generation_time_order_vs_owned_observations": key.order,
            "failure_stage": format!("{:?}", key.stage), "decision_reason": format!("{:?}", key.reason), "count": count,
        })).collect();
        let history: Vec<_> = self.history.values().map(|h| json!({
            "ownership_basis": "historical-independent-owned-window-observation-only",
            "event_ownership_proof": false, "hwnd": h.observation.hwnd, "retained_pid": h.observation.pid,
            "retained_process_birth": h.observation.birth, "creator_thread": h.observation.creator_thread,
            "observed_identity_epoch": h.epoch, "first_observation_sequence": h.first_sequence,
            "last_observation_sequence": h.last_sequence, "first_observed_ms": h.first_ms,
            "last_observed_ms": h.observation.received_ms, "evidence_event_index": h.observation.evidence_event_index,
            "complete_hwnd_generation_proof": false,
        })).collect();
        json!({"measurement_only": true, "changes_qualification": false,
            "anonymous_categories": anonymous, "historical_owned_cache": history,
            "events_with_fresh_owned_cache_observation": self.validated_events,
            "decisions": self.decisions, "unresolved_decisions": self.unresolved_decisions,
            "last_unresolved_count_after_recording": self.last_unresolved_after,
            "counter_increment_preceded_diagnostic": self.counter_order_valid,
            "per_collection_cap": CAP, "truncated": self.truncated,
            "invalid_owned_observations_rejected": self.invalid_observations,
            "timing_order_limit": "Modular millisecond timestamp comparison only; coarse clock-basis diagnostic, never ownership or generation proof",
            "chronology_limit": "Observed sequence/identity changes only; no complete HWND generation or broker ownership claim"})
    }
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}
fn order(event: Event, history: &History) -> &'static str {
    let lag = (event.received_ms as u32).wrapping_sub(event.generated_ms);
    let Some(generated) = event.received_ms.checked_sub(u64::from(lag)) else {
        return "unresolvable-clock-wrap";
    };
    if generated < history.first_ms {
        "before-first-owned-observation"
    } else if generated > history.observation.received_ms {
        "after-last-owned-observation"
    } else {
        "within-owned-observation-span"
    }
}
