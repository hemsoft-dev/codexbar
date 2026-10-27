# Throwaway Windows-native probe for chief#43 / codexbar#271 (never merged). Runs one real CodexBar build with a
# GitHub-CLI Copilot account through several background refresh cycles while ConsoleWatch records every window that
# is created or shown, then optionally runs a creation-flag matrix for `gh` from a GUI-subsystem launcher.
# Windows PowerShell 5.1: its C# compiler builds Watch.cs and the launcher with System.Drawing available.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Label,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [int]$AppSeconds = 240,
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

$agy = Join-Path $env:LOCALAPPDATA 'agy\bin\agy.exe'
if (-not (Test-Path $agy)) { $found = Get-Command agy -ErrorAction SilentlyContinue; $agy = if ($found) { $found.Source } else { '' } }
$result = [ordered]@{
    label = $Label; exe = $Exe; exeSha256 = (Get-FileHash $Exe -Algorithm SHA256).Hash.ToLowerInvariant()
    exeSubsystem = Get-Subsystem $Exe; startedUtc = [DateTime]::UtcNow.ToString('o')
    gh = (Get-Command gh).Source; ghVersion = [string]((& gh --version) -join ' ')
    agy = $agy
}
$settings = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) "console-probe-settings-$Label")
# One Copilot account on the GitHub CLI's sign-in with no username: CodexBar asks `gh auth status --json hosts` for
# every account, then `gh auth token --user` for each, on every refresh. One Gemini account with the Antigravity
# CLI's coding quotas: `agy --version`, then `agy -p /usage --output-format json`. One-minute refresh interval.
@'
{"accountConfigurationVersion":1,"refreshIntervalSeconds":60,"accounts":[
 {"id":"probe-copilot","providerId":"Copilot","displayLabel":"Probe","enabled":true,"authenticationMethod":"CommandLine"},
 {"id":"probe-gemini","providerId":"Gemini","displayLabel":"Probe Gemini","enabled":true,"authenticationMethod":"BrowserSession","geminiCodingFromCli":true}]}
'@ | Set-Content -Path (Join-Path $settings 'settings.json') -Encoding ascii
$history = Join-Path $env:USERPROFILE '.codexbar\history.jsonl'
$historyBefore = if (Test-Path $history) { @(Get-Content $history).Count } else { 0 }

# Make agy's self-updater due again (its 15-minute marker), so each build meets it.
$updater = Join-Path $env:USERPROFILE '.gemini\antigravity-cli\updater'
foreach ($marker in 'last_check.timestamp', 'update.lock') { Remove-Item -LiteralPath (Join-Path $updater $marker) -Force -ErrorAction SilentlyContinue }
$result.updaterMarkers = if (Test-Path $updater) { [string[]]@(Get-ChildItem $updater -Name) } else { [string[]]@() }
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
            @{ name = 'agy-plain'; mode = 'plain'; cmd = @($agy, '--version') },
            @{ name = 'agy-contained-version'; mode = 'contained'; cmd = @($agy, '--version') },
            @{ name = 'agy-contained-usage'; mode = 'contained'; cmd = @($agy, '-p', '/usage', '--output-format', 'json', '--print-timeout', '25s') },
            @{ name = 'control-newconsole'; mode = 'newconsole'; cmd = @('cmd.exe', '/d', '/c', 'gh', '--version', '&', 'ping', '-n', '3', '127.0.0.1') }
        )
        foreach ($case in $cases) {
            if ($case.cmd[0] -eq '') { continue }
            [ConsoleWatch]::Mark("matrix-$($case.name)")
            # The launcher itself waits up to 60 s. Start-Process -Wait would also wait for every descendant, such as
            # the browser agy opens for sign-in.
            $p = Start-Process -FilePath $launch -ArgumentList (@($case.mode, $matrixFile) + $case.cmd) -PassThru
            [void]$p.WaitForExit(90000)
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
    Write-Host "[$Label] $([DateTime]::UtcNow.ToString('o')) stopping the watcher"
    $lines = [ConsoleWatch]::Stop()
    Write-Host "[$Label] $([DateTime]::UtcNow.ToString('o')) watcher stopped"
    $lines | Set-Content -Path (Join-Path $OutDir 'windows.jsonl') -Encoding utf8
}

$result.historyNewLines = if (Test-Path $history) { @(Get-Content $history).Count - $historyBefore } else { 0 }
# History holds only account ids, metric keys, numbers and times.
$result.historySample = if (Test-Path $history) { [string[]]@(Get-Content $history | Select-Object -Skip $historyBefore | Select-Object -First 30) } else { [string[]]@() }
$result.finishedUtc = [DateTime]::UtcNow.ToString('o')
$result | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir 'result.json') -Encoding utf8
Write-Host "[$Label] window events: $($lines.Count); new history lines: $($result.historyNewLines)"
