# Windows Antigravity connection qualification

The Google error that prompted #254 was reported by the account owner.
Automated tests reproduce the old Settings action invoking the Google-cookie
browser; they do not reproduce Google's exact error code.

## Repeatable manual check

Use a normal local Windows desktop session and an account approved for testing.
Do not sign out an existing real user or delete their credentials to force a
first-login case. The CLI's SSH URL/code flow is different and does not prove
that local default-browser consent works.

1. Install a supported `agy` (at least 1.1.11), and confirm `agy --version`.
   Leave normal browser windows open to check that they survive the flow.
2. Run the candidate CodexBar build with an isolated `CODEXBAR_SETTINGS_DIR`.
   In Settings > Accounts, add a Gemini account with a test label. Choose
   **Connect Antigravity** for that row.
3. With an existing approved Antigravity session, expect the usage check to
   enable coding quotas without opening an interactive sign-in window. Confirm
   that the account's label and ID persist after restart and usage refreshes.
4. For new consent, use a separate Windows test user without Antigravity
   credentials. After the initial check reports that sign-in is needed, choose
   **Open Antigravity sign-in**. Expect the ordinary `agy` terminal and the
   user's default browser. There must be no disposable browser profile,
   embedded browser, debugging port, cookie extraction or browser-policy bypass.
5. Complete consent as the intended Google account. Do not type a model
   prompt. Choose **Check connection** in CodexBar. Expect coding quotas to
   become enabled only after the `/usage` response succeeds. Merely opening or
   closing the terminal must not count as success.
6. Cancel before consent completes. Expect CodexBar's CLI to close and the
   account to remain disconnected. Normal browser windows and previously saved
   Gemini Apps sessions must remain. If Antigravity already saved credentials,
   those remain vendor-owned and may be reused on the next attempt.
7. Leave a connection dialog open for ten minutes. Expect a timeout, the owned
   CLI to close and no new connection setting to be saved. Close the dialog
   and verify another attempt is possible.
8. Close the CLI before finishing, decline consent, or interrupt connectivity;
   choose **Check connection**. Expect an actionable failure, with no false
   connected state. With a missing or old CLI, expect installation/update
   guidance and no model prompt.
9. **Disconnect coding quotas** must leave the shared vendor sign-in and
   saved Gemini Apps session alone. **Forget saved Gemini Apps session** must
   remove only CodexBar's saved website session. Existing account history and
   labels must survive both actions.

Capture the Settings entry point, CLI handoff and verified connected state,
plus cancellation and a failure state. Keep OAuth parameters, authorization
codes, tokens, email addresses and unrelated windows out of captures. Publish
only current-head images in the PR; a synthetic handoff must be labelled as
such and is not proof of real Google consent.

## Automated and live evidence

Current-head validation receipts and screenshots belong in the pull request's
Validation section. UI tests exercise the real Settings handlers with a fake
CLI service; they cover reuse, explicit handoff/check, cancellation, timeout,
failures and persistence. Provider tests check a prompt-free native launch and
owned-process cleanup. The existing version gate protects `/usage` from older
CLI versions that could send it to a model.

Real fresh-account Google consent has not yet been performed for this change.
Document that boundary even if existing-session usage succeeds on Windows.
CodexBar delegates OAuth to the vendor; its tests do not qualify the vendor's
internal PKCE, state or callback implementation.
