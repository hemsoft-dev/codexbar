#!/usr/bin/env python3
"""Exercise the actual pinned producer, including an assertion weakened on purpose."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "rust-mutations.py"
spec = importlib.util.spec_from_file_location("rust_mutations", SCRIPT)
mutation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mutation)


class ActualMutationGate(unittest.TestCase):
    def test_malformed_policy_fails_before_starting_a_tool(self):
        for policy in [None, False, 0, "", {}, {"schema": 1, "tool_version": "27.1.0", "targets": []}]:
            with self.assertRaises(ValueError):
                mutation.qualify(Path.cwd(), policy, Path("unused"), "/missing/cargo-mutants")

    def test_real_assertions_qualify_and_weakened_assertions_fail(self):
        tool = os.environ.get("CARGO_MUTANTS_BIN") or shutil.which("cargo-mutants")
        self.assertIsNotNone(tool, "Install the pinned cargo-mutants first")
        with tempfile.TemporaryDirectory(prefix="codexbar-mutation-fixture-") as temp:
            root = Path(temp) / "fixture"
            (root / "src").mkdir(parents=True)
            (root / "Cargo.toml").write_text('[package]\nname="mutation-fixture"\nversion="0.1.0"\nedition="2024"\n')
            source = root / "src/lib.rs"
            production = "pub fn quota_used(used:u64,limit:u64)->bool { used >= limit }\n"
            source.write_text(production + '#[test] fn boundary(){ assert!(!quota_used(9,10)); assert!(quota_used(10,10)); assert!(quota_used(11,10)); }\n')
            for args in [["cargo", "generate-lockfile"], ["git", "init", "-q"], ["git", "add", "."],
                         ["git", "-c", "user.name=Mutation fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"]]:
                subprocess.run(args, cwd=root, check=True)
            policy = {"schema": 1, "tool_version": json.loads(mutation.POLICY.read_text())["tool_version"],
                      "targets": [{"file": "src/lib.rs", "function": "quota_used", "package": "mutation-fixture", "minimum_caught": 1, "maximum_unviable": 0}]}
            good = Path(temp) / "good"
            receipt = mutation.qualify(root, policy, good, tool)
            self.assertGreater(receipt["counts"]["caught"], 0)
            generated = json.loads((good / "selected.json").read_text())
            report = json.loads((good / "mutants.out/outcomes.json").read_text())
            for field, value in [("end_time", None), ("outcomes", []), ("caught", 999)]:
                broken = copy.deepcopy(report)
                broken[field] = value
                with self.assertRaises(ValueError, msg=field):
                    mutation.evaluate(generated, broken, policy)
            with self.assertRaises(ValueError):
                mutation.evaluate([], report, policy)
            # A pre-existing successful report cannot qualify a later startup failure.
            with self.assertRaises(FileExistsError):
                mutation.qualify(root, policy, good, "/missing/cargo-mutants")
            fresh = Path(temp) / "startup-failure"
            with self.assertRaises(FileNotFoundError):
                mutation.qualify(root, policy, fresh, "/missing/cargo-mutants")
            self.assertFalse((fresh / "gate.json").exists())
            # Deliberately remove every behavioral assertion; baseline tests still pass.
            source.write_text(production + '#[test] fn boundary(){ let _ = quota_used(9,10); }\n')
            bad = Path(temp) / "weak"
            with self.assertRaises(RuntimeError):
                mutation.qualify(root, policy, bad, tool)
            rejected = json.loads((bad / "gate.json").read_text())
            self.assertGreater(rejected["counts"]["missed"], 0)
            self.assertTrue(rejected["failures"])


if __name__ == "__main__":
    unittest.main()
