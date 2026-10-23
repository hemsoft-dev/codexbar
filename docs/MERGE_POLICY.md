# Merge policy

Main requires these checks from GitHub Actions: `build`, `security-scan`,
`rust (windows-latest)`, `rust (macos-latest)`, `rust (ubuntu-latest)` and
`codex-review`. Conversation resolution and enforcement for administrators
remain enabled. There is no admin bypass and no additional human approval
requirement. A clean comment-only Codex review is sufficient review evidence.

The review gate reads the connected Codex bot's structured summary, not a
comment that merely says `@codex review`, an eyes reaction, or another user's
copy of a result. The summary must name the complete current head SHA, be
completed, and report no findings. Running, malformed, missing, or old-head
results cannot pass. Each new head needs its own review. The workflow reads
GitHub metadata and executes only the script from trusted main, never PR code.

Request `@codex review` once per head. If it remains hung for an hour, or the
integration explicitly refuses the review, use the maintainer-authorized
Claude Opus 5.5 fallback. Fix findings and repeat review until clean. Publish
its clean result as a COMMENT review on the exact commit, as `HemSoft`, with
this first-line receipt, followed by the review's scope and evidence:

```text
<!-- codexbar-review-fallback:v1 {"head":"FULL_40_CHARACTER_SHA","clean":true,"provider":"claude","model":"claude-opus-5-5","codex":"hung"} -->
```

Use `unavailable` instead of `hung` for an explicit integration refusal.
The gate verifies the review's actual commit and live maintainer permission.
The latest HemSoft review on that head must carry the clean receipt. It cannot
override Codex's completed findings. This is a reviewed alternative to a hung
integration, not a switch that skips review. Unresolved threads still block
merge through native conversation protection.

The gate refreshes on PR head changes, bot comment updates and reviews. A
maintainer can refresh existing evidence with
`gh workflow run codex-review.yml -f number=PR_NUMBER`. This does not request a
review or change the evidence requirements. There is no polling or schedule.

## Required check setup

After the review workflow has merged to main, preview the additive settings:

```sh
python3 .github/scripts/require-merge-checks.py
```

Then apply them with `--apply`. The helper discovers GitHub Actions' app ID
from main's security job and changes only required statuses. It preserves
existing requirements and strictness, conversation resolution, administrator
enforcement, review settings and bypass policy. Read back
`gh api repos/hemsoft-dev/codexbar/branches/main/protection` and verify the six
contexts and source app IDs. Repository settings are live configuration;
merging this document alone does not activate them.

The path-filtered Package workflow is deliberately not globally required.
GitHub can leave a required check pending when its workflow is skipped by a
path filter. Packaging changes still run Package, and its applicable results
must pass before merge. Normal CI and the review status run for every PR.

Validate the live protection with a disposable PR: missing review must leave
`codex-review` pending, and an old-head result must not pass its successor.
Confirm `mergeStateStatus` is blocked with missing/failed requirements. Close
the fixture without merging. Never attempt an admin merge to test protection.

Policy references: [required status checks](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks),
[protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches),
and [safe metadata workflows](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target).
