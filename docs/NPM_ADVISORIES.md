# Markdown tooling and npm advisories

The shipped app is Rust. The root npm graph contains development tools for Markdown
linting. Use Node.js 22 or newer; CI uses the supported Node.js 24 LTS line.

```sh
npm ci --ignore-scripts
npm run lint:md
npm run audit:npm
npm run test:npm-audit
```

`lint:md` keeps the existing `**/*.md` scope, `node_modules` exclusion and
repository Markdown configuration. CI skips installation scripts because linting
does not need them. Developers who want the pre-commit hook can still run the
normal `npm install` command documented in the README.

## Blocking policy

`audit:npm` runs `npm audit --include=dev --audit-level=low`. Any reported low,
moderate, high or critical vulnerability fails the gate, including transitive
development dependencies. There are no advisory exceptions. Registry, network or
other npm failures also fail the command. The check does not run `npm audit fix`
or change the lockfile.

The existing CI build job runs this command and retains its raw JSON report as
`npm-audit` for 14 days, including on failure. npm's affected-package counts include
packages that inherit a vulnerable dependency. They are not a count of distinct
advisories or demonstrated exploits. See the [npm audit documentation](https://docs.npmjs.com/cli/v11/commands/npm-audit/).

## Qualified dependency graph

On October 23, 2026 EDT, a fresh install of the previous lock reported nine
affected packages, six high and three low. Updating markdownlint-cli alone left
seven affected packages. The qualified graph reports zero findings at every
severity. markdownlint-cli is pinned at 0.49.1, and the lockfile records the full
graph and integrity hashes.

Three narrowly scoped overrides move beyond upstream's restricted minor ranges:

| Parent | Override | Reason |
| --- | --- | --- |
| markdownlint-cli | js-yaml 5.5.0 | [Merge-source CPU exhaustion](https://github.com/advisories/GHSA-r3ph-w7gj-g6xm) affects versions through 5.4.0. |
| markdownlint-cli | smol-toml 1.9.1 | [Quadratic key parsing](https://github.com/advisories/GHSA-r4xh-jqrq-34v2) affects versions through 1.8.0. |
| micromark-extension-math | katex 0.18.2 | [Trust restrictions can be bypassed after prototype pollution](https://github.com/advisories/GHSA-238p-pmpm-9mq7) before 0.18.2. |

The normal lock refresh also selects brace-expansion 5.0.12, which fixes the
[remaining expansion denial of service](https://github.com/advisories/GHSA-q2hr-2g5m-vwhr).
These are dependency upgrades, not advisory suppressions. Remove an override
when a reviewed upstream release permits the fixed version itself. Regenerate
with `npm install --package-lock-only --ignore-scripts --no-audit`, then repeat
the commands above. Review the complete lockfile diff.

## Qualification tests

`test:npm-audit` invokes the same package script against the current lock and
requires a successful, version-2 npm report with zero findings. It also copies an
actual npm-generated vulnerable KaTeX 0.18.1 development lock into a fresh temporary
directory. The real registry-backed audit must report the known low-severity
[KaTeX advisory](https://github.com/advisories/GHSA-238p-pmpm-9mq7) and exit 1, even
with conflicting defaults that omit development packages and block only critical
findings. A registry error cannot count as this expected rejection. The fixture
is never installed or executed, and its lockfile must remain byte-identical.

The tests also exercise the upgraded CLI with JSON, YAML and TOML configuration,
Markdown containing math, and an invalid heading that must still fail lint. This
checks the parser interfaces affected by the overrides. Tests require the npm
registry; they do not simulate a denial of service or claim application exposure.
