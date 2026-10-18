---
version: 1
slug: "crates-codexbar-app-src-dashboard-rs"
primary_target: "crates/codexbar-app/src/dashboard.rs"
related_targets: []
---

## Scope

CodexBar for Windows, Usage dashboard view (native GPUI + gpui-kit window, borderless, restored by tray click). Mode: Operate.

## Audience and task

Power users who juggle several AI subscriptions and accounts. The task is to see in one glance which limit blocks them first, when it resets, and the pace behind it; then drill into one account. They open the window many times a day and briefly.

## Direction contract

THESIS: An editor-native, keyboard-first pane shaped like the gpui-kit Chart gallery. It refuses the category default of a grid of equal KPI tiles, and instead ranks every account by urgency in one dense table with Chart-gallery cards for the focused account.
OWN-WORLD: HemSoft gold on black: black ground #0A0A0A, cards #121212, hairlines #1F1F1F, data in the gold family (#D4AF37, #B8860B, #8B6914), orange #F97316 and red #DC2626 for status, always with a text tag. Text is Segoe UI Variable. Cards follow the Chart-gallery anatomy: title, muted period, chart, a semibold takeaway, and a muted caption.
STORY: The visitor sees the red "Limit soon" row first, reads when it resets and how fast it is burning, selects any row to inspect its curve, pool and activity, and leaves knowing which tool to switch to.
FIRST VIEWPORT: A slim title strip holds the wordmark, the Usage/Spend/History tabs, the update age and a refresh button. A 216px rail holds search (Ctrl+K) and the views. The top of the main column is an eight-row urgency table (Account with status tag, Limit, Used, %, Progress, Resets, 14-day trend). Below it, a focus panel with the account heading and three equal Chart cards: the 5-hour window area chart (this window vs the previous one), the weekly-window donut, and requests-by-hour bars. A status line sits at the bottom.
FORM: Editor-Native (pick card) restyled toward the gpui-kit Chart page, layout A "Table over chart cards"; candidate 1 of 7 on the ordered list; seed key 387738d1. Approved comp: .impeccable/mocks/comp-a.png.
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Deviations from the comp, by rule

- Selection is a fill, not a cyan leading-edge bar (gpui-kit Design Guides forbid edge-bar selection markers).
- The logo is a placeholder icon until the real app icon ships as an asset.
- The trend column shows 14 days (the data the model holds); the comp showed 7.

## Unresolved

Spend and History views are placeholders (issues #85, #86). Real provider data, tray integration and settings follow in later phases.
