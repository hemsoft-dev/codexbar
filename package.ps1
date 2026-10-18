<#
.SYNOPSIS
Builds CodexBar as a signed MSIX package and publishes it to a local App Installer channel (#93).

.DESCRIPTION
Each run builds the release executable, packs it with packaging\AppxManifest.xml, signs the package with a
self-signed certificate kept in your certificate store (created on first use, private key not exportable), and writes
the package plus CodexBar.appinstaller into the channel folder. Windows checks that file for updates when CodexBar
starts, so publishing a newer build updates every installation from the channel.

The package version is the workspace version from Cargo.toml with its major part plus one (App Installer rejects a
zero major), plus the commit count: Cargo 0.1.0 at commit 312 is 1.1.0.312. Commit before publishing again, or pass
-Revision.

Windows installs a self-signed package only after its certificate is trusted on the PC: -Trust adds it to the local
machine's Trusted People store, which asks once for administrator approval. See docs\PACKAGING.md.

.PARAMETER Channel
The channel folder. Default: %LOCALAPPDATA%\CodexBar\channel.

.PARAMETER Revision
The fourth version part instead of the commit count.

.PARAMETER Trust
Trusts the signing certificate on this PC (administrator approval), so Windows installs the package.

.PARAMETER Install
Installs or updates CodexBar from the channel after publishing.

.PARAMETER RenewCertificate
Creates a new signing certificate even though one exists, for when the current one is about to expire. Every PC that
installs CodexBar must trust the new one (-Trust here, Import-Certificate elsewhere) before it can install updates.

.PARAMETER Rollback
Points the channel at an earlier version already in the channel folder instead of building. Installations move back
to it the next time CodexBar starts (or right away with -Install).

.EXAMPLE
.\package.ps1 -Trust -Install

.EXAMPLE
.\package.ps1 -Rollback 1.1.0.311 -Install
#>
[CmdletBinding()]
param(
    [string]$Channel = (Join-Path $env:LOCALAPPDATA 'CodexBar\channel'),
    [ValidateRange(0, 65535)]
    [int]$Revision = -1,
    [switch]$Trust,
    [switch]$Install,
    [switch]$RenewCertificate,
    [string]$Rollback
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The manifest's Publisher must equal the signing certificate's subject.
$publisher = 'CN=HemSoft CodexBar Self-Signed'
$packageName = 'HemSoft.CodexBar'

# The Windows App Runtime framework the widget provider needs (#94), from Microsoft's NuGet package. The channel ships
# it and the App Installer file lists it, so Windows installs it with CodexBar where it is missing.
$runtimeNuGet = 'Microsoft.WindowsAppSDK.Runtime'
$runtimeNuGetVersion = '2.5.1'

function Find-SdkTool([string]$Name) {
    $roots = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'),
        (Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windows.sdk.buildtools')
    )
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) { continue }
        # The newest SDK first: tools sit in ...\10.0.<build>.0\x64\.
        $tool = Get-ChildItem -LiteralPath $root -Recurse -Filter $Name -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Directory.Name -eq 'x64' } |
            Sort-Object { if ($_.FullName -match '\\(10\.\d+\.\d+\.\d+)\\x64\\') { [version]$Matches[1] } else { [version]'0.0' } } -Descending |
            Select-Object -First 1
        if ($tool) { return $tool.FullName }
    }
    throw "$Name wasn't found. Install the Windows SDK (winget install Microsoft.WindowsSDK.10.0.26100)."
}

