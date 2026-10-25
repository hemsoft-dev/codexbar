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


class ObserverPolicy(unittest.TestCase):
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
