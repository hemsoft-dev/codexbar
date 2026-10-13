# Browser sign-in (OAuth) for CodexBar

CodexBar's Rust app has one shared sign-in path for providers that support it
(`crates/codexbar-providers/src/oauth.rs`, issue #77). It follows
[RFC 8252, OAuth 2.0 for Native Apps](https://www.rfc-editor.org/rfc/rfc8252):

- **System browser.** Sign-in opens the user's default browser. CodexBar never
  shows a provider's sign-in page inside its own window.
- **Authorization code with PKCE.** Every sign-in uses a fresh S256 code
  challenge ([RFC 7636](https://www.rfc-editor.org/rfc/rfc7636)) and a random
  `state`.
- **Loopback redirect.** The callback listener binds only to `127.0.0.1` (or
  `::1`) on a port Windows picks. It lives for one sign-in. Requests to another
  path, with a wrong `state`, or malformed are answered and ignored. Only the
  matching `state` completes the sign-in.
- **Ending a sign-in.** A sign-in ends on success, on a refusal (only the
  provider's short error code is kept), after the timeout, or when the user
  cancels.
- **Token storage.** Tokens are stored as the account's secret in Windows
  Credential Manager (`CodexBar:account:<id>`), never in `settings.json`. A
  renewal replaces them in one write and keeps the old refresh token when the
  provider doesn't rotate it.
- **No secrets in output.** Errors, logs and the UI never show tokens,
  authorization codes or token-endpoint response bodies.

## Sign-in through a provider's own CLI

Where a provider's official CLI can sign in for another program, CodexBar uses
that instead of a client registration of its own. The CLI runs its own sign-in
and keeps writing its own credential file; CodexBar only reads it.

- **ChatGPT / Codex (#78).** An account with the OAuth sign-in method gets its
  own Codex home in the settings folder (`codex\<account id>`, with
  `cli_auth_credentials_store = "file"`). CodexBar starts
  `codex app-server` for that home and uses its JSON-RPC methods:
  `account/login/start` (browser sign-in; CodexBar opens the returned page and
  Codex listens for the redirect), `account/login/cancel`, `account/logout`,
  and `account/read` with `refreshToken: true` to renew tokens. A sign-in is
  renewed when its access token expires within a day, or once after the usage
  endpoint refuses it, at most once per home every 15 minutes. Each ChatGPT
  user and workspace is its own dashboard account, so signing a home in to
  another identity never inherits the previous one's history. An account with
  the Automatic method reads the Codex CLI's own `~/.codex` (or `CODEX_HOME`),
  the import and fallback path. The app-server process tree runs in a Windows
  job that ends with each session.

- **GitHub Copilot (#79).** An account with the OAuth sign-in method is signed
  in by the GitHub CLI's own device sign-in (`gh auth login --web
  --insecure-storage`), run in a private, temporary config folder inside the
  settings folder with the clipboard copy and prompts off. Not in a terminal,
  `gh` prints the one-time code and `https://github.com/login/device`; CodexBar
  shows the code and opens the page. Once `gh` reports the sign-in, CodexBar
  reads the token with `gh auth token`, keeps it in Credential Manager under
  the account (`CodexBar:account:<id>`) and deletes the folder. The user's own
  `gh` accounts, keyring entries and git configuration are never touched.
  An account with org billing set also asks for `manage_billing:enterprise`,
  which the enterprise AI-credit report needs. `gh` runs in a kill-on-close
  Windows job, and folders an interrupted sign-in left behind are deleted at
  startup.
  GitHub CLI tokens don't expire; Sign out deletes CodexBar's copy (revoke it
  at github.com/settings/applications to end it on GitHub's side). Accounts with
  the Automatic or Command line method use the GitHub CLI's own accounts, the
  import and fallback path.

- **Claude (#80).** An account with the Browser session sign-in method (OAuth
  already means Claude Code's own sign-in for Claude) gets its own Claude Code
  config folder in the settings folder (`claude\<account id>`, used as
  `CLAUDE_CONFIG_DIR`). `claude auth login` is an interactive terminal UI, so
  CodexBar opens it in a console window of its own and waits for it to close
  with a sign-in. Renewal is Claude Code's own: when a token expires within
  five minutes, or the usage endpoint refuses it, CodexBar runs `claude auth
  status --json` for that folder, which refreshes the token under Claude
  Code's refresh lock (`.oauth_refresh.lock.owner`), then reads the file again;
  at most once per folder every 15 minutes. CodexBar never writes the
  credentials, so a stale refresh can't overwrite newer ones. Sign out runs
  `claude auth logout`. Each Claude account and organization
  (`.claude.json` `oauthAccount`) is its own dashboard account. Claude Code's
  own `~/.claude` is the import and fallback path, renewed the same way. Every
  process runs in a kill-on-close job.

- **Cursor (#81).** An account with the OAuth sign-in method gets its own
  folder in the settings folder (`cursor\<account id>`), which Cursor's CLI
  (`cursor-agent`) uses as its `APPDATA` and `CURSOR_CONFIG_DIR`. Its sign-in
  then lives in that folder's `Cursor\auth.json`, apart from the Cursor app's.
  With `NO_OPEN_BROWSER` set, `cursor-agent login` prints the sign-in page;
  CodexBar opens it and waits for the CLI to finish. The account's identity
  (its token's subject) and email (`cursor-agent status`) are remembered.
  Signing a signed-in account in again asks first, because it replaces that
  sign-in. Sign out runs `cursor-agent logout`. The Cursor app's own sign-in
  (Automatic) is the fallback.

- **OpenCode (#102).** OpenCode has no CLI that can sign in for another
  program, but its Console offers a device approval
  ([RFC 8628](https://www.rfc-editor.org/rfc/rfc8628)). An OpenCode Go account
  with the OAuth method asks `https://opencode.ai/console/auth/device/code`
  for an approval with CodexBar's own public client label `codexbar-windows`
  (never the iOS app's) and opens the Console's approval page in the default
  browser. There the user signs in to OpenCode, approves CodexBar and picks the
  workspace; the dialog shows the code the page should show. CodexBar accepts
  only the exact `https://opencode.ai/console/device` page, polls at the
  interval the Console sets (longer after `slow_down`), and stops at expiry,
  denial or Cancel. The token must name a workspace, and
  `/console/auth/session` must name the user, before anything is saved. The
  sign-in (tokens, expiry, workspace and user) is kept in Credential Manager
  under the account, and the account is bound to that workspace and user:
  signing it in again as someone else is refused, and another workspace is
  another account. Tokens are renewed with the refresh grant a minute before
  they expire, one renewal at a time, and a renewal is saved only over the
  sign-in it renewed, so a removed or replaced sign-in never comes back. Go and
  Zen are read from `/console/api/go/status` and `/console/api/billing/status`
  with the workspace in `x-org-id`; redirects are never followed. Sign out
  deletes CodexBar's copy; it doesn't revoke the approval at OpenCode. These are
  OpenCode's first-party Console endpoints, not a published API, so a changed
  shape fails visibly. The dashboard-cookie method (Browser session) still
  works for existing setups.

## Approved client registrations

A provider can use this path only once a human has confirmed its client
registration and redirect. Using another application's client ID (for example
the Codex CLI's or Claude Code's) is not approved. Those tools' own sign-ins
are read instead, as before.

| Provider | Client ID | Redirect | Scopes | Approved by | Date |
| --- | --- | --- | --- | --- | --- |
| None yet | | | | | |

To add a provider:

1. Register CodexBar as a native (public) client with the provider. Allow the
   redirect `http://127.0.0.1/<path>` with any port. RFC 8252 section 7.3
   requires providers to accept any loopback port.
2. Record the registration in the table above, with who approved it.
3. Give the provider adapter an `OAuthClient` with those values. Then wire its
   sign-in (#78 Codex, #79 Copilot, #80 Claude, #81 Cursor).
