Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'CopilotMetrics.Process.psm1')

# Windows argv decoding, including doubled quotes and backslashes before quotes.
# This is parsing only: no shell, P/Invoke compiler or process is needed.
function Split-CopilotWindowsArgument {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$CommandLine)
    $position = 0
    while ($position -lt $CommandLine.Length) {
        while ($position -lt $CommandLine.Length -and $CommandLine[$position] -in @(' ', "`t")) { $position++ }
        if ($position -eq $CommandLine.Length) { break }
        $word = [Text.StringBuilder]::new()
        $quoted = $false
        while ($position -lt $CommandLine.Length) {
            if (-not $quoted -and $CommandLine[$position] -in @(' ', "`t")) { break }
            $slashes = 0
            while ($position -lt $CommandLine.Length -and $CommandLine[$position] -eq '\') { $slashes++; $position++ }
            if ($position -lt $CommandLine.Length -and $CommandLine[$position] -eq '"') {
                [void]$word.Append(('\' * [int][math]::Floor($slashes / 2)))
                if ($slashes % 2) { [void]$word.Append('"'); $position++ }
                elseif ($quoted -and $position + 1 -lt $CommandLine.Length -and $CommandLine[$position + 1] -eq '"') {
                    [void]$word.Append('"'); $position += 2
                }
                else { $quoted = -not $quoted; $position++ }
            }
            else {
                [void]$word.Append(('\' * $slashes))
                if ($position -lt $CommandLine.Length) {
                    if (-not $quoted -and $CommandLine[$position] -in @(' ', "`t")) { break }
                    [void]$word.Append($CommandLine[$position]); $position++
                }
            }
        }
        if ($quoted) { throw 'Unclosed action argument quote.' }
        $word.ToString()
    }
}

function Get-CopilotMetricsTaskAction {
    param(
        [Parameter(Mandatory)][xml]$TaskXml,
        [Parameter(Mandatory)][string]$CollectorPath,
        [Parameter(Mandatory)][string]$PowerShellPath
    )
    $ns = [Xml.XmlNamespaceManager]::new($TaskXml.NameTable)
    $ns.AddNamespace('t', 'http://schemas.microsoft.com/windows/2004/02/mit/task')
    $actions = @($TaskXml.SelectNodes('/t:Task/t:Actions/*', $ns))
    if ($actions.Count -ne 1 -or $actions[0].LocalName -ne 'Exec') { throw 'Expected one Exec action.' }
    $enabled = $TaskXml.SelectSingleNode('/t:Task/t:Settings/t:Enabled', $ns)
    if (-not $enabled -or $enabled.InnerText -ne 'false') { throw 'Input task must already be disabled.' }
    $action = $actions[0]
    $command = $action.SelectSingleNode('t:Command', $ns)
    $arguments = $action.SelectSingleNode('t:Arguments', $ns)
    $working = $action.SelectSingleNode('t:WorkingDirectory', $ns)
    if (-not $command -or $command.InnerText -ine $PowerShellPath -or -not $arguments -or -not $working -or -not $working.InnerText) {
        throw 'Expected the existing native Windows PowerShell action and working directory.'
    }
    $argv = @(Split-CopilotWindowsArgument $arguments.InnerText)
    $index = 0
    $noProfile = $false
    while ($index -lt $argv.Count -and $argv[$index] -ine '-File') {
        switch ($argv[$index].ToLowerInvariant()) {
            '-noprofile' { $noProfile = $true; $index++ }
            '-nologo' { $index++ }
            '-noninteractive' { $index++ }
            '-executionpolicy' {
                $index++
                if ($index -ge $argv.Count -or $argv[$index] -ine 'Bypass') { throw 'Unexpected execution policy.' }
                $index++
            }
            default { throw 'Interactive or arbitrary PowerShell action rejected.' }
        }
    }
    if (-not $noProfile -or $index + 1 -ge $argv.Count -or $argv[$index + 1] -ine $CollectorPath) {
        throw 'Action must use -NoProfile -File for the expected collector.'
    }
    foreach ($argument in @($argv | Select-Object -Skip ($index + 2))) {
        if ($argument.StartsWith('-')) {
            $name = ($argument.Substring(1) -split ':', 2)[0]
            if ($name -and 'Token'.StartsWith($name, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Literal credentials cannot be persisted in task artifacts.'
            }
        }
    }
    [pscustomobject]@{ Node = $action; Arguments = $arguments.InnerText; WorkingDirectory = $working.InnerText }
}

function Get-CopilotMetricsTaskHostSource {
    param(
        [Parameter(Mandatory)][string]$PowerShellPath,
        [Parameter(Mandatory)][string]$Arguments,
        [Parameter(Mandatory)][string]$WorkingDirectory
    )
    $source = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Text;
public static class CopilotMetricsTaskHost {
    static string Decode(string s) { return Encoding.UTF8.GetString(Convert.FromBase64String(s)); }
    public static ProcessStartInfo CreateStartInfo() {
        return new ProcessStartInfo {
            FileName = Decode("__POWERSHELL__"),
            Arguments = "-NoLogo -NonInteractive " + Decode("__ARGUMENTS__"),
            WorkingDirectory = Decode("__WORKING__"),
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
    }
    [STAThread]
    public static int Main(string[] args) {
        if (args.Length != 0) return 2;
        try {
            using (var process = new Process { StartInfo = CreateStartInfo() }) {
                if (!process.Start()) return 1;
                process.StandardInput.Close();
                var stdout = process.StandardOutput.BaseStream.CopyToAsync(Stream.Null);
                var stderr = process.StandardError.BaseStream.CopyToAsync(Stream.Null);
                process.WaitForExit();
                System.Threading.Tasks.Task.WaitAll(stdout, stderr);
                return process.ExitCode;
            }
        } catch { return 1; }
    }
}
'@
    foreach ($entry in @{
        '__POWERSHELL__' = $PowerShellPath
        '__ARGUMENTS__' = $Arguments
        '__WORKING__' = $WorkingDirectory
    }.GetEnumerator()) {
        $source = $source.Replace($entry.Key, [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($entry.Value)))
    }
    $source
}

function New-CopilotMetricsCompilerStartInfo {
    param(
        [Parameter(Mandatory)][string]$SourcePath,
        [Parameter(Mandatory)][string]$OutputPath,
        [ValidateSet('winexe', 'exe')][string]$Target = 'winexe'
    )
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path ([Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()) 'csc.exe'
    $start.Arguments = '/nologo /noconfig /nowin32manifest /errorreport:none /reference:' +
        (ConvertTo-CopilotProcessArgument (Join-Path ([Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()) 'System.dll')) +
        ' /target:' + $Target + ' /out:' +
        (ConvertTo-CopilotProcessArgument $OutputPath) + ' ' + (ConvertTo-CopilotProcessArgument $SourcePath)
    $start.WorkingDirectory = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($OutputPath))
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start
}

function Invoke-CopilotMetricsCompilation {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$OutputPath,
        [ValidateSet('winexe', 'exe')][string]$Target = 'winexe'
    )
    if ($env:OS -ne 'Windows_NT' -or $PSVersionTable.PSEdition -ne 'Desktop') {
        throw 'Compile with Windows PowerShell 5.1 and its .NET Framework compiler.'
    }
    if (Test-Path -LiteralPath $OutputPath) { throw 'Compiler output already exists.' }
    $sourcePath = "$OutputPath.cs"
    if (Test-Path -LiteralPath $sourcePath) { throw 'Compiler source already exists.' }
    $start = New-CopilotMetricsCompilerStartInfo -SourcePath $sourcePath -OutputPath $OutputPath -Target $Target
    $process = [Diagnostics.Process]::new()
    try {
        [IO.File]::WriteAllText($sourcePath, $Source)
        $process.StartInfo = $start
        [void]$process.Start()
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $diagnostics = $stdout.GetAwaiter().GetResult() + $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "Task host compilation failed: $diagnostics" }
    }
    finally {
        $process.Dispose()
        if (Test-Path -LiteralPath $sourcePath) { Remove-Item -LiteralPath $sourcePath -Force }
    }
}

Export-ModuleMember -Function Split-CopilotWindowsArgument, Get-CopilotMetricsTaskAction, Get-CopilotMetricsTaskHostSource, New-CopilotMetricsCompilerStartInfo, Invoke-CopilotMetricsCompilation
