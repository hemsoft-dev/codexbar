"""Real temporary Git histories and current-job outcome failures for CI input selection."""
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "ci-inputs.py"
spec = importlib.util.spec_from_file_location("ci_inputs", SCRIPT)
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


@contextlib.contextmanager
def inside(path):
    previous = Path.cwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous)


class Inputs(unittest.TestCase):
    def test_only_known_independent_inputs_skip_retired_work(self):
        for name in ["crates/app/src/main.rs", "crates/app/theme.json", "Cargo.lock", "Cargo.toml",
                     "rust-toolchain.toml", ".cargo/audit.toml", "README.md", "docs/policy.md"]:
            with self.subTest(name=name):
                self.assertFalse(policy.needs_retired(name))

    def test_shared_dotnet_unknown_and_ambiguous_paths_select_retired_work(self):
        for name in ["src/Core/a.cs", "src/README.md", "other/a.CS", "other/a.csproj", "a.slnx",
                     "Directory.Build.props", "a.targets", "a.xaml", "a.resx", "a.razor", "global.json",
                     "NuGet.config", "dotnet-tools.json", ".editorconfig", "stylecop.json",
                     "scripts/test.ps1", ".githooks/pre-commit", ".github/workflows/ci.yml",
                     ".github/actions/check/action.yml", ".config/dotnet-tools.json", "package-lock.json",
                     "new-tool.json", "crates/app/other.cs", "crates/../src/a.cs", "/tmp/a.rs", "",
                     "crates\\app\\main.rs"]:
            with self.subTest(name=name):
                self.assertTrue(policy.needs_retired(name))


