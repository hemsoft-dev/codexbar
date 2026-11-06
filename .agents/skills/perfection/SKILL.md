---
name: perfection
description: "V1.1 - Commands: audit, fix, status. Retired C# / WPF app (src/) only: runs its quality gates and drives its metrics to perfection: 100% test coverage, CRAP < 30, zero build warnings, clean format, zero vulnerabilities, and markdown lint. Not for the Rust product; its gates are in AGENTS.md."
version: "1.1.0"
lastModified: "2026-11-05"
hooks:
  PostToolUse:
    - matcher: "Read|Write|Edit"
      hooks:
        - type: prompt
          prompt: |
            If a file was read, written, or edited in the perfection directory (path contains 'perfection'), verify that history logging occurred.

            Check if History/{YYYY-MM-DD}.md exists and contains an entry for this interaction with:
            - Format: "## HH:MM - {Action Taken}"
            - One-line summary
            - Accurate timestamp (obtained via `Get-Date -Format "HH:mm"` command, never guessed)

            If history entry is missing or incomplete, provide specific feedback on what needs to be added.
            If history entry exists and is properly formatted, acknowledge completion.
  Stop:
    - matcher: "*"
      hooks:
        - type: prompt
          prompt: |
            Before stopping, if perfection was used (check if any files in perfection directory were modified), verify that the interaction was logged:

            1. Check if History/{YYYY-MM-DD}.md exists in perfection directory
            2. Verify it contains an entry with format "## HH:MM - {Action Taken}" where HH:MM was obtained via `Get-Date -Format "HH:mm"` (never guessed)
            3. Ensure the entry includes a one-line summary of what was done

            If history entry is missing:
            - Return {"decision": "block", "reason": "History entry missing. Please log this interaction to History/{YYYY-MM-DD}.md with format: ## HH:MM - {Action Taken}\n{One-line summary}\n\nCRITICAL: Get the current time using `Get-Date -Format \"HH:mm\"` command - never guess the timestamp."}

            If history entry exists:
            - Return {"decision": "approve"}

            Include a systemMessage with details about the history entry status.
---

# Perfection — Retired C# App Quality Gate Audit

Drive every quality metric of the retired C# / WPF app under `src/` to its
maximum possible score. When invoked, systematically run its quality gates and
fix every finding.

