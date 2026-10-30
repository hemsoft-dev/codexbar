# Antigravity desktop sign-in detection, issue 289

The [report and screenshots](https://github.com/hemsoft-dev/codexbar/issues/289)
show agy 1.4.10 signed in while CodexBar reports signed out.

## Diagnosis on Home

Read-only SSH inspection on October 29, 2026 found:

- Installed CLI: `C:\Users\User\AppData\Local\agy\bin\agy.exe`.
- Running app: `C:\Users\User\AppData\Local\CodexBar\bin\codexbar.exe`, session 1.
- HOME and USERPROFILE both `C:\Users\User`; APPDATA is its normal Roaming folder.
- Vendor data is under `.gemini\antigravity-cli`. CodexBar does not check auth
  file paths or parse a credential format. It delegates auth to agy.
- Vendor `cli.log` at 19:58 EDT records
  `Using file-based token storage because SSH session detected`, then
  `Print mode: silent auth failed`. CodexBar had injected loopback
  SSH_CONNECTION and SSH_CLIENT to prevent browser sign-in. This changes the
  credential backend, hiding the signed-in desktop keyring.
- CodexBar's legacy `AppData\Local\CodexBar\log.txt` last changed October 2.
  Its Rust settings directory contains snapshots/history and position-debug.log,
  but no provider diagnostic log. The vendor log supplies the direct evidence.
- Probe commands remain `--version`, with a 10-second deadline, then
  `-p /usage --output-format json --print-timeout 25s`, with a 30-second
  outer deadline. A sign-in prompt maps to SignedOut; a deadline maps separately
  to TimedOut. This was a store selection error, not a timeout misclassification.

[Vendor authentication documentation](https://antigravity.google/docs/cli/install/)
describes the OS keyring and remote sign-in modes. Inspection of the installed
1.4.10 executable also confirmed its headless-auth switches. Flags alone still
allow frontend OAuth attempts, so they are supplemented by Windows OS process
containment rather than treated as a browser-prevention guarantee.

## Fix and validation boundary

Remove inherited SSH mode for the usage probe. Keep the native HOME and user
profile; do not read, copy or log vendor tokens. Disable interactive backend
auth and self-update. On Windows launch detached inside a one-process job before
resuming the CLI. Browser-helper creation is denied by the OS. Explicit user
sign-in retains its separate unrestricted launch.

Regression coverage drives the actual CliRunner subprocess path with inherited
SSH and conflicting headless settings, verifies native-store selection, and
covers both usage success and a signed-out prompt. Windows also verifies no
console input device and denied child creation. Older unqualified versions are
rejected before /usage. Existing provider tests cover signed-out classification
and timeout independently.

Home's SSH logon could not reuse the session-1 desktop keyring. No session-1
launch, settings write, install, or interactive sign-in was performed. A live
connected-state screenshot remains a post-install check for the Chief; fixture
coverage is not proof of that desktop check.

## Release handoff

Prepare package version **1.1.0.247** from a single squash commit on main after
2cac438. Confirm the main commit count before packaging; concurrent merges may
increase the revision. Cargo stays 0.1.0 under the existing package-version scheme.
The Chief owns merge, tag and installation. Preserve Home's settings and launch
only the app executable directly in session 1 through a windowless launcher,
never a bare PowerShell. Check connection and usage after installation and attach
the connected-state screenshot to the PR.
