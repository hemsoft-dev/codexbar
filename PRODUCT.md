# Product

<!-- impeccable:product-schema 1 -->

## Platform

windows-desktop

Native Windows app, not web, iOS or Android. The UI is being rewritten in Rust with GPUI and gpui-kit (formerly
gpui-component). HTML mockups are only a design medium; every surface must be buildable from gpui-kit primitives and
GPUI's own drawing.

## Stack

Rust workspace in this repository: a provider and refresh core, auth with Windows Credential Manager, a SQLite history
store, and a GPUI + gpui-kit app. It replaces the current C#/WPF/.NET 9 app, which keeps shipping until the Rust app
reaches parity. Decided by Franz on 2026-10-06.

## Users

Power users like the maintainer: developers paying for several AI subscriptions and accounts at once (ChatGPT/Codex,
Claude, GitHub Copilot, Cursor, OpenRouter, OpenCode Go/Zen, Moonshot, and coming: Gemini, Grok, Greptile, GitHub
Billing). They check limits many times a day while working and need to know whether they can keep using a tool or should
switch to another one. Optimize for density, speed and repeat use, not first-time hand-holding.

## Product Purpose

Keep every AI provider limit, reset time, credit balance and spend visible in one place on Windows, so a heavy user is
never surprised by a lockout or a bill. Success: one glance tells you what is close to a limit, when it resets, and where
the money is going.

## Positioning

The Windows counterpart of steipete/CodexBar (macOS) and HemSoft's codexbar-ios. It reads usage from the credentials
each provider's own CLI or account already has, keeps everything on the device, and covers many accounts per provider,
so it shows your whole AI usage across tools, not one vendor's view.

## Operating Context

- A tray icon is always present. Clicking it brings a **borderless window showing the full dashboard** to the
  foreground. This replaces the small flyout.
- There are **multiple dashboard views**. A tray click restores whichever view the user last had open; it never resets
  to a default view.
- The dashboard sits next to editors, terminals and AI coding agents. It is opened briefly and often, read, then
  dismissed.
- Refresh runs in the background on an interval (1, 2, 5 or 15 minutes) and on demand.

## Capabilities and Constraints

- Providers report different metric shapes: rolling windows (5-hour, weekly), monthly quotas, credit balances,
  monetary spend and extra-usage money, plus per-model or per-product breakdowns. Some metrics are experimental or
  unverified and must be labeled as such.
- Multiple accounts per provider, account groups (#89), urgency ordering (#90), bounded history and charts (#85, #86),
  threshold alerts with Windows notifications (#87, #88), and a Settings experience (#91) are in scope.
- A failed refresh keeps the last good snapshot and shows that it is stale. Never show false zeroes or guessed quotas.
- Times and numbers use the user's own timezone and locale (#84).
- Light and dark themes with provider branding and accessibility (#92).
- Parity backlog: issue #70.

## Brand Commitments

- Name: CodexBar.
- Brand colors: HemSoft gold on black, from the HemSoft site tokens (`D:\github\HemSoft\www\src\app\globals.css`):
  gold `#D4AF37`, dark gold `#B8860B`, deep gold `#8B6914`, black `#0A0A0A`, cards `#121212`, raised `#1A1A1A`,
  borders `#1F1F1F`, text `#F5F5F5`, muted text `#A0A0A0`, destructive `#DC2626`. Not the cyan/navy of the app icons.
- Explicit inspiration from Franz: the gpui-kit gallery's Chart page (<https://gpui-kit.com/gallery/> → Chart). Each
  card has a title, a period, the chart, a one-line takeaway ("Trending up by 6.6% this month") and a quiet caption.

## Evidence on Hand

- Real provider data shapes come from `src/CodexBar.Core/Providers/*` and the codexbar-ios providers
  (`D:\github\HemSoft\codexbar-ios\CodexBarIOS\Services`).
- There are no real screenshots of live accounts for design work. Mockups use synthetic, labeled data. Never invent
  provider prices, plan names or quota sizes as facts.

## Product Principles

1. Urgency first: whatever will block the user soonest is read first.
2. Quiet until it matters: healthy usage stays calm, and color and motion appear only when attention is needed.
3. Truthful numbers: stale, experimental and unknown are visible states, never hidden by a confident display.
4. Instant: the window shows immediately with the last snapshot, and fresh data streams in afterwards.
5. Many accounts, one picture: providers and accounts are comparable side by side.

## Accessibility & Inclusion

Keyboard navigation for every view, and screen-reader names through AccessKit (gpui-kit). Severity is never conveyed by
color alone. Respect the Windows light/dark setting and reduce-motion.
