# GitHub Billing

A GitHub Billing account (#104) shows one owner's spend across GitHub products
this billing month: your own billing, or an organization's you can read. It is a
provider of its own, apart from Copilot.

## Signing in

Add a GitHub Billing account in Settings > Accounts. Leave **Organization**
blank for your own billing, or give the organization's name. CodexBar signs the
account in with your browser through the GitHub CLI's device sign-in, in a
private folder, and keeps the token in Windows Credential Manager under the
account ([OAUTH.md](OAUTH.md)).

- Your own billing needs GitHub's `user` scope, which the sign-in asks for.
  GitHub's `user` scope also allows changing your profile and reading private
  email addresses; CodexBar only reads your login.
- An organization's billing works with the GitHub CLI's `repo` scope, for an
  owner or billing manager there. CodexBar only reads billing with it.
- Before the token is kept, CodexBar checks it can read the chosen owner's
  billing for this month. A token that can't is never saved, and one billing
  account per owner can be added.
- An account follows one owner. Changing its organization makes it a new
  account that signs in again, and the old owner's history and token are
  deleted. One following your own billing that signs in under another login
  starts afresh the same way: GitHub's login can't tell another user from a
  renamed one, so the old login's history is never shown as the new one's.
- With no GitHub Billing account left, GitHub Billing is off.
- The token is CodexBar's own: it never replaces or widens a Copilot sign-in or
  the GitHub CLI's own accounts. Sign out deletes CodexBar's copy; revoke it at
  github.com/settings/applications to end it on GitHub's side.

## What is shown

CodexBar reads the owner's usage summary
(`/users/{user}/settings/billing/usage/summary` or
`/organizations/{org}/settings/billing/usage/summary`) for the current UTC
month. A summary for another month or another owner is refused, never shown as
this one.

- **Net spend**, **Gross charges** and **Discounts** this month, each as spend
  with no limit. The month's end is shown as the reset.
- One line per product with its net, gross and discounts, and each SKU's
  quantity and unit price at GitHub's own precision (for example
  `actions_linux 16,596 minutes at $0.006/minute`).
- A month-end projection of net spend, only once at least a day of the month
  has passed and something has been spent.
- GitHub bills in US dollars and its API returns no currency, so amounts are
  dollars. If a response ever names a currency, it must name one known currency
  for every amount; partial or conflicting currencies hide the amounts rather
  than relabel them.

A sign-in GitHub rejects, a missing permission, an owner with no billing data
and a rate limit each show as such on the account's row.

## Included allowances (#105)

For plans GitHub's
[included-usage table](https://docs.github.com/en/billing/reference/product-usage-included)
lists at the owner's scope (personal Free and Pro, organization Free and Team,
checked 2026-10-12), each allowance this month is a bar that can pass 100%, and a
line with used, included and left (or over). Enterprise allowances are pooled
above the organization and aren't shown.

- **Actions minutes** count standard GitHub-hosted runners at the Linux rate
  (a macOS minute is about ten), with prices that must match GitHub's
  [runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing).
  Larger and self-hosted runners don't use included minutes. An unknown runner
  or price leaves the allowance out.
- **Actions storage** is the GB-hours of the month turned into GB held on
  average.
- **Packages storage and data transfer** show only a verified zero or billable
  usage: GitHub doesn't say which package usage is free.
- **Git LFS storage and bandwidth**, and on personal plans **Codespaces compute**
  (machine hours times each SKU's cores) and **Codespaces storage**.

GitHub bills a covered SKU only once its included usage is used up, so billable
usage means the allowance is spent and the billable part is exactly what went
past it. Before then, only private repositories' usage counts, which takes each
repository's visibility. GitHub reports visibility as it is now, so a repository
that moved to another owner, or can't be read, leaves the allowance out; one
made public or private within the month can be misjudged until something is
billable. Quantities that don't add up, or billable usage before the allowance
is used, also leave that allowance out. One allowance that can't be verified
never hides the others or the spend.

## Organization budgets

An organization's budgets are listed with how much of each this month's usage
has spent, measured from the organization's own usage for organization and
repository budgets, and whether it stops usage. Other scopes (cost centers,
enterprises) list their amount and say their spending can't be measured here.

## Not verified here

A comparison of the totals with GitHub's billing page for the same owner and
month is left to the account owner.
