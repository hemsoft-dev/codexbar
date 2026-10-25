param(
    [string]$Enterprise = 'bertelsmann',
    [string]$Org = 'Relias-Engineering',
    [int]$Year = (Get-Date).Year,
    [int]$Month = (Get-Date).Month,
    [int]$Top = 25,
    [string[]]$User,
    [string]$ApiVersion = '2026-03-10',
    [int]$Delay = 5,
    [int]$DataRetentionDays = 3,
    [string]$Token,
    [string]$GitHubUser,
    [string]$DataDir = (Join-Path (Split-Path -Parent $PSScriptRoot) 'data'),
    [string]$RunStamp = (Get-Date -Format 'yyyyMMdd-HHmmss'),
    [string]$InputPath,
    [string]$RepairInputFile,
    [switch]$Refresh,
    [switch]$IncludeOrganizationFilter,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$InformationPreference = 'Continue'

Import-Module (Join-Path $PSScriptRoot 'CopilotMetrics.Core.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'CopilotMetrics.Process.psm1') -Force

$enterpriseName = $Enterprise
$yearValue = $Year
$monthValue = $Month
$dataDirValue = $DataDir
$runStampValue = $RunStamp
$inputPathValue = $InputPath
$repairInputFileValue = $RepairInputFile
$refreshValue = $Refresh.IsPresent
$includeOrganizationFilterValue = $IncludeOrganizationFilter.IsPresent
$githubUserValue = $GitHubUser
$delaySecondsValue = $Delay
$dataRetentionDaysValue = $DataRetentionDays
$usingDefaultInputPath = $false
$tokenSource = if ($Token) { '-Token' } else { $null }
$collectionStrategy = Get-CopilotMetricsCollectionStrategy
$defaultMonthlyAICreditAllowance = 7000
$specialMonthlyAICreditAllowanceByUser = @{
    fhemmerrelias = 250000
}

function Get-EasternTimeZone {
    foreach ($timeZoneId in @('Eastern Standard Time', 'America/New_York')) {
        try {
            return [TimeZoneInfo]::FindSystemTimeZoneById($timeZoneId)
        }
        catch [TimeZoneNotFoundException] {
            $null = $_
        }
        catch [InvalidTimeZoneException] {
            $null = $_
        }
    }

    throw 'Could not resolve Eastern Time zone.'
}

function Get-EasternTimestamp {
    $easternTimeZone = Get-EasternTimeZone
    $easternNow = [TimeZoneInfo]::ConvertTime([DateTimeOffset]::UtcNow, $easternTimeZone)
    $easternNow.ToString('dddd, MMMM d, yyyy h:mm tt')
}

function Write-MetricsSection {
    param([Parameter(Mandatory)][string]$Title)

    Write-Information ''
    Write-Information "== $Title =="
}

function Write-MetricsDetail {
    param(
        [Parameter(Mandatory)][string]$Label,
        [Parameter(Mandatory)][AllowEmptyString()][string]$Value
    )

    Write-Information ('  {0,-14} {1}' -f "$($Label):", $Value)
}

function Show-CopilotMetricsHeader {
    Write-Information ''
    Write-Information 'Copilot Metrics'
    Write-Information '---------------'
    Write-MetricsDetail -Label 'Displayed' -Value "$(Get-EasternTimestamp) Eastern Time"
    Write-MetricsDetail -Label 'Copyright' -Value '2024 Relias LLC'
}

Show-CopilotMetricsHeader

if ($Help) {
    $scriptName = Split-Path -Leaf $PSCommandPath
    @"
$scriptName - GitHub Copilot billing metrics

USAGE
  .\scripts\$scriptName
  .\scripts\$scriptName -Top 10
  .\scripts\$scriptName -InputPath .\data\copilot-metrics.json
  .\scripts\$scriptName -Refresh -Year 2026 -Month 6
  .\scripts\$scriptName -Refresh -Org Relias-Engineering -Top 25
  .\scripts\$scriptName -Refresh -GitHubUser fhemmerrelias -Top 25
  .\scripts\$scriptName -Refresh -User fhemmerrelias
  .\scripts\$scriptName -RepairInputFile .\data\copilot-metrics.json

DEFAULT MODE
  With no arguments, reads and ranks the latest saved monthly snapshot from:
    .\data\copilot-metrics.json

  If copilot-metrics.json is missing, falls back to legacy saved runs:
    .\data\top10.json
    .\data\top10-*.json

  Default and -InputPath modes do not call GitHub or modify data files.

REFRESH MODE
  -Refresh enumerates current Copilot seats, requests one month-to-date AI
  Credit response per user from the enterprise ai_credit/usage endpoint,
  checkpoints every attempted user, and atomically replaces:
    .\data\copilot-metrics.json

  A completed run fetches every selected user again so GitHub backfill is never
  hidden behind a successful older cache entry. An interrupted run resumes its
  existing RefreshRunId and reuses only successful users already checkpointed
  by that same run.

SCOPE PARAMETERS
  -Enterprise <slug>       GitHub enterprise slug. Default: bertelsmann.
  -Org <name>              Organization used to enumerate Copilot seats.
                           Default: Relias-Engineering.
  -IncludeOrganizationFilter
                           Adds organization=<Org> to each enterprise AI Credit
                           query. By default, users are scoped by current org
                           seats while usage remains enterprise-scoped.
  -GitHubUser <login>      GitHub CLI account used when token environment
                           variables are not set. Default: fhemmerrelias for
                           the default Relias-Engineering scope.

PERIOD PARAMETERS
  -Year <yyyy>             Billing year. Default: current local year.
  -Month <1-12>            Billing month. Default: current local month.

USER PARAMETERS
  -Top <n>                 Number of ranked users to display. Default: 25.
  -User <login[]>          Optional explicit user list. Omit to enumerate all
                           current Copilot seat assignees in -Org.

DATA PARAMETERS
  -InputPath <file>        Read and rank a saved JSON run without calling GitHub.
  -Refresh                 Query GitHub and write a fresh monthly snapshot.
  -RepairInputFile <file>  Repair missing or failed monthly user responses.
                           Legacy daily caches are incompatible and are fully
                           recollected through the monthly endpoint.
  -Delay <seconds>         Minimum delay between GitHub API calls. Default: 5.
  -DataRetentionDays <n>   Days to retain old generated files. Default: 3.
  -DataDir <path>          Output folder. Default: .\data.
  -RunStamp <text>         Temporary save filename suffix.

OUTPUT CONTRACT
  Refresh and repair output uses CollectionStrategy=$collectionStrategy.
  Failed attempts remain retryable. When prior monthly data exists, a failed
  attempt keeps those response items but marks the user failed and records the
  error, so consumers do not silently replace known data with zero.

AUTHENTICATION
  Token lookup order:
    1. -Token
    2. EnterpriseBillingToken
    3. GH_TOKEN
    4. GITHUB_TOKEN
    5. gh auth token --user <GitHubUser>
    6. gh auth token for the active account
"@
    return
}

if ($delaySecondsValue -lt 0) {
    throw '-Delay must be 0 or greater.'
}
if ($dataRetentionDaysValue -lt 0) {
    throw '-DataRetentionDays must be 0 or greater.'
}
if ($yearValue -lt 2000 -or $yearValue -gt 9999) {
    throw '-Year must be a four-digit year.'
}
if ($monthValue -lt 1 -or $monthValue -gt 12) {
    throw '-Month must be between 1 and 12.'
}
if ($inputPathValue -and $repairInputFileValue) {
    throw 'Use either -InputPath or -RepairInputFile, not both.'
}
if ($refreshValue -and ($inputPathValue -or $repairInputFileValue)) {
    throw 'Use -Refresh without -InputPath or -RepairInputFile.'
}
if (-not $refreshValue -and -not $inputPathValue -and -not $repairInputFileValue) {
    $inputPathValue = Join-Path $dataDirValue 'copilot-metrics.json'
    $usingDefaultInputPath = $true
}

function Get-DefaultGitHubUser {
    if ($enterpriseName -eq 'bertelsmann' -and $Org -eq 'Relias-Engineering') {
        return 'fhemmerrelias'
    }

    $null
}

if (-not $githubUserValue) {
    $githubUserValue = Get-DefaultGitHubUser
}
if (-not $inputPathValue -and -not $Token -and $env:EnterpriseBillingToken) {
    $Token = $env:EnterpriseBillingToken
    $tokenSource = 'EnterpriseBillingToken'
}
if (-not $inputPathValue -and -not $Token -and $env:GH_TOKEN) {
    $Token = $env:GH_TOKEN
    $tokenSource = 'GH_TOKEN'
}
if (-not $inputPathValue -and -not $Token -and $env:GITHUB_TOKEN) {
    $Token = $env:GITHUB_TOKEN
    $tokenSource = 'GITHUB_TOKEN'
}
if (-not $inputPathValue -and -not $Token) {
    $githubCliToken = Get-CopilotGitHubCliToken -UserName $githubUserValue
    if ($githubCliToken) {
        $Token = $githubCliToken.Token
        $tokenSource = $githubCliToken.Source
    }
}
if (-not $inputPathValue -and -not $Token) {
    throw 'No GitHub token found. Set EnterpriseBillingToken, GH_TOKEN, GITHUB_TOKEN, run gh auth login, or pass -Token.'
}

$headers = @{
    Accept                   = 'application/vnd.github+json'
    Authorization            = "Bearer $Token"
    'X-GitHub-Api-Version'   = $ApiVersion
}
$script:lastGitHubApiCallCompletedAt = $null

function Wait-GitHubApiDelay {
    if ($delaySecondsValue -le 0 -or $null -eq $script:lastGitHubApiCallCompletedAt) {
        return
    }

    $elapsed = [DateTimeOffset]::UtcNow - $script:lastGitHubApiCallCompletedAt
    $remainingDelay = $delaySecondsValue - $elapsed.TotalSeconds
    if ($remainingDelay -gt 0) {
        Start-Sleep -Seconds ([math]::Ceiling($remainingDelay))
    }
}

function Get-GitHubErrorDetail {
    param($ErrorRecord)

    $detail = $ErrorRecord.Exception.Message
    $response = $ErrorRecord.Exception.Response
    if (-not $response) {
        return $detail
    }

    try {
        $stream = $response.GetResponseStream()
        if ($stream) {
            $reader = [System.IO.StreamReader]::new($stream)
            $body = $reader.ReadToEnd()
            if ($body) { return "$detail - $body" }
        }
    }
    catch {
        $null = $_
    }

    $detail
}

function Invoke-GitHubApi {
    param([Parameter(Mandatory)][string]$Path)

    Wait-GitHubApiDelay
    try {
        Invoke-RestMethod -Uri "https://api.github.com$Path" -Headers $headers -Method Get
    }
    catch {
        $detail = Get-GitHubErrorDetail -ErrorRecord $_
        if ($Path -like '*/copilot/billing*' -and $detail -match '404|Not Found') {
            $detail = "$detail. Copilot billing endpoints require an organization-owner token for '$Org'. Token source: $tokenSource"
        }
        throw [System.InvalidOperationException]::new("$detail [$Path]", $_.Exception)
    }
    finally {
        $script:lastGitHubApiCallCompletedAt = [DateTimeOffset]::UtcNow
    }
}

function Invoke-GitHubRawApi {
    param([Parameter(Mandatory)][string]$Path)

    Wait-GitHubApiDelay
    try {
        $response = Invoke-WebRequest -Uri "https://api.github.com$Path" -Headers $headers -Method Get -UseBasicParsing
        $body = [string]$response.Content
        [pscustomobject]@{
            Path       = $Path
            StatusCode = [int]$response.StatusCode
            Body       = $body
            Json       = if ($body) { $body | ConvertFrom-Json } else { $null }
        }
    }
    catch {
        $detail = Get-GitHubErrorDetail -ErrorRecord $_
        throw [System.InvalidOperationException]::new("$detail [$Path]", $_.Exception)
    }
    finally {
        $script:lastGitHubApiCallCompletedAt = [DateTimeOffset]::UtcNow
    }
}

function Get-CopilotMetricsOutputPath {
    if (-not (Test-Path -Path $dataDirValue)) {
        New-Item -Path $dataDirValue -ItemType Directory | Out-Null
    }

    Join-Path $dataDirValue 'copilot-metrics.json'
}

function Save-CopilotMetricsRun {
    param(
        [Parameter(Mandatory)]$RunData,
        [Parameter(Mandatory)][string]$Path
    )

    $temporaryPath = "$Path.$runStampValue.tmp"
    $RunData | ConvertTo-Json -Depth 100 | Set-Content -Path $temporaryPath -Encoding UTF8
    Move-Item -LiteralPath $temporaryPath -Destination $Path -Force
}

function Remove-ExpiredCopilotMetricsDataFile {
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory)][string]$CurrentPath)

    if (-not (Test-Path -LiteralPath $dataDirValue)) {
        return
    }

    $retentionCutoff = (Get-Date).AddDays(-$dataRetentionDaysValue)
    $currentFullPath = [System.IO.Path]::GetFullPath($CurrentPath)
    $cleanupPatterns = @(
        'copilot-metrics.backup-*.json',
        'copilot-metrics.bad-*.json',
        'copilot-metrics.json.*.tmp',
        'top10-*.json',
        'top10.json'
    )
    $removedCount = 0

    foreach ($pattern in $cleanupPatterns) {
        foreach ($file in @(Get-ChildItem -LiteralPath $dataDirValue -Filter $pattern -File -ErrorAction Stop)) {
            if ([System.IO.Path]::GetFullPath($file.FullName) -eq $currentFullPath) { continue }
            if ($file.LastWriteTime -ge $retentionCutoff) { continue }

            if ($PSCmdlet.ShouldProcess($file.FullName, 'Remove expired Copilot metrics data file')) {
                Remove-Item -LiteralPath $file.FullName -Force
                $removedCount++
            }
        }
    }

    if ($removedCount -gt 0) {
        Write-Information "Cleaned up $removedCount generated data file(s) older than $dataRetentionDaysValue day(s)."
    }
}

