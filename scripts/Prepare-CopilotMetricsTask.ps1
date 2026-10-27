# Prepare artifacts only. This script never registers, updates or starts a task.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$TaskXmlPath,
    [Parameter(Mandatory)][string]$CollectorPath,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT' -or $PSVersionTable.PSEdition -ne 'Desktop') {
    throw 'Prepare with Windows PowerShell 5.1, not pwsh. No task was changed.'
}
Import-Module (Join-Path $PSScriptRoot 'CopilotMetrics.Task.psm1') -Force
$collector = (Get-Item -LiteralPath $CollectorPath).FullName
if ([IO.Path]::GetFileName($collector) -ne 'copilot-metrics.ps1') { throw 'Expected the metrics collector.' }
$dependencyHashes = @{}
foreach ($module in @('CopilotMetrics.Core.psm1', 'CopilotMetrics.Process.psm1')) {
    $modulePath = Join-Path (Split-Path -Parent $collector) $module
    if (-not (Test-Path -LiteralPath $modulePath -PathType Leaf)) { throw "Missing collector dependency: $module" }
    $dependencyHashes[$module] = (Get-FileHash -LiteralPath $modulePath -Algorithm SHA256).Hash
}
$xml = [xml]::new()
$xml.XmlResolver = $null
$xml.Load((Get-Item -LiteralPath $TaskXmlPath).FullName)
$powerShell = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
$action = Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collector -PowerShellPath $powerShell
if (-not (Test-Path -LiteralPath $action.WorkingDirectory -PathType Container)) { throw 'Working directory is missing.' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw 'Output directory must be new; existing artifacts are never overwritten.' }
[void](New-Item -ItemType Directory -Path $output)
$launcher = Join-Path $output 'CopilotMetrics.TaskHost.exe'
$source = Get-CopilotMetricsTaskHostSource -PowerShellPath $powerShell -Arguments $action.Arguments -WorkingDirectory $action.WorkingDirectory
Invoke-CopilotMetricsCompilation -Source $source -OutputPath $launcher
# Fail closed before emitting any action that could regain a console parent.
$bytes = [IO.File]::ReadAllBytes($launcher)
if ($bytes.Length -lt 64 -or [BitConverter]::ToUInt16($bytes, 0) -ne 0x5a4d) { throw 'Invalid compiled task host.' }
$pe = [BitConverter]::ToInt32($bytes, 0x3c)
if ($pe -lt 0 -or $pe -gt $bytes.Length - 94 -or [BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550 -or
    [BitConverter]::ToUInt16($bytes, $pe + 24 + 68) -ne 2) { throw 'Task host must use the Windows GUI subsystem.' }
# Windows PowerShell's XML adapter only accepts a bare string; Join-Path returns one wrapped in a PSObject.
$action.Node.Command = [string]$launcher
$action.Node.Arguments = ''
# Principal, task security, triggers, working directory and settings are not
# reconstructed. The input XML remains untouched and the output stays disabled.
$preparedXml = Join-Path $output 'task.disabled.xml'
$xml.Save($preparedXml)
[pscustomobject]@{
    Launcher = $launcher
    TaskXml = $preparedXml
    LauncherSha256 = (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash
    SourceTaskSha256 = (Get-FileHash -LiteralPath $TaskXmlPath -Algorithm SHA256).Hash
    CollectorSha256 = (Get-FileHash -LiteralPath $collector -Algorithm SHA256).Hash
    DependencySha256 = $dependencyHashes
    Registered = $false
    Enabled = $false
}
