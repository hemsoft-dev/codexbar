# Local fixture replay. Windows native auth/runtime probes remain separately
# opt-in. These HTTP fixtures never make provider requests or use real tokens.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$stage = Join-Path ([IO.Path]::GetTempPath()) ('metrics-collector-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $stage)
$oldTokens = @{}
$oldPath = $env:PATH
$env:PATH = $stage # No real gh or credential helper is reachable in fixture replay.
$fixtureContext = @{ Count = 0 }
function Assert-Collector([bool]$Condition, [string]$Message) {
    $fixtureContext.Count++
    if (-not $Condition) { throw $Message }
}
foreach ($key in @('EnterpriseBillingToken', 'GH_TOKEN', 'GITHUB_TOKEN')) {
    $oldTokens[$key] = [Environment]::GetEnvironmentVariable($key)
    [Environment]::SetEnvironmentVariable($key, $null)
}
try {
    function Invoke-WebRequest {
        param($Uri, $Headers, $Method, [switch]$UseBasicParsing)
        Assert-Collector ($Headers.Authorization -eq "Bearer $expectedToken") 'Token precedence changed.'
        Assert-Collector ($Uri -like 'https://api.github.com/enterprises/fixture/settings/billing/ai_credit/usage?*') 'The enterprise API source must remain unchanged.'
        [pscustomobject]@{ StatusCode = 200; Content = '{"usageItems":[{"model":"Fixture","grossQuantity":12,"grossAmount":0.12,"netAmount":0.1}]}' }
    }
    function Invoke-RestMethod { throw 'Unexpected seat enumeration in explicit-user fixture.' }
    $collector = Join-Path $root 'copilot-metrics.ps1'
    $data = Join-Path $stage 'data'
    foreach ($source in @('GITHUB_TOKEN', 'GH_TOKEN', 'EnterpriseBillingToken', 'explicit')) {
        $expectedToken = "fixture-$source"
        if ($source -ne 'explicit') { [Environment]::SetEnvironmentVariable($source, $expectedToken) }
        $parameters = @{ Refresh=$true; Enterprise='fixture'; Org='fixture'; User=@('fixture'); Delay=0; DataDir=$data }
        if ($source -eq 'explicit') { $parameters.Token = $expectedToken }
        # Do not impose the test's inherited StrictMode on the unchanged CLI.
        & { Set-StrictMode -Off; & $collector @parameters 6>$null | Out-Null }
        $saved = Get-Content (Join-Path $data 'copilot-metrics.json') -Raw | ConvertFrom-Json
        Assert-Collector ($saved.RefreshStatus -eq 'Complete' -and $saved.Users[0].Success) 'Monthly writes must retain successful per-user checkpoints.'
        Assert-Collector ($saved.Users[0].Responses[0].Response.usageItems[0].grossQuantity -eq 12) 'The metrics payload cannot change.'
    }
    $before = (Get-FileHash (Join-Path $data 'copilot-metrics.json')).Hash
    foreach ($key in $oldTokens.Keys) { [Environment]::SetEnvironmentVariable($key, $null) }
    & { Set-StrictMode -Off; & $collector -InputPath (Join-Path $data 'copilot-metrics.json') 6>$null | Out-Null }
    Assert-Collector ((Get-FileHash (Join-Path $data 'copilot-metrics.json')).Hash -eq $before) 'Saved-input mode cannot authenticate, call HTTP or mutate the snapshot.'
    Write-Output "Passed $($fixtureContext.Count) collector replay assertions with synthetic credentials and HTTP fixtures."
}
finally {
    $env:PATH = $oldPath
    foreach ($key in $oldTokens.Keys) { [Environment]::SetEnvironmentVariable($key, $oldTokens[$key]) }
    Remove-Item -LiteralPath $stage -Recurse -Force
}
