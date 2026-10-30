# Gemini

A Gemini account (#106) shows the consumer **Gemini Apps** limits from
`gemini.google.com/usage`: the rolling five-hour and the weekly limit, with
Gemini's own reset times. It is not Gemini API, Gemini CLI or Code Assist quota;
it can also show the Gemini coding quotas (#107, below). The integration is
**experimental** and off until a Gemini account is added.

## Connect Antigravity

In Settings > Accounts, add or select a Gemini account and choose **Connect
Antigravity**. CodexBar checks the existing local Antigravity session through
its version-gated `/usage` command. A successful response enables **Coding
quotas** for that account without opening a sign-in window.

If that check fails, the dialog explains the failure. **Open Antigravity
sign-in** starts the installed `agy` in its own terminal, with no prompt or
model turn. Complete the CLI's normal sign-in in your default system browser,
then choose **Check connection**. A terminal opening or closing is never taken
as proof of authentication: the usage check must succeed. Missing or old CLI
versions require installation or an update first.

Antigravity owns the browser consent, OAuth client, redirects, credentials and
renewal. Its [documented authentication flow](https://www.antigravity.google/docs/install?tab=cli)
reuses the OS secure keyring shared by its app, CLI and IDE integrations; when
needed, its local CLI opens the default browser. CodexBar neither borrows an
OAuth client nor reads or copies the CLI's tokens. The local handoff removes
inherited SSH detection variables so the vendor does not select its separate
remote URL/code workflow.

The dialog times out after ten minutes. Cancel, timeout and successful
verification close only CodexBar's owned CLI process, not the user's browser.
Credentials Antigravity has already saved remain its own. **Disconnect coding
quotas** disables this account's feed without signing the shared CLI out.
One Gemini account can use that shared sign-in. Connect the same Google
account whose history belongs here: CodexBar cannot verify the CLI's Google
identity, and changing it can mix usage in an existing account's history.

## Previously saved Gemini Apps sessions

Gemini Apps website usage and Antigravity coding quotas are separate. Existing
Gemini Apps sessions remain in the OS credential store and keep working until
the website rejects or expires them. Account IDs, labels and history are kept.
**Forget saved Gemini Apps session** removes only CodexBar's saved copy; removing
the CodexBar account also forgets its copy, without signing Google out.

New Gemini Apps website sign-in is unavailable in CodexBar. The former
controlled Edge/Chrome cookie flow is retired because Google can reject that
browser. An Antigravity OAuth token cannot provide Google website cookies,
and there is no supported native OAuth source for these website limits.
CodexBar does not use an embedded browser, CDP, cookie extraction, user-agent
spoofing or a browser-policy bypass for Google sign-in. Profiles left by the
retired flow are cleaned at startup; user browser profiles are untouched.

## Reading usage

CodexBar reads `gemini.google.com/usage` for its page tokens (`SNlM0e`, `cfb2h`,
`FdrFJe`), then posts Gemini's usage RPC (`jSf9Qc`) to
`gemini.google.com/_/BardChatUi/data/batchexecute`. Each limit is
`[limit, fractionUsed, period, [[resetSeconds, …]]]`; period 1 is the five-hour
limit and 2 the weekly one. A limit Gemini doesn't return stays missing and
says so, never zero. A redirect, a signed-out page or Gemini's rejection (code 7)
asks for a new sign-in. A rate limit waits as long as Google's `Retry-After`
asks, up to an hour. A saved session Credential Manager can't read is reported
as such, not as signed out. Redirects are never followed, so the cookies go nowhere
but `gemini.google.com`.

## Coding quotas (#107)

The coding quotas are the Antigravity daily quota summary: the **Gemini Models**
and **Other models** (Claude and GPT) groups, each with a five-hour and a weekly
limit, plus a separate **Argon weekly** bucket. CodexBar reads them through Google's own Antigravity CLI,
`agy -p "/usage" --output-format json`, a documented script interface that runs
no model turn. The CLI keeps its own Google sign-in and renews it, so CodexBar
borrows no OAuth client and stores no token.

- CodexBar first runs `agy --version` and requires 1.4.10 or newer for the
  qualified background auth path. `/usage` has run no model turn since 1.1.11.
  Detection keeps the desktop credential store, removes inherited SSH mode,
  and on Windows denies child-process creation so agy cannot launch a browser.
  See [the diagnosis and release handoff](ANTIGRAVITY_AUTH_289.md).
- Use **Connect Antigravity** in Settings to verify the CLI session, or select
  *From the Antigravity CLI* explicitly for an already configured session.
  Choose it only for the Google account `agy` is signed in to: CodexBar cannot
  check, and matching labels prove nothing.
- The CLI keeps one sign-in for the Windows user (in Windows' credential store,
  not its home folder), so one Gemini account can show coding quotas.
- Buckets map by id and window, never by display name: `gemini-5h`,
  `gemini-weekly`, `3p-5h` and `3p-weekly` become **Gemini Models five-hour**,
  **Gemini Models weekly**, **Other models five-hour** and **Other models
  weekly**. Percent used is `1 − remaining_fraction`, and resets come only from
  `reset_time`. `gemini-argon-wtu-weekly` maps independently to **Argon weekly**;
  third-party buckets never substitute for Gemini or Argon. Weekly windows show
  [SUS](SUS.md) when enough forecast data exists.
- A missing, duplicate, disabled, malformed or expired bucket stays unavailable
  with a note, and the valid ones still show. With no usable bucket and no Gemini Apps
  limits, the account says so instead of asking for a sign-in.
- Gemini Apps and coding quotas are separate connections: either works without
  the other, one failing never hides the other, and signing Gemini Apps out
  leaves the coding quotas alone.

## Security notes and limits

- The two cookies are Google session credentials, not a Gemini-only key: they can
  reach more of the Google account. They are kept only in Credential Manager,
  sent only to `gemini.google.com`, and never logged or shown.
- New authentication runs through the vendor CLI and normal browser. On Windows
  only the owned CLI root is terminated; a kill-on-close job would also capture
  a newly started default browser, so it is deliberately not used for this handoff.
- This is Gemini's private web contract, not a published API: the RPC id and
  page tokens can change without notice, and the integration then fails with a
  "format changed" message rather than guessing.

## Qualification

The previous coding adapter was read live through `agy` 1.3.3 on
2026-10-12: four buckets and `num_turns` 0. That historical result does not
qualify the new connection UI. See [Windows connection checks](ANTIGRAVITY_LOGIN_TESTING.md)
for current-head evidence and the explicit manual Google-consent boundary.

The SUS implementation also checked the structured producer shape with CLI 1.4.6
on October 23, 2026, including the separate Argon bucket and zero model turns.
The checked-in response fixture uses synthetic fractions and reset instants.
