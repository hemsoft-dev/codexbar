# Privacy and data handling

CodexBar runs on your PC only. It has no server, no account of its own, no
analytics and no telemetry. It talks to each provider's own API, with the
sign-in or key you gave it, and to nothing else.

## Network destinations

CodexBar sends requests only to providers that are on and have a sign-in or key
to use. On a first start, before any account is set up, every provider except
Moonshot, Greptile, GitHub Billing, Gemini, Grok and X Subscription is on: one already signed in on this PC (the Codex CLI, Claude Code, the
Cursor app, the GitHub CLI) is fetched right away, and one without a sign-in or
key makes no request. To stop one, add an account for it in Settings > Accounts
(the provider's own sign-in, Automatic) and switch that account off.

| Provider | Endpoint | Authenticated with |
| --- | --- | --- |
| ChatGPT / Codex | `https://chatgpt.com/backend-api/wham/usage` | The Codex sign-in (`auth.json`) |
| Claude | `https://api.anthropic.com/api/oauth/usage` and `…/api/oauth/profile` (current plan only) | The Claude Code sign-in (`.credentials.json`) |
| Copilot | `https://api.github.com/copilot_internal/user`, and for org billing `https://api.github.com/enterprises/…/settings/billing/ai_credit/usage` (with an enterprise) or `https://api.github.com/organizations/…/settings/billing/ai_credit/usage` and `https://api.github.com/organizations/…/settings/billing/budgets` (a Copilot Business organization without one), and `https://api.github.com/orgs/…/copilot/billing` | A GitHub CLI token |
| Cursor | `https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage` and `…/GetPlanInfo` (current plan) | The Cursor sign-in (`auth.json`) |
| OpenRouter | `https://openrouter.ai/api/v1/credits` | Your API key |
| Moonshot (Kimi) | `https://api.moonshot.ai/v1/users/me/balance` | Your API key |
| Greptile | `https://api.greptile.com/mcp` (only the read-only `list_code_reviews` tool) | Your organization API key |
| Grok | `https://cli-chat-proxy.grok.com/v1/billing` and `…/v1/settings` (its plan); `https://auth.x.ai` userinfo once at sign-in | The Grok CLI's sign-in (`auth.json` in the account's Grok home) |
| X Subscription | The Grok endpoints above, with the Grok CLI's sign-in; `https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus` (the Grok Bot limit), with the Cursor sign-in | Each sign-in only on its own service: the Grok CLI's `auth.json`, the Cursor sign-in (`auth.json`) |
| Gemini | `https://gemini.google.com/usage` and `https://gemini.google.com/_/BardChatUi/data/batchexecute` (the usage RPC only) | Two Google session cookies (`__Secure-1PSID`, `__Secure-1PSIDTS`) kept by CodexBar |
| GitHub Billing | `https://api.github.com/user`; the owner's `…/settings/billing/usage/summary` and `…/settings/billing/usage`; for an organization `…/orgs/{org}` (its plan) and `…/settings/billing/budgets`; `…/repos/{owner}/{repo}` for repositories in this month's Actions usage (their visibility, remembered for six hours) | The account's own GitHub CLI token, kept by CodexBar |
| OpenCode Go / Zen | OAuth accounts: `https://opencode.ai/console/api/go/status` and `…/console/api/billing/status`; cookie accounts: `https://opencode.ai/workspace/…` | The account's OpenCode sign-in (OAuth), or your dashboard auth cookie |

The optional account-profile lookups keep only the effective subscription name
or tier. Claude profile names, emails and organization identifiers are not kept
or logged. Plan lookup failure leaves valid usage visible. See
[subscription plan sources and limits](SUBSCRIPTION_PLANS.md).

For the GitHub CLI's Copilot accounts, CodexBar also runs `gh auth status` on
each refresh to list them; the GitHub CLI checks each account with GitHub while
it does.

A Gemini account with coding quotas on runs Google's Antigravity CLI
(`agy -p "/usage" --output-format json`) on each refresh; the CLI reads the
quotas from Google with its own sign-in. It runs no model turn.

Signing an account in from Settings runs the provider's own CLI (Codex CLI,
GitHub CLI, Claude Code or the Cursor CLI), which talks to that provider's
sign-in service. CodexBar opens the provider's sign-in page in your default
browser. OpenCode has no such CLI step: CodexBar asks the OpenCode Console
(`https://opencode.ai/console/auth/…`) for an approval code, opens its approval
page, and confirms the approved user and workspace with the Console. See [OAUTH.md](OAUTH.md) for how each sign-in works.

## Secrets

- **API keys and cookies** you paste in Settings are stored in Windows
  Credential Manager under `CodexBar:account:<id>`, not in a file. A stored
  key is the one used; an environment variable (`OPENROUTER_API_KEY`,
  `MOONSHOT_API_KEY`, …) stands in only for an account with no stored key, and
  the account's row in Settings says which is in use. Keys an older CodexBar left in its settings
  file are moved to Credential Manager at startup. If that move fails, the key
  stays in the file and in use, Settings says so, and the move is tried again
  at the next start.
- **Copilot accounts CodexBar signs in** keep their GitHub CLI token in
  Credential Manager, split across parts when it is long.
- **Codex, Claude and Cursor accounts CodexBar signs in** keep their sign-in
  where the provider's CLI writes it, in that account's own folder under the
  settings folder (`codex\`, `claude\`, `cursor\`). CodexBar reads those files
  and never writes them.
- **Sign-ins of the providers' own apps** (`~/.codex`, `~/.claude`, the Cursor
  app's, your `gh` accounts) are read, never written by CodexBar. One exception
  is renewal: when a Codex or Claude sign-in (the CLI's own or one CodexBar
  made) is about to expire or is refused, CodexBar asks that CLI to renew it
  (`codex app-server`; for Claude, `claude mcp get` with a server name nobody
  configures, which renews the sign-in and connects to no MCP server). The CLI
  then writes the renewed tokens, as it would when you use it. Claude Code's
  renewal also asks Anthropic for the account's claude.ai connector list
  (`https://api.anthropic.com/v1/mcp_servers`), as it does whenever it lists
  MCP servers; CodexBar doesn't read the answer.
- Secrets never appear in the UI, error messages, notifications, logs or
  saved snapshots. Errors keep only a provider's short error text or status.

## Files

Everything CodexBar writes lives in `%USERPROFILE%\.codexbar`. With
`CODEXBAR_SETTINGS_DIR` set (for isolated testing), everything except
`history.jsonl` moves to that folder:

| File | What it holds |
| --- | --- |
| `settings.json` | Accounts (provider, name, sign-in method, username or workspace id, org billing settings), refresh interval, zoom |
| `dashboard.json` | Groups, order, appearance, alert settings, which alerts were shown, hidden history series, each Codex account's starting credit balance, the dashboard window's last display, position, size and state |
| `snapshots.json` | The last good usage of each account (ids, account labels, provider-reported plan names, metric names, numbers, times), so the dashboard shows something at startup and while a provider fails |
| `history.jsonl` | Usage samples for the history charts, kept **30 days** and pruned at startup |
| `codex\`, `claude\`, `cursor\` | One folder per account CodexBar signed in, holding that provider CLI's own sign-in files |

Removing an account in Settings deletes its saved key or token, its folder and
its usage history (for accounts that are their own dashboard account). Reset
accounts does the same for every account. Removing the `.codexbar` folder
removes everything else CodexBar stored.

## Notifications

Alerts (nearing or hitting a limit) are shown as Windows notifications. They
contain the account's name and the alert, never a secret.
