#!/usr/bin/env python3
"""Hosted Windows only. Build both pinned seams before one bounded, non-repeating raw-chain experiment."""
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

BASE = "7bb492fd3622727eee81b1e31f7256a15e71c3dd"
SEAM = ["crates/codexbar-providers/src/command.rs",
        "crates/codexbar-providers/src/command/pipe.rs",
        "crates/codexbar-providers/src/contained.rs"]
FIXTURE = ["crates/codexbar-providers/tests/command_runner.rs",
           "crates/codexbar-providers/tests/window_diagnostics.rs",
           "crates/codexbar-providers/tests/window_binding.rs",
           "crates/codexbar-providers/tests/command_runner/windows_console.rs",
           *[f"crates/codexbar-providers/tests/command_runner/windows_console/{name}.rs"
             for name in ("raw", "station", "identity", "observer", "hooks", "abrupt", "diagnostics", "binding", "binding_policy")],
           "scripts/qualify-provider-console.py", "scripts/tests/test_provider_console.py"]
ROOT = Path(__file__).resolve().parent.parent
EXPERIMENT_SECONDS = 30
OUTPUT_CAP = 16 * 1024 * 1024


def sha(data):
    return hashlib.sha256(data).hexdigest()


def build(label, output):
    folder = output / label
    folder.mkdir()
    # Original seam, candidate seam and normal gates never share compiled artifacts.
    target = Path(os.environ["RUNNER_TEMP"]) / "codexbar-console-qualification" / label
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    command = ["cargo", "test", "-p", "codexbar-providers", "--test", "command_runner",
               "--no-run", "--locked", "--message-format=json"]
    with (folder / "build.log").open("wb") as log:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=log)
    (folder / "cargo.jsonl").write_bytes(result.stdout)
    if result.returncode:
        raise RuntimeError(f"{label} observer compilation failed, exit {result.returncode}; not visibility RED")
    artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith(b"{")]
    executables = [item["executable"] for item in artifacts
                   if item.get("reason") == "compiler-artifact" and item.get("executable")
                   and item["target"]["name"] == "command_runner"]
    if len(executables) != 1:
        raise RuntimeError(f"{label}: expected one freshly qualified fixture artifact")
    return {"target_dir": str(target), "executable": executables[0],
            "binary_sha256": sha(Path(executables[0]).read_bytes()), "observations": [],
            "requested_root_flags_from_source": "CREATE_NO_WINDOW|CREATE_SUSPENDED; runtime adds flags" if label == "original-production-seam" else "0x80414",
            "default_failure_state": {"ready": "unknown", "event_loss": "unknown",
                                      "coverage": "unknown", "cleanup": "unknown"}}


