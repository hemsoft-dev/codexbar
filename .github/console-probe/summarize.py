"""Throwaway: per-phase console/window verdicts from ConsoleWatch's windows.jsonl and the 4688 process log.

A window counts as a visible console window when a hook SHOW event or the poll's first sighting finds it on screen
(visible, not cloaked, not minimized, non-empty) and it has a console class or belongs to a console-capable process.
Usage: python -I summarize.py <probe-out> -> writes <probe-out>/summary.json and prints a Markdown table.
"""

import json
import pathlib
import sys

CONSOLE_PROCS = {"gh", "conhost", "OpenConsole", "WindowsTerminal", "cmd", "powershell", "pwsh", "git", "node", "agy"}


def load(path):
    with open(path, encoding="utf-8-sig") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def summarize(events):
    phases = {}
    for event in events:
        if event.get("source") == "phase":
            phases.setdefault(event["phase"], {"consoleWindows": {}, "visibleConsoleWindows": {}})
    for event in events:
        if "class" not in event:
            continue
        entry = phases.setdefault(event["phase"], {"consoleWindows": {}, "visibleConsoleWindows": {}})
        brief = {k: event[k] for k in ("hwnd", "class", "title", "pid", "process", "source", "t", "rect", "visible")}
        if event["consoleClass"]:
            entry["consoleWindows"].setdefault(event["hwnd"], brief)
        consoleish = event["consoleClass"] or event["process"] in CONSOLE_PROCS
        if consoleish and event["onScreen"] and event["source"] in ("hook-show", "poll-new-visible"):
            entry["visibleConsoleWindows"].setdefault(event["hwnd"], brief)
    return {
        name: {"consoleWindows": list(p["consoleWindows"].values()), "visibleConsoleWindows": list(p["visibleConsoleWindows"].values())}
        for name, p in phases.items()
    }


def main():
    out = pathlib.Path(sys.argv[1])
    summary = {"labels": {}}
    rows = ["| Run | Phase | Console-class windows seen | Visible on screen |", "| --- | --- | --- | --- |"]
    for windows in sorted(out.glob("*/windows.jsonl")):
        label = windows.parent.name
        events = load(windows)
        phases = summarize(events)
        result_file = windows.parent / "result.json"
        result = json.loads(result_file.read_text(encoding="utf-8-sig")) if result_file.exists() else {}
        summary["labels"][label] = {
            "hookInstalled": any(e.get("source") == "hook" and e.get("installed") for e in events),
            "result": result,
            "phases": phases,
        }
        for name, phase in phases.items():
            visible = ", ".join(f"{w['class']} ({w['process']}, '{w['title'][:40]}')" for w in phase["visibleConsoleWindows"]) or "none"
            rows.append(f"| {label} | {name} | {len(phase['consoleWindows'])} | {visible} |")
    processes = out / "processes.json"
    if processes.exists():
        records = json.loads(processes.read_text(encoding="utf-8-sig") or "[]")
        records = records if isinstance(records, list) else [records]
        summary["processStarts"] = {
            "gh": sum(1 for r in records if r["image"].lower().endswith("\\gh.exe")),
            "agy": sum(1 for r in records if r["image"].lower().endswith("\\agy.exe")),
            "conhost": sum(1 for r in records if r["image"].lower().endswith("\\conhost.exe")),
            "openConsoleHandoffs": sum(1 for r in records if r["image"].lower().endswith("\\openconsole.exe")),
        }
    (out / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print("\n".join(rows))
    if "processStarts" in summary:
        print(f"\nProcess starts: {summary['processStarts']}")


if __name__ == "__main__":
    main()
