# Integration test against the real pinned scanner and the current RustSec database.
[CmdletBinding()]
param(
    [string]$DatabasePath = (Join-Path ([IO.Path]::GetTempPath()) 'codexbar-rustsec-advisory-db'),
    [string]$AuditPath = 'cargo-audit'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$output = Join-Path ([IO.Path]::GetTempPath()) "codexbar-advisory-negative-$([guid]::NewGuid())"
$scanner = Join-Path $root 'scripts/Invoke-RustAdvisoryScan.ps1'
$fixture = Join-Path $PSScriptRoot 'fixtures/rust-advisory/Cargo.lock'
$pwsh = (Get-Process -Id $PID).Path

try {
    & $pwsh -NoProfile -File $scanner -Lockfile $fixture -OutputDirectory $output -DatabasePath $DatabasePath -AuditPath $AuditPath
    if ($LASTEXITCODE -eq 0) { throw 'The known-vulnerable lockfile unexpectedly passed.' }
    # Nonzero alone could be a missing executable, network error, or malformed fixture.
    $report = Get-Content -LiteralPath (Join-Path $output 'cargo-audit.json') -Raw | ConvertFrom-Json
    $finding = @($report.vulnerabilities.list | Where-Object {
        $_.advisory.id -eq 'RUSTSEC-2024-0421' -and $_.package.name -eq 'idna' -and $_.package.version -eq '0.5.0'
    })
    if ($finding.Count -ne 1) { throw 'The scanner did not detect the expected idna advisory.' }

    # Seed the same output directory with the previous valid negative report.
    # A startup error must remove it, rather than qualifying stale evidence.
    & $pwsh -NoProfile -File $scanner -Lockfile $fixture -OutputDirectory $output -AuditPath (Join-Path $output 'missing-cargo-audit')
    if ($LASTEXITCODE -eq 0 -or (Test-Path -LiteralPath (Join-Path $output 'cargo-audit.json'))) {
        throw 'Scanner startup failure reused stale evidence.'
    }
    Write-Output 'PASS: actual idna advisory blocks the scan; failed startup cannot reuse stale evidence.'
} finally {
    if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Recurse -Force }
}
