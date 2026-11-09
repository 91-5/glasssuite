# Enumerate windows of a process: title, class, physical-pixel rect, relevant ex-style bits.
# Also prints the current foreground window, so "did it steal focus" can be checked.
#
# ASCII-only on purpose: PowerShell 5.1 mis-decodes UTF-8 without BOM, and a mis-decoded
# multi-byte CJK comment can swallow the following code line (burned once in make-icon.ps1).
param(
    [string]$ProcessName = "GlassSuite"
)
$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;

public class WinProbe {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr h, int idx);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
if (-not $proc) {
    Write-Output "process '$ProcessName' not running"
    exit 1
}
$wantedPids = @($proc | ForEach-Object { $_.Id })
Write-Output ("process ids: " + ($wantedPids -join ", "))

$rows = New-Object System.Collections.ArrayList

$callback = {
    param([IntPtr]$hWnd, [IntPtr]$lParam)
    $ownerPid = [uint32]0
    [void][WinProbe]::GetWindowThreadProcessId($hWnd, [ref]$ownerPid)
    if ($wantedPids -contains [int]$ownerPid) {
        $titleBuf = New-Object System.Text.StringBuilder 512
        [void][WinProbe]::GetWindowTextW($hWnd, $titleBuf, 512)
        $classBuf = New-Object System.Text.StringBuilder 256
        [void][WinProbe]::GetClassNameW($hWnd, $classBuf, 256)
        $rect = New-Object WinProbe+RECT
        [void][WinProbe]::GetWindowRect($hWnd, [ref]$rect)
        $ex = [WinProbe]::GetWindowLongPtr($hWnd, -20).ToInt64()
        [void]$rows.Add([pscustomobject]@{
            Title      = $titleBuf.ToString()
            Class      = $classBuf.ToString()
            Visible    = [WinProbe]::IsWindowVisible($hWnd)
            L = $rect.Left; T = $rect.Top; R = $rect.Right; B = $rect.Bottom
            W = $rect.Right - $rect.Left; H = $rect.Bottom - $rect.Top
            ExStyle    = ("0x{0:X}" -f $ex)
            TopMost    = (($ex -band 0x8) -ne 0)
            NoActivate = (($ex -band 0x8000000) -ne 0)
            ToolWindow = (($ex -band 0x80) -ne 0)
            AppWindow  = (($ex -band 0x40000) -ne 0)
        })
    }
    return $true
}

$del = [WinProbe+EnumWindowsProc]$callback
[void][WinProbe]::EnumWindows($del, [IntPtr]::Zero)

foreach ($w in $rows) {
    if ($w.W -le 0 -or $w.H -le 0) { continue }
    Write-Output ("title='{0}' class='{1}' visible={2} rect=({3},{4},{5},{6}) {7}x{8}" -f $w.Title, $w.Class, $w.Visible, $w.L, $w.T, $w.R, $w.B, $w.W, $w.H)
    Write-Output ("   exstyle={0} topmost={1} noactivate={2} toolwindow={3} appwindow={4}" -f $w.ExStyle, $w.TopMost, $w.NoActivate, $w.ToolWindow, $w.AppWindow)
}

$fg = [WinProbe]::GetForegroundWindow()
$fgPid = [uint32]0
[void][WinProbe]::GetWindowThreadProcessId($fg, [ref]$fgPid)
$fgTitle = New-Object System.Text.StringBuilder 512
[void][WinProbe]::GetWindowTextW($fg, $fgTitle, 512)
Write-Output ("foreground: pid={0} title='{1}' (is GlassSuite={2})" -f $fgPid, $fgTitle.ToString(), ($wantedPids -contains [int]$fgPid))
