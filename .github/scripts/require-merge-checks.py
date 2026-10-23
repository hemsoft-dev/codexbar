#!/usr/bin/env python3
"""Add required checks without changing any other main protection setting."""
import argparse
import json
import subprocess


def api(path, method="GET", payload=None):
    command = ["gh", "api", path, "--method", method]
    if payload is not None:
        command += ["--input", "-"]
    result = subprocess.run(command, input=None if payload is None else json.dumps(payload),
                            text=True, capture_output=True, check=True)
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="Apply the additive status requirement")
    args = parser.parse_args()
    repo = "hemsoft-dev/codexbar"
    path = f"repos/{repo}/branches/main/protection/required_status_checks"
    # The checks source is discovered from a real CI result, not a user's status.
    checks = api(f"repos/{repo}/commits/main/check-runs")["check_runs"]
    app_ids = {check["app"]["id"] for check in checks if check["name"] == "security-scan"}
    if len(app_ids) != 1:
        raise RuntimeError("Cannot identify the CI GitHub App from main's security-scan")
    app_id = app_ids.pop()
    protection = api(f"repos/{repo}/branches/main/protection")
    existing = protection.get("required_status_checks", {})
    required = {check["context"]: check for check in existing.get("checks", [])}
    for name in existing.get("contexts", []):
        required.setdefault(name, {"context": name, "app_id": -1})
    for name in ["build", "security-scan", "rust (windows-latest)", "rust (macos-latest)",
                 "rust (ubuntu-latest)", "codex-review"]:
        required[name] = {"context": name, "app_id": app_id}
    payload = {"strict": existing.get("strict", False), "checks": list(required.values())}
    print(json.dumps(payload, indent=2))
    if args.apply:
        # This endpoint changes only required statuses. It never replaces review,
        # conversation, admin-enforcement or bypass settings.
        result = api(path, "PATCH", payload)
        assert {(c["context"], c["app_id"]) for c in payload["checks"]}.issubset(
            {(c["context"], c["app_id"]) for c in result["checks"]})
        print("Required checks verified")


if __name__ == "__main__":
    main()
