//! Portable privacy/measurement policy tests. No native process, console, GUI or provider is launched.
#[path = "command_runner/windows_console/diagnostics.rs"]
mod diagnostics;
use diagnostics::{Decision, Diagnostics, Event, OwnedObservation, Reason, Stage};

fn event() -> Event {
    Event {
        hwnd: 918273645,
        thread: 81726354,
        kind: 0x8001,
        generated_ms: 160,
        received_ms: 180,
        callback_sequence: 7,
    }
}
fn observation() -> OwnedObservation {
    OwnedObservation {
        hwnd: event().hwnd,
        pid: 7654321,
        birth: 12345678901234,
        creator_thread: event().thread,
        received_ms: 150,
        evidence_event_index: 11,
    }
}
fn unresolved(before: usize) -> Decision {
    Decision {
        stage: Stage::ThreadOpen,
        reason: Reason::Unresolved,
        unresolved_before: before,
        unresolved_after: before + 1,
        cached_creator_thread_match: None,
        observation_sequence_before_resolution: 0,
        independent_observation: None,
    }
}

#[test]
fn cache_misses_are_bounded_anonymous_aggregates_before_discard() {
    let mut diagnostics = Diagnostics::new();
    for index in 0..10_000 {
        diagnostics.record(
            Event {
                hwnd: event().hwnd + index as u64,
                thread: event().thread + index as u32,
                ..event()
            },
            unresolved(index),
        );
    }
    let report = diagnostics.report();
    assert_eq!(report["anonymous_categories"].as_array().unwrap().len(), 1);
    assert_eq!(report["anonymous_categories"][0]["count"], 10_000);
    assert_eq!(report["unresolved_decisions"], 10_000);
    assert_eq!(report["last_unresolved_count_after_recording"], 10_000);
    assert_eq!(report["counter_increment_preceded_diagnostic"], true);
    assert_eq!(report["historical_owned_cache"], serde_json::json!([]));
    assert_eq!(
        report["events_with_fresh_owned_cache_observation"],
        serde_json::json!([])
    );
    let text = report.to_string();
    for private in [
        "918273645",
        "81726354",
        "7654321",
        "12345678901234",
        "callback_sequence",
        "event_generated_ms",
    ] {
        assert!(
            !text.contains(private),
            "unmatched raw field escaped redaction: {private}"
        );
    }
}

#[test]
fn historical_owned_cache_does_not_attribute_unresolved_or_mismatched_events() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe_owned(observation());
    diagnostics.record(
        Event {
            thread: event().thread + 1,
            ..event()
        },
        Decision {
            cached_creator_thread_match: Some(false),
            ..unresolved(0)
        },
    );
    let report = diagnostics.report();
    let category = &report["anonymous_categories"][0];
    assert_eq!(category["cached_creator_thread_match"], false);
    assert_eq!(category["event_kind"], "destroy");
    assert_eq!(category["failure_stage"], "ThreadOpen");
    assert_eq!(
        category["generation_time_order_vs_owned_observations"],
        "after-last-owned-observation"
    );
    assert!(category.get("hwnd").is_none());
    assert!(category.get("event_thread").is_none());
    assert_eq!(
        report["events_with_fresh_owned_cache_observation"],
        serde_json::json!([])
    );
    let historical = &report["historical_owned_cache"][0];
    assert_eq!(historical["event_ownership_proof"], false);
    assert_eq!(historical["complete_hwnd_generation_proof"], false);
    assert_eq!(historical["evidence_event_index"], 11);
    assert!(!report.to_string().contains(&(event().thread + 1).to_string()));
}