function Invoke-Tool([string]$Path, [string[]]$Arguments) {
    $output = & $Path @Arguments 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path -Leaf $Path) failed with exit code ${LASTEXITCODE}:`n$output" }
    Write-Verbose $output
}

# Draws CodexBar's logo (the tray icon's three gold bars on black) at any size; non-square images center it.
function New-Logo([string]$Path, [int]$Width, [int]$Height) {
    Add-Type -AssemblyName System.Drawing
    $bitmap = [System.Drawing.Bitmap]::new($Width, $Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.Clear([System.Drawing.Color]::Transparent)
        $side = [Math]::Min($Width, $Height)
        $scale = $side / 32.0
        $left = ($Width - $side) / 2.0
        $top = ($Height - $side) / 2.0
        $radius = 6 * $scale
        $square = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $square.AddArc($left, $top, 2 * $radius, 2 * $radius, 180, 90)
        $square.AddArc($left + $side - 2 * $radius, $top, 2 * $radius, 2 * $radius, 270, 90)
        $square.AddArc($left + $side - 2 * $radius, $top + $side - 2 * $radius, 2 * $radius, 2 * $radius, 0, 90)
        $square.AddArc($left, $top + $side - 2 * $radius, 2 * $radius, 2 * $radius, 90, 90)
        $square.CloseFigure()
        $graphics.FillPath([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(0x0A, 0x0A, 0x0A)), $square)
        $gold = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(0xD4, 0xAF, 0x37))
        $track = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(0x2A, 0x2A, 0x2A))
        foreach ($bar in @(@(6, 26), @(14, 20), @(22, 14))) {
            $y = $top + $bar[0] * $scale
            $graphics.FillRectangle($track, $left + 5 * $scale, $y, 22 * $scale, 4 * $scale)
            $graphics.FillRectangle($gold, $left + 5 * $scale, $y, ($bar[1] - 5) * $scale, 4 * $scale)
        }
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

# The widget picker's screenshot: a medium CodexBar widget as the Widgets board shows it, with sample accounts.
function New-WidgetScreenshot([string]$Path) {
    Add-Type -AssemblyName System.Drawing
    $width = 300
    $height = 304
    $bitmap = [System.Drawing.Bitmap]::new($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
        $graphics.Clear([System.Drawing.Color]::FromArgb(0x20, 0x20, 0x20))
        $muted = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(0xA0, 0xA0, 0xA0))
        $text = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
        $track = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(0x3A, 0x3A, 0x3A))
        $small = [System.Drawing.Font]::new('Segoe UI', 10)
        $bold = [System.Drawing.Font]::new('Segoe UI Semibold', 11)
        $graphics.DrawString('All accounts', $small, $muted, 16, 14)
        $tiles = @(
            @{ Name = "Claude $([char]0x00B7) Work"; Line = 'Weekly: 42% used'; Used = 0.42; Color = [System.Drawing.Color]::FromArgb(0x6C, 0xCB, 0x5F) },
            @{ Name = "ChatGPT $([char]0x00B7) Codex"; Line = '5-hour window: 81% used'; Used = 0.81; Color = [System.Drawing.Color]::FromArgb(0xFC, 0xE1, 0x00) }
        )
        $y = 48
        foreach ($tile in $tiles) {
            $graphics.DrawString($tile.Name, $bold, $text, 16, $y)
            $graphics.DrawString($tile.Line, $small, $text, 16, $y + 24)
            $graphics.FillRectangle($track, 16, $y + 50, $width - 32, 6)
            $graphics.FillRectangle([System.Drawing.SolidBrush]::new($tile.Color), 16, $y + 50, ($width - 32) * $tile.Used, 6)
            $y += 92
        }
        $graphics.DrawString('Updated 1m ago', $small, $muted, 16, $height - 34)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

# The certificate is created once and then kept: a new one would make every PC that trusts the old one refuse updates,
# so renewing is an explicit step (-RenewCertificate).
function Get-SigningCertificate {
    $existing = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object { $_.Subject -eq $publisher -and $_.HasPrivateKey } |
        Sort-Object NotAfter -Descending |
        Select-Object -First 1
    if ($existing -and -not $RenewCertificate) {
        if ($existing.NotAfter -lt (Get-Date)) {
            throw "The signing certificate expired on $($existing.NotAfter). Run with -RenewCertificate, then trust the new certificate on every PC."
        }
        if ($existing.NotAfter -lt (Get-Date).AddDays(30)) {
            Write-Warning "The signing certificate expires on $($existing.NotAfter). Run with -RenewCertificate and trust the new one on every PC before then."
        }
        return $existing
    }
    Write-Information "Creating the self-signed signing certificate '$publisher' in your certificate store..." -InformationAction Continue
    New-SelfSignedCertificate -Type Custom -Subject $publisher -FriendlyName 'CodexBar package signing (self-signed)' `
        -KeyUsage DigitalSignature -KeyExportPolicy NonExportable -CertStoreLocation Cert:\CurrentUser\My `
        -NotAfter (Get-Date).AddYears(5) `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
}

