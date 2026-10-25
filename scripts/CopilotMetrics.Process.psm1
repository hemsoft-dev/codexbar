Set-StrictMode -Version Latest

# ProcessStartInfo.Arguments also works on Windows PowerShell 5.1. Quote using
# the Windows argv rules rather than invoking cmd.exe or another shell.
function ConvertTo-CopilotProcessArgument {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Value)

    '"' + [regex]::Replace(
        [regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}

function New-CopilotTokenStartInfo {
    param([string]$UserName)

    # Resolve an application, never an alias, function, .cmd wrapper or script.
    $command = Get-Command -Name $(if ($env:OS -eq 'Windows_NT') { 'gh.exe' } else { 'gh' }) `
        -CommandType Application -ErrorAction Stop | Select-Object -First 1
    if ($env:OS -eq 'Windows_NT' -and [IO.Path]::GetExtension($command.Source) -ne '.exe') {
        throw 'GitHub CLI must be a native executable.'
    }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $command.Source
    $start.Arguments = 'auth token'
    if ($UserName) { $start.Arguments += ' --user ' + (ConvertTo-CopilotProcessArgument $UserName) }
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables['GH_PROMPT_DISABLED'] = '1'
    $start
}

function Get-CopilotGitHubCliToken {
    param([string]$UserName)

    $process = [Diagnostics.Process]::new()
    try {
        $process.StartInfo = New-CopilotTokenStartInfo -UserName $UserName
        [void]$process.Start()
        $process.StandardInput.Close()
        # Drain both pipes concurrently; neither diagnostics nor tokens go to
        # the host's output streams, including on a failed authentication read.
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $token = $stdout.GetAwaiter().GetResult().Trim()
        [void]$stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -eq 0 -and $token) {
            return [pscustomobject]@{
                Token = $token
                Source = $(if ($UserName) { "gh auth token --user $UserName" } else { 'gh auth token for the active account' })
            }
        }
    }
    catch {
        # Do not propagate native output, command lines or token material.
        $null = $_
    }
    finally {
        $process.Dispose()
    }
    if ($UserName) {
        throw "No GitHub CLI token found for -GitHubUser $UserName. Run 'gh auth login --user $UserName' or pass -Token."
    }
    $null
}

Export-ModuleMember -Function ConvertTo-CopilotProcessArgument, New-CopilotTokenStartInfo, Get-CopilotGitHubCliToken
