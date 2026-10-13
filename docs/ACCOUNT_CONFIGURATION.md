# Account configuration

CodexBar's accounts live in `settings.json` (in `%USERPROFILE%\.codexbar`) as
`accountConfigurationVersion: 1` and an `accounts` array. Accounts are added,
edited, switched off and removed in **Settings > Accounts**; every change is
saved at once. Removing an account and Reset accounts ask first, and so does
signing a signed-in Cursor account in again. Signing a Codex, Claude or Copilot
account in again with another identity replaces it without asking (see
[Identity and history](#identity-and-history)).

## Account records

| Field | Meaning |
| --- | --- |
| `id` | Stable id (32 hex digits for accounts CodexBar creates) |
| `providerId` | `Codex`, `Claude`, `Copilot`, `Cursor`, `OpenRouter`, `OpenCodeGo`, `OpenCodeZen`, `Moonshot`, `Greptile`, `GitHubBilling` or `Gemini` |
| `displayLabel` | The account's name |
| `enabled` | Whether it is fetched and shown |
| `authenticationMethod` | `Automatic`, `ApiKey`, `CommandLine`, `OAuth` or `BrowserSession` (see below) |
| `externalAccountId` | A Copilot account's GitHub username; for an account CodexBar signed in, the identity it holds (an OpenCode user id) |
| `workspaceId` | The OpenCode workspace (`wrk_…`): typed in for a cookie account, set by the sign-in for an OAuth one |
| `legacyCardKey` | Kept from older CodexBar versions for ordering |
| `copilotEnterprise`, `copilotOrganization`, `copilotPoolTotal` | Copilot org billing for an Enterprise seat; written only when set |
| `githubBillingOrganization` | The organization a GitHub Billing account follows; absent for the signed-in user's own billing |

None of these are secrets. Pasted keys and cookies, the GitHub tokens of
Copilot accounts CodexBar signs in, and the sign-ins of OpenCode accounts on
OAuth are kept in Windows Credential Manager under `CodexBar:account:<id>`. Codex, Claude and Cursor accounts CodexBar signs in keep
their tokens in the files those CLIs write, in the account's own folder under
the settings folder (`codex\`, `claude\`, `cursor\`). See [PRIVACY.md](PRIVACY.md).

## Sign-in methods

| Provider | The provider's own sign-in | Signed in by CodexBar |
| --- | --- | --- |
| ChatGPT / Codex | Automatic (`~/.codex`) | OAuth: its own Codex home, through the Codex CLI |
| Claude | OAuth or Automatic (`~/.claude`) | Browser session: its own Claude Code folder, through a Claude Code window |
| Copilot | Automatic or Command line (the GitHub CLI's accounts; a username limits it to one) | OAuth: the GitHub CLI's device sign-in, token kept by CodexBar |
| Cursor | Automatic (the Cursor app) | OAuth: its own folder, through the Cursor CLI |
| OpenRouter, Moonshot, Greptile | API key (or the environment variable) | |
| GitHub Billing | | OAuth: the GitHub CLI's device sign-in, token kept by CodexBar, apart from Copilot |
| Gemini | | Browser session: a temporary Edge or Chrome profile, two Google session cookies kept by CodexBar (experimental) |
| OpenCode Go / Zen | Browser session (a copied dashboard cookie) | OAuth (OpenCode Go): OpenCode's device approval in the browser, sign-in kept by CodexBar |

Accounts CodexBar signs in show **Sign in…**, **Sign in again…** and **Sign out**
in Settings. [OAUTH.md](OAUTH.md) explains each one. Several accounts can be added
for every provider except OpenCode Zen; OpenCode Go takes one OAuth account per
workspace. Adding the first account CodexBar signs in
for Codex, Claude or Cursor keeps the provider's own sign-in as an account of its
own, so it doesn't disappear.

## Identity and history

Each signed-in identity (a ChatGPT user and workspace, a Claude account and
organization in a CodexBar folder, a Cursor user, a GitHub username) is its own
dashboard account with its own history, groups and alerts. Signing an account in
to another identity starts a new dashboard account; the history CodexBar kept for
the identity it replaced is deleted, so identities never mix. Removing an account
deletes its saved secret, its CodexBar folder and, for an account that is its own
dashboard account, its usage history. Claude Code's own sign-in keeps the
account-neutral `claude` id, because its profile file can name another account
than its credentials.

## Compatibility

- A missing or syntax-invalid `settings.json` behaves like an empty one.
- A file from a newer CodexBar (a higher `accountConfigurationVersion`), an
  invalid account record, or provider keys that differ only in case make
  Settings read-only, and the file is never overwritten.
- API keys an older CodexBar kept in the file are moved to Credential Manager at
  startup; a key that can't be moved stays usable and is tried again next time.
- Settings that only the Rust app uses (groups, order, appearance, alerts) are in
  `dashboard.json`.
