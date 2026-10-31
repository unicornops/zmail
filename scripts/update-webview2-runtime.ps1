# update-webview2-runtime.ps1 - Installs or updates the WebView2 Evergreen
# runtime to the latest version, with Microsoft's bootstrapper.
#
# Usage: pwsh scripts/update-webview2-runtime.ps1
#
# Users get WebView2 updates automatically with Microsoft Edge, so CI tests
# the runtime they are getting rather than whatever the runner image has.
# Prints the installed runtime version before and after.

$ErrorActionPreference = 'Stop'

# The Evergreen runtime's EdgeUpdate client id.
$client = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'

function Get-WebView2Version {
    foreach ($key in @(
            "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$client",
            "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$client",
            "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$client")) {
        $version = (Get-ItemProperty -Path $key -Name pv -ErrorAction SilentlyContinue).pv
        if ($version -and $version -ne '0.0.0.0') {
            return $version
        }
    }
    return $null
}

$before = Get-WebView2Version
Write-Host "WebView2 runtime before: $(if ($before) { $before } else { 'not installed' })"

# Microsoft's Evergreen bootstrapper (the "Evergreen Bootstrapper" link on
# https://developer.microsoft.com/microsoft-edge/webview2/).
$setup = Join-Path $env:RUNNER_TEMP 'MicrosoftEdgeWebview2Setup.exe'
Invoke-WebRequest -Uri 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $setup
$signature = Get-AuthenticodeSignature $setup
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
    Write-Error "the bootstrapper is not signed by Microsoft: $($signature.Status) $($signature.SignerCertificate.Subject)"
}
$install = Start-Process $setup -ArgumentList @('/silent', '/install') -Wait -PassThru
Write-Host "bootstrapper exit code: $($install.ExitCode)"

$after = Get-WebView2Version
Write-Host "WebView2 runtime after: $(if ($after) { $after } else { 'not installed' })"
if (-not $after) {
    Write-Error 'the WebView2 runtime is not installed'
}
