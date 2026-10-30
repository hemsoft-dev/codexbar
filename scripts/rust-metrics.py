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
import re
import selectors
import shlex
import stat
import time
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
            "scope": {"excluded_files": EXCLUDES, "test": False, "capture_feature": False}, "pins": PINS, "totals": totals,
            "entries": sorted(entries, key=lambda e: e["id"])}



# Failure diagnostics are opaque byte retention, never another measurement or merge.
FAILURE_LIMITS = {"entries": 4096, "files": 128, "file_bytes": 32 * 1024 * 1024,
                  "total_bytes": 256 * 1024 * 1024, "seconds": 10,
                  "list_bytes": 1024 * 1024, "log_bytes": 128 * 1024,
                  "tool_bytes": 16 * 1024, "metadata_bytes": 4 * 1024 * 1024}



class _FailureCaptureLimit(ValueError):
    """Only fixed diagnostic cap names, safe to report without exception payloads."""


def _failure_directory(path, create=False):
    """Walk by directory descriptors. No component may redirect the diagnostic reader."""
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    fd = os.open("/", flags)
    try:
        for name in Path(os.path.abspath(path)).parts[1:]:
            if create:
                try:
                    os.mkdir(name, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            expected = os.stat(name, dir_fd=fd, follow_symlinks=False)
            child = os.open(name, flags, dir_fd=fd)
            actual = os.fstat(child)
            if (expected.st_dev, expected.st_ino) != (actual.st_dev, actual.st_ino):
                os.close(child)
                raise OSError("directory identity changed")
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def _failure_stat(value):
    return {"size": value.st_size, "mtime_ns": value.st_mtime_ns, "ctime_ns": value.st_ctime_ns,
            "device": value.st_dev, "inode": value.st_ino, "mode": value.st_mode}


def _failure_open(directory, name, expected=None):
    before = expected or os.stat(name, dir_fd=directory, follow_symlinks=False)
    if not stat.S_ISREG(before.st_mode):
        raise OSError("not a direct regular file")
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    actual = os.fstat(fd)
    if not stat.S_ISREG(actual.st_mode) or _failure_stat(before) != _failure_stat(actual):
        os.close(fd)
        raise OSError("file identity changed before read")
    return fd, actual


class _FailureCapture:
    """Owns the diagnostic deadline, byte budgets and exclusively created output descriptors."""
    def __init__(self, output):
        self.caps = dict(FAILURE_LIMITS)
        self.started = time.time()
        self.deadline = time.monotonic() + self.caps["seconds"]
        self.issues, self.raw_bytes, self.tool_bytes = [], 0, 0
        self.fd = self.raw_fd = None
        output_fd = _failure_directory(output, create=True)
        try:
            for ordinal in range(128):
                self.check()
                name = "failure-evidence" + (f"-{ordinal}" if ordinal else "")
                try:
                    os.mkdir(name, 0o700, dir_fd=output_fd)
                except FileExistsError:
                    continue
                self.fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=output_fd)
                self.path = Path(output) / name
                os.mkdir("raw", 0o700, dir_fd=self.fd)
                self.raw_fd = os.open("raw", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.fd)
                break
            else:
                raise OSError("exclusive evidence directories exhausted")
        except BaseException:
            self.close()
            raise
        finally:
            os.close(output_fd)

    def close(self):
        for fd in [self.raw_fd, self.fd]:
            if fd is not None:
                os.close(fd)

    def check(self):
        if time.monotonic() >= self.deadline:
            raise TimeoutError("failure evidence deadline")

    def issue(self, code):
        if code not in self.issues and len(self.issues) < 128:
            self.issues.append(code[:128])

    def write(self, name, data, directory=None):
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=self.fd if directory is None else directory)
        try:
            view = memoryview(data)
            while view:
                self.check()
                count = os.write(fd, view[:65536])
                if count <= 0:
                    raise OSError("evidence write made no progress")
                view = view[count:]
        finally:
            os.close(fd)

    def document(self, name, value):
        data = (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode()
        if len(data) > self.caps["metadata_bytes"]:
            raise _FailureCaptureLimit("metadata_bytes")
        # A small final status document must still be possible after the capture deadline.
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=self.fd)
        try:
            with os.fdopen(fd, "wb", closefd=False) as stream:
                stream.write(data)
        finally:
            os.close(fd)

    def read(self, directory, name, limit, tail=False):
        self.check()
        fd, before = _failure_open(directory, name)
        try:
            start = max(0, before.st_size - limit) if tail else 0
            os.lseek(fd, start, os.SEEK_SET)
            data = bytearray()
            while len(data) < min(limit, before.st_size - start):
                self.check()
                chunk = os.read(fd, min(65536, limit - len(data)))
                if not chunk:
                    break
                data.extend(chunk)
            after = os.fstat(fd)
            entry = os.stat(name, dir_fd=directory, follow_symlinks=False)
            changed = _failure_stat(before) != _failure_stat(after) or _failure_stat(after) != _failure_stat(entry)
            if changed:
                self.issue("diagnostic_file_changed")
            return bytes(data), {"stat_before": _failure_stat(before), "stat_after": _failure_stat(after),
                                 "start": start, "captured_bytes": len(data), "identity_changed": changed,
                                 "truncated": start > 0 or len(data) < before.st_size}
        finally:
            os.close(fd)

    def profile(self, directory, row):
        self.check()
        name = row["name"]
        row.update(captured_bytes=0, captured_sha256=None, source_sha256=None, truncated=False)
        temporary = name + ".prefix.bin"
        source = destination = None
        digest_value = hashlib.sha256()
        try:
            source, before = _failure_open(directory, name, row.pop("_stat", None))
            row["stat_before"] = _failure_stat(before)
            limit = min(self.caps["file_bytes"], self.caps["total_bytes"] - self.raw_bytes)
            destination = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                  0o600, dir_fd=self.raw_fd)
            row["retained_as"] = "raw/" + temporary
            while row["captured_bytes"] < min(before.st_size, limit):
                self.check()
                chunk = os.read(source, min(65536, limit - row["captured_bytes"]))
                if not chunk:
                    break
                view = memoryview(chunk)
                while view:
                    self.check()
                    written = os.write(destination, view)
                    if written <= 0:
                        raise OSError("raw copy made no progress")
                    digest_value.update(view[:written])
                    row["captured_bytes"] += written
                    self.raw_bytes += written
                    view = view[written:]
            row["stat_after"] = _failure_stat(os.fstat(source))
            row["entry_after"] = _failure_stat(os.stat(name, dir_fd=directory, follow_symlinks=False))
            row["identity_changed"] = row["stat_before"] != row["stat_after"] or row["stat_after"] != row["entry_after"]
            row["truncated"] = row["captured_bytes"] != before.st_size
            if row["identity_changed"]:
                self.issue("raw_identity_changed")
            if row["truncated"]:
                self.issue("raw_copy_byte_cap_or_short_read")
            if not row["truncated"] and not row["identity_changed"]:
                # link is exclusive; never replace an unexpected output. Only the diagnostic alias is removed.
                os.link(temporary, name, src_dir_fd=self.raw_fd, dst_dir_fd=self.raw_fd, follow_symlinks=False)
                os.unlink(temporary, dir_fd=self.raw_fd)
                row["retained_as"] = "raw/" + name
                row["source_sha256"] = digest_value.hexdigest()
        except BaseException as error:
            row["error"] = {"type": type(error).__name__, "errno": getattr(error, "errno", None)}
            row["truncated"] = True
            self.issue("raw_read_or_copy_failed")
        finally:
            for fd in [source, destination]:
                if fd is not None:
                    os.close(fd)
            row.pop("_stat", None)
            if destination is not None:
                row["captured_sha256"] = digest_value.hexdigest()

    def command(self, argv):
        """Only version queries. Bound pipe reads and child cleanup by the shared deadline."""
        self.check()
        record = {"argv": argv, "status": "failed", "output": ""}
        process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        chunks = bytearray()
        try:
            with selectors.DefaultSelector() as selector:
                os.set_blocking(process.stdout.fileno(), False)
                selector.register(process.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    self.check()
                    if self.tool_bytes >= self.caps["tool_bytes"]:
                        raise _FailureCaptureLimit("tool_bytes")
                    for key, _ in selector.select(min(0.05, max(0, self.deadline - time.monotonic()))):
                        chunk = os.read(key.fd, min(4096, self.caps["tool_bytes"] - self.tool_bytes))
                        if not chunk:
                            selector.unregister(key.fileobj)
                        else:
                            chunks.extend(chunk)
                            self.tool_bytes += len(chunk)
                record["returncode"] = process.wait(timeout=max(0.001, self.deadline - time.monotonic()))
                record["status"] = "complete" if process.returncode == 0 else "failed"
        except BaseException as error:
            record["error"] = type(error).__name__
            if isinstance(error, _FailureCaptureLimit):
                record["cap_hit"] = str(error)
        finally:
            if process.poll() is None:
                process.kill()
                try:
                    process.wait(timeout=max(0.001, self.deadline - time.monotonic()))
                except subprocess.TimeoutExpired:
                    record["cleanup"] = "deadline"
            process.stdout.close()
            record["output"] = chunks.decode(errors="replace")
        return record


def _failure_tool_identity(capture):
    records = []
    for argv in [["rustc", "+" + PINS["toolchain"], "-vV"],
                 ["cargo-llvm-cov", "llvm-cov", "--version"],
                 ["rustc", "+" + PINS["toolchain"], "--print", "sysroot"]]:
        try:
            records.append(capture.command(argv))
        except BaseException as error:
            records.append({"argv": argv, "status": "failed", "error": type(error).__name__})
    host = re.search(r"^host: ([a-zA-Z0-9_-]+)$", records[0].get("output", ""), re.MULTILINE)
    if host and records[2]["status"] == "complete":
        path = Path(records[2]["output"].strip()) / "lib/rustlib" / host[1] / "bin/llvm-profdata"
        try:
            records.append(capture.command([str(path), "--version"]))
            for command in getattr(capture, "observed_merge_commands", []):
                observed = shlex.split(command)
                if not observed or observed[0] != str(path):
                    records.append({"status": "failed", "error": "observed_merge_tool_differs_from_sysroot"})
        except BaseException as error:
            records.append({"argv": [str(path)], "status": "failed", "error": type(error).__name__})
    else:
        records.append({"status": "failed", "error": "llvm_profdata_path_unresolved"})
    return {"pins": PINS, "status": "complete" if all(r["status"] == "complete" for r in records) else "partial",
            "queries": records, "llvm_profdata_path_basis": "rustc sysroot/host; actual merge argv recorded separately"}


def retain_collection_failure(root, output, target, context, original):
    root, output, target = (Path(os.path.abspath(path)) for path in [root, output, target])
    capture = _FailureCapture(output)
    directory = output_directory = None
    rows, lists, commands, candidates, log_info = [], [], [], [], {"status": "absent"}
    count, exact, inspected, raw_count = 0, True, 0, 0
    try:
        # Only this collection's direct files, accessed through verified directory descriptors.
        try:
            directory = _failure_directory(Path(target) / "llvm-cov-target")
            with os.scandir(directory) as entries:
                while inspected < capture.caps["entries"]:
                    capture.check()
                    try:
                        entry = next(entries)
                    except StopIteration:
                        break
                    inspected += 1
                    if entry.name.endswith(".profraw"):
                        raw_count += 1
                        row = {"name": entry.name, "source_sha256": None}
                        try:
                            value = os.stat(entry.name, dir_fd=directory, follow_symlinks=False)
                            row.update(size=value.st_size, mtime_ns=value.st_mtime_ns, stat_observed=_failure_stat(value), _stat=value)
                        except OSError as error:
                            row["error"] = {"type": type(error).__name__, "errno": error.errno}
                            capture.issue("raw_stat_failed")
                        rows.append(row)
                    elif entry.name.endswith("-profraw-list"):
                        lists.append(entry.name)
                else:
                    exact = False
                    capture.issue("directory_entry_cap")
        except BaseException as error:
            exact = False
            capture.issue("profile_inventory_" + type(error).__name__)
        # Read only the bounded tail of this collection's existing log. Never execute logged paths.
        rejected = None
        try:
            output_directory = _failure_directory(output)
            tail, log_info = capture.read(output_directory, "coverage.log", capture.caps["log_bytes"], tail=True)
            for line in tail.decode(errors="replace").splitlines():
                match = re.search(r"warning: (.+\.profraw): invalid instrumentation", line)
                if match and os.path.dirname(match[1]) == str(Path(target) / "llvm-cov-target"):
                    rejected = os.path.basename(match[1])
                match = re.search(r"Running .+ \(([^)]+)\)$", line)
                if match:
                    path = os.path.abspath(match[1])
                    if os.path.commonpath([path, str(Path(target) / "llvm-cov-target")]) == str(Path(target) / "llvm-cov-target"):
                        if len(candidates) < 64 and len(path) <= 1024:
                            candidates.append({"path": path, "basis": "Cargo log artifact candidate only"})
                        else:
                            capture.issue("producer_candidate_cap")
                if "failed to merge profile data" in line:
                    for quoted in re.findall(r"`([^`]+)`", line):
                        if "llvm-profdata" in quoted:
                            commands.append(quoted[:4096])
                            if len(quoted) > 4096:
                                capture.issue("merge_argv_text_cap")
        except FileNotFoundError:
            pass
        except BaseException as error:
            capture.issue("coverage_log_" + type(error).__name__)
        rows.sort(key=lambda row: (row["name"] != rejected, row["name"]))
        for row in rows:
            if count >= capture.caps["files"] or capture.raw_bytes >= capture.caps["total_bytes"]:
                row.pop("_stat", None)
                row["error"] = {"type": "retention_cap"}
                capture.issue("raw_file_or_total_byte_cap")
                continue
            try:
                capture.profile(directory, row)
                count += 1
            except BaseException as error:
                row.pop("_stat", None)
                row["error"] = {"type": type(error).__name__}
                capture.issue("raw_capture_" + type(error).__name__)
        list_info = {"status": "absent", "discovered": lists[:128], "discovered_count": len(lists)}
        if lists:
            try:
                if len(lists) != 1:
                    capture.issue("ambiguous_merge_input_lists")
                name = sorted(lists)[0]
                data, list_info = capture.read(directory, name, capture.caps["list_bytes"])
                capture.write("merge-input-list.txt", data)
                list_info.update(source_name=name, status="partial" if list_info["truncated"] else "complete")
                if list_info["truncated"]:
                    capture.issue("merge_input_list_cap")
            except BaseException as error:
                list_info = {"status": "failed", "error": type(error).__name__}
                capture.issue("merge_input_list_failed")
        capture.observed_merge_commands = commands
        tools = _failure_tool_identity(capture)
        if tools.get("status") != "complete":
            capture.issue("tool_identity_incomplete")
        capture.document("toolchain.json", tools)
        capture.document("producer-map.json", {"status": "unresolved", "executable": None,
                         "limit": "Workspace prefix and filename PID/signature tokens do not establish a writer",
                         "artifact_candidates": candidates})
        # Source hashes already obtained by collect; no additional source, environment or credential reads.
        identity = context.get("source_sha256") or {}
        if len(identity) > 512 or any(len(key) > 1024 for key in identity):
            capture.issue("source_identity_metadata_cap")
        recorded_commands = []
        for argv in context["commands"][:16]:
            if len(argv) > 64 or any(len(arg) > 1024 for arg in argv):
                capture.issue("collection_argv_cap")
            recorded_commands.append([arg[:1024] for arg in argv[:64]])
        for row in rows:
            match = re.fullmatch(r".+-([0-9]{1,20})-([0-9]{1,32})_([0-9]{1,10})\.profraw", row["name"])
            row["filename_tokens"] = ({"pid": match[1], "signature": match[2], "pool": match[3],
                                       "basis": "filename tokens only; writer not established"} if match else None)
        if time.monotonic() >= capture.deadline:
            capture.issue("capture_deadline")
        manifest = {"schema": 1, "collection": Path(output).name[:128], "phase": context["phase"],
                    "revision": context.get("revision"),
                    "source_sha256": {key[:1024]: value for key, value in list(identity.items())[:512]},
                    "original_failure": {"type": type(original).__name__, "returncode": getattr(original, "returncode", None)},
                    "collection_commands": recorded_commands, "observed_merge_commands": commands[:16],
                    "started_at": capture.started, "finished_at": time.time(), "caps": capture.caps,
                    "directory_entries_observed": inspected, "profile_count": raw_count, "count_exact": exact,
                    "profiles": rows, "profile_count_basis": "direct .profraw names, including rejected nonregular entries",
                    "merge_input_list": list_info, "coverage_log_tail": log_info,
                    "captured_raw_bytes": capture.raw_bytes, "issues": capture.issues,
                    "capture_status": "partial" if capture.issues else "complete"}
        capture.document("manifest.json", manifest)
        print(f"failure evidence capture_status={manifest['capture_status']} path={capture.path}", file=sys.stderr)
    finally:
        for fd in [directory, output_directory]:
            if fd is not None:
                os.close(fd)
        capture.close()

def coverage(command, root, env, output, context, tolerate_test_failure=False):
    # Collect every base test binary before considering a fallback.
    args = command + (["--no-fail-fast"] if tolerate_test_failure else [])
    args += ["--workspace", "--locked", "--branch", "--json", "--output-path", str(output / "llvm.json")]
    context["commands"].append(args)
    status = {"status": "passed", "tolerated": False}
    try:
        run(args, root, env, output / "coverage.log")
    except subprocess.CalledProcessError as error:
        if not tolerate_test_failure or error.returncode != 101:
            raise
        try:
            log = (output / "coverage.log").read_text(errors="replace")
        except OSError:
            raise error
        test_failed = "test result: FAILED." in log and "error: test failed, to rerun pass" in log
        producer_failed = re.search(r"error: (?:could not compile|failed to)", log, re.IGNORECASE)
        if not test_failed or producer_failed:
            raise
        # Export the profiles from that same run. Never rerun tests or ignore merge/report/compile failures.
        args = command + ["report", "--branch", "--json", "--output-path", str(output / "llvm.json")]
        context["commands"].append(args)
        run(args, root, env, output / "coverage.log")
        status = {"status": "failed", "tolerated": True, "returncode": error.returncode,
                  "failed_tests": re.findall(r"^test (.+) \.\.\. FAILED$", log, re.MULTILINE)}
        print("::warning::Base tests failed; using coverage from the completed base test run. "
              "Candidate tests and all coverage/CRAP regression gates remain strict.", file=sys.stderr)
    (output / "test-run.json").write_text(json.dumps(status, indent=2) + "\n")
    return status


def collect(root, output, target, tolerate_test_failure=False):
    context = {"phase": "preflight", "revision": None, "source_sha256": None, "commands": []}
    failure_paths = (root, output, target)
    try:
        root, output, target = root.resolve(), output.resolve(), target.resolve()
        output.mkdir(parents=True, exist_ok=True)
        if any(output.iterdir()):
            raise ValueError(f"Evidence directory must be new and empty: {output}")
        env = dict(os.environ, CARGO_TARGET_DIR=str(target), TZ="UTC")
        context["phase"] = "source_identity"
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
        context["revision"] = revision
        context["phase"] = "source_validation"
        inputs = [p.relative_to(root).as_posix() for p in sources(root)] + ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
        run(["git", "diff", "--exit-code", "HEAD", "--"] + inputs, root, log=output / "source.log")
        untracked = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "--"] + inputs, cwd=root, text=True)
        if untracked.strip():
            raise ValueError("Production Rust inputs must be committed before measurement")
        context["phase"] = "tool_check"
        for tool in ["cargo-llvm-cov"]:
            result = subprocess.check_output([tool, "llvm-cov", "--version"], env=env, text=True).strip()
            if result != f"{tool} {PINS[tool]}":
                raise ValueError(f"Unexpected tool version: {result}")
        command = ["cargo", "+" + PINS["toolchain"], "llvm-cov"]
        source_paths = sources(root)
        context["phase"] = "source_hashes"
        before = {str(path): digest(path) for path in source_paths}
        context["source_sha256"] = before
        context["phase"] = "coverage"
        test_run = coverage(command, root, env, output, context, tolerate_test_failure)
        context["phase"] = "lcov"
        context["commands"].append(command + ["report", "--branch", "--lcov", "--output-path", str(output / "lcov.info")])
        run(command + ["report", "--branch", "--lcov", "--output-path", str(output / "lcov.info")],
            root, env, output / "coverage.log")
        # Build the repository's AST adapter, even when measuring an older base tree.
        # Its public cargo-crap API supplies CC/spans; syn excludes test-only items.
        args = ["cargo", "+" + PINS["toolchain"], "run", "--locked", "--quiet", "--manifest-path",
                str(SCRIPT_ROOT.parent / "crates/codexbar-metrics/Cargo.toml"), "--"] + [str(p) for p in source_paths]
        context["phase"] = "complexity"
        with (output / "complexity.log").open("w") as log:
            result = subprocess.check_output(args, env=env, stderr=log)
        (output / "complexity.json").write_bytes(result)
        context["phase"] = "source_recheck"
        if before != {str(path): digest(path) for path in source_paths}:
            raise ValueError("Source changed during measurement")
        context["phase"] = "report"
        report = production_report(root, output / "complexity.json", output / "lcov.info")
        if revision != subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip():
            raise ValueError("Git revision changed during measurement")
        report["test_run"] = test_run
        report["commit"] = revision
        report["host"] = platform.system() + "/" + platform.machine()
        report["compiler"] = subprocess.check_output(["rustc", "+" + PINS["toolchain"], "-vV"], text=True).strip()
        report["evidence"] = {name: digest(output / name) for name in ["llvm.json", "lcov.info", "complexity.json"]}
        (output / "metrics.json").write_text(json.dumps(report, indent=2) + "\n")
        write_summary(report, output / "summary.md")
        return report
    except BaseException as original:
        try:
            retain_collection_failure(*failure_paths, context, original)
        except BaseException as error:
            cap = f" cap={error}" if isinstance(error, _FailureCaptureLimit) else ""
            print(f"failure evidence capture_status=failed error={type(error).__name__}{cap}", file=sys.stderr)
        raise


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
        base = collect(base_root, output / "base", target, tolerate_test_failure=True)
        candidate = collect(root, output / "candidate", target)
        failures = compare(base, candidate)
        (output / "gate.json").write_text(json.dumps({"base": base["commit"], "candidate": candidate["commit"], "base_test_run": base["test_run"], "failures": failures}, indent=2) + "\n")
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