function Get-CopilotMetricsInputPath {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][bool]$AllowLegacyFallback
    )

    if (Test-Path -LiteralPath $Path) { return $Path }
    if (-not $AllowLegacyFallback -or -not (Test-Path -LiteralPath $dataDirValue)) { return $Path }

    $legacyStableRun = Join-Path $dataDirValue 'top10.json'
    if (Test-Path -LiteralPath $legacyStableRun) {
        Write-Information "No copilot-metrics.json found; using legacy run $legacyStableRun"
        return $legacyStableRun
    }

    $legacyRun = Get-ChildItem -LiteralPath $dataDirValue -Filter 'top10-*.json' -File |
        Sort-Object -Property LastWriteTime, Name -Descending |
        Select-Object -First 1
    if ($legacyRun) {
        Write-Information "No copilot-metrics.json found; using latest legacy run $($legacyRun.FullName)"
        return $legacyRun.FullName
    }

    $Path
}

function Get-CopilotSeatLogin {
    $logins = [System.Collections.Generic.List[string]]::new()
    $page = 1
    $encodedOrg = [uri]::EscapeDataString($Org)

    do {
        $response = Invoke-GitHubApi "/orgs/$encodedOrg/copilot/billing/seats?per_page=100&page=$page"
        $seats = @($response.seats)
        foreach ($seat in $seats) {
            if ($seat.assignee.login) {
                $logins.Add([string]$seat.assignee.login)
            }
        }
        $page++
    } while ($seats.Count -eq 100)

    Get-CopilotUniqueLogin -Login $logins
}

