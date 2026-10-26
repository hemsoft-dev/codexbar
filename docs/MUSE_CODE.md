# Muse Code allowance

CodexBar reads the existing local Muse Code subscription session through the
non-inference `muse usage --json` command. Add one **Muse Code** account in
Settings, using **Command line**. The provider is off until an account is added.
CodexBar neither installs Muse nor starts sign-in, inference, or updates.

The parser is pinned to Muse Code `1.4.13-R6996.1`, schema 1. Its contract follows
the shipped [Pi implementation at 588be884](https://github.com/hemsoft-dev/hs-pi-dashboard/blob/588be884153122145435e5b05190815bfa54d378/internal/usage/muse.go)
and [sanitized fixture provenance](https://github.com/hemsoft-dev/hs-pi-dashboard/blob/588be884153122145435e5b05190815bfa54d378/internal/usage/testdata/muse/README.md),
merged in [Pi PR 71](https://github.com/hemsoft-dev/hs-pi-dashboard/pull/71).
The vendor documents the usage command and five-hour/weekly allowance in its
[changelog](https://dev.meta.ai/docs/muse-code/changelog). See also
[subscriptions](https://dev.meta.ai/docs/muse-code/subscriptions) and
[authentication](https://dev.meta.ai/docs/muse-code/auth).

## Field mapping

| CLI schema 1 | Rust snapshot | Widget snapshot |
| --- | --- | --- |
| `allowance.window.usedPercent` | `5-hour window`, key `5-hour-window`, raw fraction used = percent / 100 | `usedPercent` capped at 100; formatted used percentage |
| `allowance.weekly.usedPercent` | `Weekly`, key `weekly`, same direction | Same mapping |
| Documented window duration | 18,000 / 604,800 seconds | Reset and forecast for each metric |
| `resetsAtMs` | UTC epoch milliseconds, preserved per window | `resetsAt` |
| `asOfMs` | `observedAt`, separate from `fetchedAt` | Optional `observedAt`, separate from `updatedAt` |
| `tier` | Reported plan | Account display name includes the plan |
| `source` | `allowanceSource` | Optional `allowanceSource` |
| `reason`, `hint` | Never interpreted, surfaced, logged, or persisted | Absent |
| `allowance:null` | Unavailable error, no invented metrics | Unavailable, no percentages |
| Model/session tokens, `run.usage`, billing objects | Ignored | Absent |
| Unknown window-shaped keys | Counted warning, no quota metric | No inferred pool |

Each documented sibling is validated independently. Percentages must be numeric,
finite and nonnegative. Numeric zero is real zero. Reset times must be after both
the observation and fetch, within the documented duration from each. Epoch
milliseconds must be between 1e12 and 1e13. A bad sibling never removes a good one.
An over-limit raw value survives for forecasts; the bar caps at 100%.

Tier text is restricted to `^[A-Za-z0-9 ._()+-]{1,64}$`; source text to
`^[a-z0-9_-]{1,32}$`. Unknown window names are counted rather than echoed.

## Freshness and acquisition

Observations more than 60 seconds ahead of collection are rejected. An observation
older than 420 seconds is retained as last known, marked stale and excluded from
alerts and current forecasts. Observation and fetch times survive persistence,
cache hits and failed refreshes. History samples use the original observation,
not the time of a later refresh. Last-good refresh failures keep the existing
error badge and original timestamps. A widget reader independently ages the source
observation and refuses a selected Muse window after its reset.

The contained command runner uses a child-only environment. It removes every
case variant of `META_API_KEY`, `MUSE_LOGIN`, `MUSE_NO_AUTO_UPDATE`,
`MUSE_SYNC_UPDATE` and `MUSE_LAUNCHER_INSTALL`, then sets `MUSE_LOGIN=0` and
`MUSE_NO_AUTO_UPDATE=1`. Stdin is closed, cwd is the user's home, stdout and stderr
are bounded to 4 MiB each, and execution has a 15-second deadline with the existing
bounded process-tree cleanup. Command output is never included in errors.
Other providers retain their existing runner options and 8 MiB limit.

A settings-hub-owned cache survives the app's adapter reconstruction on refresh.
Success is reused for at most five minutes or the earliest reset. Failure retries
back off 5, 10, 20, then 30 minutes, without jitter. A configured account identity
change invalidates cached results; labels and metric keys stay independent of reset
timestamps. One CLI session can have only one configured account, preventing duplicate
subscription rows. The CLI schema supplies no native account identity. CodexBar cannot
detect an external switch of the CLI's signed-in user from this payload; use a distinct
configured account record to keep that user's history separate.

## Unknown states

The shipped schema has no pinned reason vocabulary. Expired authorization,
entitlement denial, PAYG and rate limiting cannot be diagnosed from free-form
`reason`/`hint` or a nonzero exit without guessing. Null allowance is unavailable;
nonzero exit is a sanitized command failure. Missing CLI, timeout, transport failure,
unsupported schema, malformed payload, invalid windows and stale observation have
distinct safe outcomes. Synthetic tests named for PAYG/entitlement prove they stay
unknown, not that those conditions have been identified on a real account.

The payload reports percent units only, no absolute cap, actual billing mode, model
scope, or verified Wonder Full entitlement. Neither model catalogs nor token totals
fill those gaps. OpenCode Muse Spark and Meta API PAYG remain separate. No price or
SUS is assigned, even if a weekly forecast is available. CodexBar does not probe
private endpoints or read authentication files for this provider.

## Serialization consumers and compatibility

| Producer / consumer | Contract |
| --- | --- |
| `codexbar-providers::muse` | Supported command output to account snapshots |
| `codexbar-store::snapshots` | `snapshots.json` v4, additive observation/source/freshness fields only for Muse; versions 1–4 and existing provider shapes retained |
| `codexbar-store::HistoryStore`, enrichment and summaries | Account ID + metric key; reset timestamps identify successive windows; observation time supplies the sample timestamp |
| Dashboard, account table, focus cards and alert evaluator | Percent used, n/a for missing quota, stale last-good status, no Muse SUS/price |
| `widget_feed::build` | Display-ready `widgets.json` v1, additive optional `observedAt`, `allowanceSource`, `freshForSeconds`; existing provider JSON shapes unchanged |
| `codexbar-store::widgets::load_widget_snapshot` | Shared typed file reader; ignores unknown fields, accepts supported version |
| `widgets::cards::tile_views` / `render` | Actual Windows widget consumer and GPUI widget preview; unavailable remains absent/null, source age and reset checked |
| Windows `widgets::provider` / `widgets::host` | Loads shared snapshot and renders the same Adaptive Card consumer |
| Settings widget builder | Uses the same `TileView` reader for its real GPUI preview |

There is no additional standalone Rust CLI/export-to-Pi feed. The retired WPF app
is not a consumer of these Rust snapshots and was not changed. Old widget readers
ignore the additive fields; the producer's stale/unavailable health remains readable.

## Validation scope

`crates/codexbar-providers/fixtures/muse` contains the pinned sanitized Pi fixtures.
Only `measured.json` has measured provenance; the other values and all command
runners in tests are synthetic. Tests cover zero, exhaustion, partial/malformed
siblings, reset, null, unsupported schema, token-only activity, unknown
PAYG/entitlement, timeout, transport/command failure, bounded scoped command
execution, cache/backoff, account persistence and the actual widget reader.
Existing provider wire shapes and legacy snapshot loading retain coverage.

Real macOS GPUI rendering uses fresh isolated demo settings with no live polling:

```sh
CODEXBAR_SETTINGS_DIR=/tmp/codexbar-273-after/settings \
CODEXBAR_CAPTURE_DIR=/tmp/codexbar-273-after CODEXBAR_CAPTURE_MUSE=1 \
  cargo run --locked --features capture -p codexbar-app -- --demo
```

The capture exercises measured, partial, exhausted, null, failed-refresh last-good
and stale table/detail states, settings, and the same widget preview consumer. The
Widgets settings page is available in capture builds on macOS for this purpose;
production macOS builds keep their existing settings pages. No live account
compatibility is claimed. Native Windows board screenshots are deferred by the
user; macOS render evidence and synthetic Windows-consumer tests do not claim
Windows desktop qualification. Static captures cover the changed values; there is
no new animated or multistep setup flow to qualify with a recording.
