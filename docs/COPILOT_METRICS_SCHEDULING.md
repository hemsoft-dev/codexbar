# Copilot metrics scheduling

The collector's direct CLI remains `scripts/copilot-metrics.ps1`. Scheduled
refreshes need a separate startup contract because a console application can
allocate a window before its script runs.

## Startup contract

`Prepare-CopilotMetricsTask.ps1` prepares a Windows GUI-subsystem task host and
a disabled task definition. It never registers, starts or enables a task.
The generated host has no UI, accepts no arguments and starts only the validated
Windows PowerShell collector action. Its `ProcessStartInfo` sets
`UseShellExecute=false`, `CreateNoWindow=true` and redirects all three streams.
It closes stdin immediately, drains stdout and stderr concurrently and returns
the collector's exit code. Scheduled console output is discarded. Direct CLI
output is unchanged.

The collector uses the same flags for its only native child, `gh auth token`.
It resolves a native executable instead of a function, alias or shell wrapper,
closes stdin, sets `GH_PROMPT_DISABLED=1` and captures both output streams.
There is no browser, login, `cmd`, `pwsh`, shell execution or credential change
in this launch chain. A named-account failure remains a failure and never
silently selects another account. Token precedence stays `-Token`,
`EnterpriseBillingToken`, `GH_TOKEN`, `GITHUB_TOKEN`, then the existing named
or active GitHub CLI account. No token is logged.

Microsoft documents the [GUI and console subsystem allocation rules](https://learn.microsoft.com/en-us/windows/console/console-allocation-policy)
and [`CreateNoWindow` requirements](https://learn.microsoft.com/en-us/dotnet/api/system.diagnostics.processstartinfo.createnowindow).
The task's `Hidden` setting and PowerShell's `-WindowStyle Hidden` are not this
contract. The generated entry point is a GUI-subsystem executable, so it needs
no console parent before it can apply the child's creation flags.

## Prepare for review without deployment

Use Windows PowerShell 5.1 and its .NET Framework `csc.exe`. Preparation starts
the compiler directly with the same no-console flags and captured streams. It
does not use the implicit compiler subprocess launched by `Add-Type`, or build
the Rust or retired C# product. Compiler response-file defaults and compiler
error-reporting UI are disabled. The generated entry point uses `/target:winexe`.

```powershell
.\scripts\Prepare-CopilotMetricsTask.ps1 `
  -TaskXmlPath C:\private\captured-task.disabled.xml `
  -CollectorPath D:\github\HemSoft\codexbar\scripts\copilot-metrics.ps1 `
  -OutputDirectory C:\private\new-metrics-task-candidate
```

The input must already be disabled and contain exactly one native Windows
PowerShell `-File` action for that collector. Interactive switches, arbitrary
commands, other script targets, shell/browser actions and literal tokens are
rejected. The output directory must be new. Preparation preserves the original
working directory, collector arguments, principal, triggers and settings, and
adds `-NoLogo -NonInteractive` to the PowerShell invocation. It emits hashes for
review and leaves the original XML unchanged. Task security descriptors and
installed source ownership/ACLs are not modified.

The candidate points to the supplied collector and its sibling modules. Before
any separately authorized deployment, the coordinator must verify the intended
source/module hashes, stable artifact location, owner/ACLs and disabled task
XML against the captured original. Preparation is not installation. Do not
register the candidate, launch the installed interactive task or enable a
trigger as a validation shortcut.

## Tests

```powershell
.\scripts\tests\CopilotMetrics.Core.Tests.ps1
```

The existing CI PowerShell step runs the portable process/task contract tests
and collector replay. They compile the actual host source as an in-process
library and inspect its `ProcessStartInfo` without invoking its entry point.
They check compiler creation flags and reject unsafe action definitions without
executing them. Synthetic HTTP and credential fixtures verify precedence,
monthly writes and read-only saved-input behavior. No real `gh` is reachable
during collector replay.

Native runtime probes are **opt-in**. CI's Windows job sets
`CODEXBAR_RUN_NATIVE_METRICS_FIXTURES=1` on its disposable hosted runner, which
enables the Windows suite there. Anywhere else it needs separate coordinator
authorization. It compiles a
synthetic native `gh` fixture, inspects the prepared binary's GUI subsystem and
creation flags, and exercises the host to PowerShell to `gh` chain. It checks
console handles, closed stdin, concurrent pipe draining, exact account argv,
failed-auth data preservation and exit propagation. It uses no provider requests
or credentials. The dedicated suite also requires `-RunNativeFixtures`, which the
process suite passes when the variable is set.

On Home, runtime probes remain held pending coordinator review of the exact
candidate and human out-of-game observation. A session-0 guard alone does not
authorize execution. Any later authorized Home validation must additionally set
`CODEXBAR_TEST_REQUIRE_SESSION_ZERO=1` and use a private, uniquely staged directory.
Never invoke the installed interactive task. Keep production triggers disabled.

### Current validation limit

The earlier session-0 test attempt staged at 20:44:02.640 EDT on October 24,
2026 exceeded its 60-second SSH transport timeout. No further Home calls were
made after that failure. Its remote completion and process state are unknown.
The later source changes have only local contract/replay validation. Native PE
and runtime evidence, including any compiler descendants, remain pending and
must not be described as passed.

## Incident evidence and limits

[CodexBar issue 269](https://github.com/hemsoft-dev/codexbar/issues/269) tracks the
source repair for [Chief issue 43](https://github.com/hemsoft-dev/chief/issues/43).
The captured task launched Windows PowerShell directly with an InteractiveToken
principal, `-Refresh`, a named GitHub account and an explicit data directory.
The captured XML has no author or creation date. No task installer was found in
repository history, so the original installer provenance is unestablished.
The preparation script supplies a versioned replacement preparation path.

The installed collector and core module match the pre-fix repository source
when LF line endings are converted to CRLF. This establishes the source of the
unsafe launch chain. It does not attribute a particular game crash to it.
The original configuration and security backups remain coordinator-owned;
this change does not alter them or authorize re-enabling the task.
