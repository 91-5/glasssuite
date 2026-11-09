# Print the current foreground window's pid and title (ASCII-only, PS 5.1 safe).
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class Fg {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
}
"@
$h = [Fg]::GetForegroundWindow()
$pid_ = [uint32]0
[void][Fg]::GetWindowThreadProcessId($h, [ref]$pid_)
$sb = New-Object System.Text.StringBuilder 512
[void][Fg]::GetWindowTextW($h, $sb, 512)
Write-Output ("foreground pid={0} title='{1}'" -f $pid_, $sb.ToString())