function Test-Elevated {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Windows trusts a self-signed package only through the local machine's Trusted People (or root) store.
function Grant-Trust($Certificate, [string]$CerPath) {
    if (Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Thumbprint -eq $Certificate.Thumbprint) {
        return
    }
    if (Test-Elevated) {
        Import-Certificate -FilePath $CerPath -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
        return
    }
    Write-Information 'Windows asks for administrator approval to trust the CodexBar certificate on this PC...' -InformationAction Continue
    $quoted = $CerPath -replace "'", "''"
    $command = "Import-Certificate -FilePath '$quoted' -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $shell = (Get-Process -Id $PID).Path
    $process = Start-Process -FilePath $shell -Verb RunAs -Wait -PassThru -ArgumentList "-NoProfile -EncodedCommand $encoded"
    if ($process.ExitCode -ne 0 -or -not (Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Thumbprint -eq $Certificate.Thumbprint)) {
        throw 'The certificate was not trusted.'
    }
}

function Get-RuntimePackage {
    $folder = Join-Path $env:USERPROFILE ".nuget\packages\$($runtimeNuGet.ToLowerInvariant())\$runtimeNuGetVersion"
    if (-not (Test-Path -LiteralPath $folder)) {
        $folder = Join-Path $PSScriptRoot "target\msix\nuget\$runtimeNuGet.$runtimeNuGetVersion"
        if (-not (Test-Path -LiteralPath $folder)) {
            Write-Information "Downloading $runtimeNuGet $runtimeNuGetVersion from nuget.org..." -InformationAction Continue
            $zip = "$folder.zip"
            New-Item -ItemType Directory -Path (Split-Path -Parent $zip) -Force | Out-Null
            Invoke-WebRequest -Uri "https://www.nuget.org/api/v2/package/$runtimeNuGet/$runtimeNuGetVersion" -OutFile $zip
            Expand-Archive -LiteralPath $zip -DestinationPath $folder
            Remove-Item -LiteralPath $zip
        }
    }
    $msix = Get-ChildItem -LiteralPath (Join-Path $folder 'tools\MSIX\win10-x64') -File -ErrorAction SilentlyContinue |
        Where-Object Name -Match '^Microsoft\.WindowsAppRuntime\.\d+(\.\d+)?\.msix$' |
        Select-Object -First 1
    if (-not $msix) { throw "No Windows App Runtime framework package in $folder." }
    $signature = Get-AuthenticodeSignature -LiteralPath $msix.FullName
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw "$($msix.Name) isn't validly signed by Microsoft ($($signature.Status))."
    }
    # Its identity, from its own manifest.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($msix.FullName)
    try {
        $reader = [IO.StreamReader]::new($archive.GetEntry('AppxManifest.xml').Open())
        try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
    }
    finally {
        $archive.Dispose()
    }
    $identity = $manifest.Package.Identity
    [pscustomobject]@{
        Path = $msix.FullName
        Name = $identity.Name
        Version = $identity.Version
        Publisher = $identity.Publisher
    }
}

function ConvertTo-FileUri([string]$Path) {
    [System.Security.SecurityElement]::Escape(([uri]$Path).AbsoluteUri)
}

