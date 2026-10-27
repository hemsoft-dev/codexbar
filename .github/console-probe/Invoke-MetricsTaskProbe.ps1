# Throwaway Windows-native probe for codexbar#269 / PR #270 (never merged). On a disposable hosted runner only:
# registers the original-shape metrics task (Task Scheduler starts console powershell.exe directly, interactive
# principal) and the task PR #270 prepares from it (GUI-subsystem host), runs each once under ConsoleWatch, then
# runs PR #270's opt-in native fixtures. The collector runs for real with `gh auth token --user` for an account that
# does not exist, so it fails authentication fast without any provider request.
param(
    [Parameter(Mandatory = $true)][string]$Root,
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$OutDir = (New-Item -ItemType Directory -Force (Join-Path $OutDir 'metrics-task')).FullName
Add-Type -Path (Join-Path $PSScriptRoot 'Watch.cs') -ReferencedAssemblies System.Drawing, System.Windows.Forms

$stage = (New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) 'metrics-task-probe')).FullName
$data = (New-Item -ItemType Directory -Force (Join-Path $stage 'data')).FullName
$collector = (Resolve-Path (Join-Path $Root 'scripts/copilot-metrics.ps1')).Path
$powerShell = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
$arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$collector`" -Refresh -GitHubUser codexbar-probe-missing-user -DataDir `"$data`""
$user = "$env:USERDOMAIN\$env:USERNAME"
$result = [ordered]@{ user = $user; session = (Get-Process -Id $PID).SessionId; runs = @() }

function Wait-Task([string]$Name) {
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    do { Start-Sleep -Milliseconds 500; $state = (Get-ScheduledTask -TaskName $Name).State } while ($state -eq 'Running' -and [DateTime]::UtcNow -lt $deadline)
    Start-Sleep -Seconds 2
    $info = Get-ScheduledTaskInfo -TaskName $Name
    [ordered]@{ task = $Name; state = [string]$state; lastResult = $info.LastTaskResult; lastRun = $info.LastRunTime.ToUniversalTime().ToString('o') }
}

[ConsoleWatch]::Start((Join-Path $OutDir 'shots'))
try {
    # Original shape: the task action starts console powershell.exe itself, so its console exists before any script.
    $action = New-ScheduledTaskAction -Execute $powerShell -Argument $arguments -WorkingDirectory $stage
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName 'CodexBarProbeOriginal' -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    [ConsoleWatch]::Mark('task-original')
    Start-ScheduledTask -TaskName 'CodexBarProbeOriginal'
    $result.runs += Wait-Task 'CodexBarProbeOriginal'

    # PR #270: prepare from the captured, disabled original, then register the prepared definition.
    Disable-ScheduledTask -TaskName 'CodexBarProbeOriginal' | Out-Null
    $original = Join-Path $stage 'original.xml'
    Export-ScheduledTask -TaskName 'CodexBarProbeOriginal' | Set-Content -Path $original -Encoding Unicode
    $bundle = & (Join-Path $Root 'scripts/Prepare-CopilotMetricsTask.ps1') -TaskXmlPath $original -CollectorPath $collector -OutputDirectory (Join-Path $stage 'prepared')
    $result.preparedLauncher = $bundle.Launcher
    $result.preparedTaskXml = Get-Content -LiteralPath $bundle.TaskXml -Raw
    Register-ScheduledTask -TaskName 'CodexBarProbePrepared' -Xml $result.preparedTaskXml -User $user -Force | Out-Null
    Enable-ScheduledTask -TaskName 'CodexBarProbePrepared' | Out-Null
    [ConsoleWatch]::Mark('task-prepared')
    Start-ScheduledTask -TaskName 'CodexBarProbePrepared'
    $result.runs += Wait-Task 'CodexBarProbePrepared'

    [ConsoleWatch]::Mark('native-fixtures')
    $fixtures = & (Join-Path $Root 'scripts/tests/CopilotMetrics.Windows.Tests.ps1') -RunNativeFixtures
    $result.nativeFixtures = [string]$fixtures
    [ConsoleWatch]::Mark('done')
} catch {
    $result.error = $_.Exception.Message
    Write-Host "metrics task probe failed: $($_.Exception.Message)"
} finally {
    foreach ($name in 'CodexBarProbeOriginal', 'CodexBarProbePrepared') { Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue }
    $lines = [ConsoleWatch]::Stop()
    $lines | Set-Content -Path (Join-Path $OutDir 'windows.jsonl') -Encoding utf8
}

$result | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir 'result.json') -Encoding utf8
foreach ($run in $result.runs) { Write-Host ("{0}: state {1}, last result {2}" -f $run.task, $run.state, $run.lastResult) }
Write-Host "native fixtures: $($result.nativeFixtures)"
