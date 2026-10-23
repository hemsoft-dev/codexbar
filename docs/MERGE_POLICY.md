# Merge policy

Main requires these checks from GitHub Actions: `build`, `security-scan`,
`rust (windows-latest)`, `rust (macos-latest)`, `rust (ubuntu-latest)` and
`codex-review`. Conversation resolution and enforcement for administrators
remain enabled. There is no admin bypass and no additional human approval
requirement. A clean comment-only Codex review is sufficient review evidence.

The review gate reads the connected Codex bot's structured summary, not a
comment that merely says `@codex review`, an eyes reaction, or another user's
copy of a result. The gate authenticates the summary's author and last editor using GraphQL.
A collaborator's edit cannot become a bot verdict, including on manual or CI
refresh. The summary must name the complete current head SHA, be
completed, and report no findings. Running, malformed, missing, or old-head
results cannot pass. Each new head needs its own review. The workflow reads
GitHub metadata and executes only the script from trusted main, never PR code.

Request `@codex review` once per head. If it remains hung for an hour, or the
integration explicitly refuses the review, use the maintainer-authorized
Claude Opus 5.5 fallback. Fix findings and repeat review until clean. Publish
its clean result as a COMMENT review on the exact commit, as the trusted maintainer, GitHub user ID `8227352`, with
this first-line receipt, followed by the review's scope and evidence:

```text
<!-- codexbar-review-fallback:v1 {"head":"FULL_40_CHARACTER_SHA","clean":true,"provider":"claude","model":"claude-opus-5-5","codex":"hung"} -->
```

Use `unavailable` instead of `hung` for an explicit integration refusal.
The gate verifies the review's actual commit, stable author ID and live
maintainer permission using the author's current login.
The latest review from that user on that head must carry the clean receipt. It cannot
override Codex's completed findings. This is a reviewed alternative to a hung
integration, not a switch that skips review. Unresolved threads still block
merge through native conversation protection.

The gate refreshes on PR head changes, bot comment updates, reviews and CI
completion. It publishes to both the reviewed head and its current test merge
revision, and rejects a concurrent change to either. It revokes prior success
on the test merge revision first, then the head, before evaluation. It checks
that the triggering edit or deletion is visible,
and reads mutable evidence again immediately before publishing a verdict. A
maintainer can refresh existing evidence with
`gh workflow run codex-review.yml -f number=PR_NUMBER`. This does not request a
review or change the evidence requirements. There is no polling or schedule.

The Actions-authored `codex-review-evidence` status on the reviewed head records
which summary and update the gate observed. It is an audit watermark, not a
substitute required check. Deleting recorded evidence, or a deletion event for
another review summary, latches failure rather than reviving older clean text.
A newer authenticated bot update clears the latch. Refreshes preserve it.
Unrelated public comments and reviews are excluded before job concurrency.
Eligible jobs use the documented `queue: max` mode so pending evidence events
are queued rather than replaced. The installed actionlint 1.7.12 does not yet
recognize that field; validation suppresses only its specific unknown-key
error. See GitHub's [concurrency syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#concurrency).

## Required check setup

During the initial PR, review events can run its workflow before the helper
exists on trusted main. That bootstrap case emits a notice and issues no
review status. It cannot satisfy a required `codex-review` check. Never load
the helper from a PR branch to work around the bootstrap.

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
