# Sends WM_CLOSE to the CodexBar window and reports whether it hid (process alive, window invisible).
[CmdletBinding()]
param([string] $ProcessName = 'codexbar')
$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Close {
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
}
"@
$proc = Get-Process -Name $ProcessName | Where-Object MainWindowHandle -ne 0 | Select-Object -First 1
if (-not $proc) { throw "No visible window for '$ProcessName'." }
$hwnd = $proc.MainWindowHandle
"before: visible=$([Win32Close]::IsWindowVisible($hwnd))"
[Win32Close]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null   # WM_CLOSE
Start-Sleep -Milliseconds 800
$alive = -not (Get-Process -Id $proc.Id -ErrorAction SilentlyContinue).HasExited
"after:  visible=$([Win32Close]::IsWindowVisible($hwnd)) processAlive=$alive"
