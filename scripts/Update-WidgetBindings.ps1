<#
.SYNOPSIS
Regenerates crates/codexbar-app/src/widgets/bindings.rs, the Rust bindings for the Windows App SDK Widgets API (#94).

.DESCRIPTION
Downloads Microsoft.WindowsAppSDK.Widgets from nuget.org (or uses the NuGet cache), then runs windows-bindgen over
its Microsoft.Windows.Widgets.winmd for the provider namespace. The versions are pinned here; the bindgen version
must generate code for the windows-core version the workspace uses. Run it after changing either version, then
build and test.
#>
[CmdletBinding()]
param(
    [string]$WidgetsVersion = '2.0.5',
    [string]$BindgenVersion = '0.65.0'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $root 'target\widget-bindings'
$package = Join-Path $env:USERPROFILE ".nuget\packages\microsoft.windowsappsdk.widgets\$WidgetsVersion"
if (-not (Test-Path -LiteralPath $package)) {
    $package = Join-Path $work "Microsoft.WindowsAppSDK.Widgets.$WidgetsVersion"
    if (-not (Test-Path -LiteralPath $package)) {
        New-Item -ItemType Directory -Path $work -Force | Out-Null
        $zip = "$package.zip"
        Invoke-WebRequest -Uri "https://www.nuget.org/api/v2/package/Microsoft.WindowsAppSDK.Widgets/$WidgetsVersion" -OutFile $zip
        Expand-Archive -LiteralPath $zip -DestinationPath $package
        Remove-Item -LiteralPath $zip
    }
}
$winmd = Join-Path $package 'metadata\Microsoft.Windows.Widgets.winmd'
if (-not (Test-Path -LiteralPath $winmd)) { throw "No Widgets metadata at $winmd." }

# A throwaway crate that runs windows-bindgen.
$tool = Join-Path $work 'bindgen'
New-Item -ItemType Directory -Path (Join-Path $tool 'src') -Force | Out-Null
@"
[package]
name = "widget-bindgen"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
windows-bindgen = "=$BindgenVersion"

[workspace]
"@ | Set-Content -LiteralPath (Join-Path $tool 'Cargo.toml') -Encoding utf8
@'
fn main() {
    windows_bindgen::bindgen(std::env::args().skip(1)).unwrap();
}
'@ | Set-Content -LiteralPath (Join-Path $tool 'src\main.rs') -Encoding utf8

$out = Join-Path $root 'crates\codexbar-app\src\widgets\bindings.rs'
cargo run --quiet --manifest-path (Join-Path $tool 'Cargo.toml') -- `
    --in default $winmd `
    --out $out `
    --filter Microsoft.Windows.Widgets.Providers Microsoft.Windows.Widgets.WidgetSize `
    --reference windows,skip-root,Windows
if ($LASTEXITCODE -ne 0) { throw "windows-bindgen failed with exit code $LASTEXITCODE." }
Write-Information "Wrote $out from Microsoft.WindowsAppSDK.Widgets $WidgetsVersion." -InformationAction Continue
