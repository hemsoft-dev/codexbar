$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$collectorPath = Join-Path (Split-Path -Parent $PSScriptRoot) 'copilot-metrics.ps1'
$tokens = $null
$parseErrors = $null
$collector = [Management.Automation.Language.Parser]::ParseFile($collectorPath, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw 'Collector parse errors.' }
$rawLaunches = @($collector.FindAll({
    param($node)
    $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'gh'
}, $true))
if ($rawLaunches.Count) {
    throw "Collector still uses $($rawLaunches.Count) raw gh launches without creation-time no-console flags."
}

Import-Module (Join-Path (Split-Path -Parent $PSScriptRoot) 'CopilotMetrics.Process.psm1') -Force
$count = 1
function Assert-Contract([bool]$Condition, [string]$Message) {
    $script:count++
    if (-not $Condition) { throw $Message }
}
$info = New-CopilotTokenStartInfo -UserName 'fixture user'
Assert-Contract (-not $info.UseShellExecute -and $info.CreateNoWindow) 'gh must have creation-time no-console flags.'
Assert-Contract ($info.RedirectStandardInput -and $info.RedirectStandardOutput -and $info.RedirectStandardError) 'All gh streams must be captured.'
Assert-Contract ($info.Arguments -eq 'auth token --user "fixture user"') 'Named-account lookup must retain its exact argv.'
Assert-Contract ($info.EnvironmentVariables['GH_PROMPT_DISABLED'] -eq '1') 'Authentication cannot prompt.'
Assert-Contract ((New-CopilotTokenStartInfo).Arguments -eq 'auth token') 'Active-account lookup must remain available.'
Assert-Contract ((ConvertTo-CopilotProcessArgument '') -eq '""') 'Empty argv must remain empty.'
Assert-Contract ((ConvertTo-CopilotProcessArgument 'a"b\') -eq '"a\"b\\"') 'Quotes and trailing backslashes must round trip.'

# All new PowerShell sources must parse on the local host without running any
# native Windows probe or collecting provider data.
foreach ($name in @('CopilotMetrics.Process.psm1', 'CopilotMetrics.Task.psm1', 'Prepare-CopilotMetricsTask.ps1')) {
    $null = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path (Split-Path -Parent $PSScriptRoot) $name), [ref]$tokens, [ref]$parseErrors)
    Assert-Contract ($parseErrors.Count -eq 0) "$name must parse."
}
if ($env:OS -eq 'Windows_NT' -and $env:CODEXBAR_RUN_NATIVE_METRICS_FIXTURES -eq '1') {
    # Compile the GUI-subsystem helper using the same Windows PowerShell 5.1
    # toolchain used by preparation. No shell, profile or interactive console.
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
    $testPath = Join-Path $PSScriptRoot 'CopilotMetrics.Windows.Tests.ps1'
    $start.Arguments = '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ' + (ConvertTo-CopilotProcessArgument $testPath) + ' -RunNativeFixtures'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    # PowerShell 7 adds its own module folders to PSModulePath; Windows PowerShell must use its defaults to load
    # its built-in modules (Get-FileHash).
    [void]$start.EnvironmentVariables.Remove('PSModulePath')
    $process = [Diagnostics.Process]::new()
    try {
        $process.StartInfo = $start
        [void]$process.Start()
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $errors = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "Windows contract tests failed: $output $errors" }
        Write-Output $output.Trim()
    }
    finally { $process.Dispose() }
}

Import-Module (Join-Path (Split-Path -Parent $PSScriptRoot) 'CopilotMetrics.Task.psm1') -Force
function Assert-Rejected([scriptblock]$Operation, [string]$Message) {
    $rejected = $false
    try { & $Operation | Out-Null } catch { $rejected = $true }
    Assert-Contract $rejected $Message
}
$powerShell = 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
$collectorFile = 'D:\source folder\scripts\copilot-metrics.ps1'
$xml = [xml]@'
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
 <Principals><Principal><UserId>existing-user</UserId><LogonType>InteractiveToken</LogonType></Principal></Principals>
 <Settings><Enabled>false</Enabled></Settings>
 <Actions><Exec><Command/><Arguments/><WorkingDirectory>D:\source folder</WorkingDirectory></Exec></Actions>
