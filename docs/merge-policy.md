# Merge policy

Main uses [an active repository ruleset](https://github.com/hemsoft-dev/codexbar/rules/26000153)
in addition to its existing branch protection. The desired ruleset is versioned in
[main.json](../.github/rulesets/main.json). It has no bypass actors, applies to
administrators, requires a pull request, and requires branches to be up to date.
Existing force-push/deletion restrictions and conversation resolution stay enabled.

The Chief's [maintainer decision](https://github.com/hemsoft-dev/codexbar/issues/237#issuecomment-6456251438)
requires zero approving reviews. HemSoft is the only human maintainer; Codex uses
COMMENTED reviews. The merge owner enforces current-head Codex review through the
process below. GitHub enforces CI and conversation resolution, but does not enforce
the Codex verdict through an approval or status requirement.

## Required CI checks

These exact contexts come from [successful main CI](https://github.com/hemsoft-dev/codexbar/actions/runs/40586766568)
and must be produced by GitHub Actions, integration ID `15368`:

- `build`
- `security-scan`
- `rust (ubuntu-latest)`
- `rust (windows-latest)`
- `rust (macos-latest)`
- `rust-metrics`
- `rust-mutations`

The three platform checks run format, Clippy, and tests. `rust-metrics` is the
strict regression gate. `build` also runs Markdown, workflow regression, npm audit,
and shared script checks. It selects the retired C# gates only when their inputs change.

The [Package workflow](../.github/workflows/package.yml) is path filtered.
`msix`, `macos-app`, and `linux-deb` are not unconditional required contexts:
unrelated changes never start them and would otherwise wait forever. When Package
runs, the merge owner must wait for every triggered job to pass as part of the
guarded merge. Adding packaging changes must not skip their qualification.

## Current-head Codex review

The merge owner must perform each step on every PR, including documentation and
dependency updates. The active ruleset requires no approvals, but a green GitHub
merge box alone does not satisfy this repository policy.

1. Capture the exact head and base using `gh pr view NUMBER --repo hemsoft-dev/codexbar
   --json headRefOid,baseRefOid`. Request `@codex review` once as the connected
   HemSoft account, unless a review of that unchanged head is already running or clean.
2. Inspect Codex's issue comments, formal reviews, inline findings, and review
   threads. Require the trusted `chatgpt-codex-connector` bot's completed clean
   result bound to the captured head. Current summaries contain
   `codex-review-state:v1` JSON with `code_review.commit_sha` equal to the full SHA,
   `status: completed`, `has_any_findings: false`, and
   `has_unresolved_findings: false`. Confirm the summary's author and last editor
   are the connector bot when edited, using GraphQL `author`, `editor`, and
   `lastEditedAt`. A collaborator-edited or deleted summary requires fresh
   authenticated review evidence.
3. A clean bot receipt with `Reviewed commit` can also qualify if its SHA prefix
   uniquely matches this PR's current head and the request/result can be bound to
   that head. Inspect the PR timeline for pushes and force pushes to establish
   that association. Silence, a request, eyes reaction, or an unbound thumbs-up
   does not qualify. A stale, running, ambiguous, finding-bearing, or missing
   result blocks the merge process even when GitHub allows merging.
4. Fix every actionable finding, push the fix, and obtain a clean review of the
   resulting head. Reply to handled threads with the fixing commit and formally
   resolve them after that clean review. Query all review thread pages and require
   zero unresolved threads. Assess any live change request from other reviewers.
5. If Codex hangs about an hour on an unchanged head, re-request once. If it stays
   hung about another hour, or explicitly refuses service, run an adversarial
   Claude Opus 5.5 `audit-pr` at xhigh effort against the exact head and base. Post
   its findings or clean receipt to the PR with those revisions. Address findings
   and repeat until clean. This documented cross-model fallback satisfies the
   review process without an approval or administrative bypass. Never fabricate
   a Codex receipt or report a fallback as a connected Codex review.
6. Immediately before merging, refresh the head, base, checks, current reviews,
   comments, and unresolved threads. Require all seven CI checks to pass on the
   current PR revision, plus all triggered Package jobs. Require clean review
   evidence for that exact head. If the head changes, repeat review and checks;
   if the base moves, update and requalify as strict CI requires.
7. Merge with the expected head pinned, for example `gh pr merge NUMBER
   --repo hemsoft-dev/codexbar --squash --match-head-commit FULL_SHA`. Do not enable
   auto-merge or use `--admin`. This command supplies the final head race guard;
   the merge owner must complete the preceding review and check verification.

Record the checked head, clean review link, CI run, thread count, and merge result
on the PR or issue. Missing review is a process refusal, not a claim that GitHub
has a required Codex status. GitHub's rules prevent missing or failed CI from
merging, including for administrators.

## Why review uses a documented process

[The previous automated-gate PR](https://github.com/hemsoft-dev/codexbar/pull/250)
was deferred with verified reliability and trust-boundary findings. In particular,
review events load PR-controlled workflow definitions, and any repository writer
can publish a status through the shared GitHub Actions identity. Checking out main
does not authenticate the workflow that publishes a verdict. Bot-authored comments
can also be edited or deleted by collaborators, so their author alone cannot prove
that their current text is a Codex verdict.

[gh-x's gate](https://github.com/hemsoft-dev/gh-x/blob/main/.github/scripts/changelog-merge/reviews.go)
handles head attribution and receipt parsing, but copying it does not isolate the
status publisher from this repository's writers or authenticate comment editors.
Under the maintainer's explicit process option, this change keeps review in the
guarded merge process rather than claiming that an unauthenticated status is a
reliable independent review gate. A future automated requirement needs a trusted
publisher, authenticated mutable evidence, correct head/test-merge attribution,
and live regression proof before activation. No new App or secret is installed.

## Applying settings

Review the diff against live settings and save the previous response first. Apply
the versioned ruleset with the GitHub REST API:

```sh
gh api --method PUT repos/hemsoft-dev/codexbar/rulesets/26000153 \
  --input .github/rulesets/main.json
gh api repos/hemsoft-dev/codexbar/rulesets/26000153
gh api repos/hemsoft-dev/codexbar/branches/main/protection
```

Check active organization rulesets too. Preserve independent legacy settings.
Adding or renaming a CI job requires updating and testing the required contexts
in the same change. Never remove a required check to make a failing PR mergeable.
