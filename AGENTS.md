# AGENTS.md — CodexBar

## Search online

Always use web search to support your statements. Good sources are:

GitHub (Issues, PR's Releases, Changelogs)
Reddit
Microsoft Developer Documentation
Social Media.

## Repository automation

Keep the normal CI workflow and the required Codex review integration; Copilot does not review in this org.
Do not install autonomous issue processing, scheduled audit, or PR promotion
workflows without an explicit maintainer request.

Main requires the CI checks in [the versioned ruleset](.github/rulesets/main.json).
Approving reviews are not required. The merge owner must verify a clean Codex
review of the exact current head and resolve every review conversation through
[the merge policy](docs/merge-policy.md), including conditional Package checks,
before a guarded merge. Codex review is enforced by this process, not a GitHub
approval or status requirement. Never bypass the ruleset or admin enforcement.

## Quality Gates

The Rust app (`crates/`) is the product; the C# / WPF app under `src/` is retired (2026-10-09). All changes must pass
these gates before merge. The pre-commit hook runs 1, 2 and 4; the `rust` CI job runs 1 to 4 on Windows, macOS and
Ubuntu:

1. **Format** — `cargo fmt --all --check` clean
2. **Lint** — `cargo clippy --workspace --all-targets --locked -- -D warnings` clean
3. **Production binary** — `cargo check -p codexbar-app --bin codexbar --locked` clean. Lint and tests build with
   gpui-kit's `test-support` feature; this builds the binary as it ships, without it.
4. **Tests** — `cargo test --workspace --locked` all green

The other required checks in [the ruleset](.github/rulesets/main.json) run only in CI:

- **`rust-metrics`** — Rust coverage and per-function CRAP must not regress against the base revision
  ([docs/RUST_TEST_METRICS.md](docs/RUST_TEST_METRICS.md)).
- **`rust-mutations`** — mutation qualification of the domain, provider and persistence assertions
  ([docs/RUST_MUTATION_TESTING.md](docs/RUST_MUTATION_TESTING.md)).
- **`security-scan`** — secret and Miasma scans plus a locked Rust advisory scan
  ([docs/RUST_DEPENDENCY_SECURITY.md](docs/RUST_DEPENDENCY_SECURITY.md)).
- **`build`** — Markdown lint (`npm run lint:md`), the npm audit, the CI workflow and Copilot Metrics script tests, and
  the retired C# gates when [the CI input selection](docs/CI_INPUTS.md) picks them.

There is no single local command for every gate. `npm run perfection:audit:retired-csharp` checks only the retired
C# app (see below); it runs no Rust gate.

## Project Structure

```text
Cargo.toml                # Workspace; members are crates/*
crates/
  codexbar-core/          # Models, metrics, alerts, layout, formatting
  codexbar-providers/     # One module per provider, HTTP and OAuth helpers; tests/ holds integration tests
  codexbar-store/         # Settings, credentials, snapshots, usage history
  codexbar-app/           # GPUI dashboard and tray host (binary `codexbar`)
  codexbar-metrics/       # Build-time tooling for the rust-metrics gate; never shipped
scripts/                  # CI, security, metrics and maintenance scripts; their tests are in scripts/tests/
src/                      # Retired C# / WPF app (see "Retired C# app")
```

## Rust Conventions

- Edition 2024; `rust-toolchain.toml` selects stable with rustfmt and clippy
- Lints come from `[workspace.lints]` in the root `Cargo.toml`; each crate opts in with `[lints] workspace = true`
- Keep Clippy clean with `-D warnings`; fix the cause rather than suppressing the lint
- Add shared dependencies to `[workspace.dependencies]` in the root `Cargo.toml`

## Testing

- Unit tests sit beside the code in `#[cfg(test)] mod tests`; cross-module provider tests go in
  `crates/codexbar-providers/tests/`
- Name tests in snake_case after the behavior they prove (`codex_results_keep_their_id_and_metric_keys`)
- Cover both success and failure paths for providers, with recorded responses under the crate's `fixtures/`
- UI tests are headless: real views in in-process GPUI test windows (`crates/codexbar-app/src/ui_tests.rs`). Never
  open desktop windows, send OS input or change focus in a test
- Tests must not read the real user profile, credentials or environment; give each test its own temporary folder

## Git

- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`
- One logical change per commit
- No merge conflict markers committed

## Retired C# app

This section applies only to changes under `src/` (or to C# project files). Such changes must also pass the C#
gates: `dotnet build` with zero warnings, `dotnet format --verify-no-changes`, `dotnet test`, coverage at or above
the current threshold, and `dotnet list package --vulnerable` clean. The pre-commit hook runs them when C# files
change, and CI's `build` job when its input selection picks them. Some `CodexBar.App.Tests` create WPF windows, so run
`dotnet test` only where that can't disturb a desktop.

`npm run perfection:audit:retired-csharp` runs these gates together with CRAP scoring and Markdown lint. It first
deletes the ignored coverage output (`TestResults/`, `CoverageReport/`) and stale WPF temporary files. If the
repo-local Debug `CodexBar.App` is running, it says so and exits with code 2 without touching the process; close the
app and run it again.

### C# conventions

- **C# 12/13** with primary constructors where appropriate
- **File-scoped namespaces**
- **Implicit usings** enabled
- **Nullable reference types** enabled
- `using` directives **inside** the file-scoped namespace (immediately after `namespace …;`)
- Async methods suffixed with `Async`
- Private fields prefixed with `_`

### C# structure

```text
src/
  CodexBar.Core/        # Provider abstractions, models, services
  CodexBar.App/         # WPF system tray app
  CodexBar.Core.Tests/  # Unit tests for Core
  CodexBar.App.Tests/   # Unit and window tests for the app
```

### C# testing

- xUnit + NSubstitute
- Name tests `[Method]_[Condition]_[ExpectedResult]`
- Cover both success and failure paths for providers
- Mock `IHttpClientFactory` and `ISettingsService`

### CRAP Score Exclusions

Source-generated code is excluded from CRAP analysis. Specifically:

- **`[GeneratedRegex]` methods** — The regex source generator produces `TryMatchAtCurrentPosition`
  runners with CC ≈ 56. These are optimized state machines with unreachable backtracking branches.
  They cannot be refactored (they're auto-generated) and cannot be fully branch-covered.
- **Any `*.g.cs` file** — All source-generator output is excluded from both coverage collection
  and CRAP scoring.

When running coverage or CRAP analysis, always use:

```powershell
dotnet test --collect:"XPlat Code Coverage" --settings src/CodexBar.Core.Tests/coverage.runsettings
reportgenerator -reports:"**/coverage.cobertura.xml" -targetdir:CoverageReport -reporttypes:JsonSummary -filefilters:"-**/*.g.cs;-**/GeneratedRegex*.cs"
```
