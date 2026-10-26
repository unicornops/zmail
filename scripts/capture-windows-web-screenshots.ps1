# capture-windows-web-screenshots.ps1 - Screenshots of the Windows web renderer.
#
# Usage: pwsh scripts/capture-windows-web-screenshots.ps1 OUTPUT_DIR
#
# Launches `zmail-app launch-ui-fixture` once per state below (seeded
# synthetic mail, no network) and captures the screen, so CI's Windows job
# can show what a message in a WebView2 looks like: the page itself, GPUI's
# menus and dialogs drawn above it, and the page cut off by the
# conversation list when scrolled (ADR 0006). Each launch's stderr, with
# ZMAIL_WEB_DEBUG on, is saved next to its PNG, with the window's screen
# rectangle.
#
# Then three interactions, driven with synthesised input relative to the
# window: the wheel over the page, a click on its link, and a selection
# copied with Ctrl+C (the clipboard is saved as reader-html-copy.txt).
#
# Requires Windows and a built app (ZMAIL_APP_BINARY, default
# target\debug\zmail-app.exe).

param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDir
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$app = if ($env:ZMAIL_APP_BINARY) { $env:ZMAIL_APP_BINARY } else { Join-Path $repoRoot 'target\debug\zmail-app.exe' }
if (-not $IsWindows) {
    Write-Error 'this script only runs on Windows'
}
if (-not (Test-Path $app)) {
    Write-Error "no app binary at $app; build it first"
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$OutputDir = (Resolve-Path $OutputDir).Path

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class ZmailWindows {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, int data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);

    const uint LEFTDOWN = 0x0002, LEFTUP = 0x0004, WHEEL = 0x0800;
    const uint KEYUP = 0x0002;

    public static void Click(int x, int y) {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(100);
        mouse_event(LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(50);
        mouse_event(LEFTUP, 0, 0, 0, UIntPtr.Zero);
    }

    public static void Drag(int fromX, int fromY, int toX, int toY) {
        SetCursorPos(fromX, fromY);
        System.Threading.Thread.Sleep(100);
        mouse_event(LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
        for (int step = 1; step <= 20; step++) {
            System.Threading.Thread.Sleep(20);
            SetCursorPos(fromX + (toX - fromX) * step / 20, fromY + (toY - fromY) * step / 20);
        }
        System.Threading.Thread.Sleep(100);
        mouse_event(LEFTUP, 0, 0, 0, UIntPtr.Zero);
    }

    public static void Wheel(int x, int y, int notches) {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(100);
        for (int i = 0; i < Math.Abs(notches); i++) {
            mouse_event(WHEEL, 0, 0, notches < 0 ? -120 : 120, UIntPtr.Zero);
            System.Threading.Thread.Sleep(50);
        }
    }

    public static void CtrlC() {
        keybd_event(0x11, 0, 0, UIntPtr.Zero);
        keybd_event(0x43, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(50);
        keybd_event(0x43, 0, KEYUP, UIntPtr.Zero);
        keybd_event(0x11, 0, KEYUP, UIntPtr.Zero);
    }
}
'@

function Save-Screen([string]$Path) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

# Starts the fixture, waits, and returns the process and its window's
# rectangle (written next to the log).
function Start-Fixture([string]$Name, [string]$View, [string]$Theme, [int]$Seconds) {
    $env:ZMAIL_WEB_DEBUG = '1'
    $process = Start-Process -FilePath $app -ArgumentList @('launch-ui-fixture', $View, $Theme) `
        -RedirectStandardOutput (Join-Path $OutputDir "$Name.out.log") `
        -RedirectStandardError (Join-Path $OutputDir "$Name.log") `
        -PassThru -WindowStyle Normal
    Start-Sleep -Seconds $Seconds
    $process.Refresh()
    if ($process.HasExited) {
        Write-Host "::warning::zmail-app exited before $Name was captured; see $Name.log"
        return $null
    }
    $rect = New-Object ZmailWindows+RECT
    if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
        [ZmailWindows]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
        [ZmailWindows]::GetWindowRect($process.MainWindowHandle, [ref]$rect) | Out-Null
    }
    "window: $($rect.Left) $($rect.Top) $($rect.Right) $($rect.Bottom)" |
        Set-Content (Join-Path $OutputDir "$Name.window.txt")
    Start-Sleep -Milliseconds 500
    return @{ Process = $process; Rect = $rect }
}

function Stop-Fixture($Fixture) {
    if ($Fixture -and -not $Fixture.Process.HasExited) {
        Stop-Process -Id $Fixture.Process.Id -Force -ErrorAction SilentlyContinue
        $Fixture.Process.WaitForExit(5000) | Out-Null
    }
}

# name view theme seconds-to-wait
$states = @(
    @('reader-html-light', 'reader-html', 'light', 12),
    @('reader-html-dark', 'reader-html', 'dark', 12),
    @('reader-html-menu-light', 'reader-html-menu', 'light', 12),
    @('reader-html-scrolled-light', 'reader-html-scrolled', 'light', 14),
    @('reader-link-warning-light', 'reader-link-warning', 'light', 12),
    @('reader-link-warning-dark', 'reader-link-warning', 'dark', 12)
)

foreach ($state in $states) {
    $name, $view, $theme, $seconds = $state
    Write-Host "Capturing $name"
    $fixture = Start-Fixture $name $view $theme $seconds
    if ($fixture) {
        Save-Screen (Join-Path $OutputDir "$name.png")
    }
    Stop-Fixture $fixture
}

# Interactions on the reader-html state. Positions are fractions of the
# window's width and height, in the page of the newsletter.
function Invoke-Interaction([string]$Name, [scriptblock]$Steps) {
    Write-Host "Capturing $Name"
    $fixture = Start-Fixture $Name 'reader-html' 'light' 12
    if (-not $fixture) {
        return
    }
    $rect = $fixture.Rect
    $at = {
        param([double]$fx, [double]$fy)
        @([int]($rect.Left + ($rect.Right - $rect.Left) * $fx), [int]($rect.Top + ($rect.Bottom - $rect.Top) * $fy))
    }
    try {
        & $Steps $at
    } catch {
        Write-Host "::warning::$Name failed: $_"
    }
    Start-Sleep -Seconds 2
    Save-Screen (Join-Path $OutputDir "$Name.png")
    Stop-Fixture $fixture
}

# The wheel over the page scrolls the conversation.
Invoke-Interaction 'reader-html-wheel' {
    param($at)
    $x, $y = & $at 0.7 0.7
    [ZmailWindows]::Wheel($x, $y, -3)
}

# A link in the page opens Zmail's open-link dialog.
Invoke-Interaction 'reader-html-link-click' {
    param($at)
    $x, $y = & $at 0.7 0.88
    [ZmailWindows]::Click($x, $y)
}

# Text selected in the page is copied by Ctrl+C, through Zmail.
Set-Clipboard -Value ' '
Invoke-Interaction 'reader-html-copy' {
    param($at)
    $fromX, $fromY = & $at 0.48 0.62
    $toX, $toY = & $at 0.9 0.65
    [ZmailWindows]::Drag($fromX, $fromY, $toX, $toY)
    Start-Sleep -Milliseconds 500
    [ZmailWindows]::CtrlC()
    Start-Sleep -Seconds 1
}
$copied = Get-Clipboard -Raw
$copied | Set-Content (Join-Path $OutputDir 'reader-html-copy.txt')
Write-Host "Copied: $copied"

Get-ChildItem $OutputDir | Format-Table Name, Length
