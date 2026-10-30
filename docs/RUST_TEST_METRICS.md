# Rust test metrics

CI's `rust-metrics` job measures the Rust product and compares the pull
request's fixed base and head revisions on the same Ubuntu runner. Pushes
compare the previous and new main commits. This is ordinary CI, with read-only
repository permissions and no scheduled audit or external analysis service.

## Base test failure fallback

Base collection runs all test binaries with `--no-fail-fast`. If cargo exits 101
and its log identifies a failed test result, the collector exports coverage
from that same run with `llvm-cov report`. It does not rerun tests. Compile and
producer errors remain fatal, as do failed profile merges and report exports.
Candidate collection never enables this fallback. The existing coverage and
per-function CRAP comparisons remain unchanged.

A fallback emits an Actions warning and records the failed test names and exit
status in `base/test-run.json`, `base/metrics.json`, and `gate.json`. The workflow
includes the base test status in its summary. Full logs and raw failure evidence
remain available if measurement fails. A failed base test can shorten that test's
execution, so its coverage is explicitly a baseline with test failures, not a
claim that the base test suite passed.

The regression suite exercises a real failing base test with the pinned tools,
verifies that the identical failure blocks candidate collection, and verifies
that uncovered candidate code still fails the coverage/CRAP gate.

## Progress alignment date dependency

The geometry test previously used the real clock while expecting a projection
tick. Gemini's demo uses 12% weekly usage and resets Thursday at 09:00 in the
local timezone. Its inferred projection exceeds the bar's 125% scale until
Friday about 01:07:41; the bar correctly clips the run and omits its tick. The
first hour also has no firm forecast. This is a weekly failure window, not an
end-of-month condition. Probes on October 1 and October 29 reproduced the same
boundary in UTC, UTC-04:00 and UTC+14:00. With CI's UTC timezone, the October 29
window is Thursday 05:00 through about 21:08 EDT.

The alignment fixture now fixes both its demo data and evaluation clock to
October 27 at 15:00 UTC, away from that reset. The product clock and reset
behavior are unchanged.

## Initial measured baseline

