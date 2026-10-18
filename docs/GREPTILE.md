# Greptile review activity

A Greptile account in CodexBar (#103) shows an organization's code-review
activity, read with an organization API key. It follows the iOS app's contract
([GREPTILE-USAGE.md](https://github.com/HemSoft/codexbar-ios/blob/e117c41/GREPTILE-USAGE.md)).

## What is read

CodexBar posts JSON-RPC `tools/call` requests to Greptile's MCP server
(`https://api.greptile.com/mcp`) for one tool only: `list_code_reviews`, in
pages of 100. It never calls a tool that starts a review, changes a repository
or touches billing. The key is sent as a bearer token and kept in Windows
Credential Manager under the account. As for every API key, an account's saved
key always wins; `GREPTILE_API_KEY` is used only for an account with no key
saved, which matters here because a Greptile key belongs to one organization.

Pages are read until a short page ends the history, up to an offset of 1,000;
a full page is always followed, whatever its `total` says. Reviews are counted
once by id. A page marked truncated, fewer unique reviews than the stated total,
or a history deeper than the cap is shown as an error, never as a short count. JSON and event-stream responses are both accepted.

## What is shown

- **Completed reviews** is a count with no limit, reset or bar. It is not a
  credit balance, a share of an allowance or a number of pull requests: Greptile
  bills completed review runs by tier, per author, and its published tools
  return no billing data.
- Reviews in other states (pending, in progress, failed, skipped, other) are
  listed in the account's message, never counted as completed.
- **Reviews used** appears only when a response carries an explicit review-named
  used and allowance pair *and* a reset date. Credit-named, malformed, Boolean,
  fractional or non-positive values never become a quota, and no allowance is
  inferred from a plan.
- A rejected key, a key without access to the organization's reviews, a rate
  limit and an unreadable response are each shown as such on the account's row.
  The last good result stays visible meanwhile.

Greptile is off until a Greptile account is added, or `GREPTILE_API_KEY` is
set, which then stands in as one implicit account. Each account reports under
its own id, so several organizations can be added side by side.

## Not verified here

A live comparison with an organization's Greptile dashboard is left to the
account owner. No paid plan, review or billing action was used to build this.
