# Gemini

A Gemini account (#106) shows the consumer **Gemini Apps** limits from
`gemini.google.com/usage`: the rolling five-hour and the weekly limit, with
Gemini's own reset times. It is not Gemini API, Gemini CLI or Code Assist quota;
coding quotas are [#107](https://github.com/hemsoft-dev/codexbar/issues/107).
The integration is **experimental** and off until a Gemini account is added.

## Signing in

Gemini Apps has no OAuth grant or CLI that can read these limits for another
program, so the sign-in uses a website session, as in the iOS app:

1. Add a Gemini account in Settings > Accounts (method Browser session).
   CodexBar opens `gemini.google.com/usage` in Microsoft Edge or Google Chrome
   on a **new, empty, temporary profile** in the settings folder
   (`gemini\browser-…`). Your own browser profiles and their cookies are never
   read.
2. Sign in to Google there yourself, with the account whose usage to show.
   Passwords, MFA and Google's checks happen entirely in the browser.
3. CodexBar asks the browser for its cookies through the browser's DevTools
   protocol (`Storage.getCookies`) every two seconds. It never reads page
   contents or passwords and runs no script in the page.
4. Only secure, unexpired `.google.com` cookies on the root path named
   `__Secure-1PSID` and the optional `__Secure-1PSIDTS` count. Host-only,
   narrower-path, insecure and other cookies are ignored; two different values
   for one name fail. With them, CodexBar reads both Gemini Apps limits; only a
   session that reads both is kept. A session Gemini rejects waits for new
   cookies; a network or server failure, or a single limit, is tried again
   after a wait that grows from five seconds to a minute.
5. The two values are saved in Windows Credential Manager under the account
   (`CodexBar:account:<id>:gemini-apps`), replacing any earlier ones in one
   write. The browser is closed and its profile deleted. Cancel, a closed window
   or a timeout (10 minutes) saves nothing and deletes the profile too; folders
   an interrupted sign-in left behind are deleted at the next start.

Sign out deletes CodexBar's copy. It doesn't sign you out of Google.

Gemini's session cookies say nothing CodexBar can check about whose they are.
Signing an account in again asks for the same Google account, since another
account's usage would continue this one's history; add another Gemini account
for a different Google account. Gemini accounts always use this browser
sign-in, whatever method was chosen. A temporary profile the browser kept
locked is deleted again when the sign-in ends, and a failure to delete it is
shown in Settings.

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

## Security notes and limits

- The two cookies are Google session credentials, not a Gemini-only key: they can
  reach more of the Google account. They are kept only in Credential Manager,
  sent only to `gemini.google.com`, and never logged or shown.
- While a sign-in runs, the browser's DevTools port accepts connections from
  this PC, so another program running as you could read that temporary
  profile's cookies too. The browser runs in a kill-on-close Windows job and
  the profile is deleted when the attempt ends.
- Edge signs a new profile in to the Windows Microsoft account by itself; those
  Microsoft cookies stay in the temporary profile and are deleted with it.
- This is Gemini's private web contract, not a published API: the RPC id and
  page tokens can change without notice, and the integration then fails with a
  "format changed" message rather than guessing.

## Not verified here

The browser mechanism was checked on a hidden desktop (Edge started on a fresh
profile, DevTools answered, cookies were listed, the browser closed and the
profile was deleted). A real Google sign-in, and a comparison of both limits and
resets with the same account's Usage page, are left to the account owner, along
with a review of Google's terms before release.
