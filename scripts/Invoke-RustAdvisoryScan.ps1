[CmdletBinding()]
param(
    [string]$Lockfile = (Join-Path $PSScriptRoot '../Cargo.lock'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/security/rust'),
    [string]$DatabasePath = (Join-Path ([IO.Path]::GetTempPath()) 'codexbar-rustsec-advisory-db'),
    [string]$AuditPath = 'cargo-audit'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Native failures are captured below so their reports survive for diagnosis.
$PSNativeCommandUseErrorActionPreference = $false

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$reportPath = Join-Path $OutputDirectory 'cargo-audit.json'
$errorPath = Join-Path $OutputDirectory 'cargo-audit.stderr.txt'
$versionPath = Join-Path $OutputDirectory 'cargo-audit-version.txt'
# A failed startup must never leave an earlier scan's evidence behind.
foreach ($path in @($reportPath, $errorPath, $versionPath)) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
}

$expectedVersion = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'rust-audit-version.txt') -Raw).Trim()
if ($expectedVersion -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid pinned cargo-audit version.' }
$actualVersion = (& $AuditPath --version) -join "`n"
if ($LASTEXITCODE -ne 0 -or $actualVersion.Trim() -ne "cargo-audit $expectedVersion") {
    throw "Expected cargo-audit $expectedVersion; got $actualVersion"
}
Set-Content -LiteralPath $versionPath -Value $actualVersion -Encoding utf8NoBOM
$Lockfile = (Resolve-Path -LiteralPath $Lockfile).Path

# Always use the repository policy, not a config inherited from the caller's cwd.
Push-Location (Join-Path $PSScriptRoot '..')
try {
    $json = (& $AuditPath audit --file $Lockfile --db $DatabasePath --json --deny unsound 2> $errorPath) -join "`n"
    $scanExit = $LASTEXITCODE
    Set-Content -LiteralPath $reportPath -Value $json -Encoding utf8NoBOM
    if ([string]::IsNullOrWhiteSpace($json)) { throw "cargo-audit produced no JSON report (exit $scanExit). See $errorPath." }
    $report = $json | ConvertFrom-Json -AsHashtable
    if ($report -isnot [System.Collections.IDictionary] -or
        -not $report.ContainsKey('database') -or -not $report.ContainsKey('vulnerabilities') -or
        -not $report.ContainsKey('warnings') -or
        $report.database.'last-commit' -notmatch '^[0-9a-f]{40}$' -or
        ($report.vulnerabilities.count -isnot [long] -and $report.vulnerabilities.count -isnot [int])) {
        throw 'cargo-audit did not produce a complete report with an advisory database revision.'
    }
    Write-Output "$actualVersion; RustSec database $($report.database.'last-commit')"
    Write-Output "Vulnerabilities: $($report.vulnerabilities.count). Reports: $OutputDirectory"
    if ($scanExit -ne 0 -or $report.vulnerabilities.found -or
        ($report.warnings.ContainsKey('unsound') -and @($report.warnings.unsound).Count -gt 0)) {
        throw "Rust dependency advisory scan failed (cargo-audit exit $scanExit). See $reportPath and $errorPath."
    }
} finally {
    Pop-Location
}
