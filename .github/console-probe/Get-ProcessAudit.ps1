# Throwaway: process creations (Security event 4688, command lines enabled by the workflow) since -Since, limited to
# the processes this probe cares about, so each gh/cmd and the conhost/OpenConsole started for it can be matched
# with its parent and its command line. Token-shaped strings are redacted.
param(
    [Parameter(Mandatory = $true)][datetime]$Since,
    [Parameter(Mandatory = $true)][string]$OutFile
)
$ErrorActionPreference = 'Stop'
$pattern = '\\(gh|conhost|OpenConsole|WindowsTerminal|codexbar|console-probe-launch|cmd|git|node|agy|PING)\.exe$'
$rows = foreach ($event in Get-WinEvent -FilterHashtable @{ LogName = 'Security'; Id = 4688; StartTime = $Since } -ErrorAction SilentlyContinue) {
    $data = @{}
    foreach ($d in ([xml]$event.ToXml()).Event.EventData.Data) { $data[$d.Name] = [string]$d.'#text' }
    if ($data.NewProcessName -notmatch $pattern -and $data.ParentProcessName -notmatch $pattern) { continue }
    [ordered]@{
        time = $event.TimeCreated.ToUniversalTime().ToString('o')
        pid = [Convert]::ToInt64($data.NewProcessId, 16)
        image = $data.NewProcessName
        parentPid = [Convert]::ToInt64($data.ProcessId, 16)
        parent = $data.ParentProcessName
        commandLine = ($data.CommandLine -replace '(gh[opsur]_|github_pat_)[A-Za-z0-9_]+', '$1<redacted>')
    }
}
$rows = @($rows | Sort-Object { $_.time })
$rows | ConvertTo-Json -Depth 4 | Set-Content $OutFile -Encoding utf8
Write-Host "process creations recorded: $($rows.Count)"
foreach ($r in $rows) { Write-Host ("{0} {1,6} <- {2,6} {3} | {4}" -f $r.time.Substring(11, 12), $r.pid, $r.parentPid, (Split-Path $r.image -Leaf), $r.commandLine) }