function Get-MonthlyAICreditAllowance {
    param([Parameter(Mandatory)][string]$Login)

    if ($specialMonthlyAICreditAllowanceByUser.ContainsKey($Login)) {
        return [double]$specialMonthlyAICreditAllowanceByUser[$Login]
    }

    [double]$defaultMonthlyAICreditAllowance
}

function Get-RunUserLookup {
    param($RunUser)

    $lookup = @{}
    foreach ($userEntry in @($RunUser)) {
        if (-not $userEntry.User) { continue }
        $loginKey = ConvertTo-CopilotLoginKey -Login $userEntry.User
        if (-not $loginKey) { continue }

        if (-not $lookup.ContainsKey($loginKey)) {
            $lookup[$loginKey] = $userEntry
            continue
        }

        $existing = $lookup[$loginKey]
        $existingSucceeded = [bool]$existing.Success
        $candidateSucceeded = [bool]$userEntry.Success
        $candidateHasMoreResponses = @($userEntry.Responses).Count -gt @($existing.Responses).Count
        if (($candidateSucceeded -and -not $existingSucceeded) -or
            ($candidateSucceeded -eq $existingSucceeded -and $candidateHasMoreResponses)) {
            $lookup[$loginKey] = $userEntry
        }
    }

    $lookup
}

