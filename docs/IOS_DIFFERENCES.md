# Differences from codexbar-ios

CodexBar for Windows follows [codexbar-ios](https://github.com/HemSoft/codexbar-ios)
(compared with its README at commit `e117c41`, October 5, 2026). These are the
differences that remain, and why.

## Providers

| Provider | iOS | Windows | Why |
| --- | --- | --- | --- |
| ChatGPT / Codex, Claude, Copilot, Cursor, OpenRouter, OpenCode Go + Zen, Moonshot, Greptile | Yes | Yes | |
| GitHub Billing: monthly spend per owner | Yes | Yes | Windows signs in through the GitHub CLI ([GITHUB_BILLING.md](GITHUB_BILLING.md)) |
| GitHub Billing: included allowances and budgets | Yes | Yes | Windows also counts billable usage past an allowance without repository visibility ([GITHUB_BILLING.md](GITHUB_BILLING.md)) |
| Grok | Yes | Yes, experimental | Windows signs in through the official Grok CLI in a home per account ([GROK.md](GROK.md)) |
| X Subscription: SuperGrok and Grok Bot weekly limits, Extra Usage Credits | Planned ([codexbar-ios#458](https://github.com/hemsoft-dev/codexbar-ios/issues/458)) | Yes, experimental | Windows reads the Grok CLI's and the Cursor app's own sign-ins ([X_SUBSCRIPTION.md](X_SUBSCRIPTION.md)) |
| Google Gemini: Gemini Apps limits | Yes | Saved sessions only, experimental | New controlled-browser sign-in is retired; Antigravity coding quotas connect independently through the native CLI ([GEMINI.md](GEMINI.md)) |
| Google Gemini: coding quotas | Yes | Yes, experimental | Windows reads them through Google's Antigravity CLI (`agy`) instead of its own OAuth client; one Gemini account can show them ([GEMINI.md](GEMINI.md)) |

Copilot organization billing on Windows is configured per account. With an
enterprise set, it covers Copilot Enterprise seats: the organization's AI
credits and each user's share. With only an organization set, it reads a
Copilot Business organization's own AI-credit report, as iOS does, for an owner
or billing manager with or without a seat there, and also lists the
organization's Copilot budgets and whether they stop usage (#233).

## Signing in

- **iOS** signs accounts in itself, in the app (browser or private sign-in),
  and for Copilot ships the public client ID used by Copilot CLI-compatible
  clients.
- **Windows** uses no borrowed client IDs. Extra accounts are signed in through
  each provider's own CLI, each in a folder of its own: the Codex CLI's
  app-server, the GitHub CLI's device sign-in, a Claude Code window, and the
  Cursor CLI. OpenCode uses the Console's device approval with CodexBar for
  Windows' own client label; Windows opens the default browser, so there is no
  separate private-browser choice. The CLIs' own sign-ins (`~/.codex`, `~/.claude`, `gh`, the Cursor
  app) are the fallback. See [OAUTH.md](OAUTH.md).
- Secrets live in the iOS Keychain on iOS and in Windows Credential Manager on
  Windows. CLI sign-ins stay in the files those CLIs write.

## Features

| Feature | iOS | Windows |
| --- | --- | --- |
| Dashboard, history charts, alerts, groups and ordering | Yes | Yes |
| Home-screen / lock-screen widgets | Yes | Not yet: Windows widgets need a packaged app ([#93](https://github.com/hemsoft-dev/codexbar/issues/93)-[#95](https://github.com/hemsoft-dev/codexbar/issues/95)) |
| watchOS companion | Yes | Not applicable |
| System tray icon and tooltip | Not applicable | Yes |
| Codex credits pool metric | Yes, opt-in per account | Yes, always on; the headline once a window is used up ([CODEX_CREDITS.md](CODEX_CREDITS.md)) |
| Distribution | App Store | Built from source for now; MSIX planned ([#93](https://github.com/hemsoft-dev/codexbar/issues/93)) |
