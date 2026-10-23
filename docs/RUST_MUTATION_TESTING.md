# Rust mutation qualification

The `rust-mutations` job in the existing CI workflow qualifies a small set of
deterministic Rust boundaries on every pull request and push to main. It is a
blocking qualification stage under this repository's merge process. It adds no
schedule, autonomous processing workflow, App, or branch-protection setting.

## Tool and commands

[cargo-mutants 27.1.0](https://github.com/sourcefrog/cargo-mutants/releases/tag/v27.1.0)
is pinned in [the target policy](../scripts/rust-mutations.json). The tool builds
with the repository's stable Rust toolchain. Its installation uses its published
lockfile; it adds no application dependency.

```sh
python3 scripts/rust-mutations.py install
python3 scripts/tests/test_rust_mutations.py
python3 scripts/rust-mutations.py run --output artifacts/rust-mutations
```

Use a fresh empty output directory for every invocation. The wrapper prints the
exact producer command in `gate.json`, selects all generated mutants within the
named functions, and runs the mutated package's library tests with `--locked`.
It first requires the unmutated libraries to build and pass their tests.

Mutations run in scratch copies, with one worker and `CARGO_TARGET_DIR` removed
from the producer environment. Each invocation owns its build artifacts. Sharing
one target directory between parallel workers can replace a test binary between
build and execution, producing misleading results. Do not enable that mode.
Both build and test use `--lib`. A caught result must include a successful build
and an actual failed Rust test result, rather than only a compilation error.

## Targets and exclusions

| Boundary | Selected production function | Reason |
| --- | --- | --- |
| Domain | `codexbar-core/src/sus.rs`, `calculate` | Projection, lockout arithmetic and invalid-input boundaries |
| Provider | `codexbar-providers/src/balance.rs`, `parse_openrouter` | Provider-reported credits minus usage, mapped to an account balance |
| Persistence | `codexbar-store/src/subscription_price.rs`, `from_json` | Saved positive prices, unknown prices and dated changes |

Paths above are under `crates/`. All mutants generated for those functions are
included. There are no individual survivor exclusions or sampling shards.

This first stage excludes other Rust functions, OS credential stores, process
management, GPUI rendering, generated bindings, capture fixtures, and retired
C# code. The selected functions need no live provider endpoint or credentials.
The mutated package's complete library test suite runs for each viable mutant;
this does not claim whole-product mutation coverage. Normal cross-platform
tests and the separate coverage/CRAP job remain in force.

## Measured baseline and break policy

The isolated local qualification on air, macOS arm64, October 23, 2026 EDT,
tested 32 mutants in 63 seconds, excluding tool installation. CI measures its
own run and publishes its actual duration.

| Function | Generated | Caught by tests | Unviable | Survived | Timeout |
| --- | ---: | ---: | ---: | ---: | ---: |
| `calculate` | 24 | 23 | 1 | 0 | 0 |
| `parse_openrouter` | 3 | 2 | 1 | 0 | 0 |
| `from_json` | 5 | 4 | 1 | 0 | 0 |
| Total | 32 | 29 | 3 | 0 | 0 |

All 29 viable mutants were caught. The three unviable mutations attempt
`Some(Default::default())` or `Ok(Default::default())` for types without a
`Default` implementation: `Score`, `AccountSnapshot`, and `Price`. Their build
failures are retained and never counted as assertion successes.

The maintained policy requires:

- Zero surviving mutants and zero timeouts. No survivor is grandfathered.
- At least 23, 2, and 4 caught mutants respectively, with at most one unviable
  mutant per target. Removing a target or weakening its caught-count floor
  cannot silently qualify.
- Exactly one passing baseline and one completed result for every generated
  mutant, consistent totals, the pinned producer version, and all named targets.
- Fresh evidence and unchanged source hashes and Git head throughout the run.
  Empty, partial, stale or structurally invalid results fail qualification.

Fix a missed assertion before merging. A legitimate refactor, tool upgrade or
target expansion must include new measured evidence and explain any policy
change in its pull request, with a clean review of that head. Do not reduce
targets or thresholds merely to clear a failure.

The SUS input-validation test checks zero usage and separately invalid
projection, reset and window-length inputs, including infinities and NaN.
Mutation testing checks those assertions by changing the guards and arithmetic.

## Regression proof and artifacts

The integration test creates an isolated real Cargo/Git project. Its initial
quota-boundary assertions catch all three generated mutants. It then removes
the behavioral assertions while leaving a passing unmutated test. The actual
producer reports three survivors and exit code 2; the same qualification code
rejects it. The test also proves rejection of incomplete or inconsistent
reports, malformed policy, and reuse of an old success after a startup failure.

CI retains `artifacts/rust-mutations/` for 14 days, even when qualification fails:

- `selected.json`: complete generated mutation inventory and diffs.
- `mutants.out/`: producer outcomes, per-mutant logs and diffs, caught/missed/
  unviable/timeout lists, and producer version.
- `producer.log`, `gate.json`, `summary.md`: command output, exact policy and
  source hashes, commit, counts, runtime, failures and job summary.

[Upstream exit-code meanings](https://mutants.rs/exit-codes.html) and the
[producer report layout](https://mutants.rs/mutants-out.html) describe the raw
evidence. The wrapper also rejects incomplete results even when a process
returns zero.

[TEST-METRICS.md](../TEST-METRICS.md) is a historical C# snapshot. Its Stryker
score is separate and does not measure these Rust assertions.
