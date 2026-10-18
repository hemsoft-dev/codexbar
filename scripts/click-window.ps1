# Posts a left click at window-relative pixel coordinates (as measured on a capture-window.ps1 screenshot).
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)] [string] $ProcessName,
    [Parameter(Mandatory)] [int] $X,
    [Parameter(Mandatory)] [int] $Y
)
$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Click {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr hWnd, ref POINT p);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[Win32Click]::SetProcessDPIAware() | Out-Null
$proc = Get-Process -Name $ProcessName | Where-Object MainWindowHandle -ne 0 | Select-Object -First 1
if (-not $proc) { throw "No window for process '$ProcessName'." }
$hwnd = $proc.MainWindowHandle
$rect = New-Object Win32Click+RECT
[Win32Click]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
$point = New-Object Win32Click+POINT
$point.X = $rect.Left + $X; $point.Y = $rect.Top + $Y
[Win32Click]::ScreenToClient($hwnd, [ref]$point) | Out-Null
$lParam = [IntPtr](($point.Y -shl 16) -bor ($point.X -band 0xFFFF))
[Win32Click]::PostMessage($hwnd, 0x0200, [IntPtr]::Zero, $lParam) | Out-Null   # WM_MOUSEMOVE
[Win32Click]::PostMessage($hwnd, 0x0201, [IntPtr]1, $lParam) | Out-Null        # WM_LBUTTONDOWN
Start-Sleep -Milliseconds 60
[Win32Click]::PostMessage($hwnd, 0x0202, [IntPtr]::Zero, $lParam) | Out-Null   # WM_LBUTTONUP
"clicked client ($($point.X), $($point.Y))"
