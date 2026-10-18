# AGENTS.md — CodexBar

## Search online

Always use web search to support your statements. Good sources are:

GitHub (Issues, PR's Releases, Changelogs)
Reddit
Microsoft Developer Documentation
Social Media.

## Repository automation

Keep the normal CI workflow and required Copilot/Codex review integrations.
Do not install autonomous issue processing, scheduled audit, or PR promotion
workflows without an explicit maintainer request.

## Quality Gates

The Rust app (`crates/`) is the product; the C# / WPF app under `src/` is retired (2026-10-09). All changes must pass
these gates before merge (the pre-commit hook and CI run them):

1. **Format** — `cargo fmt --all --check` clean
2. **Lint** — `cargo clippy --workspace --all-targets --locked -- -D warnings` clean
3. **Tests** — `cargo test --workspace --locked` all green
4. **Security** — the CI security scan clean

Changes that touch `src/` must also pass the C# gates: `dotnet build` with zero warnings,
`dotnet format --verify-no-changes`, `dotnet test`, coverage at or above the current threshold, and
`dotnet list package --vulnerable` clean.

## Conventions

- **C# 12/13** with primary constructors where appropriate
- **File-scoped namespaces**
- **Implicit usings** enabled
- **Nullable reference types** enabled
- `using` directives **inside** the file-scoped namespace (immediately after `namespace …;`)
- Async methods suffixed with `Async`
- Private fields prefixed with `_`

## Project Structure

```text
src/
  CodexBar.Core/        # Provider abstractions, models, services
  CodexBar.App/         # WPF system tray app
  CodexBar.Core.Tests/  # Unit tests for Core
```

## Testing

- xUnit + NSubstitute
- Name tests `[Method]_[Condition]_[ExpectedResult]`
- Cover both success and failure paths for providers
- Mock `IHttpClientFactory` and `ISettingsService`

## Git

- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`
- One logical change per commit
- No merge conflict markers committed

## CRAP Score Exclusions

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
