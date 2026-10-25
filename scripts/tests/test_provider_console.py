"""Portable protocol tests only. Importing the orchestration module launches no process or GUI."""
import copy
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("provider_console", Path(__file__).parents[1] / "qualify-provider-console.py")
POLICY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(POLICY)


def fixture(visible=False, console_owner=900):
    lives = [{"depth": depth, "pid": 100 + depth, "parent_pid": 99 + depth,
              "creation_time": 200 + depth, "exit_time": 800 - depth,
              "session": 2, "ack": True, "retained_exit": True} for depth in range(3)]
    rows = [{"depth": depth, "pid": 100 + depth, "hwnd": 42, "owner": console_owner,
             "valid": True, "enumerated": True, "visible": False,
             "class": "ConsoleWindowClass", "desktop": "Default", "window_desktop": "Default"}
            for depth in range(3)]
    # Independent owned-window evidence is separate from the client-reported shared console HWND.
    events = [{"phase": "provider", "source": "winevent", "hwnd": 77,
               "owner_pid": 100, "owner_creation_time": 200, "visible": visible,
               "class": "ConsoleWindowClass", "desktop": "Default", "rect": [0, 0, 100, 100]}]
    return {"ready": True, "ready_ack_us": 1000, "failure": None, "protocol_errors": [],
            "control": {"independently_seen": True, "destroyed": True, "console_hwnd": 0, "elapsed_us": 1000},
            "ready_context": {"session": 2, "desktop": "Default", "caller_pid": 99},
            "rows": rows, "visibility_counterexample": visible, "shared_hidden_console": True,
            "full_observation": True,
            "observation": {"control_seen": True, "errors": [], "event_cap_hit": False,
                            "cleanup_confirmed": True, "lifetimes": lives, "events": events,
                            "coverage": {"scope": "owned-fixture-top-level-windows", "start_100ns": 100,
                                         "end_100ns": 900, "hook_closed": True, "queue_drained": True,
                                         "control_show_event": True, "control_destroy_event": True,
                                         "unresolved_event_owners": 0, "outside_broker": "unknown", "full_lifetime": True,
                                         "window_lifecycle_complete": True}}}


def binding_receipt():
    return {"complete": True, "failure": None, "cleanup_confirmed": True,
            "scope": "cooperative-owned-fixture-only", "original_visibility_red": False,
            "provider_or_broker_generation_proof": False, "visible_control_readback": True,
            "registration": {"creator_api_readback_matches": True, "hwnd": 42, "nonce": 7, "fixture_generation": 1},
            "ready_us": 1000, "window_lifetime_us": 1000, "elapsed_us": 2000,
            "creator": {"pid": 100, "process_birth": 200, "tid": 300, "thread_birth": 400},
            "generator": {"pid": 100, "process_birth": 200, "tid": 301, "thread_birth": 401},
            "observation": {"native_show_mapped": True, "native_delayed_destroy_mapped": True,
                            "legacy_resolver_show_rejected": True, "legacy_resolver_destroy_accepted": True,
                            "native_wrong_generator_rejected": True,
                            "hook_closed": True, "unattributable_registered_numeric_hwnd_events": 0,
                            "ready_context": {"desktop": "Default", "input_desktop": "Default", "session": 2,
                                              "station": {"visible": True}},
                            "event_count": 4, "event_cap": 8192, "event_cap_hit": False,
                            "event_loss": False, "queue_drained": True,
                            "destroy_readback": {"window_query_return": 0, "window_query_error": 1400, "is_window": False},
                            "negative_policy_controls": {"outcomes": [{"case": name, "rejected": True} for name in
                                ("unknown-hwnd", "wrong-generation", "wrong-nonce", "wrong-thread-birth", "wrong-generator", "missing-live-owner")]},
                            "events": [{"phase": "owned-lifecycle-binding-control", "event_kind": kind,
                                        "hwnd": 42, "creator_tid": 300, "generator_tid": tid, "generator_thread_birth": birth,
                                        "registered_pid": 100, "process_birth": 200, "fixture_generation": 1,
                                        "mapping": mapping, "original_visibility_red": False,
                                        "callback_sequence": sequence, "existing_resolver_error": error}
                                       for kind, tid, birth, sequence, error, mapping in
                                       ((0x8002, 301, 401, 3, "WinEvent identity differs from retained window owner/thread",
                                         "cooperative-fixture-command-generation-only"),
                                        (0x8001, 300, 400, 5, None, "cooperative-fixture-command-generation-only"),
                                        (0x8002, 300, 400, 2, None, "rejected-real-creator-event-for-separate-generator-ticket"))]}}


