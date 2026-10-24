# Retired C# qualification inputs

Normal CI keeps its existing `build`, three-platform `rust`, `rust-metrics`,
`rust-mutations` and `security-scan` checks. It still runs for every push to
`main` and pull request targeting `main`. No branch-protection setting changes.

The `build` job always installs and audits the Markdown tools, runs Markdown
lint, runs the PowerShell tests and tests the CI selection policy. Only its
retired .NET setup, restore, build, format, test, tool restore, mutation test and
package vulnerability audit are conditional.

## Selection

[`scripts/ci-inputs.py`](../scripts/ci-inputs.py) compares complete Git trees,
using the event's immutable commit SHAs and a full-history checkout. A pull
request compares its merge base to its head. A normal push compares `before`
to `after`, requiring an ancestor relationship and the expected checkout.
The PR checkout must be its exact head or a test merge whose two parents
match the event's base and head. A newer checkout cannot reuse older inputs.
Unknown events, missing commits, zero SHAs, unavailable history, malformed
payloads and failed comparisons select all retired gates.

| Input | Retired gates |
| --- | --- |
| Anything under `src/`, or a C# source/project/solution/MSBuild/XAML/resource file anywhere | Run |
| Workflows, actions, scripts, Git hooks or `.config/` | Run |
| Shared configuration, package files or any otherwise unknown path | Run |
| Known Rust inputs under `crates/` or `.cargo/`, root Cargo/toolchain/rustfmt files | Skip |
| Markdown outside the source and shared-tooling directories above | Skip |

The run decision takes precedence over an independent-input match. A `.cs`
file moved into `crates/`, for example, still selects retired qualification.
Rename detection is disabled so both the deleted and added paths are checked.
NUL-delimited output preserves filenames, including whitespace. There is no
REST changed-file pagination or workflow path-filter limit.

Each run publishes `ci-inputs.json` with the comparison revisions, complete
paths and decision. Its step summary states whether C# was selected and why.

## Failure handling

There is no conditional child job or reusable success fetched from another
run. Conditional steps remain inside the existing `build` job. A final
`always()` step verifies the current job's own `steps` context. Selection must
have succeeded with an explicit `true` or `false`. Every retired step must
have both outcome and conclusion `success` when selected, or `skipped` when
unselected. Missing, failed, cancelled, timed-out and masked failure results
fail verification. Cancellation of the whole job remains cancellation, not a
passing check. A shared-tooling failure still fails `build` normally.

This preserves check names and avoids skipped-job dependency propagation,
which [GitHub documents](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax).
Git's [diff documentation](https://git-scm.com/docs/git-diff) describes the
NUL-separated filename and rename options used here.

## Validation

```sh
python3 scripts/tests/test_ci_inputs.py
node --test scripts/tests/ci-workflow.test.cjs
actionlint
```

The Python tests create temporary real Git histories for Rust-only pushes and
pull requests, C# changes, deletions, cross-boundary renames, shared inputs,
more than 300 paths, unusual names, diverged histories and missing/stale
revisions. They inject failure and cancellation outcomes for every selected
step, missing selection, masked failures and missing current-job results.
The Node tests bind that policy to the actual workflow conditions, preserved
triggers, job names, platforms and unconditional shared checks. They use the
existing Markdown toolchain's YAML parser after `npm ci --ignore-scripts`.

A workflow or selector change itself selects the full retired pipeline. Its
first implementation PR therefore exercises the C# branch. Later eligible
Rust-only changes exercise the skipped branch. Record actual job timings from
Actions rather than claiming a fixed speedup from the earlier audit samples.
