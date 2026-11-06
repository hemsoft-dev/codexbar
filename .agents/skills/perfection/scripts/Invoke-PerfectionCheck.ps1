#Requires -Version 7
<#
.SYNOPSIS
    Runs the 7 quality gates of the retired C# / WPF app (src/) and outputs a dashboard.
.DESCRIPTION
    Covers only the retired C# app under src/; it runs no Rust gate. The Rust
    product's gates are listed in AGENTS.md and run in CI.

    Executes build, format check, tests with coverage, ReportGenerator summary,
    CRAP score analysis, security audit, and markdown lint. Before running gates,
    it removes stale WPF temporary build files and the ignored coverage output
    (TestResults/ and CoverageReport/) so the coverage glob only sees the
    current run. It never stops or
    starts a process: if the repo-local Debug CodexBar.App is running, it reports
    that and exits with code 2 before any gate runs.
#>

[CmdletBinding()]
param(
    [switch]$Fix
)

$InformationPreference = 'Continue'
$ErrorActionPreference = 'Stop'
$repoRoot = Resolve-Path (Join-Path -Path $PSScriptRoot -ChildPath '..\..\..\..')
Set-Location $repoRoot

function Write-Section {
    param([string]$Name)

    Write-Information ""
    Write-Information "`e[36m=== $Name ===`e[0m"
}

function Write-Gate {
    param(
        [string]$Name,
        [bool]$Pass,
        [string]$Detail
    )

    $status = if ($Pass) { 'PASS' } else { 'FAIL' }
    Write-Information "$status $Name : $Detail"
}