</Task>
'@
$xml.Task.Actions.Exec.Command = $powerShell
$originalArguments = '-NoProfile -ExecutionPolicy Bypass -File ' + (ConvertTo-CopilotProcessArgument $collectorFile) + ' -Refresh -GitHubUser fixture -DataDir "D:\data folder"'
$xml.Task.Actions.Exec.Arguments = $originalArguments
$originalXml = $xml.OuterXml
$plan = Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collectorFile -PowerShellPath $powerShell
Assert-Contract ($plan.Arguments -eq $originalArguments) 'Original collector arguments must remain exact.'
Assert-Contract ($xml.OuterXml -eq $originalXml) 'Action validation cannot mutate input XML.'
foreach ($unsafe in @('cmd.exe', 'pwsh.exe', 'wscript.exe', 'msedge.exe', 'https://github.com/login')) {
    $xml.Task.Actions.Exec.Command = $unsafe
    Assert-Rejected { Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collectorFile -PowerShellPath $powerShell } "Reject interactive shell/browser action $unsafe without invoking it."
}
$xml.Task.Actions.Exec.Command = $powerShell
foreach ($unsafe in @('-NoExit -File ignored.ps1', '-Command "Start-Process msedge.exe"', '-NoProfile -File other.ps1', '-File "' + $collectorFile + '"')) {
    $xml.Task.Actions.Exec.Arguments = $unsafe
    Assert-Rejected { Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collectorFile -PowerShellPath $powerShell } 'Reject arbitrary/interactive PowerShell or profile startup.'
}
foreach ($credential in @('-Token secret', '-Tok:secret', '-Toke secret', '-To secret', '-T secret')) {
    $xml.Task.Actions.Exec.Arguments = $originalArguments + ' ' + $credential
    Assert-Rejected { Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collectorFile -PowerShellPath $powerShell } 'Never copy literal credential arguments, including abbreviations.'
}
$xml.Task.Actions.Exec.Arguments = $originalArguments
$xml.Task.Settings.Enabled = 'true'
Assert-Rejected { Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collectorFile -PowerShellPath $powerShell } 'Enabled inputs cannot bypass containment.'
$xml.Task.Settings.Enabled = 'false'
$null = $xml.Task.Actions.AppendChild($xml.Task.Actions.Exec.CloneNode($true))
Assert-Rejected { Get-CopilotMetricsTaskAction -TaskXml $xml -CollectorPath $collectorFile -PowerShellPath $powerShell } 'Additional task actions cannot escape the validated launch chain.'

# Actual host source, compiled in process as a library on Air. Calling its plan
# factory creates only ProcessStartInfo. Main is never invoked here.
$source = Get-CopilotMetricsTaskHostSource -PowerShellPath $powerShell -Arguments $originalArguments -WorkingDirectory 'D:\source folder'
$typeName = 'MetricsContractProbe' + [guid]::NewGuid().ToString('N')
$source = $source.Replace('CopilotMetricsTaskHost', $typeName)
$type = Add-Type -TypeDefinition $source -PassThru | Where-Object FullName -eq $typeName
$start = $type.GetMethod('CreateStartInfo').Invoke($null, @())
Assert-Contract (-not $start.UseShellExecute -and $start.CreateNoWindow) 'Actual generated host must set no-console creation flags.'
Assert-Contract ($start.RedirectStandardInput -and $start.RedirectStandardOutput -and $start.RedirectStandardError) 'Actual generated host must own all child streams.'
Assert-Contract ($start.FileName -eq $powerShell -and $start.Arguments -eq ('-NoLogo -NonInteractive ' + $originalArguments)) 'Generated host must retain exact executable, account, data and CLI arguments.'
Assert-Contract ($start.WorkingDirectory -eq 'D:\source folder') 'Generated host must retain working directory.'
Assert-Contract (-not $start.UserName -and -not $start.Verb) 'Generated host cannot change credentials or elevate.'

# Exercise Windows argv decoding against generated escaped values, including
# empty strings, Unicode, whitespace, literal quotes and trailing backslashes.
$values = @('', 'plain', 'white space', 'a"b', 'trailing\', '\\"x', "tab`tvalue", 'Franz-ö')
$random = [Random]::new(269)
$alphabet = 'ab "\'
for ($sample = 0; $sample -lt 80; $sample++) {
    $word = ''
    for ($i = 0; $i -lt $random.Next(1, 25); $i++) { $word += $alphabet[$random.Next($alphabet.Length)] }
    $values += $word
}
$encoded = ($values | ForEach-Object { ConvertTo-CopilotProcessArgument $_ }) -join ' '
$decoded = @(Split-CopilotWindowsArgument $encoded)
Assert-Contract ($decoded.Count -eq $values.Count) 'Windows argv must preserve argument count.'
for ($i = 0; $i -lt $values.Count; $i++) { Assert-Contract ($decoded[$i] -ceq $values[$i]) 'Windows argv values must round trip exactly.' }
$compiler = New-CopilotMetricsCompilerStartInfo -SourcePath 'D:\source folder\host.cs' -OutputPath 'D:\source folder\host.exe'
Assert-Contract ($compiler.CreateNoWindow -and -not $compiler.UseShellExecute) 'Compiler child must have no-console creation flags.'
Assert-Contract ($compiler.RedirectStandardInput -and $compiler.RedirectStandardOutput -and $compiler.RedirectStandardError) 'Compiler child must own all streams.'
Assert-Contract ($compiler.FileName.EndsWith('csc.exe') -and $compiler.Arguments.Contains('/target:winexe')) 'Preparation must compile a GUI subsystem entry point with the native compiler.'
Write-Output "Passed $count process/task contract assertions without invoking the candidate host."
