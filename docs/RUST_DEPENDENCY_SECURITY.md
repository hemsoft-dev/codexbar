# Rust dependency security

The existing CI `security-scan` job scans the entire committed `Cargo.lock`
with [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit).
This includes every supported platform and development dependency, without
filtering by the runner's Windows target. It runs on every pull request and
push to main alongside the existing secret and Miasma scans.

The scanner version is pinned in
[`scripts/rust-audit-version.txt`](../scripts/rust-audit-version.txt).
CI installs that exact version with its own locked dependency graph and checks
the executable's reported version before scanning. The scanner fetches the
current [RustSec database](https://github.com/RustSec/advisory-db); stale database
and fetch failures fail the job. The JSON report records its exact database
commit, rather than pretending that a scanner pin also freezes advisories.

## Run locally

With Rust and PowerShell 7 installed, run from the repository root:

```powershell
$version = (Get-Content scripts/rust-audit-version.txt -Raw).Trim()
cargo install cargo-audit --version $version --locked --no-default-features
./scripts/Invoke-RustAdvisoryScan.ps1
./scripts/tests/RustAdvisoryScan.Tests.ps1
```

Reports under `artifacts/security/rust/` contain the scanner version, raw JSON,
and stderr. The existing `security-scan-reports` artifact uploads them even
when the scan fails, with 14-day retention. These contain public crate names,
versions and advisories. No provider credentials or account data are read.
Failed startup deletes previous report files before executing the scanner.

The integration test uses an isolated Cargo-generated lockfile with
`idna 0.5.0`. It requires both a failed scan and the actual
[RUSTSEC-2024-0421](https://rustsec.org/advisories/RUSTSEC-2024-0421.html)
finding. Missing tools, network failures and malformed fixtures cannot count
as successful vulnerability detection. It also seeds a previous report and
proves that a startup failure cannot reuse it.

## Blocking policy and exceptions

Vulnerability advisories at every severity and informational `unsound`
advisories fail the scan. There are no ignored advisory IDs or target filters.
Unmaintained and yanked dependencies remain visible as warnings. An
unmaintained notice alone does not assert an exploitable defect; a vulnerability
or unsoundness notice on that same package still blocks. No warning category
is silently removed from the JSON report.

The repository maintainer, HemSoft, owns dependency upgrades and warning triage.
Any future exception must name one advisory in `.cargo/audit.toml`, document
the exact dependency/version and why the affected code is unreachable, link
an upstream remediation issue, and name an owner and review-expiry date here.
Review must verify that evidence before accepting the exception. Never add a
severity cutoff, broad target exclusion, or blanket ignore to clear a failure.

As of the initial scan on October 23, 2026, eight unmaintained dependencies
remain in the graph: `instant`, `paste`, `rustls-pemfile`, `rustybuzz`,
`ttf-parser`, `zbus_names`, `zvariant` and `zvariant_derive`. Their notices stay
in the artifact for maintainer triage. They are not advisory-ID exceptions.

The project merge policy requires a green CI security scan and clean
current-head Codex review. This change adds the Rust scan to that existing
check; it does not change repository settings. Enforced required-status
configuration remains [blocked on the shared SFL reviewer App](https://github.com/hemsoft-dev/codexbar/issues/237#issuecomment-6358426342).
