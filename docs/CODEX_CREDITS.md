# Codex credits

A ChatGPT / Codex account can carry a pool of credits next to its 5-hour and
weekly windows. Once a window is used up, Codex keeps working on those credits.
CodexBar shows them so a used-up window doesn't read as a blocked account.

## Where the number comes from

The Codex provider already calls
`GET https://chatgpt.com/backend-api/wham/usage` with the account's Codex
sign-in. That response has an optional `credits` object with `has_credits`,
`unlimited` and `balance`. CodexBar reads it from the same response, so there's
no new request, permission or sign-in step. The iOS app reads the same contract
(`codexbar-ios/CODEX-CREDITS.md`).

- A finite, non-negative `balance`, sent as a decimal string or a number, is a
  count of credits. It's never money and never a percentage.
- `unlimited: true` shows as **Unlimited**, with no number and no bar.
- A missing, null, malformed or negative balance shows nothing. It's never
  treated as zero, and `has_credits` alone never supplies a number.
- A response without `credits` refreshes the windows as before.

## How the dashboard shows credits

- **A window is at 100% and credits remain** (a positive balance, or
  unlimited): the credits become the account's headline. The Limit column shows
  the starting balance, Used shows the credits left, % shows `—`, and Progress
  shows how much of the starting balance is spent. The used-up window no longer
  ranks or labels the account as blocked.
- **The windows are below 100%**: the windows stay the headline, and the credits
  are a secondary metric in the focused account's metric list and in the
  widgets.
- **No credits left** (a zero balance, or none reported): the used-up window
  blocks the account as before.

Credits have no severity threshold or pace projection: there's no limit or reset
to project against. History stores the count, and the History view charts it as
a count. Widgets show "18,400 credits left" or "Unlimited credits", never a
percentage.

## The starting balance

The bar runs from the highest balance CodexBar has seen since the last top-up:

- The first balance read for an account is its start.
- Spending down keeps the start, so the bar fills as credits run out.
- A balance higher than the start is a top-up and becomes the new start.

The start is kept per account in `dashboard.json` under `creditStarts`, with
the other settings only the Rust app uses, so it survives a restart. It's a
count of credits, not a secret. Removing an account forgets its start, and an
account whose id changes keeps it.

## The pill at 100%

A limit at 100% shows no status pill ("Limit soon") beside the account. The
`100%`, the full bar and the red status dot already say it, and "soon" is wrong
once the limit is reached. Below 100% the pills stay as before (Watch, At risk,
Limit soon).
