# Throwaway Windows-native probe for chief#43 / codexbar#271 (never merged). Runs one real CodexBar build with a
# GitHub-CLI Copilot account through several background refresh cycles while ConsoleWatch records every window that
# is created or shown, then optionally runs a creation-flag matrix for `gh` from a GUI-subsystem launcher.
# Windows PowerShell 5.1: its C# compiler builds Watch.cs and the launcher with System.Drawing available.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Label,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [int]$AppSeconds = 200,
    [switch]$Matrix
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$OutDir = (New-Item -ItemType Directory -Force (Join-Path $OutDir $Label)).FullName
if (-not ('ConsoleWatch' -as [type])) {
    Add-Type -Path (Join-Path $PSScriptRoot 'Watch.cs') -ReferencedAssemblies System.Drawing, System.Windows.Forms
}
$launch = Join-Path ([IO.Path]::GetTempPath()) 'console-probe-launch.exe'
if ($Matrix -and -not (Test-Path $launch)) {
    Add-Type -Path (Join-Path $PSScriptRoot 'Launch.cs') -OutputAssembly $launch -OutputType WindowsApplication
}

function Get-Subsystem([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    switch ([BitConverter]::ToUInt16($bytes, $pe + 24 + 68)) { 2 { 'windows-gui' } 3 { 'windows-console' } default { 'other' } }
}

$result = [ordered]@{
    label = $Label; exe = $Exe; exeSha256 = (Get-FileHash $Exe -Algorithm SHA256).Hash.ToLowerInvariant()
    exeSubsystem = Get-Subsystem $Exe; startedUtc = [DateTime]::UtcNow.ToString('o')
    gh = (Get-Command gh).Source; ghVersion = ((& gh --version) -join ' ')
}
$settings = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) "console-probe-settings-$Label")
# One Copilot account on the GitHub CLI's sign-in with no username: CodexBar asks `gh auth status --json hosts` for
# every account, then `gh auth token --user` for each, on every refresh. One-minute refresh interval.
@'
{"accountConfigurationVersion":1,"refreshIntervalSeconds":60,"accounts":[
 {"id":"probe-copilot","providerId":"Copilot","displayLabel":"Probe","enabled":true,"authenticationMethod":"CommandLine"}]}
'@ | Set-Content -Path (Join-Path $settings 'settings.json') -Encoding ascii
$history = Join-Path $env:USERPROFILE '.codexbar\history.jsonl'
$historyBefore = if (Test-Path $history) { @(Get-Content $history).Count } else { 0 }

[ConsoleWatch]::Start((Join-Path $OutDir 'shots'))
$app = $null
try {
    [ConsoleWatch]::Mark("app-$Label")
    [void][ConsoleWatch]::Desktops()
    $start = New-Object Diagnostics.ProcessStartInfo $Exe
    $start.UseShellExecute = $false
    $start.EnvironmentVariables['CODEXBAR_SETTINGS_DIR'] = $settings.FullName
    $app = [Diagnostics.Process]::Start($start)
    $result.appPid = $app.Id
    $deadline = [DateTime]::UtcNow.AddSeconds($AppSeconds)
    $shot = 0
    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 20
        if ($app.HasExited) { throw "CodexBar exited early with $($app.ExitCode)" }
        $shot++
        [ConsoleWatch]::ShootTo((Join-Path $OutDir ('desktop-{0:D2}.png' -f $shot)))
    }
    [void][ConsoleWatch]::Desktops()
    [ConsoleWatch]::Mark("app-$Label-stopping")
    & taskkill.exe /T /F /PID $app.Id | Out-Null
    Start-Sleep -Seconds 2

    if ($Matrix) {
        $matrixFile = Join-Path $OutDir 'matrix.txt'
        $cases = @(
            @{ name = 'plain'; mode = 'plain'; cmd = @('gh', 'auth', 'status', '--hostname', 'github.com', '--json', 'hosts') },
            @{ name = 'nowindow'; mode = 'nowindow'; cmd = @('gh', 'auth', 'status', '--hostname', 'github.com', '--json', 'hosts') },
            @{ name = 'contained'; mode = 'contained'; cmd = @('gh', 'auth', 'status', '--hostname', 'github.com', '--json', 'hosts') },
            @{ name = 'hidden'; mode = 'hidden'; cmd = @('gh', 'auth', 'status', '--hostname', 'github.com', '--json', 'hosts') },
            @{ name = 'detached'; mode = 'detached'; cmd = @('gh', 'auth', 'status', '--hostname', 'github.com', '--json', 'hosts') },
            @{ name = 'grandchild-contained'; mode = 'contained'; cmd = @('cmd.exe', '/d', '/c', 'gh', '--version', '&', 'gh', 'auth', 'status', '--hostname', 'github.com') },
            @{ name = 'grandchild-hidden'; mode = 'hidden'; cmd = @('cmd.exe', '/d', '/c', 'gh', '--version', '&', 'gh', 'auth', 'status', '--hostname', 'github.com') },
            @{ name = 'control-newconsole'; mode = 'newconsole'; cmd = @('cmd.exe', '/d', '/c', 'gh', '--version', '&', 'ping', '-n', '3', '127.0.0.1') }
        )
        foreach ($case in $cases) {
            [ConsoleWatch]::Mark("matrix-$($case.name)")
            $p = Start-Process -FilePath $launch -ArgumentList (@($case.mode, $matrixFile) + $case.cmd) -PassThru -Wait
            Start-Sleep -Seconds 2
        }
        [ConsoleWatch]::Mark('matrix-done')
        $result.matrix = @(Get-Content $matrixFile)
    }
} catch {
    $result.error = $_.Exception.Message
    Write-Host "[$Label] probe failed: $($_.Exception.Message)"
} finally {
    if ($null -ne $app -and -not $app.HasExited) { & taskkill.exe /T /F /PID $app.Id | Out-Null }
    $lines = [ConsoleWatch]::Stop()
    $lines | Set-Content -Path (Join-Path $OutDir 'windows.jsonl') -Encoding utf8
}

