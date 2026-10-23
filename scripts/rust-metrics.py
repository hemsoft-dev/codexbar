#!/usr/bin/env python3
"""Measure production Rust functions, retain raw evidence, and gate regressions."""
import argparse
import hashlib
import fnmatch
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile

SCRIPT_ROOT = Path(__file__).resolve().parent
PINS = json.loads((SCRIPT_ROOT / "rust-metrics-tools.json").read_text())
EXCLUDES = ["crates/codexbar-metrics/**", "**/widgets/bindings.rs", "**/ui_tests.rs",
            "**/captures.rs", "**/self_capture.rs", "**/self_capture/**"]
EPSILON = 0.01  # CRAP points, not percentage points.


def run(args, root=None, env=None, log=None):
    if log:
        with log.open("a") as stream:
            subprocess.run(args, cwd=root, env=env, stdout=stream, stderr=stream, check=True)
    else:
        subprocess.run(args, cwd=root, env=env, check=True)


def read_json(path):
    def reject_constant(value):
        raise ValueError(f"Non-finite JSON value: {value}")
    return json.loads(path.read_text(), parse_constant=reject_constant)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sources(root):
    return sorted(p for p in root.glob("crates/*/src/**/*.rs")
                  if not any(fnmatch.fnmatchcase(p.relative_to(root).as_posix(), pattern) for pattern in EXCLUDES))


def relative(path, root):
    value = Path(path)
    return (value if value.is_absolute() else root / value).resolve().relative_to(root.resolve()).as_posix()


def lcov_records(path, root):
    """Union repeated SF records, retaining branch identifiers and real hit counts."""
    records = {}
    current = None
    for row in path.read_text().splitlines():
        if row.startswith("SF:"):
            try:
                name = relative(row[3:], root)
            except ValueError:  # Dependency/compiler paths are outside production scope.
                current = None
                continue
            current = records.setdefault(name, {"lines": {}, "branches": {}})
        elif row == "end_of_record":
            current = None
        elif current is not None and row.startswith("DA:"):
            line, hits = map(int, row[3:].split(",")[:2])
            current["lines"][line] = current["lines"].get(line, 0) + hits
        elif current is not None and row.startswith("BRDA:"):
            line, block, branch, hits = row[5:].split(",")
            key = (int(line), block, branch)
            current["branches"][key] = current["branches"].get(key, 0) + (0 if hits == "-" else int(hits))
    return records


def percent(hit, total):
    return 100.0 * hit / total if total else None


def crap(complexity, coverage):
    return complexity ** 2 * (1 - (coverage or 0) / 100) ** 3 + complexity