#[test]
fn only_fresh_independent_owned_observation_allows_raw_event_evidence() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe_owned(observation());
    let floor = diagnostics.sequence();
    diagnostics.observe_owned(observation());
    diagnostics.record(
        event(),
        Decision {
            stage: Stage::Accepted,
            reason: Reason::Accepted,
            unresolved_before: 0,
            unresolved_after: 0,
            cached_creator_thread_match: Some(true),
            observation_sequence_before_resolution: floor,
            independent_observation: Some(diagnostics.sequence()),
        },
    );
    let report = diagnostics.report();
    let raw = &report["events_with_fresh_owned_cache_observation"][0];
    assert_eq!(raw["event_ownership_proof"], false);
    assert_eq!(raw["hwnd"], event().hwnd);
    assert_eq!(raw["retained_process_birth"], observation().birth);
    assert_eq!(raw["callback_sequence"], event().callback_sequence);
    assert_eq!(raw["evidence_event_index"], 11);
    assert_eq!(raw["complete_hwnd_generation_proof"], false);
    assert_eq!(report["changes_qualification"], false);
    let floor = diagnostics.sequence();
    diagnostics.observe_owned(observation());
    diagnostics.record(
        event(),
        Decision {
            stage: Stage::Accepted,
            reason: Reason::Accepted,
            unresolved_before: 0,
            unresolved_after: 0,
            cached_creator_thread_match: None,
            observation_sequence_before_resolution: floor,
            independent_observation: Some(diagnostics.sequence()),
        },
    );
    assert_eq!(
        diagnostics.report()["events_with_fresh_owned_cache_observation"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "a cache-miss decision stays anonymous even when a new owned observation was made later"
    );
}

#[test]
fn stale_token_and_reused_window_thread_or_process_identity_cannot_export_raw_tuple() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe_owned(observation());
    let old = diagnostics.sequence();
    diagnostics.record(
        event(),
        Decision {
            stage: Stage::CacheFallback,
            reason: Reason::Accepted,
            unresolved_before: 0,
            unresolved_after: 0,
            cached_creator_thread_match: Some(true),
            observation_sequence_before_resolution: old,
            independent_observation: Some(old),
        },
    );
    diagnostics.observe_owned(OwnedObservation {
        creator_thread: event().thread + 1,
        birth: observation().birth + 1,
        ..observation()
    });
    diagnostics.record(
        event(),
        Decision {
            stage: Stage::Identity,
            reason: Reason::IdentityMismatch,
            unresolved_before: 0,
            unresolved_after: 0,
            cached_creator_thread_match: Some(false),
            observation_sequence_before_resolution: old,
            independent_observation: Some(old),
        },
    );
    diagnostics.record(
        event(),
        Decision {
            stage: Stage::Accepted,
            reason: Reason::Accepted,
            unresolved_before: 0,
            unresolved_after: 0,
            cached_creator_thread_match: Some(true),
            observation_sequence_before_resolution: old,
            independent_observation: Some(diagnostics.sequence()),
        },
    );
    let report = diagnostics.report();
    assert_eq!(
        report["events_with_fresh_owned_cache_observation"],
        serde_json::json!([])
    );
    assert_eq!(report["historical_owned_cache"][0]["observed_identity_epoch"], 2);
    assert_eq!(
        report["historical_owned_cache"][0]["complete_hwnd_generation_proof"],
        false
    );
}

#[test]
fn delayed_destroy_and_orphan_remain_anonymous_without_new_validation() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe_owned(observation());
    diagnostics.record(
        Event {
            generated_ms: 140,
            received_ms: 5000,
            ..event()
        },
        Decision {
            cached_creator_thread_match: Some(true),
            ..unresolved(0)
        },
    );
    diagnostics.record(
        Event {
            hwnd: 99887766,
            ..event()
        },
        unresolved(1),
    );
    let report = diagnostics.report();
    assert_eq!(report["unresolved_decisions"], 2);
    assert_eq!(report["counter_increment_preceded_diagnostic"], true);
    assert_eq!(
        report["events_with_fresh_owned_cache_observation"],
        serde_json::json!([])
    );
    assert!(
        report["anonymous_categories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["generation_time_order_vs_owned_observations"] == "before-first-owned-observation")
    );
    assert!(!report.to_string().contains("99887766"));
}

#[test]
fn diagnostic_counter_detects_recording_before_original_unresolved_increment() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.record(
        event(),
        Decision {
            unresolved_after: 0,
            ..unresolved(0)
        },
    );
    assert_eq!(diagnostics.report()["counter_increment_preceded_diagnostic"], false);
}

