# iOS provider parity backlog refresh

Checked September 30, 2026 EDT. The existing [parity tracker](https://github.com/HemSoft/codexbar/issues/70) already had 26 open implementation issues. Keep those tickets; do not recreate their account, credential, refresh, history, alert, settings, or widget work.

## Baseline and scope

Compare [iOS commit 8ebadd7](https://github.com/HemSoft/codexbar-ios/tree/8ebadd77becafb53342ef185202c2162ffa84bd7) with [Windows commit 5563221](https://github.com/HemSoft/codexbar/tree/556322132cc60a38e061a329567c18ae84d30f20). The [iOS README](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/README.md) describes repository behavior, not a guarantee that every integration is in the current App Store release or verified against a live account.

The [Windows provider enum](https://github.com/HemSoft/codexbar/blob/556322132cc60a38e061a329567c18ae84d30f20/src/CodexBar.Core/Models/ProviderId.cs) and [provider registrations](https://github.com/HemSoft/codexbar/blob/556322132cc60a38e061a329567c18ae84d30f20/src/CodexBar.App/App.xaml.cs) contain eight providers and no Gemini, Greptile, Grok, or separate GitHub Billing adapter. The full open and closed Windows issue list was checked before creating the following tickets.

This refresh covers provider gaps. A delegated dashboard/settings comparison failed before starting with the runtime error `scopedModelIdsFromContext is not a function`; no child edits or research results were produced. Recent card-customization and other UI changes have not been exhaustively compared here.

## Additional issues

| Gap | Evidence | Issue | Priority |
| --- | --- | --- | --- |
| OpenCode browser approval, verified user/workspace, token renewal, and independent Console Go/Zen results replace routine copied-cookie setup | [iOS sign-in contract](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/OPENCODE-SIGN-IN.md), [Windows Go](https://github.com/HemSoft/codexbar/blob/556322132cc60a38e061a329567c18ae84d30f20/src/CodexBar.Core/Providers/OpenCodeGo/OpenCodeGoProvider.cs), [Windows Zen](https://github.com/HemSoft/codexbar/blob/556322132cc60a38e061a329567c18ae84d30f20/src/CodexBar.Core/Providers/OpenCodeZen/OpenCodeZenProvider.cs) | [#102](https://github.com/HemSoft/codexbar/issues/102) | P1 |
| Greptile read-only organization review activity, without invented billing allowances | [iOS provider](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/CodexBarIOS/Services/GreptileUsageProvider.swift) | [#103](https://github.com/HemSoft/codexbar/issues/103) | P1 |
| Separate GitHub Billing accounts, personal/organization owner selection, monthly monetary totals, and product detail | [iOS billing scope](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/README.md#github-billing), [provider](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/CodexBarIOS/Services/GitHubBillingUsageProvider.swift) | [#104](https://github.com/HemSoft/codexbar/issues/104) | P1 |
| Verified GitHub included-product allowances and separately scoped budgets | [iOS billing parser](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/CodexBarIOS/Services/GitHubBillingUsageParser.swift), [GitHub included products](https://docs.github.com/en/billing/reference/product-usage-included) | [#105](https://github.com/HemSoft/codexbar/issues/105) | P2 |
| Experimental Gemini Apps consumer five-hour and weekly meters through isolated website sign-in, not CLI quotas | [iOS website-session evidence and limitations](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/GEMINI-SIGN-IN.md) | [#106](https://github.com/HemSoft/codexbar/issues/106) | P2 |
| Four experimental Gemini coding quotas as a separate OAuth connection within the same six-metric Gemini account | [iOS coding contract and live-check boundaries](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/ANTIGRAVITY-SETUP.md) | [#107](https://github.com/HemSoft/codexbar/issues/107) | P2 |
| Experimental Grok consumer device approval, verified shared weekly pool, and separate Extra Usage Credits | [iOS consumer contract and live-check boundaries](https://github.com/HemSoft/codexbar-ios/blob/8ebadd77becafb53342ef185202c2162ffa84bd7/GROK-CONSUMER-CONTRACT.md) | [#108](https://github.com/HemSoft/codexbar/issues/108) | P2 |
| X Subscription account: weekly SuperGrok and Grok Bot limits and Extra Usage Credits, each once | Tracked on both: [codexbar-ios#458](https://github.com/hemsoft-dev/codexbar-ios/issues/458) | [#172](https://github.com/hemsoft-dev/codexbar/issues/172) | P2 |

P1 and P2 describe delivery order, not severity. Each new issue specifies prerequisites, acceptance criteria, isolated tests, live-verification limits, and repository quality gates. All seven have `no-agent` while they await prerequisites and provider decisions. No implementation was dispatched.

## Order of work

Start [removing billable Claude probes](https://github.com/HemSoft/codexbar/issues/71), [error/log redaction](https://github.com/HemSoft/codexbar/issues/72), and [account configuration](https://github.com/HemSoft/codexbar/issues/73). Then deliver [secure storage](https://github.com/HemSoft/codexbar/issues/74), [rich results](https://github.com/HemSoft/codexbar/issues/75), [refresh reliability](https://github.com/HemSoft/codexbar/issues/76), [shared auth](https://github.com/HemSoft/codexbar/issues/77), and [Settings](https://github.com/HemSoft/codexbar/issues/91) according to their dependencies.

The provider tickets reuse those foundations. They do not authorize broader OAuth scopes, reuse of an iOS client registration, access to another app's cookies, provider purchases, or billable inference. Browser device grants differ from authorization-code PKCE; the shared auth design must support the protocol the provider actually supplies.

OpenCode Go is no longer a Windows-only capability. Current iOS supports Go and Zen together. Keep existing Windows support and migration compatibility without perpetuating that old platform distinction.

Windows-native packaging, notifications, and widgets remain in the existing backlog. watchOS binaries, App Store mechanics, and SwiftUI-specific code are not portable requirements. A new cross-device companion needs its own product decision.

## Validation

The new issue bodies and this report pass the repository Markdown rules. GitHub receipts verify seven distinct new issues and the tracker contains all 33 implementation links. Provider source paths were checked against the immutable upstream tree; the cited GitHub billing documentation returned HTTP 200.

All repository changes, including documentation-only updates, require build, format, tests, coverage, and package vulnerability checks before merge. The pull request publishing this report records those results.
