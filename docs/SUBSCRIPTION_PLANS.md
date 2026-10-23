# Subscription plan labels

Account names and provider-reported plans are separate. Renaming an account
never changes its plan, and a plan change never changes its account ID or
history. The Usage table keeps the plan in its own label so a long account
name cannot hide the tier. The focused account, alerts and tray use the full
account name and plan. Last-good snapshots retain the reported plan at restart;
a successful refresh replaces it, including removing a plan the source no
longer supplies.

## Sources and limits

| Provider | Source used | Display and limitations |
| --- | --- | --- |
| ChatGPT / Codex | Existing `/backend-api/wham/usage`, `plan_type` | Uses the provider client's explicit plan names. `pro`, `prolite` and `promax` distinguish Pro variants, but do not expose a current 5x/20x multiplier. The UI says so rather than asserting one. |
| Claude | `/api/oauth/profile`, `organization.organization_type` and `organization.rate_limit_tier` | Exact `default_claude_max_5x` and `default_claude_max_20x` values display Max 5x and Max 20x. An unknown Max tier displays Max with a note. Pro, Team and Enterprise follow the current organization type. |
| Copilot | Existing `copilot_internal/user`, `copilot_plan`; organization `/copilot/billing`, `plan_type` | Displays the explicit plan value with readable capitalization. No tier is inferred from premium-request or AI-credit allowances. Optional org lookup failure preserves configured pool totals. |
| GitHub Billing | Existing user/org profile, `plan.name` | Displays the reported profile plan even if allowance detail is unavailable. Missing permission leaves the plan unknown, with the existing billing explanation. |
| Grok / X Subscription | Existing authenticated CLI settings, `subscription_tier_display` or its supported alternative spelling | Displays the provider's explicit name. It does not infer an X bundle tier from Grok Bot quotas or SuperGrok usage. |
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

Codex reads `plan_type` on each normal usage refresh. The other plans follow
their normal source refreshes. A scheduled downgrade is not applied early:
these adapters read the current effective provider fields and do not use
future billing schedules, prices or quota sizes to choose a name.

## Evidence

On October 22, 2026, a bounded live read on air returned `plan_type: pro` from
Codex's usage endpoint, with no multiplier or plan-display-name field. The
account endpoint refused the same sign-in with HTTP 403. Claude's live profile
returned `organization_type: claude_max`,
`rate_limit_tier: default_claude_max_20x` and `subscription_status: active`.
A bounded Cursor account read returned `planInfo.planName: Premium` from
`GetPlanInfo`; its usage endpoint supplied no plan name. Only these plan fields were retained; no tokens, emails or organization IDs
were logged or added to fixtures.

OpenAI's first-party client maps its plan codes to display names in
[`KnownPlan::display_name`](https://github.com/openai/codex/blob/8531184b49bcb160175a84e140b659a83a426df7/codex-rs/protocol/src/auth.rs).
It currently calls `pro` "Pro (More)", `prolite` "Pro" and `promax` "Pro (Max)".
Those are explicit code mappings, not inferred multiplier promises. Commercial
plans and allowances can change, as the current
[Pro tier documentation](https://help.openai.com/en/articles/9793128-about-chatgpt-pro-tiers)
explains. CodexBar does not turn a generic `pro` field into a historical 20x
allowance.

Anthropic's profile field shape is also recorded in its first-party issue
tracker, including the distinction between stale credentials and the live
profile in [issue 87419](https://github.com/anthropics/claude-code/issues/87419).
The implementation qualifies that shape with the live response and tests
Max 20x to Max 5x, Pro, unknown values, optional lookup failure and backoff.

See [Gemini connection details](GEMINI.md),
[GitHub billing](GITHUB_BILLING.md), [Grok](GROK.md),
[X Subscription](X_SUBSCRIPTION.md) and [Greptile activity](GREPTILE.md)
for the existing sources and their permission limits.
