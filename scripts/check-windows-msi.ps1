# check-windows-msi.ps1 - Installs the Zmail MSI, checks what it registers
# for mailto: links (#457), uninstalls it and checks nothing of Zmail's is
# left. CI's Windows build runs it on every pull request that changes the
# app, so crates/zmail-app/wix/main.wxs is checked before a release builds
# it. Needs an administrator (the MSI installs per machine).
#
# Usage: pwsh scripts/check-windows-msi.ps1 PATH-TO.msi

param(
    [Parameter(Mandatory = $true)][string]$Msi
)

$ErrorActionPreference = 'Stop'
$failures = 0

function Fail([string]$message) {
    Write-Host "::error::$message"
    $script:failures++
}

function Run-Msiexec([string[]]$arguments, [string]$log) {
    $process = Start-Process msiexec.exe -ArgumentList ($arguments + @('/qn', '/norestart', '/l*v', $log)) -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        Get-Content $log -Tail 80
        throw "msiexec $($arguments -join ' ') exited with $($process.ExitCode)"
    }
}

# A registry value, or $null when it (or its key) is missing.
function Value([string]$key, [string]$name) {
    $item = Get-ItemProperty -Path "Registry::HKEY_LOCAL_MACHINE\$key" -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }
    return $item.$name
}

function Expect-Value([string]$key, [string]$name, [string]$expected) {
    $actual = Value $key $name
    if ($actual -ne $expected) {
        Fail "HKLM\$key [$name] is '$actual', expected '$expected'"
    }
}

function Expect-Key([string]$key, [bool]$present) {
    $exists = Test-Path "Registry::HKEY_LOCAL_MACHINE\$key"
    if ($exists -ne $present) {
        $what = if ($present) { 'missing' } else { 'left after uninstall' }
        Fail "HKLM\$key is $what"
    }
}

$msiPath = (Resolve-Path $Msi).Path
$exe = Join-Path $env:ProgramFiles 'Zmail\zmail-app.exe'
$command = "`"$exe`" `"%1`""

Write-Host "Installing $msiPath"
Run-Msiexec @('/i', $msiPath) 'msi-install.log'

if (-not (Test-Path $exe)) { Fail "$exe was not installed" }
# Settings > Default apps lists Zmail as a mail app for mailto: links.
Expect-Value 'SOFTWARE\RegisteredApplications' 'Zmail' 'SOFTWARE\Clients\Mail\Zmail\Capabilities'
Expect-Value 'SOFTWARE\Clients\Mail\Zmail\Capabilities' 'ApplicationName' 'Zmail'
Expect-Value 'SOFTWARE\Clients\Mail\Zmail\Capabilities\URLAssociations' 'mailto' 'Zmail.Url.mailto'
Expect-Value 'SOFTWARE\Clients\Mail\Zmail' '(default)' 'Zmail'
Expect-Value 'SOFTWARE\Classes\Zmail.Url.mailto\shell\open\command' '(default)' $command
# The scheme's own handler, used until the user picks another.
Expect-Value 'SOFTWARE\Classes\mailto' 'URL Protocol' ''
Expect-Value 'SOFTWARE\Classes\mailto\shell\open\command' '(default)' $command

Write-Host 'Uninstalling'
Run-Msiexec @('/x', $msiPath) 'msi-uninstall.log'

if (Test-Path $exe) { Fail "$exe is left after uninstall" }
Expect-Key 'SOFTWARE\Clients\Mail\Zmail' $false
Expect-Key 'SOFTWARE\Classes\Zmail.Url.mailto' $false
if ($null -ne (Value 'SOFTWARE\RegisteredApplications' 'Zmail')) {
    Fail 'HKLM\SOFTWARE\RegisteredApplications [Zmail] is left after uninstall'
}
if ((Value 'SOFTWARE\Classes\mailto\shell\open\command' '(default)') -eq $command) {
    Fail 'the mailto: scheme still opens Zmail after uninstall'
}
# Windows' own definition of the scheme stays for the other mail apps.
Expect-Value 'SOFTWARE\Classes\mailto' 'URL Protocol' ''

if ($failures -gt 0) {
    exit 1
}
Write-Host 'The MSI registers Zmail for mailto: links and uninstalls cleanly.'
