# Types text into the focused control of a process's window by posting WM_CHAR messages.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)] [string] $ProcessName,
    [Parameter(Mandatory)] [string] $Text,
    [switch] $SelectAll
)
$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Type {
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
}
"@
$proc = Get-Process -Name $ProcessName | Where-Object MainWindowHandle -ne 0 | Select-Object -First 1
if (-not $proc) { throw "No window for process '$ProcessName'." }
$hwnd = $proc.MainWindowHandle
if ($SelectAll) {
    # Ctrl+A: key down VK_CONTROL, 'A', then release.
    [Win32Type]::PostMessage($hwnd, 0x0100, [IntPtr]0x11, [IntPtr]::Zero) | Out-Null
    [Win32Type]::PostMessage($hwnd, 0x0100, [IntPtr]0x41, [IntPtr]::Zero) | Out-Null
    [Win32Type]::PostMessage($hwnd, 0x0101, [IntPtr]0x41, [IntPtr]::Zero) | Out-Null
    [Win32Type]::PostMessage($hwnd, 0x0101, [IntPtr]0x11, [IntPtr]::Zero) | Out-Null
    Start-Sleep -Milliseconds 80
}
foreach ($ch in $Text.ToCharArray()) {
    [Win32Type]::PostMessage($hwnd, 0x0102, [IntPtr][int]$ch, [IntPtr]::Zero) | Out-Null   # WM_CHAR
    Start-Sleep -Milliseconds 15
}
"typed $($Text.Length) chars"