def observe(label, output, built, deadline):
    folder = output / label
    # No pipe-drain wait after timeout: stdout/stderr go directly to bounded-evidence files.
    # The outer fixture first owns a kill-on-close job containing only this inert experiment.
    if sha(Path(built["executable"]).read_bytes()) != built["binary_sha256"]:
        raise RuntimeError(f"{label}: fixture binary changed after build")
    remaining = deadline - time.monotonic()
    if remaining <= 1:
        raise RuntimeError("30s total runtime budget exhausted before next trial; no repeat")
    with (folder / "stdout.log").open("wb") as stdout, (folder / "stderr.log").open("wb") as stderr:
        child = subprocess.Popen([built["executable"], "window-observe"], cwd=ROOT,
                                 stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        try:
            child.wait(timeout=min(14, remaining - 1))
        except subprocess.TimeoutExpired as error:
            child.kill()  # Retained Popen process identity only; its job closes on process termination.
            try:
                child.wait(timeout=min(1, max(0.001, deadline - time.monotonic())))
            except subprocess.TimeoutExpired as cleanup:
                raise RuntimeError(f"{label}: retained fixture cleanup unconfirmed; coverage unknown") from cleanup
            raise RuntimeError(f"{label}: observer timeout; coverage unknown, not visibility RED") from error
    if time.monotonic() > deadline:
        raise RuntimeError("30s total runtime budget exceeded; no qualification")
    if sha(Path(built["executable"]).read_bytes()) != built["binary_sha256"]:
        raise RuntimeError(f"{label}: fixture binary changed during experiment")
    if any((folder / name).stat().st_size > OUTPUT_CAP for name in ("stdout.log", "stderr.log")):
        raise RuntimeError(f"{label}: output event/protocol cap exceeded")
    if child.returncode:
        raise RuntimeError(f"{label}: observer failed, exit {child.returncode}; not visibility RED")
    reports = [json.loads(line) for line in (folder / "stdout.log").read_bytes().splitlines()
               if line.startswith(b"{")]
    reports = [item for item in reports if item.get("result") == "visibility-observer"]
    if len(reports) != 1 or reports[0].get("route") != "raw-handle-list":
        raise RuntimeError(f"{label}: expected one raw Win32 observation report")
    built["observations"] = reports
    if not integrity(reports[0]):
        failure = reports[0].get("failure") or reports[0].get("protocol_errors") or reports[0].get("observation", {}).get("errors")
        raise RuntimeError(f"{label}: observer integrity failed, not visibility RED: {str(failure)[:1000]}")


def integrity(row):
    observation = row.get("observation", {})
    return (row.get("ready") is True and row.get("failure") is None
            and 0 < row.get("ready_ack_us", 0) <= 2_000_000
            and row.get("control", {}).get("independently_seen") is True
            and row.get("control", {}).get("destroyed") is True
            and row.get("control", {}).get("console_hwnd") == 0
            and 0 < row.get("control", {}).get("elapsed_us", 0) <= 1_000_000
            and row.get("protocol_errors") == []
            and observation.get("control_seen") is True and observation.get("errors") == []
            and observation.get("event_cap_hit") is False
            and observation.get("cleanup_confirmed") is True
            and len(row.get("rows", [])) == 3)


def binding_events_complete(row):
    """Readback witnesses from the real hook/resolver, not summary success flags alone."""
    creator, generator = row.get("creator", {}), row.get("generator", {})
    registration = row.get("registration", {})
    selected = []
    for kind, actor in ((0x8002, generator), (0x8001, creator)):
        matches = [event for event in row.get("observation", {}).get("events", [])
                   if event.get("phase") == "owned-lifecycle-binding-control" and event.get("event_kind") == kind
                   and event.get("hwnd") == registration.get("hwnd")
                   and event.get("creator_tid") == creator.get("tid") and event.get("generator_tid") == actor.get("tid")
                   and event.get("generator_thread_birth") == actor.get("thread_birth")
                   and event.get("registered_pid") == creator.get("pid") and event.get("process_birth") == creator.get("process_birth")
                   and event.get("fixture_generation") == registration.get("fixture_generation") == 1
                   and event.get("mapping") == "cooperative-fixture-command-generation-only"
                   and event.get("original_visibility_red") is False and event.get("callback_sequence", 0) > 0]
        if len(matches) != 1:
            return False
        selected.append(matches[0])
    native_negative = [event for event in row.get("observation", {}).get("events", [])
                       if event.get("phase") == "owned-lifecycle-binding-control" and event.get("event_kind") == 0x8002
                       and event.get("hwnd") == registration.get("hwnd")
                       and event.get("creator_tid") == event.get("generator_tid") == creator.get("tid")
                       and event.get("generator_thread_birth") == creator.get("thread_birth")
                       and event.get("registered_pid") == creator.get("pid") and event.get("process_birth") == creator.get("process_birth")
                       and event.get("fixture_generation") == 1 and event.get("original_visibility_red") is False
                       and event.get("existing_resolver_error") is None
                       and event.get("mapping") == "rejected-real-creator-event-for-separate-generator-ticket"
                       and 0 < event.get("callback_sequence", 0) < selected[0]["callback_sequence"]]
    return (len(native_negative) == 1
            and selected[0].get("existing_resolver_error") == "WinEvent identity differs from retained window owner/thread"
            and selected[1].get("existing_resolver_error") is None
            and selected[0]["callback_sequence"] < selected[1]["callback_sequence"])


def binding_complete(row):
    """Gate only the cooperative control. Its outcome is never original-provider RED or broker proof."""
    observation = row.get("observation", {})
    creator, generator = row.get("creator", {}), row.get("generator", {})
    context = observation.get("ready_context", {})
    return (row.get("complete") is True and "failure" in row and row.get("failure") is None and row.get("cleanup_confirmed") is True
            and row.get("scope") == "cooperative-owned-fixture-only" and row.get("original_visibility_red") is False
            and row.get("provider_or_broker_generation_proof") is False
            and row.get("visible_control_readback") is True
            and row.get("registration", {}).get("creator_api_readback_matches") is True
            and row.get("registration", {}).get("hwnd", 0) > 0 and row.get("registration", {}).get("nonce", 0) > 0
            and 0 < row.get("ready_us", 0) <= 2_000_000 and 0 < row.get("window_lifetime_us", 0) < 1_000_000
            and 0 < row.get("elapsed_us", 0) <= 10_000_000
            and creator.get("pid", 0) > 0 and creator.get("pid") == generator.get("pid")
            and creator.get("process_birth", 0) > 0 and creator.get("process_birth") == generator.get("process_birth")
            and creator.get("tid", 0) > 0 and generator.get("tid", 0) > 0 and creator["tid"] != generator["tid"]
            and creator.get("thread_birth", 0) > 0 and generator.get("thread_birth", 0) > 0
            and context.get("desktop") and context.get("desktop") == context.get("input_desktop")
            and context.get("session", 0) > 0 and context.get("station", {}).get("visible") is True
            and all(observation.get(field) is True for field in
                    ("native_show_mapped", "native_delayed_destroy_mapped", "legacy_resolver_show_rejected",
                     "legacy_resolver_destroy_accepted", "native_wrong_generator_rejected", "hook_closed"))
            and observation.get("unattributable_registered_numeric_hwnd_events") == 0
            and observation.get("event_cap") == 8192 and observation.get("event_cap_hit") is False
            and 0 < observation.get("event_count", 0) <= 8192
            and observation.get("event_loss") is False and observation.get("queue_drained") is True
            and observation.get("destroy_readback", {}).get("window_query_return") == 0
            and observation.get("destroy_readback", {}).get("window_query_error", 0) > 0
            and observation.get("destroy_readback", {}).get("is_window") is False
            and len(observation.get("negative_policy_controls", {}).get("outcomes", [])) == 6
            and {item.get("case") for item in observation["negative_policy_controls"]["outcomes"]}
            == {"unknown-hwnd", "wrong-generation", "wrong-nonce", "wrong-thread-birth", "wrong-generator", "missing-live-owner"}
            and all(item.get("rejected") is True for item in observation["negative_policy_controls"]["outcomes"])
            and binding_events_complete(row))


def validate_binding(output, built, deadline):
    folder = output / "owned-lifecycle-binding"
    folder.mkdir()
    result = {"binary_sha256": built["binary_sha256"], "observation": None, "complete": False}
    receipt = folder / "report.json"
    try:
        if sha(Path(built["executable"]).read_bytes()) != built["binary_sha256"]:
            raise RuntimeError("binding fixture binary changed after build")
        remaining = deadline - time.monotonic()
        if remaining <= 1:
            raise RuntimeError("total 30s experiment budget exhausted before owned lifecycle control")
        with (folder / "stdout.log").open("wb") as stdout, (folder / "stderr.log").open("wb") as stderr:
            child = subprocess.Popen([built["executable"], "window-binding"], cwd=ROOT,
                                     stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            try:
                child.wait(timeout=min(10, remaining - 1))
            except subprocess.TimeoutExpired as error:
                child.kill()  # Its owned native kill-on-close job closes, never a global PID kill.
                child.wait(timeout=min(1, max(0.001, deadline - time.monotonic())))
                raise RuntimeError("owned lifecycle control exceeded deadline; no comparison pair") from error
        if child.returncode or any((folder / name).stat().st_size > OUTPUT_CAP for name in ("stdout.log", "stderr.log")):
            raise RuntimeError("owned lifecycle control failed or exceeded output cap; no comparison pair")
        rows = [json.loads(line) for line in (folder / "stdout.log").read_bytes().splitlines() if line.startswith(b"{")]
        rows = [row for row in rows if row.get("result") == "owned-lifecycle-binding"]
        if len(rows) != 1:
            raise RuntimeError("missing owned lifecycle native receipt; no comparison pair")
        result["observation"] = rows[0]
        result["complete"] = binding_complete(rows[0])
        if sha(Path(built["executable"]).read_bytes()) != built["binary_sha256"]:
            raise RuntimeError("binding fixture binary changed during observation")
        if not result["complete"]:
            raise RuntimeError(f"owned lifecycle validation incomplete; no comparison pair: {rows[0].get('failure')}")
        if time.monotonic() > deadline:
            raise RuntimeError("total experiment budget exceeded after owned lifecycle control")
        return result
    except BaseException as error:
        result["failure"] = str(error)
        raise
    finally:
        receipt.write_text(json.dumps(result, indent=2) + "\n")


def measured_coverage(row):
    """Temporal/event coverage of owned fixture windows, never arbitrary broker coverage."""
    coverage = row.get("observation", {}).get("coverage", {})
    lives = row.get("observation", {}).get("lifetimes", [])
    context = row.get("ready_context", {})
    start, end = coverage.get("start_100ns"), coverage.get("end_100ns")
    if (not integrity(row) or coverage.get("scope") != "owned-fixture-top-level-windows"
            or type(start) is not int or type(end) is not int or not 0 < start < end
            or any(coverage.get(key) is not True for key in
                   ("hook_closed", "queue_drained", "control_show_event", "control_destroy_event", "full_lifetime", "window_lifecycle_complete"))
            or coverage.get("unresolved_event_owners") != 0 or len(lives) != 3
            or context.get("session", 0) <= 0 or not context.get("desktop")):
        return False
    ordered = sorted(lives, key=lambda item: item.get("depth", -1))
    if [item.get("depth") for item in ordered] != [0, 1, 2]:
        return False
    pids = [item.get("pid") for item in ordered]
    if any(type(pid) is not int or pid <= 0 for pid in pids) or len(set(pids)) != 3:
        return False
    for index, item in enumerate(ordered):
        born, exited = item.get("creation_time"), item.get("exit_time")
        parent = pids[index - 1] if index else context.get("caller_pid")
        if (type(born) is not int or type(exited) is not int or not start <= born <= exited <= end
                or item.get("parent_pid") != parent or item.get("session") != context.get("session")
                or item.get("ack") is not True or item.get("retained_exit") is not True):
            return False
    return True


def visible_owned_window(row):
    lives = row.get("observation", {}).get("lifetimes", [])
    identities = {(item.get("pid"), item.get("creation_time")) for item in lives}
    for event in row.get("observation", {}).get("events", []):
        rect = event.get("rect", [])
        if (event.get("phase") == "provider" and event.get("visible") is True
                and (event.get("owner_pid"), event.get("owner_creation_time")) in identities
                and event.get("hwnd", 0) > 0 and event.get("class")
                and event.get("desktop") == row.get("ready_context", {}).get("desktop")
                and len(rect) == 4 and rect[2] > rect[0] and rect[3] > rect[1]):
            return True
    return False


def hidden_association(row):
    rows = row.get("rows", [])
    if len(rows) != 3:
        return False
    hwnd, owner = rows[0].get("hwnd"), rows[0].get("owner")
    if type(hwnd) is not int or hwnd <= 0 or type(owner) is not int or owner <= 0:
        return False
    # GetWindowThreadProcessId identifies the window-creating process, which can be a separate
    # console host. Shared client-reported console association does not attribute broker windows
    # to a client lifetime; visible_owned_window keeps that independent identity requirement.
    return all(item.get("valid") is True and item.get("enumerated") is True
               and item.get("visible") is False and item.get("class") == "ConsoleWindowClass"
               and type(item.get("hwnd")) is int and item.get("hwnd") == hwnd
               and type(item.get("owner")) is int and item.get("owner") == owner
               and item.get("desktop") == item.get("window_desktop") == row.get("ready_context", {}).get("desktop")
               for item in rows)


def classify(before, after):
    # Positive control, mechanism association and no-reproduction never substitute for original RED.
    red = integrity(before) and visible_owned_window(before)
    hidden = integrity(after) and hidden_association(after)
    complete = (integrity(before) and integrity(after)
                and before.get("full_observation") is True and after.get("full_observation") is True)
    complete = complete and measured_coverage(before) and measured_coverage(after)
    coverage = [row.get("observation", {}).get("coverage", {}) for row in (before, after)]
    green = complete and hidden and not visible_owned_window(after)
    return {"qualification": "red-green" if red and green else "unqualified",
            "original_visibility_counterexample": red, "candidate_shared_hidden_console": hidden,
            "full_observation": complete, "outside_broker_coverage": [item.get("outside_broker", "unknown") for item in coverage],
            "qualification_scope": "owned-fixture-top-level-windows", "stop_after_pair": True}


def main():
    if os.name != "nt" or os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("This qualification may run only in hosted Windows GitHub CI")
    subprocess.run([sys.executable, str(ROOT / "scripts/tests/test_provider_console.py")], cwd=ROOT, check=True)
    output = ROOT / "artifacts/provider-console"
    output.mkdir(parents=True, exist_ok=False)
    candidate = {name: (ROOT / name).read_bytes() for name in SEAM}
    original = {name: subprocess.check_output(["git", "show", f"{BASE}:{name}"], cwd=ROOT) for name in SEAM}
    backup = output / "candidate-source"
    backup.mkdir()
    for name, data in candidate.items():
        path = backup / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    report = {"schema": 2, "original_seam_commit": BASE,
              "default_failure_state": {"ready": "unknown", "event_loss": "unknown", "coverage": "unknown", "cleanup": "unknown"},
              "checkout_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "candidate_head": os.environ["CONSOLE_CANDIDATE_HEAD"],
              "candidate_sha256": {name: sha(data) for name, data in candidate.items()},
              "original_sha256": {name: sha(data) for name, data in original.items()},
              "fixture_sha256": {name: sha((ROOT / name).read_bytes()) for name in FIXTURE},
              "os_build": platform.version(), "machine": platform.machine(),
              "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
              "experiment_limit_seconds": EXPERIMENT_SECONDS, "build_time_in_runtime_budget": False,
              "qualification": "unqualified", "restored": False}
    manifest = output / "report.json"
    manifest.write_text(json.dumps(report, indent=2) + "\n")
    try:
        try:
            for name, data in original.items():
                (ROOT / name).write_bytes(data)
            if any((ROOT / name).read_bytes() != data for name, data in original.items()):
                raise RuntimeError("original production seam bytes do not match pinned Git objects")
            report["original"] = build("original-production-seam", output)
        finally:
            # Restore/verify exact candidate bytes BEFORE candidate build or either runtime observation.
            failures = []
            for name, data in candidate.items():
                try:
                    (ROOT / name).write_bytes(data)
                    if (ROOT / name).read_bytes() != data:
                        failures.append(name)
                except OSError as error:
                    failures.append(f"{name}: {error}")
            if failures:
                raise RuntimeError(f"candidate restoration failed: {failures}")
            report["restored"] = True
            subprocess.run(["git", "diff", "--exit-code", "--", *SEAM], cwd=ROOT, check=True)
        report["candidate"] = build("candidate", output)
        # Exactly one original/candidate raw pair after both binaries exist. No retry or OS-setting changes.
        deadline = time.monotonic() + EXPERIMENT_SECONDS
        # A cooperative control proves its own registered mapping only. Never run the comparison if incomplete.
        report["owned_lifecycle_binding"] = validate_binding(output, report["candidate"], deadline)
        for label, key in (("original-production-seam", "original"), ("candidate", "candidate")):
            observe(label, output, report[key], deadline)
        before, after = (report[key]["observations"][0] for key in ("original", "candidate"))
        report.update(classify(before, after))
        if time.monotonic() > deadline:
            raise RuntimeError("30s total experiment budget exceeded during report processing")
        if after.get("visibility_counterexample") is True:
            raise RuntimeError("candidate has an attributable visible owned window")
        print(f"Visibility qualification: {report['qualification']}; no further experiment.", flush=True)
    except BaseException as error:
        report["qualification"] = "unqualified"
        report["failure"] = str(error)
        raise
    finally:
        manifest.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