function Test-IsRepoChildPath {
    param([string]$Path)

    $resolvedPath = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path.TrimEnd('\', '/')
    $root = $repoRoot.Path.TrimEnd('\', '/')
    $isRepoChild =
        [string]::Equals($resolvedPath, $root, [StringComparison]::OrdinalIgnoreCase) -or
        $resolvedPath.StartsWith("$root\", [StringComparison]::OrdinalIgnoreCase) -or
        $resolvedPath.StartsWith("$root/", [StringComparison]::OrdinalIgnoreCase)

    if (-not $isRepoChild) {
        return $false
    }

    $item = Get-Item -LiteralPath $resolvedPath -Force -ErrorAction Stop
    return ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
}

function Get-RepoLocalCodexBarProcess {
    $appExe = Join-Path -Path $repoRoot -ChildPath 'src\CodexBar.App\bin\Debug\net9.0-windows\CodexBar.App.exe'
    $processes = Get-Process -Name 'CodexBar.App' -ErrorAction SilentlyContinue

    foreach ($process in $processes) {
        $path = $null
        try {
            $path = $process.Path
        }
        catch {
            $null = $_
        }

        if ($path -and [string]::Equals($path, $appExe, [StringComparison]::OrdinalIgnoreCase)) {
            $process
        }
    }
}

function Assert-NoRepoLocalCodexBarProcess {
    $processes = @(Get-RepoLocalCodexBarProcess)
    if ($processes.Count -eq 0) {
        return
    }

    Write-Information "Preflight: the repo-local CodexBar.App is running (process $($processes.Id -join ', ')) and would lock its build output."
    Write-Information 'Preflight: this audit does not stop it. Close the app and run the audit again.'
    exit 2
}

function Invoke-WpfTemporaryArtifactPreflight {
    $scanRoots = @(
        (Join-Path -Path $repoRoot -ChildPath 'src\CodexBar.App')
        (Join-Path -Path $repoRoot -ChildPath 'src\CodexBar.App\obj\Debug\net9.0-windows')
    ) | Where-Object { Test-Path -LiteralPath $_ }

    if ($scanRoots.Count -eq 0) {
        return 0
    }

    $items = foreach ($scanRoot in $scanRoots) {
        if (-not (Test-IsRepoChildPath -Path $scanRoot)) {
            throw "Refusing to clean WPF temp artifacts outside the repo: $scanRoot"
        }

        Get-ChildItem -LiteralPath $scanRoot -Filter '*_wpftmp*' -Force -ErrorAction SilentlyContinue
    }

    foreach ($item in $items) {
        if (-not (Test-IsRepoChildPath -Path $item.FullName)) {
            throw "Refusing to remove WPF temp artifact outside the repo: $($item.FullName)"
        }

        Remove-Item -LiteralPath $item.FullName -Force
    }

    $items.Count
}

function Invoke-CoverageArtifactPreflight {
    $targets = @(
        'src\CodexBar.Core.Tests\TestResults',
        'src\CodexBar.App.Tests\TestResults',
        'CoverageReport'
    )

    $removed = 0
    foreach ($target in $targets) {
        $path = Join-Path -Path $repoRoot -ChildPath $target
        if (-not (Test-Path -LiteralPath $path)) {
            continue
        }

        if (-not (Test-IsRepoChildPath -Path $path)) {
            throw "Refusing to clean coverage artifacts outside the repo: $path"
        }

        Remove-Item -LiteralPath $path -Recurse -Force
        $removed++
    }

    $removed
}

function Get-CrapRow {
    param([System.IO.FileInfo[]]$CoverageFiles)

    foreach ($file in $CoverageFiles) {
        [xml]$coverage = Get-Content -LiteralPath $file.FullName
        foreach ($package in @($coverage.coverage.packages.package)) {
            $includePackage =
                ($file.FullName -like '*CodexBar.Core.Tests*' -and $package.name -eq 'CodexBar.Core') -or
                ($file.FullName -like '*CodexBar.App.Tests*' -and $package.name -eq 'CodexBar.App')

            if (-not $includePackage) {
                continue
            }

            foreach ($class in @($package.classes.class)) {
                $sourceFile = [string]$class.filename
                if ($sourceFile -like '*.g.cs' -or $sourceFile -like '*GeneratedRegex*.cs') {
                    continue
                }

                foreach ($method in @($class.methods.method)) {
                    $complexity = [double]$method.complexity
                    $lineCoverage = [double]$method.'line-rate'
                    $crap = ([math]::Pow($complexity, 2) * [math]::Pow(1 - $lineCoverage, 3)) + $complexity

                    [pscustomobject]@{
                        Class        = $class.name
                        Method       = $method.name
                        Complexity   = $complexity
                        LineCoverage = $lineCoverage
                        Crap         = [math]::Round($crap, 2)
                    }
                }
            }
        }
    }
}

$passCount = 0
$totalGates = 7

Write-Information 'Scope: the retired C# / WPF app under src/ only. This audit runs no Rust gate; see AGENTS.md.'
Write-Section -Name 'Preflight'
Assert-NoRepoLocalCodexBarProcess
$removedCoverageTargets = Invoke-CoverageArtifactPreflight
$removedWpfArtifacts = Invoke-WpfTemporaryArtifactPreflight
Write-Information "Preflight: removed $removedCoverageTargets stale coverage target(s)."
Write-Information "Preflight: removed $removedWpfArtifacts stale WPF temporary artifact(s)."

# 1. Build
Write-Section -Name 'Build'
$buildOutput = & dotnet build --verbosity minimal -p:TreatWarningsAsErrors=true 2>&1
$buildExitCode = $LASTEXITCODE
$buildPass = $buildExitCode -eq 0
if (-not $buildPass) {
    Write-Information ($buildOutput | Out-String)
}

Write-Gate -Name 'Build' -Pass $buildPass -Detail $(if ($buildPass) { '0 warnings' } else { 'Build failed or emitted warnings' })
if ($buildPass) { $passCount++ }

# 2. Format
Write-Section -Name 'Format'
$solutionPath = Join-Path -Path $repoRoot -ChildPath 'CodexBar.slnx'
if ($Fix) {
    $null = & dotnet format $solutionPath --verbosity minimal 2>&1
}
else {
    $null = & dotnet format $solutionPath --verify-no-changes --verbosity minimal 2>&1
}

$formatPass = $LASTEXITCODE -eq 0
Write-Gate -Name 'Format' -Pass $formatPass -Detail $(if ($formatPass) { 'Clean' } else { 'Violations found' })
if ($formatPass) { $passCount++ }

# 3-4. Tests + Coverage (Line and Branch as separate gates)
Write-Section -Name 'Tests + Coverage'
$removedWpfArtifacts = Invoke-WpfTemporaryArtifactPreflight
Write-Information "Preflight: removed $removedWpfArtifacts WPF temporary artifact(s) immediately before coverage collection."

$runsettings = Join-Path -Path $repoRoot -ChildPath 'src\CodexBar.Core.Tests\coverage.runsettings'
$testOutput = & dotnet test --collect:"XPlat Code Coverage" --settings $runsettings --verbosity minimal 2>&1
$testPass = $LASTEXITCODE -eq 0
if ($testPass) {
    Write-Information 'Tests: dotnet test completed successfully for the solution.'
}
else {
    Write-Information ($testOutput | Select-Object -Last 20 | Out-String)
}

$coverageFiles = @(Get-ChildItem -Path $repoRoot -Recurse -Filter 'coverage.cobertura.xml' -ErrorAction SilentlyContinue)
Write-Information "Coverage: found $($coverageFiles.Count) Cobertura file(s) for the current run."
$coverageArtifactPass = $coverageFiles.Count -eq 2
if (-not $coverageArtifactPass) {
    Write-Information 'Coverage: expected exactly 2 Cobertura files, one per test assembly.'
}

$coverageReportPath = Join-Path -Path $repoRoot -ChildPath 'CoverageReport'
$reportOutput = & reportgenerator -reports:"**/coverage.cobertura.xml" -targetdir:$coverageReportPath -reporttypes:JsonSummary -filefilters:"-**/*.g.cs;-**/GeneratedRegex*.cs" 2>&1
$reportPass = ($LASTEXITCODE -eq 0) -and $coverageArtifactPass
if (-not $reportPass) {
    Write-Information ($reportOutput | Select-Object -Last 20 | Out-String)
}

$summaryPath = Join-Path -Path $coverageReportPath -ChildPath 'Summary.json'
$lineRate = 0.0
$branchRate = 0.0
if (Test-Path -LiteralPath $summaryPath) {
    $summary = Get-Content -LiteralPath $summaryPath -Raw | ConvertFrom-Json
    $lineRate = [double]$summary.summary.linecoverage / 100
    $branchRate = [double]$summary.summary.branchcoverage / 100
}

$lineCovPass = $testPass -and $reportPass -and ($lineRate -ge 1.0)
Write-Gate -Name 'Line Coverage' -Pass $lineCovPass -Detail ('{0:P1}' -f $lineRate)
if ($lineCovPass) { $passCount++ }

$branchCovPass = $testPass -and $reportPass -and ($branchRate -ge 1.0)
Write-Gate -Name 'Branch Coverage' -Pass $branchCovPass -Detail ('{0:P1}' -f $branchRate)
if ($branchCovPass) { $passCount++ }

# 5. CRAP Score
Write-Section -Name 'CRAP Score'
$crapRows = @(Get-CrapRow -CoverageFiles $coverageFiles)
$avgCrap = if ($crapRows.Count -gt 0) { [math]::Round((($crapRows | Measure-Object -Property Crap -Average).Average), 2) } else { 0 }
$overThirty = @($crapRows | Where-Object { $_.Crap -gt 30 } | Sort-Object -Property Crap -Descending)
$crapPass = ($overThirty.Count -eq 0) -and ($avgCrap -le 4.0)
$crapDetail = "$($overThirty.Count) methods > 30, avg $avgCrap"
if ($overThirty.Count -gt 0) {
    $topMethods = $overThirty |
        Select-Object -First 5 -Property Class, Method, Crap |
        Format-Table -AutoSize |
        Out-String
    Write-Information $topMethods.TrimEnd()
}

Write-Gate -Name 'CRAP Score' -Pass $crapPass -Detail $crapDetail
if ($crapPass) { $passCount++ }

# 6. Security Audit
Write-Section -Name 'Security Audit'
$secOutput = & dotnet list package --vulnerable --include-transitive 2>&1
$secExitCode = $LASTEXITCODE
$hasVulnerablePackages = ($secOutput | Select-String 'has the following vulnerable packages').Count -gt 0
$secPass = ($secExitCode -eq 0) -and -not $hasVulnerablePackages
Write-Gate -Name 'Security' -Pass $secPass -Detail $(if ($secPass) { '0 vulnerabilities' } else { 'Vulnerabilities found or audit failed' })
if ($secPass) { $passCount++ }

# 7. Markdown Lint
Write-Section -Name 'Markdown Lint'
$mdLintOutput = & npm run lint:md 2>&1
$mdPass = $LASTEXITCODE -eq 0
if (-not $mdPass) {
    Write-Information ($mdLintOutput | Select-Object -Last 20 | Out-String)
}

Write-Gate -Name 'Markdown' -Pass $mdPass -Detail $(if ($mdPass) { 'Clean' } else { 'Violations found' })
if ($mdPass) { $passCount++ }

# Summary
Write-Information ''
Write-Information "`e[33m========================================`e[0m"
Write-Information "Perfection Score: $passCount / $totalGates gates passing"
Write-Information "`e[33m========================================`e[0m"

exit $(if ($passCount -eq $totalGates) { 0 } else { 1 })