function Get-PersistedRunUser {
    param(
        [Parameter(Mandatory)][hashtable]$UserLookup,
        [Parameter(Mandatory)][string[]]$Login,
        [Parameter(Mandatory)][bool]$PreserveAllCachedUsers
    )

    if ($PreserveAllCachedUsers) {
        return @($UserLookup.GetEnumerator() |
            Sort-Object -Property Name |
            ForEach-Object { $_.Value })
    }

    @(Get-CopilotUniqueLogin -Login $Login | ForEach-Object {
            $loginKey = ConvertTo-CopilotLoginKey -Login $_
            if ($UserLookup.ContainsKey($loginKey)) {
                $UserLookup[$loginKey]
            }
            else {
                ConvertTo-CopilotUserEntry -Login $_ -RefreshRunId '' -ErrorMessage 'Missing from refresh.'
            }
        })
}

function Test-CompatibleCache {
    param($Cache)

    Test-CopilotMetricsCacheScope `
        -Cache $Cache `
        -Enterprise $enterpriseName `
        -Organization $Org `
        -Year $yearValue `
        -Month $monthValue `
        -ApiVersion $ApiVersion `
        -IncludeOrganizationFilter $includeOrganizationFilterValue
}

function Get-CompatibleCopilotMetricsCache {
    param([Parameter(Mandatory)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) { return $null }

    $cache = Get-Content -Path $Path -Raw -Encoding UTF8 | ConvertFrom-Json
    Write-MetricsSection -Title 'Cache'
    Write-MetricsDetail -Label 'Path' -Value $Path
    if (Test-CompatibleCache -Cache $cache) {
        Write-MetricsDetail -Label 'Status' -Value 'Compatible monthly snapshot'
        Write-MetricsDetail -Label 'Mode' -Value 'Resume only when RefreshStatus is InProgress'
        return $cache
    }

    Write-MetricsDetail -Label 'Status' -Value 'Incompatible or legacy snapshot'
    Write-MetricsDetail -Label 'Mode' -Value 'Replace through monthly AI Credit collection'
    $null
}

function ConvertTo-CopilotMetricsRunData {
    param(
        [Parameter(Mandatory)]$RunUser,
        [Parameter(Mandatory)][int]$FetchedResponseCount,
        [Parameter(Mandatory)][int]$FailedResponseCount,
        [Parameter(Mandatory)][int]$ReusedResponseCount,
        [Parameter(Mandatory)][string]$RefreshRunId,
        [Parameter(Mandatory)][string]$RefreshStatus,
        [string]$RepairedFrom
    )

    $runData = [ordered]@{
        GeneratedAtUtc            = [DateTimeOffset]::UtcNow.ToString('o')
        Enterprise                = $enterpriseName
        Organization              = $Org
        Year                      = $yearValue
        Month                     = $monthValue
        ApiVersion                = $ApiVersion
        CollectionStrategy        = $collectionStrategy
        IncludeOrganizationFilter = $includeOrganizationFilterValue
        RefreshRunId              = $RefreshRunId
        RefreshStatus             = $RefreshStatus
        FetchedResponseCount      = $FetchedResponseCount
        FailedResponseCount       = $FailedResponseCount
        ReusedResponseCount       = $ReusedResponseCount
        Users                     = @($RunUser)
    }
    if ($RepairedFrom) {
        $runData.RepairedFrom = $RepairedFrom
    }

    [pscustomobject]$runData
}

function Show-TopUser {
    param(
        [Parameter(Mandatory)]$RunUser,
        [Parameter(Mandatory)][string]$Label
    )

    $uniqueUsers = @((Get-RunUserLookup -RunUser $RunUser).GetEnumerator() |
        Sort-Object -Property Name |
        ForEach-Object { $_.Value })
    $topUsers = @($uniqueUsers | ForEach-Object {
            $allowance = Get-MonthlyAICreditAllowance -Login $_.User
            ConvertTo-CopilotUserUsage -Login $_.User -Responses $_.Responses -MonthlyAllowance $allowance
        }) |
        Sort-Object -Property AICredits -Descending |
        Select-Object -First $Top

    Write-MetricsSection -Title $Label
    $topUsers | Format-Table -AutoSize
}

function Invoke-MonthlyRefresh {
    param(
        [Parameter(Mandatory)][string[]]$Login,
        $ExistingCache,
        [Parameter(Mandatory)][string]$CachePath,
        [switch]$RepairOnly,
        [string]$RepairedFrom,
        [Parameter(Mandatory)][bool]$PreserveAllCachedUsers
    )

    $resumeRefreshRunId = if ($ExistingCache -and -not $RepairOnly -and
        $ExistingCache.RefreshStatus -eq 'InProgress' -and $ExistingCache.RefreshRunId) {
        [string]$ExistingCache.RefreshRunId
    }
    else {
        $null
    }
    $refreshRunId = if ($resumeRefreshRunId) { $resumeRefreshRunId } else { [guid]::NewGuid().ToString('N') }
    $existingUserLookup = if ($ExistingCache) { Get-RunUserLookup -RunUser $ExistingCache.Users } else { @{} }
    $mergedUserLookup = if ($ExistingCache) { Get-RunUserLookup -RunUser $ExistingCache.Users } else { @{} }
    $plan = @(Get-CopilotRefreshPlan -Login $Login -UserLookup $existingUserLookup -ResumeRefreshRunId $resumeRefreshRunId -RepairOnly:$RepairOnly)
    $fetchItems = @($plan | Where-Object { $_.ShouldFetch })
    $reusedResponseCount = @($plan | Where-Object { -not $_.ShouldFetch }).Count
    $fetchedResponseCount = 0
    $failedResponseCount = 0
    $checkpointResponseCount = 0
    $refreshStartedAt = [DateTimeOffset]::UtcNow

    Write-MetricsSection -Title 'Refresh Plan'
    Write-MetricsDetail -Label 'Fetch' -Value "$($fetchItems.Count) monthly user response(s)"
    Write-MetricsDetail -Label 'Reuse' -Value "$reusedResponseCount response(s) from this run or repair input"
    Write-MetricsDetail -Label 'Checkpoint' -Value 'After every attempted user'
    if ($resumeRefreshRunId) {
        Write-MetricsDetail -Label 'Resume' -Value "Continuing interrupted run $resumeRefreshRunId"
    }

    $fetchIndex = 0
    foreach ($planItem in $plan) {
        if (-not $planItem.ShouldFetch) { continue }

        $fetchIndex++
        $loginValue = [string]$planItem.Login
        Write-Progress -Id 1 -Activity 'Fetching monthly per-user Copilot AI Credit usage' `
            -Status "$fetchIndex / $($fetchItems.Count): $loginValue" `
            -PercentComplete (($fetchIndex / $fetchItems.Count) * 100)

        $path = Get-CopilotAICreditUsagePath `
            -Enterprise $enterpriseName `
            -Year $yearValue `
            -Month $monthValue `
            -Login $loginValue `
            -Organization $(if ($includeOrganizationFilterValue) { $Org } else { '' })
        $userEntry = $null
        try {
            $rawResponse = Invoke-GitHubRawApi -Path $path
            $response = ConvertTo-CopilotMonthlyResponse `
                -Path $path `
                -StatusCode $rawResponse.StatusCode `
                -RawJson $rawResponse.Body `
                -Response $rawResponse.Json `
                -RefreshRunId $refreshRunId
            $userEntry = ConvertTo-CopilotUserEntry -Login $loginValue -RefreshRunId $refreshRunId -Response $response
            $fetchedResponseCount++
        }
        catch {
            $failedResponseCount++
            $message = $_.Exception.Message
            Write-Warning "Monthly refresh failed for $loginValue; preserving prior monthly response when available: $message"
            $userEntry = ConvertTo-CopilotUserEntry `
                -Login $loginValue `
                -RefreshRunId $refreshRunId `
                -ExistingEntry $planItem.ExistingEntry `
                -ErrorMessage $message
        }

        $loginKey = ConvertTo-CopilotLoginKey -Login $loginValue
        $mergedUserLookup[$loginKey] = $userEntry
        $checkpointResponseCount++
        $checkpointUsers = Get-PersistedRunUser `
            -UserLookup $mergedUserLookup `
            -Login $Login `
            -PreserveAllCachedUsers $PreserveAllCachedUsers
        $checkpointData = ConvertTo-CopilotMetricsRunData `
            -RunUser $checkpointUsers `
            -FetchedResponseCount $fetchedResponseCount `
            -FailedResponseCount $failedResponseCount `
            -ReusedResponseCount $reusedResponseCount `
            -RefreshRunId $refreshRunId `
            -RefreshStatus 'InProgress' `
            -RepairedFrom $RepairedFrom
        Save-CopilotMetricsRun -RunData $checkpointData -Path $CachePath
    }

    Write-Progress -Id 1 -Activity 'Fetching monthly per-user Copilot AI Credit usage' -Completed
    $persistedUsers = Get-PersistedRunUser `
        -UserLookup $mergedUserLookup `
        -Login $Login `
        -PreserveAllCachedUsers $PreserveAllCachedUsers
    $runData = ConvertTo-CopilotMetricsRunData `
        -RunUser $persistedUsers `
        -FetchedResponseCount $fetchedResponseCount `
        -FailedResponseCount $failedResponseCount `
        -ReusedResponseCount $reusedResponseCount `
        -RefreshRunId $refreshRunId `
        -RefreshStatus 'Complete' `
        -RepairedFrom $RepairedFrom
    Save-CopilotMetricsRun -RunData $runData -Path $CachePath
    Remove-ExpiredCopilotMetricsDataFile -CurrentPath $CachePath

    $elapsed = [DateTimeOffset]::UtcNow - $refreshStartedAt
    Write-MetricsSection -Title 'Result'
    Write-MetricsDetail -Label 'Elapsed' -Value ('{0:mm\:ss}' -f $elapsed)
    Write-MetricsDetail -Label 'Fetched' -Value ([string]$fetchedResponseCount)
    Write-MetricsDetail -Label 'Failed' -Value ([string]$failedResponseCount)
    Write-MetricsDetail -Label 'Reused' -Value ([string]$reusedResponseCount)
    Write-MetricsDetail -Label 'Checkpointed' -Value ([string]$checkpointResponseCount)
    Write-MetricsDetail -Label 'Saved' -Value $CachePath

    Show-TopUser -RunUser $persistedUsers -Label "Top $Top Copilot AI Credit consumers for $Org ($yearValue-$('{0:D2}' -f $monthValue) month-to-date)"
}

if ($inputPathValue) {
    $inputPathValue = Get-CopilotMetricsInputPath -Path $inputPathValue -AllowLegacyFallback $usingDefaultInputPath
    if (-not (Test-Path -LiteralPath $inputPathValue)) {
        throw "No saved Copilot metrics run found at $inputPathValue. Run .\scripts\$(Split-Path -Leaf $PSCommandPath) -Refresh to create it."
    }

    $cache = Get-Content -Path $inputPathValue -Raw -Encoding UTF8 | ConvertFrom-Json
    Show-TopUser -RunUser $cache.Users -Label "Top $Top Copilot AI Credit consumers from $inputPathValue"
    return
}

if ($repairInputFileValue) {
    if (-not (Test-Path -LiteralPath $repairInputFileValue)) {
        throw "Repair input file not found: $repairInputFileValue"
    }

    $repairCache = Get-Content -Path $repairInputFileValue -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($repairCache.Enterprise) { $enterpriseName = [string]$repairCache.Enterprise }
    if ($repairCache.Organization) { $Org = [string]$repairCache.Organization }
    if ($repairCache.Year) { $yearValue = [int]$repairCache.Year }
    if ($repairCache.Month) { $monthValue = [int]$repairCache.Month }

    Write-MetricsSection -Title 'Scope'
    Write-MetricsDetail -Label 'Enterprise' -Value $enterpriseName
    Write-MetricsDetail -Label 'Organization' -Value $Org
    Write-MetricsDetail -Label 'Mode' -Value 'Repair monthly snapshot'
    $seatLogins = @(Get-CopilotSeatLogin)
    if ($seatLogins.Count -eq 0) {
        throw "No Copilot users found for $Org."
    }

    $compatibleRepairCache = if (Test-CompatibleCache -Cache $repairCache) { $repairCache } else { $null }
    $cachePath = Get-CopilotMetricsOutputPath
    Invoke-MonthlyRefresh `
        -Login $seatLogins `
        -ExistingCache $compatibleRepairCache `
        -CachePath $cachePath `
        -RepairOnly `
        -RepairedFrom $repairInputFileValue `
        -PreserveAllCachedUsers $false
    return
}

$usingExplicitUsers = $User -and $User.Count -gt 0
$cachePath = Get-CopilotMetricsOutputPath
$existingCache = Get-CompatibleCopilotMetricsCache -Path $cachePath

Write-MetricsSection -Title 'Scope'
Write-MetricsDetail -Label 'Enterprise' -Value $enterpriseName
Write-MetricsDetail -Label 'Organization' -Value $Org
Write-MetricsDetail -Label 'Period' -Value "$yearValue-$('{0:D2}' -f $monthValue) month-to-date"
Write-MetricsDetail -Label 'Users' -Value $(if ($usingExplicitUsers) { 'Explicit list' } else { 'Current Copilot seats' })

$logins = if ($usingExplicitUsers) {
    @(Get-CopilotUniqueLogin -Login $User)
}
else {
    Write-MetricsDetail -Label 'Seat lookup' -Value 'Fetching from GitHub'
    @(Get-CopilotSeatLogin)
}
if ($logins.Count -eq 0) {
    throw "No Copilot users found for $Org."
}
if (-not $includeOrganizationFilterValue -and -not $usingExplicitUsers) {
    Write-MetricsDetail -Label 'Billing scope' -Value "Enterprise user usage filtered to $Org seat assignees"
}
Write-MetricsDetail -Label 'User count' -Value ([string]$logins.Count)

Invoke-MonthlyRefresh `
    -Login $logins `
    -ExistingCache $existingCache `
    -CachePath $cachePath `
    -PreserveAllCachedUsers ([bool]$usingExplicitUsers)