class GitInputs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "-b", "main")
        self.git("config", "user.name", "CI fixture")
        self.git("config", "user.email", "ci-fixture@example.invalid")
        self.write("src/legacy.cs", "legacy\n")
        self.write("crates/app/main.rs", "rust\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.PIPE).decode().strip()

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def select(self, name="push", base=None, head=None):
        base, head = base or self.base, head or self.git("rev-parse", "HEAD")
        event = {"before": base, "after": head} if name == "push" else {
            "pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        with inside(self.root):
            return policy.select(name, event)

    def test_rust_only_push_and_pull_request_use_current_revisions(self):
        self.write("crates/app/main.rs", "changed\n")
        head = self.commit()
        for event in ["push", "pull_request"]:
            report = self.select(event)
            self.assertFalse(report["retired"])
            self.assertEqual(report["head"], head)
            self.assertEqual(report["checkout"], head)
            self.assertEqual(report["paths"], ["crates/app/main.rs"])

    def test_deletion_of_legacy_source_selects_retired_work(self):
        (self.root / "src/legacy.cs").unlink()
        self.commit()
        self.assertTrue(self.select()["retired"])

    def test_legacy_source_changes_select_retired_work(self):
        self.write("src/legacy.cs", "changed\n")
        self.commit()
        self.assertTrue(self.select()["retired"])

    def test_stale_pull_request_head_cannot_select_using_a_newer_checkout(self):
        self.write("src/legacy.cs", "newer checkout\n")
        self.commit()
        self.assertTrue(self.select("pull_request", self.base, self.base)["retired"])

    def test_exact_synthetic_merge_parents_bind_the_pull_request_event(self):
        self.git("checkout", "-qb", "feature")
        self.write("crates/app/main.rs", "feature\n")
        head = self.commit()
        self.git("checkout", "main")
        self.write("src/legacy.cs", "already qualified main\n")
        base = self.commit()
        self.git("merge", "--no-ff", "-qm", "synthetic merge", "feature")
        report = self.select("pull_request", base, head)
        self.assertFalse(report["retired"])
        self.assertEqual(report["checkout"], self.git("rev-parse", "HEAD"))

    def test_rename_out_of_legacy_source_classifies_both_sides(self):
        self.git("mv", "src/legacy.cs", "crates/app/old.txt")
        self.commit()
        report = self.select()
        self.assertTrue(report["retired"])
        self.assertEqual(set(report["paths"]), {"src/legacy.cs", "crates/app/old.txt"})

    def test_rename_into_legacy_source_selects_retired_work(self):
        self.git("mv", "crates/app/main.rs", "src/old.txt")
        self.commit()
        self.assertTrue(self.select()["retired"])

    def test_shared_or_unknown_input_runs_retired_work(self):
        for name in ["scripts/shared.ps1", ".github/workflows/ci.yml", "new-input"]:
            self.write(name, "changed\n")
            self.commit()
            self.assertTrue(self.select()["retired"])

    def test_full_diff_is_not_limited_to_three_hundred_files(self):
        for i in range(320):
            self.write(f"crates/app/{i:04}.rs", "rust\n")
        self.write("src/z-final.cs", "legacy\n")
        self.commit()
        report = self.select()
        self.assertEqual(len(report["paths"]), 321)
        self.assertTrue(report["retired"])

    @unittest.skipIf(os.name == "nt", "Windows filesystems reject newline filenames")
    def test_nul_diff_preserves_newlines_in_filenames(self):
        self.write("crates/app/a\nname.rs", "rust\n")
        self.commit()
        report = self.select()
        self.assertEqual(report["paths"], ["crates/app/a\nname.rs"])
        self.assertFalse(report["retired"])

    def test_pull_request_uses_merge_base_and_ignores_unrelated_new_base_changes(self):
        self.git("checkout", "-qb", "feature")
        self.write("crates/app/main.rs", "feature\n")
        head = self.commit()
        self.git("checkout", "main")
        self.write("src/legacy.cs", "new main\n")
        base = self.commit()
        self.git("checkout", "feature")
        report = self.select("pull_request", base, head)
        self.assertFalse(report["retired"])
        self.assertEqual(report["comparison_base"], self.base)
        self.assertTrue(self.select("push", base, head)["retired"], "a non-ancestor push is uncertain")

    def test_missing_zero_invalid_and_stale_commit_inputs_run_retired_work(self):
        self.write("crates/app/main.rs", "changed\n")
        head = self.commit()
        for base in ["0" * 40, "f" * 40, "--output=unsafe", "short"]:
            self.assertTrue(self.select(base=base)["retired"])
        self.assertTrue(self.select(head=self.base)["retired"], "stale push must not opt out")
        with inside(self.root):
            for event in [{}, None, [], {"pull_request": {}}]:
                self.assertTrue(policy.select("pull_request", event)["retired"])
            self.assertTrue(policy.select("unknown", {})["retired"])
        self.assertNotEqual(head, self.base)

    def test_missing_event_file_emits_explicit_true_and_a_report(self):
        env = os.environ | {"GITHUB_EVENT_PATH": str(self.root / "missing.json"),
                            "GITHUB_OUTPUT": str(self.root / "outputs"),
                            "GITHUB_STEP_SUMMARY": str(self.root / "summary")}
        result = subprocess.run([sys.executable, str(SCRIPT), "select"], cwd=self.root, env=env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "outputs").read_text().strip(), "retired=true")
        self.assertTrue(json.loads((self.root / "artifacts/ci-inputs.json").read_text())["retired"])


def outcomes(selected):
    result = {"inputs": {"outcome": "success", "conclusion": "success", "outputs": {"retired": selected}}}
    state = "success" if selected == "true" else "skipped"
    result.update({name: {"outcome": state, "conclusion": state} for name in policy.RETIRED_STEPS})
    return result


class Outcomes(unittest.TestCase):
    def test_selected_success_and_unselected_explicit_skips_pass(self):
        policy.verify(outcomes("true"))
        policy.verify(outcomes("false"))

    def test_each_selected_step_rejects_failure_cancellation_skips_and_missing_results(self):
        for name in policy.RETIRED_STEPS:
            for state in ["failure", "cancelled", "skipped", "timed_out", "", "stale"]:
                with self.subTest(name=name, state=state):
                    steps = outcomes("true")
                    steps[name] = {"outcome": state, "conclusion": state}
                    with self.assertRaises(ValueError):
                        policy.verify(steps)
            steps = outcomes("true")
            del steps[name]
            with self.assertRaises(ValueError):
                policy.verify(steps)

    def test_continue_on_error_cannot_hide_selected_failure(self):
        steps = outcomes("true")
        steps["test"] = {"outcome": "failure", "conclusion": "success"}
        with self.assertRaises(ValueError):
            policy.verify(steps)

    def test_missing_failed_or_invalid_selection_never_passes(self):
        for selector in [{}, {"outcome": "cancelled"},
                         {"outcome": "failure", "conclusion": "success", "outputs": {"retired": "false"}},
                         {"outcome": "success", "conclusion": "success", "outputs": {}},
                         {"outcome": "success", "conclusion": "success", "outputs": {"retired": "unknown"}}]:
            steps = outcomes("false")
            steps["inputs"] = selector
            with self.assertRaises(ValueError):
                policy.verify(steps)

    def test_false_selection_cannot_reuse_a_success_from_selected_work(self):
        steps = outcomes("false")
        steps["build"] = {"outcome": "success", "conclusion": "success"}
        with self.assertRaises(ValueError):
            policy.verify(steps)

    def test_verification_cli_fails_for_a_missing_current_job_result(self):
        steps = outcomes("true")
        del steps["mutations"]
        result = subprocess.run([sys.executable, str(SCRIPT), "verify"],
                                env=os.environ | {"CI_STEPS_JSON": json.dumps(steps)}, capture_output=True)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
