#!/usr/bin/env python3
"""Hosted Windows only: observe exact original production seam and candidate, with separate build artifacts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

BASE = "7bb492fd3622727eee81b1e31f7256a15e71c3dd"
SEAM = ["crates/codexbar-providers/src/command.rs",
        "crates/codexbar-providers/src/command/pipe.rs",
        "crates/codexbar-providers/src/contained.rs"]
ROOT = Path(__file__).resolve().parent.parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def observe(label, output):
    folder = output / label
    folder.mkdir()
    # Never share compiled artifacts across the two different production seams or the normal workspace gates.
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
    try:
        result = subprocess.run([executables[0], "window-observe"], cwd=ROOT, env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=45)
    except subprocess.TimeoutExpired as error:
        (folder / "stdout.log").write_bytes(error.stdout or b"")
        (folder / "stderr.log").write_bytes(error.stderr or b"")
        raise RuntimeError(f"{label} observer timed out; not visibility RED") from error
    (folder / "stdout.log").write_bytes(result.stdout)
    (folder / "stderr.log").write_bytes(result.stderr)
    print(result.stdout.decode("utf-8", errors="replace"), flush=True)
    if result.returncode:
        print(result.stderr.decode("utf-8", errors="replace"), flush=True)
        raise RuntimeError(f"{label}: observer failed, exit {result.returncode}; not visibility RED")
    reports = [json.loads(line) for line in result.stdout.splitlines() if line.startswith(b"{")]
    reports = [item for item in reports if item.get("result") == "visibility-observer"]
    if len(reports) != 2:
        raise RuntimeError(f"{label}: missing Rust/raw Win32 observation reports")
    return {"target_dir": str(target), "executable": executables[0], "observations": reports}


def main():
    if os.name != "nt" or os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("This qualification may run only in hosted Windows GitHub CI")
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
    report = {"original_seam_commit": BASE,
              "checkout_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "candidate_head": os.environ["CONSOLE_CANDIDATE_HEAD"],
              "candidate_sha256": {name: sha(data) for name, data in candidate.items()},
              "original_sha256": {name: sha(data) for name, data in original.items()},
              "qualification": "pending", "restored": False}
    manifest = output / "report.json"
    manifest.write_text(json.dumps(report, indent=2) + "\n")
    try:
        try:
            for name, data in original.items():
                (ROOT / name).write_bytes(data)
            if any((ROOT / name).read_bytes() != data for name, data in original.items()):
                raise RuntimeError("original production seam bytes do not match pinned Git objects")
            report["original"] = observe("original-production-seam", output)
        finally:
            # Restore every file even if another restore fails. Verify bytes, not only a clean Git diff.
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
        report["candidate"] = observe("candidate", output)
        before = any(item["visibility_counterexample"] for item in report["original"]["observations"])
        after = any(item["visible_descendant_window"] for item in report["candidate"]["observations"])
        candidate_hidden = all(item["interactive_station"] and item["shared_hidden_console"]
                               for item in report["candidate"]["observations"])
        report["qualification"] = "red-green" if before and candidate_hidden and not after else "unqualified"
        report["original_visibility_counterexample"] = before
        report["candidate_shared_hidden_console"] = candidate_hidden
        if after:
            raise RuntimeError("candidate still has a visible descendant window")
        print(f"Visibility qualification: {report['qualification']}. Candidate mechanism checks run separately.", flush=True)
    except BaseException as error:
        report["failure"] = str(error)
        raise
    finally:
        manifest.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
