#!/usr/bin/env python3
"""Select retired .NET work conservatively; verify outcomes in the existing build job."""
from __future__ import annotations

import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys

RETIRED_STEPS = (
    "dotnet", "restore", "build", "format", "test", "tools", "mutations", "security",
)
DOTNET_SUFFIXES = {".cs", ".csproj", ".sln", ".slnx", ".props", ".targets", ".xaml", ".resx", ".razor"}
RUST_FILES = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rustfmt.toml", ".rustfmt.toml"}


def needs_retired(path: str) -> bool:
    """Only known independent inputs can opt out. Everything else runs .NET."""
    parts = PurePosixPath(path).parts
    if not parts or path.startswith("/") or ".." in parts or "\\" in path:
        return True
    lower = path.lower()
    if lower.startswith("src/") or PurePosixPath(lower).suffix in DOTNET_SUFFIXES:
        return True
    if path.startswith((".github/", "scripts/", ".githooks/", ".config/")):
        return True
    if lower.endswith(".md"):
        return False
    return not (path.startswith(("crates/", ".cargo/")) or path in RUST_FILES)


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], stderr=subprocess.PIPE, timeout=30)


def commit(value: object) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", value):
        raise ValueError("event does not identify a full commit SHA")
    if set(value) == {"0"}:
        raise ValueError("event has no previous commit")
    git("cat-file", "-e", value + "^{commit}")
    return value


def select(event_name: str, event: dict) -> dict:
    try:
        checkout = git("rev-parse", "HEAD").decode("ascii").strip()
        if event_name == "pull_request":
            pr = event["pull_request"]
            base, head = commit(pr["base"]["sha"]), commit(pr["head"]["sha"])
            parents = git("show", "-s", "--format=%P", checkout).decode("ascii").split()
            if checkout != head and parents != [base, head]:
                raise ValueError("pull request event does not match this head or its exact test merge")
            start = git("merge-base", base, head).decode("ascii").strip()
        elif event_name == "push":
            base, head = commit(event["before"]), commit(event["after"])
            if checkout != head:
                raise ValueError("push event does not match the checked-out revision")
            git("merge-base", "--is-ancestor", base, head)
            start = base
        else:
            raise ValueError("unsupported or uncertain event")
        # Disable rename detection so both sides of cross-boundary moves are classified.
        # NUL separation preserves spaces, newlines and quoted names without GitHub file-list limits.
        raw = git("diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", start, head, "--")
        paths = [name.decode("utf-8") for name in raw.split(b"\0") if name]
        selected = any(needs_retired(path) for path in paths)
        return {"retired": selected, "reason": "relevant or unknown input" if selected else "known Rust/Markdown inputs only",
                "base": base, "head": head, "comparison_base": start, "checkout": checkout, "paths": paths}
    except (KeyError, TypeError, ValueError, OSError, subprocess.SubprocessError):
        return {"retired": True, "reason": "uncertain event or Git comparison; run all retired gates"}


def verify(steps: dict) -> None:
    selector = steps.get("inputs", {})
    if selector.get("outcome") != "success" or selector.get("conclusion") != "success":
        raise ValueError("input selection did not succeed in this job")
    selected = selector.get("outputs", {}).get("retired")
    if selected not in ("true", "false"):
        raise ValueError("missing or invalid retired-work selection")
    expected = "success" if selected == "true" else "skipped"
    for name in RETIRED_STEPS:
        step = steps.get(name, {})
        if step.get("outcome") != expected or step.get("conclusion") != expected:
            raise ValueError(f"retired step {name} must be {expected}, got {step}")


def main() -> int:
    if sys.argv[1:] == ["verify"]:
        try:
            verify(json.loads(os.environ["CI_STEPS_JSON"]))
        except (KeyError, ValueError, TypeError, AttributeError) as error:
            print(f"Retired qualification failed: {error}", file=sys.stderr)
            return 1
        print("Current-job retired qualification outcomes verified")
        return 0
    if sys.argv[1:] != ["select"]:
        print("usage: ci-inputs.py select|verify", file=sys.stderr)
        return 2
    try:
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
        report = select(os.environ.get("GITHUB_EVENT_NAME", ""), event)
    except (KeyError, OSError, ValueError):
        report = {"retired": True, "reason": "event payload unavailable; run all retired gates"}
    value = str(report["retired"]).lower()
    print(json.dumps(report, indent=2))
    Path("artifacts").mkdir(exist_ok=True)
    Path("artifacts/ci-inputs.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"retired={value}\n")
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
        summary.write(f"### Retired C# qualification\n\nSelected: `{value}`. {report['reason']}.\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