def production_report(root, cache_path, lcov_path):
    cache = read_json(cache_path)
    if cache["schema"] != 1 or cache["complexity_producer"] != "cargo-crap " + PINS["cargo-crap"]:
        raise ValueError("Unsupported complexity producer/schema/weight")
    coverage = lcov_records(lcov_path, root)
    entries, selected_lines, selected_branches = [], {}, {}
    files = cache["files"]
    expected = {p.resolve() for p in sources(root)}
    if not files or {Path(record["path"]).resolve() for record in files} != expected or len(files) != len(expected):
        raise ValueError("Incomplete/duplicate source complexity evidence")
    for record in sorted(files, key=lambda record: record["path"]):
        name = relative(record["path"], root)
        if not name.startswith("crates/") or "/src/" not in name:
            raise ValueError(f"Out-of-scope source: {name}")
        data = (root / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != record["sha256"]:
            raise ValueError(f"Stale source analysis: {name}")
        counts = {}
        file_cov = coverage.get(name, {"lines": {}, "branches": {}})
        for function in record["functions"]:
            start, end = function["start_line"], function["end_line"]
            cc = function["cyclomatic"]
            if not 1 <= start <= end or not math.isfinite(cc) or cc < 1:
                raise ValueError("Invalid function complexity/span")
            lines = {line: hits for line, hits in file_cov["lines"].items() if start <= line <= end}
            branches = {key: hits for key, hits in file_cov["branches"].items() if start <= key[0] <= end}
            selected_lines.update({(name, line): hits for line, hits in lines.items()})
            selected_branches.update({(name,) + key: hits for key, hits in branches.items()})
            line_cov = percent(sum(hits > 0 for hits in lines.values()), len(lines))
            # No instrumented lines means unknown, including cfg-inactive functions.
            # Do not inherit cargo-crap's 100% value for an empty span.
            ordinal = counts.get(function["name"], 0)
            counts[function["name"]] = ordinal + 1
            entries.append({"id": f"{name}|{function['name']}|{ordinal}", "file": name,
                            "function": function["name"], "line": start, "end_line": end,
                            "complexity": cc, "line_coverage": line_cov,
                            "lines": len(lines), "lines_hit": sum(h > 0 for h in lines.values()),
                            "branches": len(branches), "branches_hit": sum(h > 0 for h in branches.values()),
                            "crap": crap(cc, line_cov)})
    if not entries or not selected_lines or not selected_branches:
        raise ValueError("Missing production function/line/branch measurements")
    totals = {}
    for kind, values in [("lines", selected_lines), ("branches", selected_branches)]:
        totals[kind] = {"count": len(values), "hit": sum(h > 0 for h in values.values())}
        totals[kind]["percent"] = percent(totals[kind]["hit"], totals[kind]["count"])
    totals["functions"] = len(entries)
    totals["unmapped_functions"] = sum(e["line_coverage"] is None for e in entries)
    return {"schema": 1, "basis": "production-function executable lines; unknown coverage pessimistic",
            "scope": EXCLUDES, "pins": PINS, "totals": totals,
            "entries": sorted(entries, key=lambda e: e["id"])}


def collect(root, output, target):
    root, output, target = root.resolve(), output.resolve(), target.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError(f"Evidence directory must be new and empty: {output}")
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), TZ="UTC")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    inputs = [p.relative_to(root).as_posix() for p in sources(root)] + ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
    run(["git", "diff", "--exit-code", "HEAD", "--"] + inputs, root, log=output / "source.log")
    untracked = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "--"] + inputs, cwd=root, text=True)
    if untracked.strip():
        raise ValueError("Production Rust inputs must be committed before measurement")
    for tool in ["cargo-llvm-cov"]:
        result = subprocess.check_output([tool, "llvm-cov", "--version"], env=env, text=True).strip()
        if result != f"{tool} {PINS[tool]}":
            raise ValueError(f"Unexpected tool version: {result}")
    command = ["cargo", "+" + PINS["toolchain"], "llvm-cov"]
    source_paths = sources(root)
    before = {str(path): digest(path) for path in source_paths}
    run(command + ["--workspace", "--locked", "--branch", "--json", "--output-path", str(output / "llvm.json")],
        root, env, output / "coverage.log")
    run(command + ["report", "--branch", "--lcov", "--output-path", str(output / "lcov.info")],
        root, env, output / "coverage.log")
    # Build the repository's AST adapter, even when measuring an older base tree.
    # Its public cargo-crap API supplies CC/spans; syn excludes test-only items.
    args = ["cargo", "+" + PINS["toolchain"], "run", "--locked", "--quiet", "--manifest-path",
            str(SCRIPT_ROOT.parent / "crates/codexbar-metrics/Cargo.toml"), "--"] + [str(p) for p in source_paths]
    with (output / "complexity.log").open("w") as log:
        result = subprocess.check_output(args, env=env, stderr=log)
    (output / "complexity.json").write_bytes(result)
    if before != {str(path): digest(path) for path in source_paths}:
        raise ValueError("Source changed during measurement")
    report = production_report(root, output / "complexity.json", output / "lcov.info")
    if revision != subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip():
        raise ValueError("Git revision changed during measurement")
    report["commit"] = revision
    report["host"] = platform.system() + "/" + platform.machine()
    report["compiler"] = subprocess.check_output(["rustc", "+" + PINS["toolchain"], "-vV"], text=True).strip()
    report["evidence"] = {name: digest(output / name) for name in ["llvm.json", "lcov.info", "complexity.json"]}
    (output / "metrics.json").write_text(json.dumps(report, indent=2) + "\n")
    write_summary(report, output / "summary.md")
    return report


def validate(report):
    if not isinstance(report, dict) or report.get("schema") != 1 or not report.get("entries"):
        raise ValueError("Incomplete metrics document")
    if len({e["id"] for e in report["entries"]}) != len(report["entries"]):
        raise ValueError("Duplicate function identities")
    for entry in report["entries"]:
        cc, cov = entry["complexity"], entry["line_coverage"]
        if not math.isfinite(cc) or cc < 1 or (cov is not None and (not math.isfinite(cov) or not 0 <= cov <= 100)):
            raise ValueError("Invalid function measurement")
        for kind in ["lines", "branches"]:
            if type(entry[kind]) is not int or type(entry[kind + "_hit"]) is not int or not 0 <= entry[kind + "_hit"] <= entry[kind]:
                raise ValueError("Invalid function coverage counts")
        expected_cov = percent(entry["lines_hit"], entry["lines"])
        if (cov is None) != (expected_cov is None) or (cov is not None and abs(cov - expected_cov) > 1e-6):
            raise ValueError("Invalid function coverage percentage")
        if not math.isfinite(entry["crap"]) or abs(entry["crap"] - crap(cc, cov)) > 1e-6:
            raise ValueError("Invalid CRAP evidence")
    for kind in ["lines", "branches"]:
        value = report["totals"][kind]
        if type(value["hit"]) is not int or type(value["count"]) is not int or not 0 <= value["hit"] <= value["count"] or value["count"] <= 0:
            raise ValueError("Missing coverage measurement")
        if not math.isfinite(value["percent"]) or abs(value["percent"] - percent(value["hit"], value["count"])) > 1e-6:
            raise ValueError("Invalid coverage percentage")


