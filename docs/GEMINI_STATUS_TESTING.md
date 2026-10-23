# Gemini subscription status qualification

Gemini Models weekly is the only active subscription pool. Its CLI ID is
`gemini-weekly`, mapped to metric key `gemini-models-weekly`. Argon uses the
different ID `gemini-argon-wtu-weekly` and metric key `argon-weekly`.

The shared account status policy supplies the table, subscription/fleet SUS,
account severity, urgency order, alert details, notifications, tray SUS and
widget status. Other pools remain visible as neutral detail information with
no SUS or risk badge. An unavailable Models weekly pool gives `n/a`, no bar and
no account risk; it never falls back to Argon, Other models or Gemini Apps.
A successful partial refresh clears held alerts for excluded or missing pools.
Other providers retain their existing policy.

## Sanitized home observation

The checked-in
[quota snapshot fixture](../crates/codexbar-app/fixtures/gemini-home-snapshot-20261023.json)
is an allowlisted saved provider result read from home's running Windows app on
October 23, 2026 at 7:41 PM EDT. It retains quota values, reset instants and metric
labels, with a synthetic account ID. It contains no credentials, identity,
settings, messages or other accounts. It is not claimed to be a raw API response.

At that observation, Models weekly was present with about 2.7858% used and a
projection near 24.57%. Argon had about 31.3175% used and projected past its limit.
The old aggregation could raise account risk even when the preferred row already
showed Models weekly. The missing-Models case is tested independently, by retaining
Argon only. Do not infer that Models was missing in the observed live result.

Two hidden PowerShell CLI probes timed out before returning a usable response;
`agy --version` reported 1.4.6. This does not prove the running app's fetch failed.
The saved quota result and the headless Rust diagnostic below supply separate,
explicit verification routes. Never print raw command stdout or token files.

## Automated qualification

Normal workspace tests cover the CLI parser with present, missing, disabled,
malformed, expired and late Models buckets; shared status/SUS/alert behavior;
held-alert recovery; the actual saved provider shape; widget output; and rendered
normal/wide transitions between present and missing Models. Positive tests retain
valid risk and alerts when Models weekly itself runs low. Informational Argon
never supplies a score, status bar or alert.

Native screenshot capture on air uses isolated settings and the sanitized fixture:

```sh
CODEXBAR_CAPTURE_DIR=<fresh-directory> \
CODEXBAR_SETTINGS_DIR=<fresh-directory>/settings \
CODEXBAR_CAPTURE_GEMINI_STATUS=1 \
cargo run -p codexbar-app --features capture --locked -- --demo
```

Add `CODEXBAR_CAPTURE_WIDE=1` for the wide viewport. The harness captures missing
and present Models in light and dark themes, and asserts the row SUS, fleet SUS,
unavailable note and absence of an account risk badge before saving frames.
The missing state is a simulated partial response, not the observed home payload.

## Read-only verification on home

Do not run the GUI, change focus, restart CodexBar or overwrite its installed
binary. Build in an owned worktree with a separate target directory. The ordinary
Rust test below creates no GPUI app or window. It runs the app's hidden bounded
zero-turn CLI reader, prints only recognized quota fields, then loads the existing
saved snapshot read-only. It checks the row, score, assessment and alerts against
Models weekly, then checks a missing-Models variant. A live CLI error is disclosed;
it cannot be reported as a successful fresh fetch. Saved-snapshot verification is
reported separately.

```powershell
$env:CODEXBAR_GEMINI_DIAGNOSTIC_DIR = Join-Path $env:USERPROFILE '.codexbar'
cargo test -p codexbar-app --locked live_gemini_status_from_saved_provider_data -- --ignored --nocapture
```

This test is ignored in ordinary CI because access to a real local profile needs
explicit authorization. It never saves settings or snapshots and never executes
`run.ps1`. A passing headless check verifies the provider/status path; it does not
claim that Franz has restarted or visually inspected the new app.
