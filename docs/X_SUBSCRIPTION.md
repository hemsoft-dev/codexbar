# X Subscription

On 2026-10-12 xAI and Cursor started selling Grok, Grok Bot, Cursor and X Premium as one plan,
[The X Subscription](https://x.ai/news/x-subscription) (Free, Plus, Premium, Super and Ultra). An X Subscription
account (#172) shows what Grok's **Settings > Usage** page shows, each once. It is **experimental** and off until an
X Subscription account is added.

| Metric | Source | Sign-in |
| --- | --- | --- |
| **Weekly SuperGrok limit**: percent used and reset | `cli-chat-proxy.grok.com/v1/billing?format=credits`, read as for a [Grok account](GROK.md) | The Grok CLI's own sign-in (`grok login`) |
| **Weekly Grok Bot limit**: percent used and its own reset | `api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus` | The Cursor app's own sign-in (the Cursor CLI's on macOS and Linux) |
| **Extra usage credits**: prepaid balance | The Grok billing response's `prepaidBalance` | The Grok CLI's own sign-in |

The plan name comes from `…/v1/settings`, as for a Grok account.

## Why two sign-ins

Grok's own Usage page shows the Grok Bot limit from Cursor's ledger: on 2026-10-16 both showed 87% used, resetting
at 21:10 UTC. The xAI token is refused by Cursor (401), and the Grok CLI proxy has no Grok Bot endpoint, so the card
reads each ledger with its own sign-in. CodexBar stores no token for either; it reads the files (or, on macOS, the
Keychain item) the Grok CLI and Cursor keep, and renews the Grok sign-in through the CLI as a Grok account does.

## What is shown

- The two weekly limits are separate windows with separate resets, and are never added together.
- The Weekly SuperGrok limit follows the [Grok rules](GROK.md#what-is-shown): unified billing and a current weekly
  period only; an omitted percent is 0% only for a verified paid plan, and the card says so.
- The Weekly Grok Bot limit takes `usagePercent` and `nextResetTimestampUtc`. An explicit zero is reported usage. A
  missing, `null`, negative or malformed percent, a missing or passed reset, a pooled team allowance
  (`usesPooledEnterpriseAllowance`) or a plan without an included allowance shows no meter and says why.
  `includedLimitZero` decides the allowance when present; the older `hasNonZeroIncludedLimit` only when it's absent.
- Grok Bot grants run a fixed seven-day week, and `currentPeriodStart` can fall mid-week, so its pace runs from seven
  days before the reset.
- Without the Cursor sign-in, or when Cursor refuses it, the card keeps the Grok metrics and says why Grok Bot is
  missing. Without the Grok sign-in the account shows a sign-in error.
- A Grok account that reads the same Grok CLI sign-in points to the X Subscription card instead of showing the weekly
  pool twice. Cursor accounts show no Grok Bot limit on this app, so nothing there repeats.

## Not covered yet

- Only the Automatic sign-in (the CLIs' own sign-ins on this computer), so there is one X Subscription account.
  Settings refuses another method or a second account. A separate X Subscription sign-in per account isn't offered
  yet.
- The live shape was read on 2026-10-16 from an account that hasn't switched to The X Subscription (SuperGrok and
  Cursor Pro, with Cursor offering "See Unified Plans"). How a switched account's Cursor and Grok responses change
  isn't verified. A comparison on a switched account is left to its owner.
- Cursor's third-party model spending on The X Subscription (prepaid top-ups at API prices) isn't shown here; a
  Cursor account still shows Cursor's own usage and on-demand spend.

The iOS app tracks the same change in hemsoft-dev/codexbar-ios#458.
