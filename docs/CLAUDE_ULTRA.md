# Claude Max and Ultra

[Anthropic pricing](https://claude.com/pricing) lists Max at $100/month and Ultra
at $200/month. The [Ultra help article](https://support.claude.com/en/articles/17728952-what-is-the-ultra-plan)
confirms that Max 20x becomes Ultra. Max 5x is now Max. Verified Ultra accounts
have no five-hour limit; unverified Ultra accounts temporarily retain one.
Weekly and model-scoped weekly quotas still apply.

CodexBar maps the observed legacy profile tiers `default_claude_max_20x` and
`default_claude_max_5x` to Ultra and Max. It retains their $200/$100 price weights.
An unspecified `claude_max` tier displays `Max (tier unknown)` with no inferred
price. No new OAuth aliases or verification fields are assumed.

Neither those official pages nor a sanitized account payload supplied for
[#292](https://github.com/hemsoft-dev/codexbar/issues/292) establishes a wire field
that authoritatively confirms verification or no session limit. Therefore
CodexBar never displays "No 5-hour limit" from a tier, null, absent or malformed
session field. Weekly-only data remains usable, with an unknown/unavailable
session explanation. A valid structured session takes precedence over flat null.
Finite sessions remain visible for every tier, including Ultra.

The existing ten-minute cache and 403/429 backoff remain unchanged. A successful
refresh replaces metrics and messages, removing obsolete session values and
restoring them when reported again. Failed refreshes retain the dashboard's
last-good stale snapshot. Messages persist in the existing snapshot format;
older snapshots with no session also display an unknown session in the wide table.
Focused cards and widgets use actual metrics and fall back when a saved session
metric disappears. They never synthesize a session reset or zero usage.

## Native evidence

Build on Windows with `cargo build -p codexbar-app --locked --features capture`.
Set `CODEXBAR_CAPTURE_DIR` to a fresh evidence folder,
`CODEXBAR_SETTINGS_DIR` to its `settings` subfolder, and
`CODEXBAR_CAPTURE_CLAUDE_ULTRA=1`, then run the binary with `--demo`.
Both directory overrides are required by the fixture isolation assertion.
Repeat with `CODEXBAR_CAPTURE_WIDE=1` to request a 2600x1000 viewport.
On a smaller high-DPI display, put `{"zoomLevel":0.6}` in the isolated
`settings/settings.json` before the wide run. This uses the app's normal zoom,
without changing Windows settings. The Claude capture asserts the actual wide
table is selected, and the native session columns expose their semantic labels;
a large window request alone cannot qualify as wide evidence.
Each run exercises finite Ultra, weekly-only Ultra, restored finite Ultra, Max,
and malformed session states in light and dark themes through production parsing.
The capture asserts semantic labels and saved session-card fallback, then saves
native rendered screenshots and `report.json`. Real credentials are never read.