class ObserverPolicy(unittest.TestCase):
    def test_cooperative_binding_gate_requires_complete_native_control_receipt(self):
        self.assertTrue(POLICY.binding_complete(binding_receipt()))
        for field in binding_receipt():
            with self.subTest(missing=field):
                row = binding_receipt()
                del row[field]
                self.assertFalse(POLICY.binding_complete(row))

    def test_binding_control_unknown_or_wrong_identity_prevents_comparison(self):
        for change in ({"unattributable_registered_numeric_hwnd_events": 1}, {"native_show_mapped": False},
                       {"native_delayed_destroy_mapped": False}, {"legacy_resolver_show_rejected": False},
                       {"hook_closed": False}, {"negative_policy_controls": {"outcomes": []}}):
            row = binding_receipt()
            row["observation"].update(change)
            self.assertFalse(POLICY.binding_complete(row))
        row = binding_receipt()
        row["generator"]["tid"] = row["creator"]["tid"]
        self.assertFalse(POLICY.binding_complete(row))
        row = binding_receipt()
        row["generator"]["thread_birth"] = 0
        self.assertFalse(POLICY.binding_complete(row))
        for events in ([], binding_receipt()["observation"]["events"][:1]):
            row = binding_receipt()
            row["observation"]["events"] = events
            self.assertFalse(POLICY.binding_complete(row))
        for key, value in (("generator_tid", 300), ("generator_thread_birth", 999),
                           ("callback_sequence", 6), ("existing_resolver_error", "unrelated API failure")):
            row = binding_receipt()
            row["observation"]["events"][0][key] = value
            self.assertFalse(POLICY.binding_complete(row))

    def test_binding_control_never_substitutes_for_provider_red_or_waives_limits(self):
        for change in ({"original_visibility_red": True}, {"provider_or_broker_generation_proof": True},
                       {"ready_us": 2_000_001}, {"window_lifetime_us": 1_000_001},
                       {"elapsed_us": 10_000_001}, {"cleanup_confirmed": False}):
            row = binding_receipt()
            row.update(change)
            self.assertFalse(POLICY.binding_complete(row))
        original = fixture()
        original["binding"] = binding_receipt()
        self.assertEqual(POLICY.classify(original, fixture())["qualification"], "unqualified")

    def test_binding_control_requires_real_negative_event_and_exact_named_policy_controls(self):
        for change in ({"event_kind": 0x8001}, {"generator_tid": 999}, {"callback_sequence": 9},
                       {"generator_thread_birth": 999}, {"existing_resolver_error": "API failure"}):
            row = binding_receipt()
            row["observation"]["events"][2].update(change)
            self.assertFalse(POLICY.binding_complete(row))
        row = binding_receipt()
        row["observation"]["negative_policy_controls"]["outcomes"][0]["case"] = "wrong-generator"
        self.assertFalse(POLICY.binding_complete(row))

    def test_binding_control_missing_desktop_or_event_loss_remains_incomplete(self):
        for change in ({"event_count": 8193}, {"event_cap_hit": True}, {"event_cap": 8193},
                       {"event_loss": "unknown"}, {"queue_drained": False},
                       {"ready_context": {}}, {"ready_context": {"desktop": "other", "input_desktop": "Default"}}):
            row = binding_receipt()
            row["observation"].update(change)
            self.assertFalse(POLICY.binding_complete(row))

    def test_shared_hidden_console_host_can_differ_from_all_client_pids(self):
        candidate = fixture()
        self.assertNotIn(candidate["rows"][0]["owner"], [item["pid"] for item in candidate["rows"]])
        self.assertTrue(POLICY.hidden_association(candidate))
        self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "red-green")

    def test_shared_console_host_does_not_attribute_a_separate_visible_broker_window(self):
        original = fixture()
        original["observation"]["events"][0].update(owner_pid=900, owner_creation_time=999, visible=True)
        self.assertFalse(POLICY.visible_owned_window(original))
        self.assertEqual(POLICY.classify(original, fixture())["qualification"], "unqualified")

    def test_shared_owner_still_allows_the_root_client_to_create_the_window(self):
        self.assertTrue(POLICY.hidden_association(fixture(console_owner=100)))

    def test_invalid_shared_host_and_handle_are_rejected(self):
        for field in ("owner", "hwnd"):
            for value in (None, 0, -1, "900", True, 900.0):
                with self.subTest(field=field, value=value):
                    candidate = fixture()
                    for row in candidate["rows"]:
                        row[field] = value
                    self.assertFalse(POLICY.hidden_association(candidate))

    def test_shared_host_does_not_weaken_window_validation(self):
        for field, value in (("owner", 901), ("hwnd", 43), ("valid", False),
                             ("enumerated", False), ("visible", True), ("class", "OtherWindow"),
                             ("desktop", "OtherDesktop"), ("window_desktop", "OtherDesktop")):
            with self.subTest(field=field):
                candidate = fixture()
                candidate["rows"][1][field] = value
                self.assertFalse(POLICY.hidden_association(candidate))
        for field in ("owner", "hwnd", "valid", "enumerated", "class", "desktop", "window_desktop"):
            with self.subTest(missing=field):
                candidate = fixture()
                del candidate["rows"][1][field]
                self.assertFalse(POLICY.hidden_association(candidate))

    def test_attributable_root_red_and_measured_owned_green(self):
        report = POLICY.classify(fixture(True), fixture())
        self.assertEqual(report["qualification"], "red-green")
        self.assertEqual(report["qualification_scope"], "owned-fixture-top-level-windows")
        self.assertEqual(report["outside_broker_coverage"], ["unknown", "unknown"])

    def test_no_reproduction_and_control_are_never_red(self):
        original = fixture()
        original["observation"]["events"][0].update(phase="positive-control", visible=True)
        original["visibility_counterexample"] = True  # Summary booleans cannot override event ownership.
        self.assertEqual(POLICY.classify(original, fixture())["qualification"], "unqualified")

    def test_foreign_owner_and_reused_pid_creation_are_not_red(self):
        for change in ({"owner_pid": 777}, {"owner_creation_time": 201}, {"hwnd": 0},
                       {"class": ""}, {"desktop": "other"}, {"rect": [0, 0, 0, 0]}):
            with self.subTest(change=change):
                original = fixture(True)
                original["observation"]["events"][0].update(change)
                self.assertFalse(POLICY.visible_owned_window(original))

    def test_missing_coverage_field_fails_closed(self):
        for field in ("start_100ns", "end_100ns", "scope", "hook_closed", "queue_drained",
                      "control_show_event", "control_destroy_event", "unresolved_event_owners", "full_lifetime", "window_lifecycle_complete"):
            with self.subTest(field=field):
                candidate = fixture()
                del candidate["observation"]["coverage"][field]
                self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")

    def test_false_ack_and_unknown_ownership_fail_closed(self):
        for field, value in (("hook_closed", False), ("queue_drained", False),
                             ("control_show_event", False), ("control_destroy_event", False),
                             ("unresolved_event_owners", 1), ("start_100ns", 300), ("end_100ns", 700)):
            with self.subTest(field=field):
                candidate = fixture()
                candidate["observation"]["coverage"][field] = value
                self.assertFalse(POLICY.measured_coverage(candidate))

    def test_lost_process_lifetime_and_topology_fail_closed(self):
        for change in ({"creation_time": 99}, {"exit_time": 0}, {"exit_time": 901},
                       {"retained_exit": False}, {"ack": False}, {"parent_pid": 777},
                       {"session": 0}, {"depth": 0}, {"pid": 100}):
            with self.subTest(change=change):
                candidate = fixture()
                candidate["observation"]["lifetimes"][1].update(change)
                self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")

    def test_errors_caps_control_deadlines_and_early_exit_fail_closed(self):
        for change in ({"errors": ["lost event"]}, {"event_cap_hit": True}, {"cleanup_confirmed": False}):
            candidate = fixture()
            candidate["observation"].update(change)
            self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")
        for change in ({"failure": "early exit"}, {"protocol_errors": ["missing sample"]},
                       {"full_observation": False}, {"ready_ack_us": 2_000_001}, {"ready": False}):
            candidate = fixture()
            candidate.update(change)
            self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")
        for change in ({"elapsed_us": 1_000_001}, {"destroyed": False}, {"console_hwnd": 3}):
            candidate = fixture()
            candidate["control"].update(change)
            self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")

    def test_clock_reversal_and_missing_root_cannot_claim_coverage(self):
        candidate = fixture()
        candidate["observation"]["coverage"].update(start_100ns=999, end_100ns=900)
        self.assertFalse(POLICY.measured_coverage(candidate))
        candidate = fixture()
        candidate["observation"]["lifetimes"].pop(0)
        self.assertFalse(POLICY.measured_coverage(candidate))

    def test_candidate_visible_or_mismatched_shared_owner_is_not_green(self):
        for candidate in (fixture(True), fixture()):
            if not candidate["visibility_counterexample"]:
                candidate["rows"][0]["owner"] = 777
            self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")

    def test_both_invisible_is_unqualified_without_mutating_evidence(self):
        original, candidate = fixture(), fixture()
        saved = copy.deepcopy((original, candidate))
        self.assertEqual(POLICY.classify(original, candidate)["qualification"], "unqualified")
        self.assertEqual((original, candidate), saved)

    def test_measurement_diagnostics_cannot_override_unknown_or_lost_events(self):
        for coverage in ({"unresolved_event_owners": 2}, {"window_lifecycle_complete": False}):
            candidate = fixture()
            candidate["observation"]["coverage"].update(coverage)
            candidate["observation"]["resolver_diagnostics"] = {
                "counter_increment_preceded_diagnostic": True, "events_with_fresh_owned_cache_observation": [{}],
                "historical_owned_cache": [{}], "decisions": 100,
            }
            self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")
        candidate = fixture()
        candidate["observation"].update(errors=["callback queue lost events"], resolver_diagnostics={"decisions": 100})
        self.assertEqual(POLICY.classify(fixture(True), candidate)["qualification"], "unqualified")


if __name__ == "__main__":
    unittest.main()
