# Sub Utilization Score

Weekly subscription windows show SUS beside their usage bars, in the focused
account, the tray tooltip, and Windows widget tiles. SUS is 0–100. Green is
90–100, yellow 75–89, and red below 75. The accessible label also names the band.
Five-hour windows have no SUS. Provider-labelled weekly periods from six to
eight days also qualify, covering calendar offset changes without accepting
short or monthly periods. Age and lockout calculations use the actual reported
length, including 167- and 169-hour weeks.

The score uses the metric's existing projection and time-to-limit, so its inputs
agree with the forecast beside the bar. A window must be at least 12 hours old,
have a known weekly length/reset, and have a forecast. Otherwise it says `SUS n/a`.

- Projected usage at reset at or below 100%: SUS is the projected percentage.
- Above 100%: `lockout = max(hours_to_reset - hours_to_limit, 0)` and
  `SUS = clamp(100 - 100 * lockout / window_hours, 0, 100)`.
- Without a known pre-reset exhaustion time there is no lockout penalty, matching
  `sus` in [Chief's reference](https://github.com/hemsoft-dev/chief/blob/main/scripts/usage.py).

Reached quotas use the same zero time-to-limit as their bars, retaining the
remaining lockout penalty. Finite negative reference inputs also retain their
elapsed lockout time. Scores round ties to even,
like Python. The reference cases are 208% projected,
25 hours until limit, 113 hours until reset: SUS 48; and 132%, 52 hours, 92 hours:
SUS 76. Scores are calculated again on the dashboard clock, including the age
threshold and effective price changes.

## Fleet and prices

Average and round each subscription account's eligible weekly pools first.
Exclude `n/a` pools and unavailable/restored/failed accounts. Weight each scored
subscription once by its monthly USD price. Separate paid accounts at the same
vendor remain separate subscriptions; multiple pools within one account never
multiply its price. If any scored subscription's price is unknown, all scored
subscriptions have equal weight. With no eligible subscriptions, fleet SUS is
`n/a`. The header names the weight mode and the number of contributing
subscriptions.

The requested default weights are Codex `pro` and Claude Max 20x $200, Codex
`prolite` and Claude Max 5x $100, and connected Antigravity coding quotas $19.99.
These are explicit defaults, not prices inferred from quota sizes. Other tiers
are unknown. Codex account metadata supplies an explicit scheduled plan change
and its `changes_at`, or `entitlement.renews_at` when that change has no own date.
A renewal date alone never schedules a downgrade. An explicitly dated target
with an unknown price expires the old known price at that instant and switches
the fleet to equal weights. Claude's current profile tier updates on refresh; Codex metadata retains its existing four-hour cache.

In Settings > Accounts, **SUS price…** overrides the monthly USD price and can
set a next price with a known RFC3339 effective instant including its timezone.
A blank current price explicitly means unknown. A blank next price with an
effective instant becomes unknown at that date. **Use provider price** removes
the override. Account settings schema 2 preserves these overrides and reads
version 1 records; older writers refuse schema 2. A future price applies
exactly at its configured instant. Set
Claude's scheduled downgrade here if its API supplies no billing date. No
billing date is guessed. The configured source record travels with automatic Codex and Claude results,
so overrides also match their credential-derived dashboard IDs. Overrides
preserve the account ID, credentials and usage history. Snapshot schema 4 persists price metadata; versions 1–3 still
load, and older writers reject schema 4 rather than discard its new fields.

## Antigravity sources

The existing zero-inference `agy -p /usage --output-format json` reader requires
CLI 1.1.11 or later, `SUCCESS`, `command.name == "usage"`, and `num_turns == 0`.
It reads `command.data.groups[].buckets[]`: `id`, `window`,
`remaining_fraction` and `reset_time`. Weekly Gemini, Argon and third-party pools
have separate metric keys. Missing Gemini or Argon is reported explicitly;
third-party pools never substitute for either.

The new fixture preserves the structured shape and bucket IDs observed in a
bounded zero-turn read of Antigravity CLI 1.4.6 on October 23, 2026. Its fractions
and reset instants are synthetic. It contains no account identity or credentials.
The public [/usage documentation](https://www.antigravity.google/docs/cli/commands/usage/)
describes refresh and model quotas, but does not specify this JSON schema.

## Grok/X source for Chief

Keep verified provider quotas. The X documentation confirms a
[weekly first-party pool](https://docs.x.ai/x-subscription/usage-limits).
CodexBar's existing Grok CLI adapter reads:

```text
GET https://cli-chat-proxy.grok.com/v1/billing?format=credits
Authorization: Bearer <official Grok CLI access token>
x-xai-token-auth: xai-grok-cli
Accept: application/json
```

The CLI `auth.json` token must have issuer `https://auth.x.ai` and match the
configured subject. CodexBar uses the CLI's own renewal path. Tokens and raw
responses must not be logged.

A verified weekly quota requires all of:

- `config.isUnifiedBillingUser == true`;
- `config.currentPeriod.type == "USAGE_PERIOD_TYPE_WEEKLY"`;
- parseable `config.currentPeriod.start` and `.end`, with `start <= now < end`;
- an explicitly present, finite, nonnegative `config.creditUsagePercent`.

Usage fraction is `creditUsagePercent / 100`. Reset is `currentPeriod.end`,
window length is `end - start`, and elapsed-period pace uses `start` and `now`.
No quota is inferred from prepaid credit, spend, product-level shares, a monthly
period, a paid plan name, or an absent percentage. Explicit zero is valid;
an omitted percentage is unavailable.

The separate Grok Bot adapter reads
`POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus`,
with Cursor authentication, `usagePercent` and its reset fields. It remains a
separate metric; CodexBar does not assert that different CLI sign-ins belong to
the same subscription. The optional Grok plan lookup is
`GET https://cli-chat-proxy.grok.com/v1/settings`.

When the Grok quota is unavailable, rolling seven-day local activity may come
from `<Grok CLI home>/sessions/*/*/usage.json`, using `sessionId`,
`turns[].turnNumber` and `turns[].endedAt`. Duplicate session/turn pairs count
once. The inclusive seven-day interval excludes future timestamps. This covers
only that local CLI home, excluding web, T3 and other machines. Malformed or
unreadable files produce a partial-coverage note; wholly unreadable data is
unavailable. Reads have file and aggregate size limits. At least one successfully parsed usage file is required to show a count. Empty
folders and missing activity sources never become an invented zero; a parsed
file with no in-range turns can legitimately report zero. Turn counts have no percentage or SUS.

## Reproduce UI evidence

Build with `cargo build -p codexbar-app --features capture --locked`. Run the
binary with `--demo`, `CODEXBAR_CAPTURE_SUS=1`, a fresh `CODEXBAR_CAPTURE_DIR`, and
`CODEXBAR_SETTINGS_DIR` pointing to a fresh subdirectory of that evidence folder.
`CODEXBAR_CAPTURE_WIDE=1` also requests and asserts a 2600×1000 logical viewport.
The harness asserts rendered fixture values before capturing light/dark states,
including dated price changes, unknown prices, young Argon, and verified X
quota versus activity. These synthetic UI fixtures never read live providers.
