# Grok

A Grok account (#108) shows the consumer Grok usage of a paid plan: the shared
**weekly usage** pool and **Extra Usage Credits**, from the Grok CLI's own billing
service. It is **experimental**: that service is xAI's first-party CLI contract,
not a published API. Grok is off until a Grok account is added.

On The X Subscription, add an [X Subscription](X_SUBSCRIPTION.md) account instead: it shows this weekly pool and
the credits together with the Grok Bot limit. A Grok account on the same Grok CLI sign-in then points there instead
of showing the pool twice.

## Signing in

- **Automatic** reads the Grok CLI's own sign-in (`GROK_HOME`, or `~/.grok`).
  Sign in with `grok login`.
- **OAuth** gives the account a Grok CLI home of its own in the settings folder
  and signs it in through the official CLI (`grok login`, in a console window),
  so several xAI accounts can be added. See [OAUTH.md](OAUTH.md). The sign-in is
  confirmed with xAI's OIDC userinfo before it is kept, and the account stays
  bound to that xAI subject: signing it in as someone else is refused.

The CLI keeps and renews the token; CodexBar borrows no OAuth client and stores
no token of its own. It reads the CLI's `auth.json` and, when the token is about
to expire or the billing service refuses it, runs `grok models` for that home,
which renews it without a model call.

## What is shown

CodexBar reads `cli-chat-proxy.grok.com/v1/billing?format=credits` with the
token, and `…/v1/settings` for the plan name.

- **Weekly usage** shows only for unified billing with a current weekly period,
  from `creditUsagePercent`, resetting at the period's end.
- An omitted, null or malformed percentage is unavailable, including on paid
  plans. Only an explicit valid value can produce a quota meter or SUS.
- Without quota data, tracked local CLI turns over seven days can show with
  `SUS n/a`. Coverage excludes web, T3 and other machines. See the exact fields,
  bounds and weekly score rules in [SUS sources](SUS.md#grokx-source-for-chief).
- **Extra usage credits** (`prepaidBalance`, USD cents) are a balance of their
  own, never part of the weekly pool. An empty object is zero, as the CLI reads
  it; a missing or malformed one is not.
- Product shares are listed as detail, not as meters. API or developer-team
  spending, monthly billing dates and Cursor's Grok Bot allowance are separate
  ledgers and never shown here. A monthly period or no unified billing shows no
  weekly meter.

## Not verified here

The live billing shape was read once with the maintainer's own CLI sign-in on
2026-10-12 (SuperGrok, weekly period, no percent, a zero balance). A comparison
of the weekly usage, reset and credits with Grok's Settings > Usage, and a
`grok login` sign-in through CodexBar, are left to the account owner.
