$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$modulePath = Join-Path (Split-Path -Parent $PSScriptRoot) 'CopilotMetrics.Core.psm1'
Import-Module $modulePath -Force

$script:testCount = 0

function Assert-Equal {
    param(
        [Parameter(Mandatory)]$Expected,
        [Parameter(Mandatory)]$Actual,
        [Parameter(Mandatory)][string]$Because
    )

    $script:testCount++
    if ($Expected -ne $Actual) {
        throw "Assertion failed: $Because. Expected '$Expected', got '$Actual'."
    }
}

function Assert-True {
    param(
        [Parameter(Mandatory)][bool]$Value,
        [Parameter(Mandatory)][string]$Because
    )

    Assert-Equal -Expected $true -Actual $Value -Because $Because
}

function Assert-False {
    param(
        [Parameter(Mandatory)][bool]$Value,
        [Parameter(Mandatory)][string]$Because
    )

    Assert-Equal -Expected $false -Actual $Value -Because $Because
}

$strategy = Get-CopilotMetricsCollectionStrategy
Assert-Equal -Expected 'monthly-ai-credit-v1' -Actual $strategy -Because 'the cache strategy is explicit and versioned'

$path = Get-CopilotAICreditUsagePath `
    -Enterprise 'example enterprise' `
    -Year 2026 `
    -Month 9 `
    -Login 'octo user' `
    -Organization 'example org'
Assert-Equal `
    -Expected '/enterprises/example%20enterprise/settings/billing/ai_credit/usage?month=9&organization=example%20org&product=Copilot&user=octo%20user&year=2026' `
    -Actual $path `
    -Because 'monthly AI Credit requests include encoded scope and user filters'

$unfilteredPath = Get-CopilotAICreditUsagePath `
    -Enterprise 'example' `
    -Year 2026 `
    -Month 9 `
    -Login 'octocat'
Assert-False -Value $unfilteredPath.Contains('organization=') -Because 'organization filtering remains opt-in'

$legacyCache = [pscustomobject]@{
    Enterprise                = 'example'
    Organization              = 'org'
    Year                      = 2026
    Month                     = 9
    ApiVersion                = '2026-03-10'
    IncludeOrganizationFilter = $false
}
Assert-False -Value (Test-CopilotMetricsCacheScope `
        -Cache $legacyCache `
        -Enterprise 'example' `
        -Organization 'org' `
        -Year 2026 `
        -Month 9 `
        -ApiVersion '2026-03-10' `
        -IncludeOrganizationFilter $false) `
    -Because 'legacy daily caches cannot satisfy the monthly strategy'

$currentCache = [pscustomobject]@{
    CollectionStrategy        = $strategy
    Enterprise                = 'example'
    Organization              = 'org'
    Year                      = 2026
    Month                     = 9
    ApiVersion                = '2026-03-10'
    IncludeOrganizationFilter = $false
}
Assert-True -Value (Test-CopilotMetricsCacheScope `
        -Cache $currentCache `
        -Enterprise 'example' `
        -Organization 'org' `
        -Year 2026 `
        -Month 9 `
        -ApiVersion '2026-03-10' `
        -IncludeOrganizationFilter $false) `
    -Because 'matching monthly caches remain resumable'

$raw = '{"usageItems":[{"model":"Model A","grossQuantity":12.5,"grossAmount":0.125,"netAmount":0.05},{"model":"Code Review model","grossQuantity":7.5,"grossAmount":0.075,"netAmount":0.02}]}'
$apiResponse = $raw | ConvertFrom-Json
$response = ConvertTo-CopilotMonthlyResponse `
    -Path '/billing' `
    -StatusCode 200 `
    -RawJson $raw `
    -Response $apiResponse `
    -RefreshRunId 'run-1' `
    -RefreshedAtUtc '2026-09-16T12:00:00Z'
$entry = ConvertTo-CopilotUserEntry `
    -Login 'octocat' `
    -RefreshRunId 'run-1' `
    -Response $response `
    -RefreshedAtUtc '2026-09-16T12:00:00Z'
$usage = ConvertTo-CopilotUserUsage -Login 'octocat' -Responses $entry.Responses -MonthlyAllowance 100
Assert-Equal -Expected 20 -Actual $usage.AICredits -Because 'monthly response items are summed once'
Assert-Equal -Expected 0.2 -Actual $usage.GrossCostUsd -Because 'monthly gross amounts are summed once'
Assert-Equal -Expected 'Model A' -Actual $usage.TopModel -Because 'the highest-quantity model is selected'
Assert-Equal -Expected '37.5%' -Actual $usage.PRReviewPct -Because 'code review share is calculated from monthly totals'

$lookup = @{ (ConvertTo-CopilotLoginKey -Login 'octocat') = $entry }
$freshPlan = @(Get-CopilotRefreshPlan -Login @('octocat') -UserLookup $lookup)
Assert-True -Value $freshPlan[0].ShouldFetch -Because 'a completed prior run is refreshed again for current month-to-date data'

$resumePlan = @(Get-CopilotRefreshPlan -Login @('octocat') -UserLookup $lookup -ResumeRefreshRunId 'run-1')
Assert-False -Value $resumePlan[0].ShouldFetch -Because 'a successful checkpoint from the same interrupted run is reused'
Assert-True -Value $resumePlan[0].ReusedFromCurrentRun -Because 'resume reuse is recorded explicitly'

$failedEntry = ConvertTo-CopilotUserEntry `
    -Login 'octocat' `
    -RefreshRunId 'run-2' `
    -ExistingEntry $entry `
    -ErrorMessage 'temporary failure' `
    -RefreshedAtUtc '2026-09-16T13:00:00Z'
Assert-False -Value $failedEntry.Success -Because 'a failed attempt remains retryable'
Assert-Equal -Expected 1 -Actual @($failedEntry.Responses).Count -Because 'a failed attempt preserves the prior monthly response'
Assert-Equal -Expected 20 -Actual (ConvertTo-CopilotUserUsage -Login 'octocat' -Responses $failedEntry.Responses).AICredits -Because 'preserved data does not become zero'

$failedLookup = @{ (ConvertTo-CopilotLoginKey -Login 'octocat') = $failedEntry }
$failedResumePlan = @(Get-CopilotRefreshPlan -Login @('octocat') -UserLookup $failedLookup -ResumeRefreshRunId 'run-2')
Assert-True -Value $failedResumePlan[0].ShouldFetch -Because 'failed checkpoints are retried when an interrupted run resumes'

$repairPlan = @(Get-CopilotRefreshPlan -Login @('octocat') -UserLookup $lookup -RepairOnly)
Assert-False -Value $repairPlan[0].ShouldFetch -Because 'repair mode reuses a valid monthly response'

$pendingEntry = ConvertTo-CopilotUserEntry `
    -Login 'pending-user' `
    -RefreshRunId '' `
    -ErrorMessage 'Missing from refresh.' `
    -RefreshedAtUtc '2026-09-18T05:00:00Z'
Assert-False -Value $pendingEntry.Success -Because 'a full-refresh checkpoint can represent users that have not been attempted yet'
Assert-Equal -Expected '' -Actual $pendingEntry.RefreshRunId -Because 'unattempted users are not attributed to the active refresh run'
Assert-Equal -Expected 0 -Actual @($pendingEntry.Responses).Count -Because 'an unattempted user has no fabricated response'

Write-Output "Passed $script:testCount Copilot metrics assertions."

& (Join-Path $PSScriptRoot 'CopilotMetrics.Process.Tests.ps1')
& (Join-Path $PSScriptRoot 'CopilotMetrics.Collector.Tests.ps1')