Measured on October 23, 2026 EDT, against product revision
[`0614051`](https://github.com/hemsoft-dev/codexbar/commit/0614051bc181ded6b7124dafaf9c7e4241bdde97)
on air, macOS arm64. Compiler `1.101.0-nightly`, commit `1a2c4797e`, LLVM
`23.1.4`, from the pinned October 22 nightly:

| Measurement | Result |
| --- | ---: |
| Production executable lines hit | 19,711 / 22,600 = 87.2168% |
| Production branch outcomes hit | 2,715 / 3,480 = 78.0172% |
| Production source functions | 1,669 |
| Functions without mapped executable lines | 105 |
| Existing functions above CRAP 30 | 39 |

Examples from the measured baseline:

| Function | CC | Line coverage | CRAP |
| --- | ---: | ---: | ---: |
| [PendingLogin::start](https://github.com/hemsoft-dev/codexbar/blob/0614051bc181ded6b7124dafaf9c7e4241bdde97/crates/codexbar-providers/src/gh_login.rs) | 12 | 0% | 156 |
| [Currency::symbol](https://github.com/hemsoft-dev/codexbar/blob/0614051bc181ded6b7124dafaf9c7e4241bdde97/crates/codexbar-core/src/metric.rs#L83) | 20 | 9 / 22 lines | 102.531931 |

The second sample independently recomputes as
`20² × (1 − 9/22)³ + 20 = 102.53193087903833`.
These are observed measurements, not imported C# targets. CI computes a fresh
Ubuntu baseline from the exact base commit; it never compares Linux results
against this macOS snapshot.

## Producers and scope

[`scripts/rust-metrics-tools.json`](../scripts/rust-metrics-tools.json) pins
`cargo-llvm-cov 0.9.1` and `nightly-2026-10-22`. Branch instrumentation
[requires nightly Rust](https://github.com/taiki-e/cargo-llvm-cov/tree/v0.9.1#usage).
The application's normal stable toolchain and format, lint, test and security
checks remain unchanged.

The build-only `codexbar-metrics` workspace member uses the public
[`cargo-crap 0.9.0` complexity API](https://docs.rs/cargo-crap/0.9.0/cargo_crap/complexity/)
and Rust's `syn` syntax tree to identify function spans and exclude test-only
items. It is not a dependency of the app and is never packaged. All workspace
dependencies, including this tool, remain in the advisory-scanned lockfile.

Production scope is handwritten functions under the four product crates'
`src/` trees. LLVM line and branch records are intersected with the union of
those spans. Inline test modules, test functions and test-only helpers are
excluded, including `cfg(all(test, ...))`; `cfg(any(test, windows))` remains
eligible production code. The nonshipping `capture` feature is treated as
disabled, including capture fixtures embedded in product source files.
Closures belong to their containing function.

File exclusions are explicit in the collector:

- `codexbar-metrics` itself, which is build-only measurement tooling.
- `ui_tests.rs`, `captures.rs` and `self_capture` sources, which are test or
  screenshot harnesses.
- `widgets/bindings.rs`, generated by `windows-bindgen` from Windows App SDK
  metadata. Handwritten widget code remains in scope.

Build scripts, examples, benches, integration-test files, dependencies and the
retired C# `src/` tree are outside this production-function scope. There are no
handwritten high-complexity function exemptions.

Instrumentation runs workspace unit and integration tests, with optional capture
features and doctests disabled. Conditional test-only statements embedded inside
an otherwise production function remain attributed to that function.

The raw LLVM totals include inline test scaffolding and are retained only as
source evidence. The filtered `metrics.json` totals and `summary.md` are the
production measurements. Nested function spans are unioned for global counts,
so overlapping source lines are counted once.

## Coverage basis and limits

Line coverage uses LLVM's executable `DA` records within each production
function. Branch coverage uses its `BRDA` outcomes within the same spans.
Only reported counters enter either denominator. LLVM may omit branches in
uninstantiated or uncalled code, so a high branch figure alone does not prove
all functions were tested. The uncovered-regression fixture demonstrates this
case and is still rejected by line coverage and CRAP.

CRAP is computed independently for each function:

```text
CRAP = CC² × (1 − line_coverage_fraction)³ + CC
```

Cyclomatic complexity includes each `?` with weight 1.0, using the pinned
producer's McCabe definition. CRAP uses line coverage, not branch coverage.
A function with CC 6 and measured zero line coverage scores `36 + 6 = 42`.

No executable lines means **unknown**, not 100%. Such functions remain in the
report and receive pessimistic zero coverage for CRAP. This exposes
platform-inactive functions instead of hiding them. The global coverage
percentages describe instrumented functions on the measured host. They do not
claim Windows coverage from an Ubuntu or macOS run. The existing Windows CI
test job continues to qualify Windows behavior separately.

## Non-regression policy

The job rebuilds both revisions with identical producer versions, scope,
compiler, host and UTC timezone. It fails on:

- Any decrease in measured production line or branch percentage beyond
  `0.000001` percentage points, a floating-point comparison tolerance.
- Any existing function's CRAP increase greater than `0.01` points.
- Any new function above CRAP 30, including one with unknown coverage.
- Missing, empty, stale or malformed evidence, duplicate function identities,
  changed source hashes, incomparable producers or failed collection/cleanup.

Existing functions above 30 are visible baseline debt, not a claim of passing
an absolute CRAP-30 target. They may not deteriorate. New functions above 30
must gain coverage or be refactored; CC above 30 requires refactoring even at
full coverage. No automatic baseline refresh or threshold lowering clears a
failure. Review changes to the scope, pins and policy as changes to the gate.

Identity is source path, qualified function name and source-order occurrence
for duplicate names. A relocation or rename is treated as a new function and
must meet the new-function ceiling. Line-number shifts alone do not reset it.

## Commands and evidence

With Python 3.9+, Rust and the documented platform build dependencies:

```sh
python3 scripts/rust-metrics.py install
python3 scripts/tests/test_rust_metrics.py
python3 scripts/rust-metrics.py collect --output /tmp/new-rust-metrics
python3 scripts/rust-metrics.py ci --base FULL_BASE_SHA --output /tmp/new-rust-comparison
```

Output directories must be new and empty. Each run uses fresh workspace
profiles, a fixed Git revision and source hashes. Collection failures cannot
reuse a prior report. Raw LLVM JSON, LCOV, AST spans, source hashes, compiler
version, per-function results and logs are uploaded in the `rust-metrics`
artifact for 14 days. The job summary lists line/branch totals and the worst
30 CRAP values. Base and candidate measurements remain separate.

The integration test creates a disposable real Cargo/Git workspace, runs the
pinned compiler and coverage producer, and checks full coverage of a small
function. It then adds an uncovered CC-6 function, verifies the independently
recomputed CRAP 42 and requires the configured gate to reject it. Test helpers
must be absent from production totals. Malformed metrics and cleanup failures
also have regression coverage.

[`TEST-METRICS.md`](../TEST-METRICS.md) is the historical retired C# snapshot;
none of its percentages or mutation scores describes this Rust product.