# The App Installer file Windows checks at each start. Its own version is the publishing time, so a rollback (which
# points at a lower package version) still reads as the newer file; ForceUpdateFromAnyVersion lets it move back.
function Write-AppInstaller([string]$Version, [string]$PackageFile, $Runtime) {
    $now = Get-Date
    $fileVersion = [version]('1.{0}.{1}.{2}' -f $now.Year, ($now.Month * 100 + $now.Day), ($now.Hour * 100 + $now.Minute))
    $appInstaller = Join-Path $Channel 'CodexBar.appinstaller'
    # Every publication must read as newer, also twice in one minute or after the clock moved back.
    if (Test-Path -LiteralPath $appInstaller) {
        $previous = $null
        try { $previous = [version]([xml](Get-Content -LiteralPath $appInstaller -Raw)).AppInstaller.Version } catch { }
        if ($previous -and $fileVersion -le $previous) {
            $fileVersion = if ($previous.Revision -lt 65535) {
                [version]::new($previous.Major, $previous.Minor, $previous.Build, $previous.Revision + 1)
            }
            else {
                [version]::new($previous.Major, $previous.Minor, $previous.Build + 1, 0)
            }
        }
    }
    $content = @"
<?xml version="1.0" encoding="utf-8"?>
<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2018" Version="$fileVersion" Uri="$(ConvertTo-FileUri $appInstaller)">
  <MainPackage Name="$packageName" Publisher="$publisher" Version="$Version" ProcessorArchitecture="x64" Uri="$(ConvertTo-FileUri $PackageFile)" />
  <Dependencies>
    <Package Name="$($Runtime.Name)" Publisher="$([System.Security.SecurityElement]::Escape($Runtime.Publisher))" Version="$($Runtime.Version)" ProcessorArchitecture="x64" Uri="$(ConvertTo-FileUri $Runtime.ChannelPath)" />
  </Dependencies>
  <UpdateSettings>
    <OnLaunch HoursBetweenUpdateChecks="0" />
    <AutomaticBackgroundTask />
    <ForceUpdateFromAnyVersion>true</ForceUpdateFromAnyVersion>
  </UpdateSettings>
</AppInstaller>
"@
    [IO.File]::WriteAllText($appInstaller, $content, [Text.UTF8Encoding]::new($false))
    $appInstaller
}

New-Item -ItemType Directory -Path $Channel -Force | Out-Null
# File URIs need an absolute path.
$Channel = (Resolve-Path -LiteralPath $Channel).ProviderPath
$runtime = Get-RuntimePackage
$runtimeFile = Join-Path $Channel "$($runtime.Name)_$($runtime.Version)_x64.msix"
if (-not (Test-Path -LiteralPath $runtimeFile)) { Copy-Item -LiteralPath $runtime.Path -Destination $runtimeFile }
$runtime | Add-Member -NotePropertyName ChannelPath -NotePropertyValue $runtimeFile