#[test]
fn raw_history_and_category_collections_have_finite_caps() {
    let mut diagnostics = Diagnostics::new();
    for index in 0..200 {
        let observation = OwnedObservation {
            hwnd: event().hwnd + index,
            ..observation()
        };
        diagnostics.observe_owned(observation);
        let floor = diagnostics.sequence();
        diagnostics.observe_owned(observation);
        diagnostics.record(
            Event {
                hwnd: observation.hwnd,
                ..event()
            },
            Decision {
                stage: Stage::Accepted,
                reason: Reason::Accepted,
                unresolved_before: 0,
                unresolved_after: 0,
                cached_creator_thread_match: Some(true),
                observation_sequence_before_resolution: floor,
                independent_observation: Some(diagnostics.sequence()),
            },
        );
    }
    for kind in [0x8000, 0x8001, 0x8002, 0x8003, 0xffff] {
        for stage in [Stage::WindowOwner, Stage::OwnedFilter, Stage::Metadata] {
            for reason in [
                Reason::OutsideOwned,
                Reason::NotTopLevel,
                Reason::QueryFailed,
                Reason::MissingMetadata,
                Reason::IdentityMismatch,
            ] {
                diagnostics.record(
                    Event {
                        hwnd: 99887766,
                        kind,
                        ..event()
                    },
                    Decision {
                        stage,
                        reason,
                        unresolved_before: 0,
                        unresolved_after: 0,
                        cached_creator_thread_match: None,
                        observation_sequence_before_resolution: diagnostics.sequence(),
                        independent_observation: None,
                    },
                );
            }
        }
    }
    let report = diagnostics.report();
    assert_eq!(report["historical_owned_cache"].as_array().unwrap().len(), 64);
    assert_eq!(
        report["events_with_fresh_owned_cache_observation"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(report["anonymous_categories"].as_array().unwrap().len(), 64);
    assert_eq!(report["truncated"], true);
    assert_eq!(report["changes_qualification"], false);
}

#[test]
fn tick_wrap_and_observation_epochs_are_chronology_only() {
    let mut diagnostics = Diagnostics::new();
    let wrapped = (1u64 << 32) + 20;
    diagnostics.observe_owned(OwnedObservation {
        received_ms: wrapped,
        ..observation()
    });
    diagnostics.observe_owned(OwnedObservation {
        received_ms: wrapped + 10,
        ..observation()
    });
    diagnostics.record(
        Event {
            generated_ms: 10,
            received_ms: wrapped + 20,
            ..event()
        },
        Decision {
            cached_creator_thread_match: Some(true),
            ..unresolved(0)
        },
    );
    let report = diagnostics.report();
    assert_eq!(
        report["anonymous_categories"][0]["generation_time_order_vs_owned_observations"],
        "before-first-owned-observation"
    );
    let history = &report["historical_owned_cache"][0];
    assert_eq!(history["first_observation_sequence"], 1);
    assert_eq!(history["last_observation_sequence"], 2);
    assert_eq!(history["observed_identity_epoch"], 1);
    assert_eq!(history["complete_hwnd_generation_proof"], false);
    assert_eq!(history["event_ownership_proof"], false);
}

#[test]
fn invalid_owned_input_is_redacted_and_all_failure_stages_are_categorical() {
    let mut diagnostics = Diagnostics::new();
    diagnostics.observe_owned(OwnedObservation {
        birth: 0,
        ..observation()
    });
    for stage in [
        Stage::WindowOwner,
        Stage::ThreadOwner,
        Stage::OwnedFilter,
        Stage::Metadata,
        Stage::EventCap,
    ] {
        for reason in [
            Reason::OutsideOwned,
            Reason::NotTopLevel,
            Reason::QueryFailed,
            Reason::MissingMetadata,
            Reason::CapReached,
        ] {
            diagnostics.record(
                event(),
                Decision {
                    stage,
                    reason,
                    unresolved_before: 0,
                    unresolved_after: 0,
                    cached_creator_thread_match: None,
                    observation_sequence_before_resolution: 0,
                    independent_observation: None,
                },
            );
        }
    }
    let report = diagnostics.report();
    assert_eq!(report["invalid_owned_observations_rejected"], 1);
    assert_eq!(report["historical_owned_cache"], serde_json::json!([]));
    assert_eq!(
        report["events_with_fresh_owned_cache_observation"],
        serde_json::json!([])
    );
}