**Scope:** this skill and its script cover only the retired C# app. They run no
Rust gate, so a passing score says nothing about the Rust product in `crates/`.
For Rust work, use the gates in [AGENTS.md](../../../AGENTS.md#quality-gates),
which CI runs on every pull request.

## Perfection Targets

| Gate | Command | Target | What It Measures |
|------|---------|--------|------------------|
| **Build** | `dotnet build` | 0 warnings | Compilation health |
| **Code Format** | `dotnet format --verify-no-changes` | 0 violations | Whitespace, indentation, style |
| **Test Coverage (Line)** | `dotnet test --collect:"XPlat Code Coverage"` | 100% | Lines exercised by tests |
| **Test Coverage (Branch)** | `dotnet test --collect:"XPlat Code Coverage"` | 100% | Branches exercised by tests |
| **CRAP Score** | ReportGenerator JSON analysis | 0 methods > 30, avg ≤ 4.0 | Change Risk Anti-Patterns |
| **Security Audit** | `dotnet list package --vulnerable` | 0 vulnerabilities | Known CVEs in dependencies |
| **Markdown Lint** | `markdownlint "**/*.md"` | 0 errors | Documentation quality |

## Commands

### `perfection audit`

Default command. Run all quality gates and produce a consolidated report.

**Preferred repo command:**

```powershell
npm run perfection:audit:retired-csharp
```

This runs `.agents/skills/perfection/scripts/Invoke-PerfectionCheck.ps1`, which
performs the required preflight before the quality gates:

- if the repo-local Debug `CodexBar.App.exe` is running, it says so and exits
  with code 2 before any gate runs; it never stops or starts a process, so
  close the app yourself and run it again;
- removes stale WPF `*_wpftmp*` temporary artifacts before coverage collection;
- removes stale ignored coverage outputs (`TestResults/` and `CoverageReport/`)
  before using the `**/coverage.cobertura.xml` ReportGenerator glob.

**Steps:**

1. Run each gate in this order (fail-fast: NO — run all, report all):

   ```powershell
   dotnet build                    # Build with zero warnings
   dotnet format --verify-no-changes  # Formatting
   dotnet test --collect:"XPlat Code Coverage" --settings src/CodexBar.Core.Tests/coverage.runsettings  # Tests + coverage
   dotnet list package --vulnerable   # Security audit
   ```

2. For CRAP scores, after test coverage completes:
   - Run ReportGenerator with JSON output and file filters to exclude generated code:

     ```powershell
     reportgenerator -reports:"**/coverage.cobertura.xml" -targetdir:CoverageReport -reporttypes:JsonSummary -filefilters:"-**/*.g.cs;-**/GeneratedRegex*.cs"
     ```

   - Flag any method with CRAP > 30
   - **Exclude** source-generated regex methods (see "Generated Code Exclusions" below)
   - Report average CRAP score

3. Present consolidated report:

   ```markdown
   ## Perfection Audit — CodexBar

   | Gate | Status | Detail |
   |------|--------|--------|
   | Build | ✅ / ❌ | 0 warnings / N warnings |
   | Format | ✅ / ❌ | Clean / N violations |
   | Test Coverage (Line) | ✅ / ❌ | 100% / X% |
   | Test Coverage (Branch) | ✅ / ❌ | 100% / Y% |
   | CRAP Score | ✅ / ❌ | All < 30 / N methods ≥ 30 |
   | Security Audit | ✅ / ❌ | 0 vulns / N found |
   | Markdown Lint | ✅ / ❌ | Clean / N errors |

   **Perfection Score: X/7 gates passing**
   ```

### `perfection fix`

Automatically fix as many failing gates as possible in priority order.

**Priority order** (most impactful first):

1. **Build warnings** — fix root causes, never suppress
2. **Format violations** — run `dotnet format` (auto-fix)
3. **Security vulnerabilities** — update packages
4. **Test coverage gaps** — write tests for uncovered code
5. **CRAP score** — refactor complex functions + add tests
6. **Markdown lint** — fix lint errors

**For each gate:**

1. Run the check
2. If failing: apply fixes
3. Re-run to verify the fix worked
4. Move to next gate
5. After all gates: re-run full `perfection audit` to confirm

**Rules:**

- Never skip a failing gate — attempt every fix
- After each fix, re-run that specific gate to verify
- If a fix introduces failures in other gates, roll back and try a different approach
- Commit fixes in logical batches (one commit per gate, not one giant commit)

### `perfection status`

Quick one-liner status of all gates without running them. Reads from the most
recent audit results if available, otherwise runs a fresh audit.

## Integration with Other Skills

- **scorecard**: Use `scorecard status` and `scorecard improve` for the scorecard gate (when configured)
- **crap**: Use the `crap` skill for detailed CRAP score analysis if available

## Generated Code Exclusions

Source-generated code (e.g. `[GeneratedRegex]` methods) is **excluded** from CRAP analysis.
These methods produce compiler-generated state machines with unreachable backtracking branches
that cannot be meaningfully refactored or branch-covered.

**Exclusion mechanisms (all must be applied together):**

1. **Coverage collection** — `coverage.runsettings` excludes:
   - Files matching `**/*.g.cs` or `**/GeneratedRegex*.cs`
   - Methods decorated with `GeneratedCodeAttribute` or `ExcludeFromCodeCoverageAttribute`
   - Auto-property accessors (`SkipAutoProps`) — intentionally excluded as trivial code

2. **ReportGenerator CRAP report** — pass file filters:

   ```powershell
   reportgenerator -reports:"**/coverage.cobertura.xml" -targetdir:CoverageReport \
     -reporttypes:JsonSummary -filefilters:"-**/*.g.cs;-**/GeneratedRegex*.cs"
   ```

3. **Manual triage** — if a method with CRAP > 30 appears in the report and its
   class name contains `RegexRunner` or `GeneratedRegex`, or it resides in a `*.g.cs`
   file, it is source-generated and should be excluded from the CRAP gate count.

**Decision rationale** (GitHub Issue #36): Generated regex runners (`TryMatchAtCurrentPosition`,
CC ≈ 56) are produced by `System.Text.RegularExpressions.Generator`. Their complexity is
inherent to optimized regex compilation and is not a maintenance risk.

## Notes

- Coverage thresholds are enforced via coverlet.runsettings (configure when ready)
- The `coverage:ratchet` approach auto-tightens coverage thresholds over time
- Build warnings are treated as errors — fix root causes per AGENTS.md
- Always pass `--settings src/CodexBar.Core.Tests/coverage.runsettings` when collecting coverage
