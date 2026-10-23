#!/usr/bin/env python3
"""Run a bounded, pinned Rust mutation qualification and reject incomplete evidence."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
POLICY = ROOT / "scripts/rust-mutations.json"


def identity(mutant):
    return mutant["file"], mutant["function"]["function_name"]


def validate_policy(policy):
    if not isinstance(policy, dict) or type(policy.get("schema")) is not int or policy["schema"] != 1:
        raise ValueError("Expected mutation policy schema 1")
    if not isinstance(policy.get("tool_version"), str) or not re.fullmatch(r"\d+\.\d+\.\d+", policy["tool_version"]):
        raise ValueError("Pin an exact mutation tool version")
    targets = policy.get("targets")
    if not isinstance(targets, list) or not targets:
        raise ValueError("Select explicit mutation targets")
    seen = set()
    for target in targets:
        if not isinstance(target, dict):
            raise ValueError("Invalid mutation target")
        for key in ["file", "function", "package"]:
            if not isinstance(target.get(key), str) or not target[key]:
                raise ValueError(f"Missing target {key}")
        path = Path(target["file"])
        if path.is_absolute() or ".." in path.parts or path.suffix != ".rs":
            raise ValueError("Mutation files must be repository-relative Rust files")
        key = target["file"], target["function"]
        if key in seen:
            raise ValueError("Duplicate mutation target")
        seen.add(key)
        for field, floor in [("minimum_caught", 1), ("maximum_unviable", 0)]:
            if type(target.get(field)) is not int or target[field] < floor:
                raise ValueError(f"Invalid {field}")


def evaluate(generated, report, policy):
    validate_policy(policy)
    targets = {(t["file"], t["function"]): t for t in policy["targets"]}
    if not targets or len(targets) != len(policy["targets"]):
        raise ValueError("Empty or duplicate mutation targets")
    names = [m["name"] for m in generated]
    if not names or len(set(names)) != len(names) or {identity(m) for m in generated} != set(targets):
        raise ValueError("Missing, duplicate, or out-of-scope generated mutants")
    if report["cargo_mutants_version"] != policy["tool_version"] or not isinstance(report["end_time"], str) or not report["end_time"]:
        raise ValueError("Wrong producer version or incomplete run")
    baselines = [o for o in report["outcomes"] if o["scenario"] == "Baseline"]
    if len(baselines) != 1 or baselines[0]["summary"] != "Success":
        raise ValueError("The unmutated baseline must pass")
    if [p["phase"] for p in baselines[0]["phase_results"]] != ["Build", "Test"] or any(p["process_status"] != "Success" for p in baselines[0]["phase_results"]):
        raise ValueError("Missing baseline build/test success")
    mutants = [o for o in report["outcomes"] if isinstance(o["scenario"], dict)]
    observed = [o["scenario"]["Mutant"]["name"] for o in mutants]
    if len(observed) != len(names) or set(observed) != set(names) or len(report["outcomes"]) != len(names) + 1:
        raise ValueError("Not every generated mutant has exactly one outcome")
    counts = Counter(o["summary"] for o in mutants)
    fields = {"caught": "CaughtMutant", "missed": "MissedMutant", "timeout": "Timeout", "unviable": "Unviable", "success": "Success"}
    if set(counts) - set(fields.values()) or type(report["total_mutants"]) is not int or report["total_mutants"] != len(names):
        raise ValueError("Unknown outcome or inconsistent total")
    if any(type(report[k]) is not int or report[k] != counts[v] for k, v in fields.items()):
        raise ValueError("Inconsistent outcome counts")
    failures = []
    for key in ["missed", "timeout", "success"]:
        if report[key]:
            failures.append(f"{key}: {report[key]}")
    for key, target in targets.items():
        own = Counter(o["summary"] for o in mutants if identity(o["scenario"]["Mutant"]) == key)
        if own["CaughtMutant"] < target["minimum_caught"]:
            failures.append(f"{key}: caught {own['CaughtMutant']} below {target['minimum_caught']}")
        if own["Unviable"] > target["maximum_unviable"]:
            failures.append(f"{key}: unexpected unviable mutants")
    return failures


def source_hashes(root):
    paths = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root).decode().split("\0")
    return {p: hashlib.sha256((root / p).read_bytes()).hexdigest()
            for p in sorted(set(paths)) if p and (p.endswith((".rs", ".toml")) or p == "Cargo.lock" or p.startswith("scripts/rust-mutations"))}


def qualify(root, policy, output, tool):
    validate_policy(policy)
    root, output = Path(root).resolve(), Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    # A startup failure must never consume a previous invocation's report.
    if any(output.iterdir()):
        raise FileExistsError(f"Use a fresh empty evidence directory: {output}")
    version = subprocess.check_output([tool, "mutants", "--version"], text=True).strip()
    if version != "cargo-mutants " + policy["tool_version"]:
        raise ValueError(f"Expected cargo-mutants {policy['tool_version']}, got {version}")
    # Each invocation owns its build artifacts. Parallel workers sharing CARGO_TARGET_DIR
    # can overwrite the same test binary between build and execution.
    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)
    before = source_hashes(root)
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    args = [tool, "mutants", "--no-config"]
    for file in sorted({t["file"] for t in policy["targets"]}):
        args += ["--file", file]
    functions = "|".join(re.escape(t["function"]) for t in policy["targets"])
    args += ["--re", rf"\b({functions})( ->|$)"]
    for package in sorted({t["package"] for t in policy["targets"]}):
        args += ["--package", package]
    generated = json.loads(subprocess.check_output(args + ["--list", "--json"], cwd=root, text=True, env=env))
    wanted = {(t["file"], t["function"]) for t in policy["targets"]}
    names = [m["name"] for m in generated]
    if not names or len(set(names)) != len(names) or {identity(m) for m in generated} != wanted:
        raise ValueError("Generated inventory does not cover the exact targets")
    (output / "selected.json").write_text(json.dumps(generated, indent=2))
    command = args + ["--cargo-arg=--locked", "--cargo-arg=--lib", "--timeout", "60", "--build-timeout", "300", "--jobs", "1", "--output", str(output), "--caught", "--unviable"]
    started = time.monotonic()
    with (output / "producer.log").open("w") as log:
        result = subprocess.run(command, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
    report = json.loads((output / "mutants.out/outcomes.json").read_text())
    failures = evaluate(generated, report, policy)
    for outcome in report["outcomes"]:
        if outcome["summary"] == "CaughtMutant":
            phases = outcome["phase_results"]
            log = (output / "mutants.out" / outcome["log_path"]).read_text()
            if [p["phase"] for p in phases] != ["Build", "Test"] or phases[0]["process_status"] != "Success" or phases[1]["process_status"] != {"Failure": 101} or "test result: FAILED" not in log:
                failures.append("Caught outcome lacks a compiled test assertion failure")
    if result.returncode != 0:
        failures.append(f"Producer exit code {result.returncode}")
    if source_hashes(root) != before or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip() != head:
        failures.append("Source changed during qualification")
    counts = {k: report[k] for k in ["total_mutants", "caught", "missed", "timeout", "unviable"]}
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True).strip())
    receipt = {"schema": 1, "head": head, "source_dirty": dirty, "tool": version, "command": command, "policy": policy,
               "source_sha256": before, "elapsed_seconds": time.monotonic() - started,
               "counts": counts, "failures": failures}
    (output / "gate.json").write_text(json.dumps(receipt, indent=2))
    summary = f"Rust mutation qualification: {counts}\n\n" + ("PASS" if not failures else "FAIL: " + "; ".join(failures)) + "\n"
    (output / "summary.md").write_text(summary)
    print(summary, flush=True)
    if failures:
        raise RuntimeError("Mutation qualification failed; see gate.json and mutants.out")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["install", "run"])
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/rust-mutations")
    args = parser.parse_args()
    policy = json.loads(POLICY.read_text())
    if args.action == "install":
        subprocess.run(["cargo", "install", "cargo-mutants", "--version", policy["tool_version"], "--locked"], check=True)
    else:
        tool = os.environ.get("CARGO_MUTANTS_BIN") or shutil.which("cargo-mutants")
        if not tool:
            raise FileNotFoundError("Install the pinned tool with: python3 scripts/rust-mutations.py install")
        qualify(ROOT, policy, args.output, tool)


if __name__ == "__main__":
    main()
