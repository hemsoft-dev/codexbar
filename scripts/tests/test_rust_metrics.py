#!/usr/bin/env python3
"""Exercise real pinned producers in a disposable Git/Cargo workspace."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("rust_metrics", Path(__file__).resolve().parents[1] / "rust-metrics.py")
metrics = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metrics)


class MetricsIntegration(unittest.TestCase):
    def test_actual_producers_reject_an_uncovered_regression(self):
        with tempfile.TemporaryDirectory(prefix="codexbar-metrics-fixture-") as temp:
            root = Path(temp) / "repo"
            source = root / "crates/fixture/src/lib.rs"
            source.parent.mkdir(parents=True)
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/*"]\nresolver = "3"\n')
            (source.parents[1] / "Cargo.toml").write_text('[package]\nname = "metrics-fixture"\nversion = "0.0.0"\nedition = "2024"\n')
            source.write_text('''pub fn branch(value: bool) -> u32 {
    if value { 1 } else { 0 }
}
#[cfg(test)] fn test_only_helper() -> u32 { 42 }
#[cfg(test)] mod tests {
    #[test] fn covers_both() {
        assert_eq!(super::branch(true), 1);
        assert_eq!(super::branch(false), 0);
        assert_eq!(super::test_only_helper(), 42);
    }
}
''')
            metrics.run(["git", "init", "--quiet"], root)
            metrics.run(["cargo", "generate-lockfile"], root)
            self.commit(root)
            before = metrics.collect(root, Path(temp) / "base", Path(temp) / "target")
            self.assertEqual([entry["function"] for entry in before["entries"]], ["branch"])
            self.assertEqual(before["totals"]["lines"]["percent"], 100)
            self.assertEqual(before["totals"]["branches"]["percent"], 100)
            self.assertEqual(metrics.compare(before, before), [])
            # Deliberately add an untested, complex production function.
            with source.open("a") as stream:
                stream.write('''pub fn uncovered(value: i32) -> i32 {
    if value == 1 { return 1; }
    if value == 2 { return 2; }
    if value == 3 { return 3; }
    if value == 4 { return 4; }
    if value == 5 { return 5; }
    0
}
''')
            self.commit(root)
            after = metrics.collect(root, Path(temp) / "candidate", Path(temp) / "target")
            failures = metrics.compare(before, after)
            self.assertTrue(any("New function above CRAP 30" in failure for failure in failures), failures)
            self.assertTrue(any("coverage decreased" in failure for failure in failures), failures)
            entry = next(entry for entry in after["entries"] if entry["function"] == "uncovered")
            # Independent arithmetic sample, using the measured 0% and CC 6.
            self.assertEqual(entry["complexity"], 6)
            self.assertEqual(entry["line_coverage"], 0)
            self.assertEqual(entry["crap"], 6 * 6 * (1 - 0) ** 3 + 6)
            changed = copy.deepcopy(before)
            changed["entries"][0]["line_coverage"] = 0
            changed["entries"][0]["lines_hit"] = 0
            changed["entries"][0]["crap"] = metrics.crap(changed["entries"][0]["complexity"], 0)
            self.assertTrue(any("CRAP increased" in failure for failure in metrics.compare(before, changed)))
            invalid = copy.deepcopy(before)
            invalid["totals"]["lines"]["count"] = 0
            with self.assertRaises(ValueError):
                metrics.compare(before, invalid)
            with self.assertRaises(ValueError):
                metrics.compare(before, None)
            invalid = copy.deepcopy(before)
            invalid["entries"][0]["line_coverage"] = float("nan")
            with self.assertRaises(ValueError):
                metrics.compare(before, invalid)
            print(json.dumps({"fixture_base": before["totals"], "fixture_candidate": after["totals"],
                              "expected_rejections": failures}, indent=2))

    def test_cleanup_cannot_replace_a_measurement_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            primary = ValueError("original measurement failure")
            cleanup = RuntimeError("cleanup failure")
            retained = Path(temp) / "retained"
            retained.mkdir()
            with patch.object(metrics, "run", side_effect=[None, cleanup]), \
                    patch.object(metrics, "collect", side_effect=primary), \
                    patch.object(metrics.tempfile, "mkdtemp", return_value=str(retained)):
                with self.assertRaises(ValueError) as caught:
                    metrics.ci(Path(temp), Path(temp) / "out", Path(temp) / "target", "a" * 40)
                self.assertIs(caught.exception, primary)

    @staticmethod
    def commit(root):
        metrics.run(["git", "add", "."], root)
        metrics.run(["git", "-c", "user.name=Metrics fixture", "-c", "user.email=fixture@example.invalid",
                     "commit", "--quiet", "-m", "fixture"], root)


if __name__ == "__main__":
    unittest.main()
