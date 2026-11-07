# Rust dependency security

The existing CI `security-scan` job scans the entire committed `Cargo.lock`
with [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit).
This includes every supported platform and development dependency, without
filtering by the runner's Windows target. It runs on every pull request and
push to main alongside the existing secret and Miasma scans. GitHub's own
secret scanning, push protection and Dependabot alerts run beside these CI
scans; see [GitHub security features](#github-security-features).

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

A second Cargo-generated lockfile contains `inconceivable 0.9.0`, which has
the informational [RUSTSEC-2022-0058](https://rustsec.org/advisories/RUSTSEC-2022-0058.html)
notice. The test requires that exact notice in `warnings.notice` and a
successful scan. This prevents a shortened informational-category list from
silently hiding notices. Neither fixture is compiled or shipped.

## Blocking policy and exceptions

Vulnerability advisories at every severity and informational `unsound`
advisories fail the scan. There are no ignored advisory IDs or target filters.
Informational notices, unmaintained and yanked dependencies remain visible as warnings. An
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
current-head Codex review. The Rust scan was added to that existing check
without changing repository settings. Enforced required-status
configuration remains [blocked on the shared SFL reviewer App](https://github.com/hemsoft-dev/codexbar/issues/237#issuecomment-6358426342).

## GitHub security features

Since November 5, 2026 ([#299](https://github.com/hemsoft-dev/codexbar/issues/299)),
the repository also uses three security features that GitHub provides free on
public repositories. They run on GitHub, outside CI. No workflow, job or
required check was added for them.

| Feature | Live setting | What it does |
| --- | --- | --- |
| [Secret scanning](https://docs.github.com/en/code-security/secret-scanning/introduction/about-secret-scanning) | `secret_scanning` enabled | Scans the Git history on all branches, plus issue, pull request and discussion text, for supported secret formats and opens an alert for what it finds. |
| [Push protection](https://docs.github.com/en/code-security/secret-scanning/introduction/about-push-protection) | `secret_scanning_push_protection` enabled | Blocks a command-line push, web commit or REST API commit that contains a supported provider secret before it is published. |
| [Dependabot alerts](https://docs.github.com/en/code-security/dependabot/dependabot-alerts/about-dependabot-alerts) | `GET /vulnerability-alerts` returns 204 | Opens an alert when a GitHub Advisory Database entry matches a dependency on the default branch, both when a new advisory is published and when a push changes the dependency graph. |

These settings stay off: Dependabot security updates
(`dependabot_security_updates`, automated security fixes), Dependabot version
updates (there is no `.github/dependabot.yml`), non-provider patterns,
validity checks, AI detection and delegated bypass. Security updates open pull
requests on their own. In line with the `AGENTS.md` rule on autonomous
automation, they stay off unless the maintainer turns them on.

### How they relate to the CI scans

- Gitleaks in `security-scan` scans the checkout's full history, but only after
  a push. On this public repository, a secret it finds is already published.
  Push protection stops supported secret formats before that. Gitleaks remains
  the merge gate and keeps its own rules from `.gitleaks.toml`.
- cargo-audit and `npm audit` run only on a pull request or a push to main.
  Dependabot alerts also report an advisory published between commits. They use
  the GitHub Advisory Database, not the RustSec database that CI fetches, so the
  two can disagree for a while.
- Dependabot reads the default branch only. Pull request changes are covered by
  the CI scans until they merge.
- GitHub alerts are not status checks. They never block a merge, and the CI
  scans stay the merge gate.

### Roles and alert handling

The repository maintainer, HemSoft, owns alert triage. Alerts appear on the
repository's Security and quality tab. By default, Dependabot emails people with write,
maintain or admin access who watch the repository for security alerts. The
REST API for secret scanning alerts needs repository admin access.

Anyone with write access can bypass a push-protection block by giving a reason.
GitHub records the bypass and opens an alert; "I'll fix it later" leaves it
open. Don't bypass a block for a real credential: remove it from the commit,
and rotate it if it was ever pushed.

Keep secret values out of issues, pull requests, commit messages and logs. The
secret scanning alert list returns the literal secret unless `hide_secret=true`
is passed, so read only counts or fields such as `number`, `state` and
`secret_type`. Dismiss an alert only with a stated reason after checking it.

### Vulnerable test fixtures

Two test fixtures contain vulnerable versions on purpose. Do not upgrade, edit
or delete them to clear an alert: the tests below need the vulnerable versions.
Keep them out of the dependency graph instead.

- [`scripts/fixtures/npm-vulnerable-markdown/package-lock.json.fixture`](../scripts/fixtures/npm-vulnerable-markdown/package-lock.json.fixture)
  locks KaTeX 0.18.1 (low-severity GHSA-238p-pmpm-9mq7).
  [`scripts/tests/npm-audit.test.cjs`](../scripts/tests/npm-audit.test.cjs)
  copies it and `package.json.fixture` to a temporary folder under their real
  names and requires that advisory and a failing audit. Under those names the
  fixture raised [Dependabot alert 1](https://github.com/hemsoft-dev/codexbar/security/dependabot/1)
  in the repository, so it is stored with a `.fixture` suffix. The dependency
  graph reads only real manifest names, and Dependabot alerts come from the
  graph: `dependabot.yml` options do not control them. Keep the suffix on any
  new vulnerable npm fixture.
- The cargo-audit fixtures under
  [`scripts/tests/fixtures/rust-advisory/`](../scripts/tests/fixtures/rust-advisory/)
  lock `idna 0.5.0` and `inconceivable 0.9.0`. On November 5, 2026 the
  dependency graph listed the root `Cargo.lock`, the root `package-lock.json`
  and the npm fixture lock, but not these lockfiles. If they ever raise an
  alert, keep them out of the graph the same way rather than changing them.

### Limits and checks

Both scans run asynchronously after they are turned on. The first reads on
November 5, 2026 found 0 secret scanning alerts and the one Dependabot alert
above. That does not prove the historical scan has finished, so an empty alert
list is not evidence of a clean history.

Read the current state with:

```sh
gh api repos/hemsoft-dev/codexbar --jq .security_and_analysis
gh api -i repos/hemsoft-dev/codexbar/vulnerability-alerts
gh api "repos/hemsoft-dev/codexbar/dependabot/alerts?state=open" --jq length
gh api "repos/hemsoft-dev/codexbar/secret-scanning/alerts?state=open&hide_secret=true" --jq length
```

The second command answers `HTTP/2.0 204` while Dependabot alerts are on.