if ($Rollback) {
    $version = $Rollback
    $packageFile = Join-Path $Channel "CodexBar_${version}_x64.msix"
    if (-not (Test-Path -LiteralPath $packageFile)) {
        throw "The channel has no CodexBar $version ($packageFile)."
    }
    # The certificate that signed that package, which may be older than the current one after a renewal.
    # Intact, and signed by a CodexBar certificate from this store. Before -Trust the chain reads as untrusted
    # (UnknownError), which is expected for a self-signed certificate.
    $signature = Get-AuthenticodeSignature -LiteralPath $packageFile
    $certificate = $signature.SignerCertificate
    $ours = $certificate -and $certificate.Subject -eq $publisher -and
        (Get-ChildItem Cert:\CurrentUser\My | Where-Object Thumbprint -eq $certificate.Thumbprint)
    if ($signature.Status -notin @('Valid', 'UnknownError') -or -not $ours) {
        throw "CodexBar $version in the channel isn't intact or isn't signed by a CodexBar certificate here ($($signature.Status))."
    }
}
else {
    $certificate = Get-SigningCertificate
    $cargo = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Cargo.toml') -Raw
    if ($cargo -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') {
        throw 'Cargo.toml has no [workspace.package] version.'
    }
    # App Installer requires a nonzero major version, so the package's major is Cargo's plus one.
    $base = "$([int]$Matches[1] + 1).$($Matches[2]).$($Matches[3])"
    if ($Revision -lt 0) {
        $Revision = [int](git -C $PSScriptRoot rev-list --count HEAD)
        if ($LASTEXITCODE -ne 0) { throw 'git rev-list failed; pass -Revision.' }
    }
    $version = "$base.$Revision"
    $packageFile = Join-Path $Channel "CodexBar_${version}_x64.msix"
    if (Test-Path -LiteralPath $packageFile) {
        throw "The channel already has CodexBar $version. Commit your changes or pass -Revision with a higher number."
    }

    $commit = (git -C $PSScriptRoot rev-parse --short HEAD)
    if ($LASTEXITCODE -ne 0) { $commit = 'unknown' }
    if (git -C $PSScriptRoot status --porcelain --untracked-files=no) { $commit = "$commit-modified" }

    Write-Information "Building CodexBar $version ($commit)..." -InformationAction Continue
    $builtExe = $null
    $env:CODEXBAR_BUILD = $commit
    # The package declares x64, so it builds x64 on any host. The C runtime is linked in, so the package runs on PCs
    # without the Visual C++ Redistributable. Its own target folder keeps these flags from rebuilding run.ps1's copy
    # every time.
    $rustFlags = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = "$rustFlags -C target-feature=+crt-static".Trim()
    Push-Location $PSScriptRoot
    try {
        cargo build --release --locked -p codexbar-app --target x86_64-pc-windows-msvc --target-dir target\msix\build `
            --message-format=json-render-diagnostics | ForEach-Object {
            try { $message = $_ | ConvertFrom-Json } catch { return }
            if ($message.reason -eq 'compiler-artifact' -and $message.target.name -eq 'codexbar' -and $message.executable) {
                $builtExe = $message.executable
            }
        }
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    finally {
        Pop-Location
        Remove-Item Env:\CODEXBAR_BUILD -ErrorAction SilentlyContinue
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = $rustFlags
    }
    if (-not $builtExe) { throw 'Cargo did not report the codexbar executable.' }
    $imports = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($builtExe))
    if ($imports -match '(?i)VCRUNTIME140|MSVCP140') {
        throw 'codexbar.exe still needs the Visual C++ runtime DLLs; the static C runtime did not apply.'
    }

    $layout = Join-Path $PSScriptRoot 'target\msix\layout'
    if (Test-Path -LiteralPath $layout) { Remove-Item -LiteralPath $layout -Recurse -Force }
    New-Item -ItemType Directory -Path (Join-Path $layout 'Assets') -Force | Out-Null
    Copy-Item -LiteralPath $builtExe -Destination (Join-Path $layout 'codexbar.exe')
    # The provider logos embedded in codexbar.exe are MIT-licensed; their notice ships with it.
    New-Item -ItemType Directory -Path (Join-Path $layout 'ThirdPartyNotices') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'crates\codexbar-app\assets\brand\LICENSE.md') `
        -Destination (Join-Path $layout 'ThirdPartyNotices\provider-logos.md')
    $manifest = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'packaging\AppxManifest.xml') -Raw).
        Replace('{{VERSION}}', $version).Replace('{{PUBLISHER}}', $publisher).
        Replace('{{RUNTIME_NAME}}', $runtime.Name).Replace('{{RUNTIME_VERSION}}', $runtime.Version).
        Replace('{{RUNTIME_PUBLISHER}}', [System.Security.SecurityElement]::Escape($runtime.Publisher))
    [IO.File]::WriteAllText((Join-Path $layout 'AppxManifest.xml'), $manifest, [Text.UTF8Encoding]::new($false))
    New-Logo (Join-Path $layout 'Assets\Square44x44Logo.png') 44 44
    New-Logo (Join-Path $layout 'Assets\Square150x150Logo.png') 150 150
    New-Logo (Join-Path $layout 'Assets\Wide310x150Logo.png') 310 150
    New-Logo (Join-Path $layout 'Assets\StoreLogo.png') 50 50
    New-Logo (Join-Path $layout 'Assets\WidgetIcon.png') 64 64
    New-WidgetScreenshot (Join-Path $layout 'Assets\WidgetScreenshot.png')

    $unsigned = Join-Path $PSScriptRoot 'target\msix\CodexBar.msix'
    Invoke-Tool (Find-SdkTool 'makeappx.exe') @('pack', '/d', $layout, '/p', $unsigned, '/o')
    # Timestamped, so packages stay installable after the certificate expires. Without a timestamp server (offline)
    # it signs without one and says so.
    $signtool = Find-SdkTool 'signtool.exe'
    $signArgs = @('sign', '/fd', 'SHA256', '/sha1', $certificate.Thumbprint, '/s', 'My')
    $stamped = $false
    foreach ($server in @('http://timestamp.digicert.com', 'http://timestamp.sectigo.com')) {
        try {
            Invoke-Tool $signtool ($signArgs + @('/tr', $server, '/td', 'SHA256', $unsigned))
            $stamped = $true
            break
        }
        catch {
            Write-Warning "Timestamp server $server failed: $($_.Exception.Message.Split([Environment]::NewLine)[0])"
        }
    }
    if (-not $stamped) {
        Write-Warning 'Signing without a timestamp: this package stops installing once the certificate expires.'
        Invoke-Tool $signtool ($signArgs + @($unsigned))
    }
    Move-Item -LiteralPath $unsigned -Destination $packageFile
}

