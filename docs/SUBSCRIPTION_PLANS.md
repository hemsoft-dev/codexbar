# Subscription plan labels

Account names and provider-reported plans are separate. Renaming an account
never changes its plan, and a plan change never changes its account ID or
history. The Usage table keeps the plan in its own label so a long account
name cannot hide the tier. The focused account, alerts and tray use the full
account name and plan. Last-good snapshots retain the reported plan at restart;
a successful refresh replaces it, including removing a plan the source no
longer supplies. Snapshots are written as schema version 3, so older builds
refuse to overwrite plans they do not understand. Versions 1 and 2 remain
readable and upgrade on the next save. Their known Codex and Claude plan
labels, such as `Pro` or `Work · Max`, split into the account name and reported
plan before a refresh. An old generic Pro or Max stays generic until the
provider supplies its current tier. Unrecognized labels stay account names.

## Sources and limits

| Provider | Source used | Display and limitations |
| --- | --- | --- |
| ChatGPT / Codex | `/backend-api/accounts/check/v4-2023-04-27`, selected account and entitlement; existing `/backend-api/wham/usage` fallback | Prefers `account.plan_display_name`, otherwise maps explicit `account.plan_type`: `pro` is Pro, `prolite` is Pro Lite, `promax` is Pro Max. A future `entitlement.scheduled_plan_change` with an explicit `changes_at` displays the target and local date, for example Pro → Pro Lite on Oct 26. |
| Claude | `/api/oauth/profile`, `organization.organization_type` and `organization.rate_limit_tier` | Exact `default_claude_max_5x` and `default_claude_max_20x` values display Max 5x and Max 20x. An unknown Max tier displays Max with a note. Pro, Team and Enterprise follow the current organization type. |
| Copilot | Existing `copilot_internal/user`, `copilot_plan`; organization `/copilot/billing`, `plan_type` | Displays the explicit plan value with readable capitalization. No tier is inferred from premium-request or AI-credit allowances. Optional org lookup failure preserves configured pool totals. |
| GitHub Billing | Existing user/org profile, `plan.name` | Displays the reported profile plan even if allowance detail is unavailable. Missing permission leaves the plan unknown, with the existing billing explanation. |
| Grok / X Subscription | Existing authenticated CLI settings, `subscription_tier_display` or its supported alternative spelling | Displays the provider's explicit name even when no unified billing pool exists. It does not infer an X bundle tier from Grok Bot quotas or SuperGrok usage. |
| OpenCode Go + Zen | Existing Console `/api/go/status` subscription result | A subscription belonging to the signed-in member displays Go. Zen credit balances alone do not identify a subscription. |
| Cursor | `GetPlanInfo`, `planInfo.planName`, using the existing Connect RPC sign-in | Displays the exact provider name. `GetCurrentPeriodUsage` supplies usage, not a guessed subscription tier. Optional plan lookup failure does not discard usage; 429 Retry-After is honored. |
| Gemini | Existing Apps usage and Antigravity CLI usage | The connected usage shapes provide quotas, without an explicit subscription plan field. No plan is guessed from a quota or model list. |
| OpenRouter / Moonshot | Existing credit/balance endpoints | A prepaid balance is not a subscription plan. The generic provider name remains. |
| Greptile | Existing read-only review-activity endpoint | Review counts do not identify a billing plan. The generic name remains. |

When no plan is reported, the focused Usage view says the connected source
does not report a subscription plan. Provider-specific lookup failures also
say why. Unknown plan identifiers are not converted into multipliers.

## Refresh and scheduled changes

Claude's local `subscriptionType` and `rateLimitTier` can remain stale after a
plan change, so the live provider uses the current OAuth profile instead. The
profile request shares the existing usage cache: normally at most once every
ten minutes per sign-in. It honors profile 429 Retry-After and 403 backoff
without withholding valid usage. A failed profile does not reuse a stale
precise tier. No token is renewed or rewritten by this lookup.

Codex fetches account metadata on the first successful usage refresh after
startup and then once every four hours per sign-in. The cache survives provider
reconstruction and is bound to the auth path and hashed user/workspace identity.
The same ChatGPT token and account header as usage authenticate the request.
A scheduled change expires the cache early at `changes_at`, so the next usage
refresh reads the newly effective plan. The label shows the current plan until
the provider changes it; an upcoming target is never applied early. Lookup
failures are cached for four hours and honor longer Retry-After delays. They
preserve usage and fall back to its current `plan_type`, with a lookup-failure
note. The app passes its existing date style into the provider, so scheduled
dates follow the user's month/day order and translated month names. Cached
metadata is formatted again on each usage refresh. Other providers keep their
normal source refreshes.

## Evidence

On October 23, 2026, a bounded live read on mini with the existing Codex
sign-in returned `account.plan_type: pro`, `account.plan_display_name: Pro`,
`entitlement.subscription_plan: chatgptpro`, and a scheduled `prolite` change
at October 26, 2026, 9:33 AM EDT. Both `renews_at` and `changes_at` carried
that timestamp. The implementation uses the explicit change time, not renewal
alone, to date the upcoming label. Earlier HTTP 403 qualification on air was
not a statement that the API lacks plan details.

Claude's live profile returned `organization_type: claude_max`,
`rate_limit_tier: default_claude_max_20x` and `subscription_status: active`.
A bounded Cursor account read returned `planInfo.planName: Premium` from
`GetPlanInfo`; its usage endpoint supplied no plan name. Only selected plan
fields were retained; no tokens, emails or organization IDs were logged or
added to fixtures. The current OpenAI project also records the account-check
endpoint in [issue 34192](https://github.com/openai/codex/issues/34192).

Anthropic's profile field shape is also recorded in its first-party issue
tracker, including the distinction between stale credentials and the live
profile in [issue 87419](https://github.com/anthropics/claude-code/issues/87419).
The implementation qualifies that shape with the live response and tests
Max 20x to Max 5x, Pro, unknown values, optional lookup failure and backoff.

See [Gemini connection details](GEMINI.md),
[GitHub billing](GITHUB_BILLING.md), [Grok](GROK.md),
[X Subscription](X_SUBSCRIPTION.md) and [Greptile activity](GREPTILE.md)
for the existing sources and their permission limits.