def compare(base, candidate):
    validate(base)
    validate(candidate)
    for key in ["schema", "basis", "scope", "pins", "host", "compiler"]:
        if base[key] != candidate[key]:
            raise ValueError(f"Incomparable metrics: {key}")
    old = {e["id"]: e for e in base["entries"]}
    failures = []
    for kind in ["lines", "branches"]:
        if candidate["totals"][kind]["percent"] + 1e-6 < base["totals"][kind]["percent"]:
            failures.append(f"Production {kind} coverage decreased")
    for entry in candidate["entries"]:
        previous = old.get(entry["id"])
        if previous and entry["crap"] > previous["crap"] + EPSILON:
            failures.append(f"CRAP increased: {entry['id']}: {previous['crap']:.6f} -> {entry['crap']:.6f}")
        elif not previous and entry["crap"] > 30:
            failures.append(f"New function above CRAP 30: {entry['id']}: {entry['crap']:.6f}")
    return failures


def write_summary(report, path):
    rows = ["# Rust production metrics", "", f"Revision `{report['commit']}`, {report['host']}.", "",
            "CRAP uses executable-line coverage, not branch coverage. Unknown spans are pessimistic.", ""]
    for kind in ["lines", "branches"]:
        value = report["totals"][kind]
        rows.append(f"- {kind}: {value['hit']}/{value['count']} = {value['percent']:.4f}%")
    rows += [f"- Production functions: {report['totals']['functions']}; unknown spans: {report['totals']['unmapped_functions']}.", "",
             "| Function | Location | CC | Line coverage | CRAP |", "| --- | --- | ---: | ---: | ---: |"]
    for entry in sorted(report["entries"], key=lambda e: (-e["crap"], e["id"]))[:30]:
        cov = "unknown" if entry["line_coverage"] is None else f"{entry['line_coverage']:.4f}%"
        url = f"https://github.com/hemsoft-dev/codexbar/blob/{report['commit']}/{entry['file']}#L{entry['line']}"
        rows.append(f"| `{entry['function']}` | [{entry['file']}:{entry['line']}]({url}) | {entry['complexity']} | {cov} | {entry['crap']:.6f} |")
    path.write_text("\n".join(rows) + "\n")


def ci(root, output, target, base_sha):
    output.mkdir(parents=True, exist_ok=False)
    # Never recursively delete a registered worktree after a failed Git removal.
    temp = Path(tempfile.mkdtemp(prefix="codexbar-metrics-"))
    base_root = temp / "base"
    run(["git", "worktree", "add", "--detach", str(base_root), base_sha], root)
    primary_failure = None
    try:
        base = collect(base_root, output / "base", target)
        candidate = collect(root, output / "candidate", target)
        failures = compare(base, candidate)
        (output / "gate.json").write_text(json.dumps({"base": base["commit"], "candidate": candidate["commit"], "failures": failures}, indent=2) + "\n")
        if failures:
            raise ValueError("\n".join(failures))
    except BaseException as error:
        primary_failure = error
        raise
    finally:
        try:
            run(["git", "worktree", "remove", str(base_root)], root)
            temp.rmdir()
        except Exception as error:
            if primary_failure is None:
                raise
            print(f"Worktree cleanup also failed; preserved {base_root} and the measurement failure: {error}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["install", "collect", "ci", "compare"])
    parser.add_argument("--root", type=Path, default=SCRIPT_ROOT.parent)
    parser.add_argument("--output", type=Path, default=Path("artifacts/rust-metrics"))
    parser.add_argument("--target", type=Path, default=Path(tempfile.gettempdir()) / "codexbar-metrics-target")
    parser.add_argument("--base")
    args = parser.parse_args()
    if args.mode == "install":
        run(["rustup", "toolchain", "install", PINS["toolchain"], "--profile", "minimal", "--component", "llvm-tools-preview"])
        for tool in ["cargo-llvm-cov"]:
            run(["cargo", "install", tool, "--version", PINS[tool], "--locked"])
    elif args.mode == "collect":
        collect(args.root, args.output, args.target)
    elif args.mode == "ci":
        if not args.base or len(args.base) != 40 or any(c not in "0123456789abcdef" for c in args.base):
            raise ValueError("A fixed full base commit is required")
        ci(args.root.resolve(), args.output.resolve(), args.target.resolve(), args.base)
    else:
        failures = compare(read_json(Path(args.base)), read_json(args.output))
        if failures:
            raise ValueError("\n".join(failures))


if __name__ == "__main__":
    main()