# The public certificate of the package the channel offers, for PCs that trust it by hand.
$cerPath = Join-Path $Channel 'CodexBar.cer'
Export-Certificate -Cert $certificate -FilePath $cerPath | Out-Null

$appInstaller = Write-AppInstaller $version $packageFile $runtime
Write-Information "Published CodexBar $version to $Channel." -InformationAction Continue

if ($Trust) {
    Grant-Trust $certificate $cerPath
}

if ($Install) {
    $currentUserSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    # CodexBar's tray app, packaged or from run.ps1; not the demo, the widget provider or a status check.
    $running = @(
        Get-CimInstance Win32_Process -Filter "Name = 'codexbar.exe'" |
            Where-Object { $_.CommandLine -notmatch '(^|\s)(--demo|-RegisterProcessAsComServer|--package-status)(\s|$)' } |
            Where-Object { (Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid).Sid -eq $currentUserSid }
    )

    # Through the App Installer file, rollbacks too, so the installation keeps its update channel. It closes a
    # running packaged CodexBar; nothing else changes unless it succeeds.
    Add-AppxPackage -AppInstallerFile $appInstaller -ForceTargetApplicationShutdown
    $installed = Get-AppxPackage -Name $packageName
    if ($installed.Version -ne $version) { throw "Windows installed CodexBar $($installed.Version), not $version." }

    # The run.ps1 copy shares the single-instance lock and would keep the package from starting, so it stops. Its
    # Start with Windows entry moves to the package's startup task, which CodexBar turns on when it next starts.
    # Stopping is asynchronous: wait until it has exited and released the lock, or the package would exit at once.
    $running |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath -notmatch '\\WindowsApps\\' } |
        ForEach-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue } |
        ForEach-Object { $_ | Stop-Process -Force -PassThru -ErrorAction SilentlyContinue } |
        Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    $settingsDir = if ($env:CODEXBAR_SETTINGS_DIR) { $env:CODEXBAR_SETTINGS_DIR } else { Join-Path $env:USERPROFILE '.codexbar' }
    $migrated = [bool](Get-ItemProperty -LiteralPath $runKey -Name 'CodexBar' -ErrorAction SilentlyContinue)
    if ($migrated) {
        New-Item -ItemType Directory -Path $settingsDir -Force | Out-Null
        New-Item -ItemType File -Path (Join-Path $settingsDir 'start-with-windows.migrate') -Force | Out-Null
        Remove-ItemProperty -LiteralPath $runKey -Name 'CodexBar'
        Write-Information 'Start with Windows moved from the run.ps1 copy to the package.' -InformationAction Continue
    }

    # Started again if it was running, and always after a move from run.ps1: the package turns its startup task on
    # when it runs.
    # The Widgets board lists widgets from a self-signed package only with Developer Mode on.
    $unlock = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' -ErrorAction SilentlyContinue
    if (-not $unlock -or $unlock.PSObject.Properties['AllowDevelopmentWithoutDevLicense'].Value -ne 1) {
        Write-Warning 'To add the CodexBar widget, turn on Developer Mode: Settings > System > For developers.'
    }

    if ($running.Count -gt 0 -or $migrated) {
        Start-Process explorer.exe "shell:AppsFolder\$($installed.PackageFamilyName)!CodexBar"
        Write-Information "Installed CodexBar $version and started it again." -InformationAction Continue
    }
    else {
        Write-Information "Installed CodexBar $version. Start it from the Start menu." -InformationAction Continue
    }
}
