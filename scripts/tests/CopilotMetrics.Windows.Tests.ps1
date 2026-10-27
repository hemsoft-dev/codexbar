# No task registration or interactive desktop tests. All children have explicit
# creation flags. On Home, the caller must also require session 0.
[CmdletBinding()]
param([switch]$RunNativeFixtures)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:CODEXBAR_TEST_REQUIRE_SESSION_ZERO -eq '1' -and
    [Diagnostics.Process]::GetCurrentProcess().SessionId -ne 0) { throw 'Home tests require session 0.' }
if ($PSVersionTable.PSEdition -ne 'Desktop') { throw 'Use Windows PowerShell 5.1.' }
if (-not $RunNativeFixtures) { throw 'Native runtime probes require explicit coordinated authorization and -RunNativeFixtures.' }
$root = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $root 'CopilotMetrics.Process.psm1') -Force
Import-Module (Join-Path $root 'CopilotMetrics.Task.psm1') -Force
$stage = Join-Path ([IO.Path]::GetTempPath()) ('codexbar metrics tests-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $stage)
$fixtureContext = @{ Count = 0 }
function Assert-Contract([bool]$Condition, [string]$Message) {
    $fixtureContext.Count++
    if (-not $Condition) { throw $Message }
}
function Assert-Rejected([scriptblock]$Operation, [string]$Message) {
    $rejected = $false
    try { & $Operation | Out-Null } catch { $rejected = $true }
    Assert-Contract $rejected $Message
}
function Invoke-NoConsole([string]$Path) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Path
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $process = [Diagnostics.Process]::new()
    try {
        $process.StartInfo = $start
        [void]$process.Start()
        $process.StandardInput.Close()
        $process.WaitForExit()
        $process.ExitCode
    } finally { $process.Dispose() }
}
$oldPath = $env:PATH
$oldPrompt = $env:GH_PROMPT_DISABLED
$oldProbe = $env:CODEXBAR_METRICS_PROBE
$oldTokens = @{}
foreach ($key in @('EnterpriseBillingToken', 'GH_TOKEN', 'GITHUB_TOKEN')) {
    $oldTokens[$key] = [Environment]::GetEnvironmentVariable($key)
    [Environment]::SetEnvironmentVariable($key, $null)
}
try {
    # Native console fixture, no auth/network/browser. Its console handle, argv,
    # prompt policy and stdin EOF prove the actual gh child creation contract.
    Invoke-CopilotMetricsCompilation -OutputPath (Join-Path $stage 'gh.exe') -Target exe -Source @'
using System;
using System.IO;
using System.Runtime.InteropServices;
public static class MetricsGhFixture {
    [DllImport("kernel32.dll")] static extern IntPtr GetConsoleWindow();
    public static int Main(string[] args) {
        var input = Console.In.ReadToEnd();
        File.WriteAllLines(Environment.GetEnvironmentVariable("CODEXBAR_METRICS_PROBE"), new [] {
            GetConsoleWindow().ToInt64().ToString(), string.Join("|", args), input.Length.ToString(),
            Environment.GetEnvironmentVariable("GH_PROMPT_DISABLED") ?? ""
        });
        Console.Error.Write(new string('x', 100000));
        Console.Write("fixture-token\r\n");
        return Array.IndexOf(args, "missing") >= 0 ? 3 : 0;
    }
}
'@
    $env:PATH = "$stage;$oldPath"
    $env:CODEXBAR_METRICS_PROBE = Join-Path $stage 'probe.txt'
    $token = Get-CopilotGitHubCliToken -UserName 'fixture user'
    Assert-Contract ($token.Token -eq 'fixture-token') 'Named-account token must be captured without its newline.'
    $probe = Get-Content -LiteralPath $env:CODEXBAR_METRICS_PROBE
    Assert-Contract ($probe[0] -eq '0') 'gh must not allocate a console.'
    Assert-Contract ($probe[1] -eq 'auth|token|--user|fixture user') 'Account argv must remain exact.'
    Assert-Contract ($probe[2] -eq '0' -and $probe[3] -eq '1') 'gh stdin must close and prompts must be disabled.'
    $null = Get-CopilotGitHubCliToken
    Assert-Contract ((Get-Content $env:CODEXBAR_METRICS_PROBE)[1] -eq 'auth|token') 'Active account must not acquire a user flag.'
    Assert-Rejected { Get-CopilotGitHubCliToken -UserName 'missing' } 'Nonzero gh exits must not return stdout as a token or switch accounts.'
    $null = Get-CopilotGitHubCliToken -UserName 'quotes" and slash\'
    Assert-Contract ((Get-Content $env:CODEXBAR_METRICS_PROBE)[1] -eq 'auth|token|--user|quotes" and slash\') 'Windows argv escaping must preserve quotes and slashes.'

    # Invoke the unmodified collector with in-process HTTP fixtures. Tokens are
    # synthetic, never printed. This covers the real precedence/account path,
    # monthly checkpoints and saved-input mode without provider requests.
    function Invoke-WebRequest {
        param($Uri, $Headers, $Method, [switch]$UseBasicParsing)
        Assert-Contract ($Headers.Authorization -eq "Bearer $expectedToken") 'Token precedence changed.'
        [pscustomobject]@{ StatusCode = 200; Content = '{"usageItems":[{"model":"Fixture","grossQuantity":12,"grossAmount":0.12,"netAmount":0.1}]}' }
    }
    $collector = Join-Path $root 'copilot-metrics.ps1'
    $data = Join-Path $stage 'data'
    foreach ($source in @('cli', 'GITHUB_TOKEN', 'GH_TOKEN', 'EnterpriseBillingToken', 'explicit')) {
        $expectedToken = if ($source -eq 'cli') { 'fixture-token' } else { "fixture-$source" }
        if ($source -ne 'cli' -and $source -ne 'explicit') { [Environment]::SetEnvironmentVariable($source, $expectedToken) }
        $parameters = @{ Refresh=$true; User=@('fixture'); GitHubUser='fixture user'; Delay=0; DataDir=$data }
        if ($source -eq 'explicit') { $parameters.Token = $expectedToken }
        # Match the collector's ordinary caller; do not impose this test's
        # StrictMode on the unchanged CLI implementation.
        & { Set-StrictMode -Off; & $collector @parameters 6>$null | Out-Null }
        $saved = Get-Content (Join-Path $data 'copilot-metrics.json') -Raw | ConvertFrom-Json
        Assert-Contract ($saved.RefreshStatus -eq 'Complete' -and $saved.Users[0].Success) 'Monthly refresh must preserve checkpoint functionality.'
    }
    $before = (Get-FileHash (Join-Path $data 'copilot-metrics.json')).Hash
    & { Set-StrictMode -Off; & $collector -InputPath (Join-Path $data 'copilot-metrics.json') 6>$null | Out-Null }
    Assert-Contract ((Get-FileHash (Join-Path $data 'copilot-metrics.json')).Hash -eq $before) 'Saved-input mode must be read-only.'
    foreach ($key in $oldTokens.Keys) { [Environment]::SetEnvironmentVariable($key, $null) }

    $powerShell = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
    $xml = [xml]@'
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
 <RegistrationInfo><URI>\Fixture\Metrics</URI></RegistrationInfo>
 <Triggers><CalendarTrigger><StartBoundary>2026-10-24T00:00:00</StartBoundary><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger></Triggers>
 <Principals><Principal id="Author"><UserId>fixture-existing-user</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
 <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><Enabled>false</Enabled></Settings>
 <Actions Context="Author"><Exec><Command/><Arguments/><WorkingDirectory/></Exec></Actions>
</Task>
'@
    # Windows PowerShell's XML adapter takes only bare strings; Join-Path results are wrapped.
    $xml.Task.Actions.Exec.Command = [string]$powerShell
    $xml.Task.Actions.Exec.Arguments = '-NoProfile -ExecutionPolicy Bypass -File ' + (ConvertTo-CopilotProcessArgument $collector) + ' -Refresh -GitHubUser missing -DataDir ' + (ConvertTo-CopilotProcessArgument $data)
    $xml.Task.Actions.Exec.WorkingDirectory = [string]$stage
    $inputXml = Join-Path $stage 'original.xml'
    $xml.Save($inputXml)
    $prepare = Join-Path $root 'Prepare-CopilotMetricsTask.ps1'
    $bundle = & $prepare -TaskXmlPath $inputXml -CollectorPath $collector -OutputDirectory (Join-Path $stage 'prepared')
    $prepared = [xml](Get-Content -LiteralPath $bundle.TaskXml -Raw)
    Assert-Contract ($prepared.Task.Settings.OuterXml -eq $xml.Task.Settings.OuterXml) 'Settings must remain byte-equivalent as XML.'
    Assert-Contract ($prepared.Task.Principals.OuterXml -eq $xml.Task.Principals.OuterXml) 'Principal must not change.'
    Assert-Contract ($prepared.Task.Triggers.OuterXml -eq $xml.Task.Triggers.OuterXml) 'Triggers must not change.'
    Assert-Contract ($prepared.Task.Actions.Exec.WorkingDirectory -eq $stage -and $prepared.Task.Actions.Exec.Arguments -eq '') 'Only action command and arguments may change.'
    Assert-Contract (-not $bundle.Registered -and -not $bundle.Enabled) 'Preparation must not deploy.'
    $bytes = [IO.File]::ReadAllBytes($bundle.Launcher)
    $pe = [BitConverter]::ToInt32($bytes, 0x3c)
    Assert-Contract ([BitConverter]::ToUInt16($bytes, $pe + 24 + 68) -eq 2) 'Task entry point must be PE Windows GUI subsystem, never a console parent.'
    $assembly = [Reflection.Assembly]::LoadFile($bundle.Launcher)
    $start = $assembly.GetType('CopilotMetricsTaskHost').GetMethod('CreateStartInfo').Invoke($null, @())
    Assert-Contract ($start.CreateNoWindow -and -not $start.UseShellExecute) 'Actual compiled host must use no-console process creation.'
    Assert-Contract ($start.RedirectStandardInput -and $start.RedirectStandardOutput -and $start.RedirectStandardError) 'Task host must own all streams.'
    Assert-Contract ($start.Arguments -eq ('-NoLogo -NonInteractive ' + $xml.Task.Actions.Exec.Arguments)) 'Original collector CLI must remain exact.'
    Assert-Contract ($start.UserName -eq '' -and -not $start.Verb) 'No credential, elevation or principal change is allowed.'
    Assert-Contract ((Invoke-NoConsole $bundle.Launcher) -eq 1) 'Collector auth failure must propagate through the task host.'
    Assert-Contract ((Get-Content $env:CODEXBAR_METRICS_PROBE)[0] -eq '0') 'Full task-host -> PowerShell -> gh chain must have no console.'
    Assert-Contract ((Get-Content $env:CODEXBAR_METRICS_PROBE)[1] -eq 'auth|token|--user|missing') 'Full chain must retain named-account selection.'
    Assert-Contract ((Get-FileHash (Join-Path $data 'copilot-metrics.json')).Hash -eq $before) 'Failed auth must leave existing metrics unchanged.'
    Assert-Rejected { & $prepare -TaskXmlPath $inputXml -CollectorPath $collector -OutputDirectory (Join-Path $stage 'prepared') } 'Artifacts cannot be overwritten.'

    foreach ($unsafe in @('cmd.exe', 'pwsh.exe', 'wscript.exe', 'msedge.exe')) {
        $xml.Task.Actions.Exec.Command = $unsafe
        $xml.Save($inputXml)
        Assert-Rejected { & $prepare -TaskXmlPath $inputXml -CollectorPath $collector -OutputDirectory (Join-Path $stage 'rejected') } "Reject interactive shell/browser action: $unsafe"
    }
    $xml.Task.Actions.Exec.Command = [string]$powerShell
    foreach ($unsafe in @('-NoExit -File ignored.ps1', '-Command "Start-Process msedge.exe"', '-File other.ps1', '-NoProfile -File "' + $collector + '" -Token literal')) {
        $xml.Task.Actions.Exec.Arguments = $unsafe
        $xml.Save($inputXml)
        Assert-Rejected { & $prepare -TaskXmlPath $inputXml -CollectorPath $collector -OutputDirectory (Join-Path $stage 'rejected') } 'Reject unbounded or credential-bearing PowerShell actions.'
    }
    $xml.Task.Settings.Enabled = 'true'
    $xml.Save($inputXml)
    Assert-Rejected { & $prepare -TaskXmlPath $inputXml -CollectorPath $collector -OutputDirectory (Join-Path $stage 'rejected') } 'Enabled input cannot bypass containment.'
    Assert-Contract (-not (Test-Path (Join-Path $stage 'rejected'))) 'Rejected definitions must produce no launcher.'
    Write-Output "Passed $($fixtureContext.Count) native Windows launch/metrics assertions; session $([Diagnostics.Process]::GetCurrentProcess().SessionId)."
}
finally {
    $env:PATH = $oldPath
    $env:GH_PROMPT_DISABLED = $oldPrompt
    $env:CODEXBAR_METRICS_PROBE = $oldProbe
    foreach ($key in $oldTokens.Keys) { [Environment]::SetEnvironmentVariable($key, $oldTokens[$key]) }
    Remove-Item -LiteralPath $stage -Recurse -Force
}