# Per phase: every console-class window seen, and every window that became visible on screen from a console-capable
# process or with a console class. CodexBar's own window is not a console window.
$events = @($lines | ForEach-Object { $_ | ConvertFrom-Json })
$windows = @($events | Where-Object { $_.PSObject.Properties['class'] })
$consoleProcs = 'gh', 'conhost', 'OpenConsole', 'WindowsTerminal', 'cmd', 'powershell', 'pwsh', 'git', 'node', 'agy'
$phases = [ordered]@{}
foreach ($marker in @($events | Where-Object { $_.source -eq 'phase' })) {
    $phases[$marker.phase] = [ordered]@{ consoleWindows = @(); visibleConsoleWindows = @() }
}
foreach ($w in $windows) {
    if (-not $phases.Contains($w.phase)) { $phases[$w.phase] = [ordered]@{ consoleWindows = @(); visibleConsoleWindows = @() } }
    $entry = $phases[$w.phase]
    $console = $w.consoleClass -or ($consoleProcs -contains $w.process)
    if ($w.consoleClass -and -not ($entry.consoleWindows | Where-Object { $_.hwnd -eq $w.hwnd })) {
        $entry.consoleWindows += [ordered]@{ hwnd = $w.hwnd; class = $w.class; process = $w.process; pid = $w.pid; visible = $w.visible; source = $w.source }
    }
    if ($console -and $w.onScreen -and -not ($entry.visibleConsoleWindows | Where-Object { $_.hwnd -eq $w.hwnd })) {
        $entry.visibleConsoleWindows += [ordered]@{ hwnd = $w.hwnd; class = $w.class; title = $w.title; process = $w.process; pid = $w.pid; source = $w.source; t = $w.t; rect = $w.rect }
    }
}
$result.hookInstalled = [bool]($events | Where-Object { $_.source -eq 'hook' -and $_.installed })
$result.phases = $phases
$result.screenshots = @($events | Where-Object { $_.source -eq 'screenshot' } | ForEach-Object { [ordered]@{ phase = $_.phase; file = $_.file } })
$result.historyNewLines = if (Test-Path $history) { @(Get-Content $history).Count - $historyBefore } else { 0 }
$result.copilotHistory = if (Test-Path $history) { @(Get-Content $history | Select-Object -Skip $historyBefore | Select-String -SimpleMatch 'probe-copilot' | Select-Object -First 20 | ForEach-Object { $_.Line }) } else { @() }
$result.finishedUtc = [DateTime]::UtcNow.ToString('o')
$result | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutDir 'result.json') -Encoding utf8

Write-Host "[$Label] hook installed: $($result.hookInstalled); new Copilot history lines: $(@($result.copilotHistory).Count)"
foreach ($name in $phases.Keys) {
    $p = $phases[$name]
    Write-Host ("[{0}] {1}: console windows={2}, visible console windows={3}" -f $Label, $name, @($p.consoleWindows).Count, @($p.visibleConsoleWindows).Count)
    foreach ($v in $p.visibleConsoleWindows) { Write-Host ("    visible: {0} '{1}' pid {2} ({3}) via {4}" -f $v.class, $v.title, $v.pid, $v.process, $v.source) }
}
