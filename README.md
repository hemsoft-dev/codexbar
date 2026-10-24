# CodexBar 🎚️ for Windows

A Windows system tray app that keeps your AI provider usage limits visible. Inspired by [steipete/CodexBar](https://github.com/steipete/CodexBar) (macOS).

Built with Rust, [GPUI](https://www.gpui.rs/) and [gpui-kit](https://gpui-kit.com/): native Windows, no Electron overhead.

## Providers

| Provider | Auth Method | What's Tracked |
|----------|-------------|----------------|
| **ChatGPT / Codex** | Codex CLI ChatGPT login (`~/.codex/auth.json`), or browser sign-in per account through the Codex CLI | 5-hour + weekly usage limits |
| **Claude** | Claude Code login (`~/.claude/.credentials.json`), or a separate sign-in per account through Claude Code | Session + weekly limits, extra-usage spend |
| **Copilot** | GitHub CLI (`gh auth`), or browser sign-in per account through the GitHub CLI | Usage limits per account; organization AI credits for Copilot Business organizations and Enterprise seats |
| **Cursor** | Cursor app sign-in (`%APPDATA%\Cursor\auth.json`; the Cursor CLI's sign-in on macOS and Linux), or browser sign-in per account through the Cursor CLI | Plan usage and spend |
| **OpenCode Go / Zen** | Browser approval on OpenCode's own page, or a dashboard cookie + workspace ID | Go limits and Zen balance |
| **OpenRouter** | API Key | Credits, usage across models |
| **Moonshot (Kimi)** | API Key | Remaining API credit balance |
| **Greptile** | Organization API key | Completed code reviews (activity, not credits) |
| **GitHub Billing** | Browser sign-in through the GitHub CLI | This month's spend across GitHub products, for you or an organization |
| **Grok** (experimental) | Grok CLI sign-in (`grok login`) | A paid plan's weekly usage and Extra Usage Credits |
| **X Subscription** (experimental) | Grok CLI sign-in and Cursor sign-in | Weekly SuperGrok and Grok Bot limits, Extra Usage Credits |
| **Gemini / Antigravity** (experimental) | Antigravity CLI shared local sign-in; previously saved Gemini Apps sessions | Antigravity coding quotas; Gemini Apps limits when a saved session still works |

## Features

- **Weekly SUS** beside quota bars, with a fleet score, price weights and explicit unavailable states. [Calculation and provider sources](docs/SUS.md).
- **Subscription plans** from provider account data, including Claude Max 5x/20x when reported. Generic sources explain missing tier detail; [plan sources and refresh behavior](docs/SUBSCRIPTION_PLANS.md).
- **System tray** icon with a usage tooltip
- **Dashboard** with an account table, focus cards, usage history and window curves; wide enough, the table shows
  each account's 5-hour and weekly windows side by side, and every reset reads as time left ("in 2d 4h"), never a
  date
- **Projection in every bar**: the share a limit is on pace to reach by its reset runs on past the solid share,
  faded, with a tooltip that says both figures and the pace behind them
- **Alerts** when an account nears or hits its limit
- **Groups and ordering** by urgency or by hand
- **Fix sign-ins from the dashboard**: a provider whose key or sign-in no longer works shows **Open settings**,
  which lands on that account in Settings
- **System, light and dark appearance**, following Windows high contrast
- **Remembers its window**: the display, position, size and state (maximized or out of the way) come back after a
  restart, with a visible fallback when that display is gone
- **Privacy-first**: on-device only, no data sent anywhere except provider APIs; keys are kept in Windows Credential
  Manager (the Keychain on macOS, the Secret Service keyring on Linux)

## Getting Started

### Requirements

- Windows 10 or later
- [Rust](https://www.rust-lang.org/tools/install) (the MSVC toolchain; `rust-toolchain.toml` pins stable)
- Visual Studio Build Tools with the "Desktop development with C++" workload (its Windows SDK supplies `rc.exe`, which compiles the executable's icon)
- Node.js 20 or later (required for `npm install` / pre-commit hook setup)

CodexBar also runs on macOS and Linux ([#161](https://github.com/hemsoft-dev/codexbar/issues/161)), built from
source. It keeps secrets in the macOS Keychain or the Linux Secret Service and finds the provider CLIs on PATH and in
`~/.local/bin`, Homebrew and `/usr/local/bin`. On macOS it is a menu-bar app with no Dock icon. Building needs:

- macOS: [Rust](https://www.rust-lang.org/tools/install) and the Xcode Command Line Tools (`xcode-select --install`).
  GPUI's shaders compile at startup, so full Xcode isn't needed.
- Linux: Rust plus the headers GPUI links against, on Debian or Ubuntu
  `sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig-dev libfreetype-dev
  libxcb1-dev libx11-xcb-dev`. The tray icon needs a desktop with StatusNotifierItem support (KDE Plasma, or GNOME
  with the AppIndicator extension), and saved keys need a Secret Service such as GNOME Keyring or KWallet.

Then `cargo run --release -p codexbar-app`, or build a package: `./package-macos.sh` makes `CodexBar.app` and a
DMG, and `./package-linux.sh` makes a `.deb` for Debian and Ubuntu (see [docs/PACKAGING.md](docs/PACKAGING.md#macos-and-linux)).
On macOS only the app from the DMG shows notifications, because macOS delivers them only to an app bundle.

Some features exist only on Windows: the MSIX package and its updates, Start with Windows, and Windows widgets. On
macOS, separate Cursor accounts can't be added yet, because the Cursor CLI keeps a single sign-in in the Keychain
there. The Automatic method reads that sign-in. Dates follow the system's locale on every platform.

### Build from source

CI scans the locked Rust dependencies for vulnerabilities and unsoundness.
See [Rust dependency security](docs/RUST_DEPENDENCY_SECURITY.md) for the pinned
scanner, local commands, report artifacts and exception policy.
CI also measures [Rust production coverage and per-function CRAP](docs/RUST_TEST_METRICS.md)
and rejects regressions against the base revision.
The [Rust mutation qualification](docs/RUST_MUTATION_TESTING.md) also verifies
selected domain, provider and persistence assertions with a pinned tool.
[Markdown tooling security](docs/NPM_ADVISORIES.md) covers the npm advisory gate,
scoped dependency overrides and Node.js prerequisites.

```powershell
git clone https://github.com/hemsoft-dev/codexbar.git
cd codexbar
npm install          # Also configures the pre-commit hook
.\run.ps1            # Builds in release and starts CodexBar in the system tray
```

`run.ps1` copies the app to `%LOCALAPPDATA%\CodexBar\bin\codexbar.exe` and replaces a running instance. For
development, `cargo run -p codexbar-app -- --demo` starts the dashboard with sample data beside the real app.

### Provider setup

1. **ChatGPT / Codex**: Run `codex` and sign in with your ChatGPT account. For more accounts, add a Codex account with the OAuth sign-in method in Settings > Accounts; CodexBar signs it in with your browser through the Codex CLI
2. **Claude**: Run `claude` and sign in. For more accounts, add a Claude account with the Browser session method in Settings > Accounts; CodexBar opens a Claude Code window to sign it in
3. **Copilot**: Uses GitHub CLI tokens — run `gh auth login` for each account, or add a Copilot account with the OAuth sign-in method and sign it in from Settings. To see an organization's AI credits, set the account's organization (and optionally the pool total): for a Copilot Business organization with no enterprise, the account needs to be its owner or billing manager, with or without a Copilot seat there; for an Enterprise seat, also set the enterprise
4. **Cursor**: Sign in to the Cursor app. For more accounts, install the Cursor CLI (`cursor-agent`) and add a Cursor account with the OAuth sign-in method in Settings > Accounts. On macOS and Linux, CodexBar reads the Cursor CLI's own sign-in instead of the app's: install `cursor-agent` and run `cursor-agent login`. Separate Cursor accounts work on Linux only
5. **OpenRouter**: Get an API key from [openrouter.ai/keys](https://openrouter.ai/keys) and add it in Settings, or set `OPENROUTER_API_KEY` (used only while no key is saved)
6. **Moonshot (Kimi)**: Get an API key from [platform.kimi.ai](https://platform.kimi.ai/) and add it in Settings, or set `MOONSHOT_API_KEY` (used only while no key is saved)
7. **OpenCode Go / Zen**: Add an OpenCode Go account in Settings > Accounts (OAuth, the default); CodexBar opens OpenCode's approval page, where you approve it and pick the workspace. Add one account per workspace. The older setup still works: an account with the Browser session method, the dashboard auth cookie and your workspace id (`wrk_…`), or `OPENCODE_GO_AUTH_COOKIE` and `OPENCODE_GO_WORKSPACE_ID`. OpenCode Zen uses the same cookie unless you give it its own (`OPENCODE_ZEN_AUTH_COOKIE`)
8. **Greptile**: Create an organization API key in Greptile and add a Greptile account with it in Settings. Greptile runs only for an account that is switched on; `GREPTILE_API_KEY` supplies the key for an enabled account that has none saved and never turns Greptile on by itself. CodexBar counts the organization's completed code reviews; see [docs/GREPTILE.md](docs/GREPTILE.md)
9. **GitHub Billing**: Add a GitHub Billing account in Settings > Accounts, with an organization name or blank for your own billing. CodexBar signs it in with your browser through the GitHub CLI; see [docs/GITHUB_BILLING.md](docs/GITHUB_BILLING.md)
10. **Gemini / Antigravity** (experimental): Add a Gemini account in Settings > Accounts and choose **Connect Antigravity**. CodexBar reuses the CLI's local sign-in, or hands off to its normal default-browser sign-in, then verifies usage. Previously saved Gemini Apps sessions remain separate; new website sign-in is unavailable. See [docs/GEMINI.md](docs/GEMINI.md)
11. **Grok** (experimental): Sign in with the Grok CLI (`grok login`) and add a Grok account in Settings (Automatic), or choose OAuth to sign an account in through the CLI in a folder of its own; see [docs/GROK.md](docs/GROK.md)
12. **X Subscription** (experimental): Sign in with the Grok CLI (`grok login`) and to Cursor (step 4), then add an X Subscription account in Settings. It shows what Grok's Settings > Usage shows, each once; see [docs/X_SUBSCRIPTION.md](docs/X_SUBSCRIPTION.md)

How each sign-in works, and what CodexBar stores and sends, is in [docs/OAUTH.md](docs/OAUTH.md) and [docs/PRIVACY.md](docs/PRIVACY.md).

## Installing, updating and support

CodexBar runs either as an MSIX package or as a build from source.

**Package.** `.\package.ps1 -Trust -Install` builds a signed MSIX package,
publishes it to a local update channel (`%LOCALAPPDATA%\CodexBar\channel`) and
installs it. The package is signed with a self-signed certificate, which
Windows must trust once: that step asks for administrator approval. After that,
`.\package.ps1` publishes updates. Windows installs them when CodexBar starts,
or from **Settings › About**. Rollback, other PCs and uninstall are covered in
[docs/PACKAGING.md](docs/PACKAGING.md).

**Build from source.** `run.ps1` builds and starts CodexBar without a package
(see [Build from source](#build-from-source)):

- **Update**: `git pull`, then `.\run.ps1`. It rebuilds, stops the running
  CodexBar and starts the new one. Settings, saved keys and history are kept.
- **Roll back**: check out an earlier commit (`git checkout <commit>`) and run
  `.\run.ps1` again. Settings stay compatible: a file from a newer CodexBar is
  opened read-only rather than overwritten. To update again afterwards, switch
  back to the branch you build from (`git switch main`)
  before `git pull`, since a checked-out commit has no branch to pull into.
- **Start with Windows**: `run.ps1` keeps an existing `CodexBar` entry in
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` pointed at the installed
  app.
- **Widgets**: Windows widgets need the packaged app on Windows 11. Because the
  package is self-signed, Windows lists its widget only with **Developer Mode**
  on (Settings › System › For developers). Then open the Widgets board (Windows
  key + W), choose **Add widgets** and pick **CodexBar usage**. See
  [docs/PACKAGING.md](docs/PACKAGING.md#widgets).
- **Uninstall**: first quit CodexBar (tray menu, Quit CodexBar). Then delete
  `%LOCALAPPDATA%\CodexBar\bin`, the `.codexbar` folder in your profile (and the
  folder in `CODEXBAR_SETTINGS_DIR`, if you set it), the `CodexBar:account:*`
  entries in Credential Manager, and the `CodexBar` startup entry if you had one
  (`Remove-ItemProperty HKCU:\Software\Microsoft\Windows\CurrentVersion\Run -Name CodexBar`).
- **Support**: report problems at
  [github.com/hemsoft-dev/codexbar/issues](https://github.com/hemsoft-dev/codexbar/issues).
  Never paste keys, tokens or cookies into an issue.

Differences from the iOS app are listed in
[docs/IOS_DIFFERENCES.md](docs/IOS_DIFFERENCES.md).

## Architecture

```text
Cargo.toml
├── crates/codexbar-core/       # Models, metrics, alerts, layout, formatting
├── crates/codexbar-providers/  # One module per provider, HTTP and OAuth helpers
├── crates/codexbar-store/      # Settings, credentials, snapshots, usage history
└── crates/codexbar-app/        # GPUI dashboard and tray host
```

The `src/` folder holds the retired C# / WPF app.

## Credits

- Inspired by [steipete/CodexBar](https://github.com/steipete/CodexBar) (MIT) by Peter Steinberger
- Inspired by [ccusage](https://github.com/ryoppippi/ccusage) for cost tracking

## License

MIT
