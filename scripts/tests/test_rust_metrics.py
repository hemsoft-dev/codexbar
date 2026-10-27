#!/usr/bin/env python3
"""Exercise real pinned producers in a disposable Git/Cargo workspace."""
import copy
import importlib.util
import hashlib
import io
import os
import shutil
import sys
import time
from contextlib import ExitStack, contextmanager
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("rust_metrics", Path(__file__).resolve().parents[1] / "rust-metrics.py")
metrics = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metrics)


class FailureEvidence(unittest.TestCase):
    def test_corrupt_copy_is_preserved_before_original_failure_escapes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            output, target = root / "candidate", root / "target"
            profiles = target / "llvm-cov-target"
            profiles.mkdir(parents=True)
            seed = b"opaque raw-profile fixture header and body" * 8
            corrupt = seed[:len(seed) // 2]
            path = profiles / "workspace-23031-123_0.profraw"
            path.write_bytes(corrupt)
            (profiles / "workspace-profraw-list").write_text(str(path) + "\n")
            primary = subprocess.CalledProcessError(17, ["cargo", "llvm-cov"])

            def checked(args, **kwargs):
                if args[:3] == ["git", "rev-parse", "HEAD"]:
                    return "a" * 40 + "\n"
                if args[:2] == ["git", "ls-files"]:
                    return ""
                return "cargo-llvm-cov " + metrics.PINS["cargo-llvm-cov"]

            def command(args, *unused, **kwargs):
                if args[0] == "cargo":
                    raise primary

            with patch.object(metrics.subprocess, "check_output", side_effect=checked), \
                    patch.object(metrics, "run", side_effect=command), \
                    patch.object(metrics, "_failure_tool_identity", return_value={"status": "complete"}, create=True):
                with self.assertRaises(subprocess.CalledProcessError) as caught:
                    metrics.collect(root, output, target)
            self.assertIs(caught.exception, primary)
            saved = output / "failure-evidence/raw" / path.name
            self.assertEqual(saved.read_bytes(), corrupt)
            self.assertEqual(path.read_bytes(), corrupt)
            manifest = json.loads((output / "failure-evidence/manifest.json").read_text())
            row = manifest["profiles"][0]
            self.assertEqual(row["source_sha256"], hashlib.sha256(corrupt).hexdigest())
            self.assertEqual(manifest["original_failure"]["returncode"], 17)
            self.assertEqual(manifest["phase"], "coverage")
            self.assertEqual(manifest["capture_status"], "complete")
            mapping = json.loads((output / "failure-evidence/producer-map.json").read_text())
            self.assertEqual(mapping["status"], "unresolved")
            self.assertIsNone(mapping["executable"])

    @staticmethod
    def context(phase="coverage"):
        return {"phase": phase, "revision": "a" * 40, "source_sha256": {}, "commands": []}

    def retain(self, root, output, target, context=None):
        with patch.object(metrics, "_failure_tool_identity", return_value={"status": "complete"}):
            metrics.retain_collection_failure(root, output, target, context or self.context(), ValueError("fixture"))
        directories = sorted(output.glob("failure-evidence*"))
        return directories[-1], json.loads((directories[-1] / "manifest.json").read_text())

    @contextmanager
    def collection_fixture(self, root, output, target, phase, primary):
        source = root / "crates/fixture/src/lib.rs"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text("pub fn fixture() {}\n")
        profiles = target / "llvm-cov-target"
        profiles.mkdir(parents=True, exist_ok=True)
        (profiles / "fixture.profraw").write_bytes(b"opaque truncated copy")
        real_digest, real_mkdir = metrics.digest, Path.mkdir
        digest_calls = 0
        events = []

        def checked(args, **kwargs):
            if args[:3] == ["git", "rev-parse", "HEAD"]:
                if phase == "source_identity":
                    raise primary
                return "a" * 40
            if args[:2] == ["git", "ls-files"]:
                return ""
            if args[0] == "cargo-llvm-cov":
                if phase == "tool_check":
                    raise primary
                return "cargo-llvm-cov " + metrics.PINS["cargo-llvm-cov"]
            if args[0] == "cargo":
                if phase == "complexity":
                    raise primary
                return b"{}"
            return "fixture compiler"

        def command(args, *unused, **kwargs):
            events.append(args)
            if args[:2] == ["git", "diff"] and phase == "source_validation":
                raise primary
            if args[0] == "cargo":
                at = "lcov" if args[3] == "report" else "coverage"
                if phase == at:
                    raise primary
                Path(args[-1]).write_text("fixture")

        def digest(path):
            nonlocal digest_calls
            if path == source:
                digest_calls += 1
                if phase == "source_hashes" or (phase == "source_recheck" and digest_calls == 2):
                    raise primary
            return real_digest(path)

        def mkdir(path, *args, **kwargs):
            if path == output and phase == "preflight":
                raise primary
            return real_mkdir(path, *args, **kwargs)

        with ExitStack() as stack:
            stack.enter_context(patch.object(metrics.subprocess, "check_output", side_effect=checked))
            stack.enter_context(patch.object(metrics, "run", side_effect=command))
            stack.enter_context(patch.object(metrics, "digest", side_effect=digest))
            stack.enter_context(patch.object(Path, "mkdir", autospec=True, side_effect=mkdir))
            stack.enter_context(patch.object(metrics, "production_report", side_effect=primary if phase == "report" else None,
                                            return_value={}))
            stack.enter_context(patch.object(metrics, "write_summary", side_effect=primary if phase == "summary" else None))
            stack.enter_context(patch.object(metrics, "_failure_tool_identity", return_value={"status": "complete"}))
            yield events

    def test_all_collection_failure_phases_preserve_original_exception(self):
        for phase in ["preflight", "source_identity", "source_validation", "tool_check", "source_hashes",
                      "coverage", "lcov", "complexity", "source_recheck", "report", "summary"]:
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                output, target = root / "candidate", root / "target"
                primary = subprocess.CalledProcessError(23, ["inert", phase])
                with self.collection_fixture(root, output, target, phase, primary):
                    with self.assertRaises(subprocess.CalledProcessError) as caught:
                        metrics.collect(root, output, target)
                self.assertIs(caught.exception, primary)
                manifest = json.loads((output / "failure-evidence/manifest.json").read_text())
                self.assertEqual(manifest["phase"], "report" if phase == "summary" else phase)
                self.assertEqual(manifest["original_failure"]["returncode"], 23)
                self.assertEqual((output / "failure-evidence/raw/fixture.profraw").read_bytes(), b"opaque truncated copy")

    def test_base_and_candidate_failure_evidence_precedes_cleanup_for_every_phase(self):
        phases = ["preflight", "source_identity", "source_validation", "tool_check", "source_hashes",
                  "coverage", "lcov", "complexity", "source_recheck", "report", "summary"]
        for failing in ["base", "candidate"]:
            for phase in phases:
                with self.subTest(failing=failing, phase=phase), tempfile.TemporaryDirectory() as temp:
                    root = Path(temp).resolve()
                    retained = root / "retained"
                    retained.mkdir()
                    output, target = root / "out", root / "target"
                    primary = subprocess.CalledProcessError(29, ["inert", phase])
                    real_collect = metrics.collect
                    measured = []
                    cleanup = []

                    def collect(source_root, destination, cargo_target):
                        measured.append(destination.name)
                        if destination.name != failing:
                            return {}
                        with self.collection_fixture(source_root, destination, cargo_target, phase, primary):
                            return real_collect(source_root, destination, cargo_target)

                    def worktree(args, *unused, **kwargs):
                        if args[2] == "remove":
                            path = output / failing / "failure-evidence/manifest.json"
                            self.assertTrue(path.is_file(), "capture must finish before worktree removal")
                            self.assertEqual(json.loads(path.read_text())["original_failure"]["returncode"], 29)
                            cleanup.append(True)
                            shutil.rmtree(retained / "base")

                    with patch.object(metrics, "run", side_effect=worktree), \
                            patch.object(metrics, "collect", side_effect=collect), \
                            patch.object(metrics.tempfile, "mkdtemp", return_value=str(retained)):
                        with self.assertRaises(subprocess.CalledProcessError) as caught:
                            metrics.ci(root, output, target, "a" * 40)
                    self.assertIs(caught.exception, primary)
                    self.assertEqual(cleanup, [True])
                    self.assertEqual(measured, ["base"] if failing == "base" else ["base", "candidate"])

    def test_byte_file_inventory_and_deadline_caps_are_explicit(self):
        for cap, value, issue in [("file_bytes", 3, "raw_copy_byte_cap_or_short_read"),
                                  ("total_bytes", 4, "raw_file_or_total_byte_cap"),
                                  ("files", 1, "raw_file_or_total_byte_cap"),
                                  ("entries", 1, "directory_entry_cap"),
                                  ("seconds", 0, None)]:
            with self.subTest(cap=cap), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                output, target = root / "out", root / "target"
                profiles = target / "llvm-cov-target"
                profiles.mkdir(parents=True)
                for name in ["a.profraw", "b.profraw"]:
                    (profiles / name).write_bytes(b"0123456789")
                caps = dict(metrics.FAILURE_LIMITS, **{cap: value})
                with patch.object(metrics, "FAILURE_LIMITS", caps):
                    if cap == "seconds":
                        with self.assertRaises(TimeoutError):
                            self.retain(root, output, target)
                        continue
                    directory, manifest = self.retain(root, output, target)
                self.assertEqual(manifest["capture_status"], "partial")
                self.assertIn(issue, manifest["issues"])
                self.assertEqual(manifest["count_exact"], cap != "entries")
                for row in manifest["profiles"]:
                    if row.get("truncated"):
                        self.assertIsNone(row["source_sha256"])
                    if row.get("retained_as"):
                        data = (directory / row["retained_as"]).read_bytes()
                        self.assertEqual(len(data), row["captured_bytes"])
                        self.assertEqual(hashlib.sha256(data).hexdigest(), row["captured_sha256"])
                self.assertEqual((profiles / "a.profraw").read_bytes(), b"0123456789")

    def test_secure_opens_reject_symlinks_and_swap_before_read(self):
        for attack in ["symlink", "swap_symlink", "swap_regular", "directory_symlink"]:
            with self.subTest(attack=attack), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                profiles = root / "target/llvm-cov-target"
                profiles.mkdir(parents=True)
                outside = root / "outside-sentinel"
                outside.write_bytes(b"outside bytes must not be read")
                source = profiles / "race.profraw"
                source.write_bytes(b"original bytes")
                if attack == "symlink":
                    source.unlink()
                    source.symlink_to(outside)
                if attack == "directory_symlink":
                    profiles.rename(root / "original-directory")
                    profiles.symlink_to(root, target_is_directory=True)
                real_open, real_read = metrics.os.open, metrics.os.read
                changed = []
                foreign = outside.stat()

                def opened(name, flags, *args, **kwargs):
                    if name == "race.profraw" and not flags & os.O_WRONLY and attack.startswith("swap") and not changed:
                        source.rename(profiles / "preserved-original")
                        if attack == "swap_symlink":
                            source.symlink_to(outside)
                        else:
                            os.link(outside, source)
                        changed.append(True)
                    return real_open(name, flags, *args, **kwargs)

                def read(fd, count):
                    actual = os.fstat(fd)
                    self.assertNotEqual((actual.st_dev, actual.st_ino), (foreign.st_dev, foreign.st_ino))
                    return real_read(fd, count)

                with patch.object(metrics.os, "open", side_effect=opened), patch.object(metrics.os, "read", side_effect=read):
                    directory, manifest = self.retain(root, root / "out", root / "target")
                self.assertEqual(manifest["capture_status"], "partial")
                self.assertFalse((directory / "raw/race.profraw").exists())
                self.assertEqual(outside.read_bytes(), b"outside bytes must not be read")

    def test_mutation_during_copy_is_reported_without_whole_source_hash(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            profiles = root / "target/llvm-cov-target"
            profiles.mkdir(parents=True)
            source = profiles / "changed.profraw"
            source.write_bytes(b"raw bytes")
            real_read = metrics.os.read
            changed = []

            def read(fd, size):
                data = real_read(fd, size)
                if not changed:
                    with source.open("ab") as stream:
                        stream.write(b"late write")
                    changed.append(True)
                return data

            with patch.object(metrics.os, "read", side_effect=read):
                directory, manifest = self.retain(root, root / "out", root / "target")
            row = manifest["profiles"][0]
            self.assertTrue(row["identity_changed"])
            self.assertIsNone(row["source_sha256"])
            self.assertTrue((directory / row["retained_as"]).is_file())
            self.assertEqual(manifest["capture_status"], "partial")

    def test_raw_read_failure_and_tool_failure_remain_visible(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            profiles = root / "target/llvm-cov-target"
            profiles.mkdir(parents=True)
            (profiles / "bad.profraw").write_bytes(b"must remain")
            real_open = metrics._failure_open

            def opened(directory, name, expected=None):
                if name == "bad.profraw":
                    raise PermissionError("fixture")
                return real_open(directory, name, expected)

            with patch.object(metrics, "_failure_open", side_effect=opened), \
                    patch.object(metrics, "_failure_tool_identity", return_value={"status": "partial"}):
                metrics.retain_collection_failure(root, root / "out", root / "target", self.context(), ValueError())
            manifest = json.loads((root / "out/failure-evidence/manifest.json").read_text())
            self.assertIn("raw_read_or_copy_failed", manifest["issues"])
            self.assertIn("tool_identity_incomplete", manifest["issues"])
            self.assertEqual(manifest["profiles"][0]["error"]["type"], "PermissionError")
            self.assertEqual((profiles / "bad.profraw").read_bytes(), b"must remain")

    def test_exclusive_destinations_and_capture_failure_cannot_replace_primary(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            old = root / "out/failure-evidence"
            old.mkdir(parents=True)
            (old / "sentinel").write_text("original")
            (root / "target/llvm-cov-target").mkdir(parents=True)
            directory, _ = self.retain(root, root / "out", root / "target")
            self.assertEqual(directory.name, "failure-evidence-1")
            self.assertEqual((old / "sentinel").read_text(), "original")
            primary = ValueError("original")
            with patch.object(metrics, "sources", side_effect=primary), \
                    patch.object(metrics.subprocess, "check_output", return_value="a" * 40), \
                    patch.object(metrics, "retain_collection_failure", side_effect=PermissionError("capture")), \
                    patch.object(metrics.sys, "stderr", new_callable=io.StringIO) as stderr:
                with self.assertRaises(ValueError) as caught:
                    metrics.collect(root, root / "another", root / "target")
            self.assertIs(caught.exception, primary)
            self.assertIn("capture_status=failed error=PermissionError", stderr.getvalue())

    def test_actual_metadata_write_and_metadata_cap_failures_preserve_original(self):
        for failure in ["write", "cap"]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                output, target = root / "out", root / "target"
                primary = subprocess.CalledProcessError(37, ["inert"])
                with self.collection_fixture(root, output, target, "coverage", primary), ExitStack() as stack:
                    if failure == "write":
                        stack.enter_context(patch.object(metrics._FailureCapture, "document", side_effect=PermissionError("fixture")))
                    else:
                        stack.enter_context(patch.object(metrics, "FAILURE_LIMITS", dict(metrics.FAILURE_LIMITS, metadata_bytes=64)))
                    stderr = stack.enter_context(patch.object(metrics.sys, "stderr", new_callable=io.StringIO))
                    with self.assertRaises(subprocess.CalledProcessError) as caught:
                        metrics.collect(root, output, target)
                    self.assertIs(caught.exception, primary)
                    self.assertIn("capture_status=failed", stderr.getvalue())
                    if failure == "cap":
                        self.assertIn("cap=metadata_bytes", stderr.getvalue())
                self.assertEqual((target / "llvm-cov-target/fixture.profraw").read_bytes(), b"opaque truncated copy")
                self.assertEqual((output / "failure-evidence/raw/fixture.profraw").read_bytes(), b"opaque truncated copy")

    def test_tool_output_and_timeout_use_shared_budget(self):
        for code in ["import os,time; os.write(1,b'x'*1000000); time.sleep(2)", "import time; time.sleep(2)"]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as temp:
                capture = metrics._FailureCapture(Path(temp).resolve())
                capture.caps["tool_bytes"] = 32
                capture.deadline = time.monotonic() + 0.1
                started = time.monotonic()
                try:
                    record = capture.command([sys.executable, "-c", code])
                finally:
                    capture.close()
                self.assertLess(time.monotonic() - started, 1)
                self.assertEqual(record["status"], "failed")
                self.assertLessEqual(len(record["output"]), 32)

    def test_merge_list_cap_and_untrusted_log_do_not_establish_producer(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            output, target = root / "out", root / "target"
            profiles = target / "llvm-cov-target"
            profiles.mkdir(parents=True)
            output.mkdir()
            raw = profiles / "workspace-23031-123_0.profraw"
            raw.write_bytes(b"corrupt copy")
            (profiles / "workspace-profraw-list").write_text(str(raw) * 4)
            (output / "coverage.log").write_text(f"Running unittests src/main.rs ({profiles}/debug/codexbar-test)\n"
                                                "Running unittests other (/outside/executable)\n"
                                                f"warning: {raw}: invalid instrumentation profile data\n")
            with patch.object(metrics, "FAILURE_LIMITS", dict(metrics.FAILURE_LIMITS, list_bytes=8)):
                directory, manifest = self.retain(root, output, target)
            self.assertTrue(manifest["merge_input_list"]["truncated"])
            self.assertEqual((directory / "merge-input-list.txt").stat().st_size, 8)
            self.assertEqual(manifest["profiles"][0]["filename_tokens"]["pid"], "23031")
            mapping = json.loads((directory / "producer-map.json").read_text())
            self.assertEqual(mapping["status"], "unresolved")
            self.assertEqual(len(mapping["artifact_candidates"]), 1)
            self.assertIsNone(mapping["executable"])


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
