# run-windows-canary.ps1 - Runs the web renderer canary (tests/canary.rs) on
# Windows with a deadline.
#
# Usage: pwsh scripts/run-windows-canary.ps1 [-Label NAME] [-TimeoutSeconds N]
#
# The canary runs as its own process, its output in files that are printed
# whatever happens, so a hang at exit (or a WebView2 process holding the
# step's output open) shows its log instead of a bare timeout. Any WebView2
# process left over is stopped. Exits with the canary's exit code.

param(
    [string]$Label = 'canary',
    [int]$TimeoutSeconds = 600
)

$ErrorActionPreference = 'Stop'

$out = "canary-$Label.out"
$err = "canary-$Label.err"
$canary = Start-Process cargo -ArgumentList @('test', '-p', 'zmail-render-web', '--test', 'canary', '--locked', '--', '--nocapture') `
    -RedirectStandardOutput $out -RedirectStandardError $err -NoNewWindow -PassThru
# Reading the handle now keeps the exit code readable later.
$null = $canary.Handle
$finished = $canary.WaitForExit($TimeoutSeconds * 1000)
Get-Content $err, $out
Get-Process msedgewebview2, canary-* -ErrorAction SilentlyContinue | Stop-Process -Force
if (-not $finished) {
    Write-Host "::error::the canary ($Label) did not exit within $TimeoutSeconds seconds"
    Stop-Process -Id $canary.Id -Force -ErrorAction SilentlyContinue
    exit 1
}
exit $canary.ExitCode
