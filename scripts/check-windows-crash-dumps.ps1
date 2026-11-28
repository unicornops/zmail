# check-windows-crash-dumps.ps1 - Crashes Zmail on purpose and checks that
# Windows keeps no dump of it (#467): none in Windows Error Reporting's
# report folders and none in WER's local dumps (%LOCALAPPDATA%\CrashDumps
# by default), which hold the store key and mail from the process's memory.
#
# Usage: pwsh scripts/check-windows-crash-dumps.ps1
#
# Needs an administrator (local dumps are a machine setting) and a debug
# build of the app (ZMAIL_APP_BINARY, default target\debug\zmail-app.exe):
# only debug builds have ZMAIL_DEBUG_FORCE_CRASH (src/hardening.rs).
#
# The runner is set up as a default Windows install would be, with WER's
# local dumps on as well: diagnostic data at Required (the runner image's
# 0 makes WER queue no report at all), reports only queued (ForceQueue, so
# nothing is sent), and local dumps on for the machine (an administrator
# may have done that, and software such as the Service Fabric SDK on the
# runner image does). Two things are watched: new files in CrashDumps (a
# local dump) and new reports in WER's ReportQueue and ReportArchive
# (under %LOCALAPPDATA% and %ProgramData%\Microsoft\Windows\WER). Then:
#
# 1. Control: the desktop app is crashed by an abort with its exclusion
#    from WER taken away while it runs (the app writes it when it starts)
#    and without the MSI's values. A local dump and a queued WER report
#    must appear, which shows this check can see both.
# 2. As a portable copy runs, with the exclusion the app writes and
#    without the MSI's values: WER must queue no report. The local dump
#    this leaves is the gap ADR 0005 records (only an administrator can
#    turn local dumps off for an executable); it is reported, not failed.
# 3. As the MSI installs it, with the registry values the MSI writes
#    (read from crates/zmail-app/wix/main.wxs; the MSI check,
#    scripts/check-windows-msi.ps1, checks it writes them): the desktop app
#    and `zmail-app tui`, each crashed by a panic that aborts and by a
#    stack overflow, must leave nothing in any of the watched folders, and
#    each abort's panic must be in the crash log.
#
# Each crash runs outside the runner's job object (started through WMI):
# inside it, Windows never hands a crash to WER, so nothing would be
# dumped either way.

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$app = if ($env:ZMAIL_APP_BINARY) { $env:ZMAIL_APP_BINARY } else { Join-Path $repoRoot 'target\debug\zmail-app.exe' }
if (-not $IsWindows) {
    Write-Error 'this script only runs on Windows'
}
if (-not (Test-Path $app)) {
    Write-Error "no app binary at $app; build it first (a debug build)"
}
$app = (Resolve-Path $app).Path
$exeName = Split-Path -Leaf $app
$wxs = Join-Path $repoRoot 'crates\zmail-app\wix\main.wxs'

$werKey = 'SOFTWARE\Microsoft\Windows\Windows Error Reporting'
$excluded = "HKCU:\Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications"
$localDumps = "HKLM:\$werKey\LocalDumps"
# CrashDumps first. WER's Temp folder is left out: WerSvc makes an empty
# one on its first use after it starts, for an excluded crash too.
$watched = @(
    (Join-Path $env:LOCALAPPDATA 'CrashDumps'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\Windows\WER\ReportQueue'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\Windows\WER\ReportArchive'),
    (Join-Path $env:ProgramData 'Microsoft\Windows\WER\ReportQueue'),
    (Join-Path $env:ProgramData 'Microsoft\Windows\WER\ReportArchive')
)
$telemetryKeys = @(
    'HKLM:\SOFTWARE\Policies\Microsoft\Windows\DataCollection',
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\DataCollection'
)
$work = Join-Path ([IO.Path]::GetTempPath()) "zmail-crash-check-$PID"
New-Item -ItemType Directory -Force -Path $work | Out-Null
$failures = 0

function Fail([string]$message) {
    Write-Host "::error::$message"
    $script:failures++
}

# Every file and folder under the watched folders.
function Snapshot {
    $files = @{}
    foreach ($dir in $watched) {
        if (Test-Path $dir) {
            Get-ChildItem -Path $dir -Recurse -Force -ErrorAction SilentlyContinue |
                ForEach-Object { $files[$_.FullName] = $true }
        }
    }
    return $files
}

# The HKLM values main.wxs installs under WER's key, as
# @{ Key; Name; Type; Value }.
function Msi-Values {
    [xml]$doc = Get-Content -Raw $wxs
    $ns = New-Object Xml.XmlNamespaceManager $doc.NameTable
    $ns.AddNamespace('w', 'http://schemas.microsoft.com/wix/2006/wi')
    $values = @()
    # GetAttribute: an element's Name and Value are also XmlNode's own.
    foreach ($node in $doc.SelectNodes('//w:RegistryValue', $ns)) {
        $key = $node.GetAttribute('Key')
        $root = $node.GetAttribute('Root')
        $parent = $node.ParentNode
        if ($parent.LocalName -eq 'RegistryKey') {
            $key = if ($key) { "$($parent.GetAttribute('Key'))\$key" } else { $parent.GetAttribute('Key') }
            if (-not $root) { $root = $parent.GetAttribute('Root') }
        }
        if ($root -eq 'HKLM' -and $key -like "$werKey\*") {
            $values += [pscustomobject]@{
                Key = $key
                Name = $node.GetAttribute('Name')
                Type = $node.GetAttribute('Type')
                Value = $node.GetAttribute('Value')
            }
        }
    }
    return $values
}

# Writes the MSI's values; returns the keys it had to create.
function Apply-MsiValues($values) {
    $created = @()
    foreach ($value in $values) {
        $path = "HKLM:\$($value.Key)"
        if (-not (Test-Path $path)) {
            New-Item -Force -Path $path | Out-Null
            $created += $path
        }
        $type = if ($value.Type -eq 'integer') { 'DWord' } else { 'String' }
        $data = if ($type -eq 'DWord') { [int]$value.Value } else { $value.Value }
        New-ItemProperty -Force -Path $path -Name $value.Name -PropertyType $type -Value $data | Out-Null
        Write-Host "  HKLM\$($value.Key) [$($value.Name)] = $($value.Value)"
    }
    return , $created
}

function Remove-MsiValues($values, $created) {
    foreach ($value in $values) {
        Remove-ItemProperty -Path "HKLM:\$($value.Key)" -Name $value.Name -ErrorAction SilentlyContinue
    }
    foreach ($path in $created) {
        Remove-Item -Recurse -Force -Path $path -ErrorAction SilentlyContinue
    }
}

# Starts the app outside the runner's job, through WMI, with `$arguments`
# and the environment in `$environment`, in a console of its own (the
# terminal UI draws on it; only stderr goes to a file). Returns the file
# its exit code goes to.
function Start-Crash([string]$label, [string]$arguments, [hashtable]$environment) {
    $codeFile = Join-Path $work "$label.code"
    $logFile = Join-Path $work "$label.log"
    $sets = ($environment.GetEnumerator() | ForEach-Object { "set `"$($_.Key)=$($_.Value)`"" }) -join ' && '
    $command = "cmd.exe /v:on /c `"$sets && `"$app`" $arguments 2> `"$logFile`" & echo !errorlevel! > `"$codeFile`"`""
    $result = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $command; CurrentDirectory = $work }
    if ($result.ReturnValue -ne 0) {
        throw "Win32_Process.Create failed ($($result.ReturnValue)) for $label"
    }
    return $codeFile
}

# Waits for the exit code in `$codeFile`; $null when it never came.
function Wait-Exit([string]$codeFile, [int]$seconds) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path $codeFile) -and (Get-Content $codeFile -ErrorAction SilentlyContinue)) {
            # cmd reports an NTSTATUS as a negative number.
            $code = [int64]((Get-Content $codeFile | Select-Object -First 1).Trim())
            if ($code -lt 0) { $code += 4294967296 }
            return [uint32]$code
        }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

# Runs one crash and returns what appeared under the watched folders, as
# @{ Dumps; Reports }. `$whileRunning` runs once the app has written its
# exclusion (or after 20 seconds, when it doesn't).
function Invoke-Crash([string]$label, [string]$arguments, [string]$crash, [scriptblock]$whileRunning) {
    Write-Host "--- $label ($arguments, $crash)"
    $logDir = Join-Path $work "$label-logs"
    $dataDir = Join-Path $work "$label-data"
    New-Item -ItemType Directory -Force -Path $logDir, $dataDir | Out-Null
    $before = Snapshot
    $codeFile = Start-Crash $label $arguments @{
        ZMAIL_DEBUG_FORCE_CRASH = $crash
        ZMAIL_LOG_DIR = $logDir
        ZMAIL_DATA_DIR = $dataDir
    }
    if ($whileRunning) {
        $deadline = (Get-Date).AddSeconds(20)
        while ((Get-Date) -lt $deadline -and $null -eq (Get-ItemProperty -Path $excluded -Name $exeName -ErrorAction SilentlyContinue)) {
            Start-Sleep -Milliseconds 200
        }
        & $whileRunning
    }
    $code = Wait-Exit $codeFile 90
    # WER writes after the process is gone.
    Start-Sleep -Seconds 15
    $after = Snapshot
    $new = @($after.Keys | Where-Object { -not $before.ContainsKey($_) } | Sort-Object)
    $output = Join-Path $work "$label.log"
    if (Test-Path $output) { Get-Content $output | ForEach-Object { Write-Host "  | $_" } }
    $codeText = if ($null -eq $code) { 'none' } else { '0x{0:X8}' -f $code }
    $dumps = @($new | Where-Object { $_ -like "$($watched[0])\*" })
    $reports = @($new | Where-Object { $_ -notlike "$($watched[0])\*" })
    Write-Host "  exit code $codeText; local dumps: $($dumps.Count); WER report files: $($reports.Count)"
    $new | ForEach-Object { Write-Host "    $_" }
    # 0xC0000409 (an abort, a fail fast) and 0xC00000FD (a stack
    # overflow), written out: PowerShell reads 0xC... as a negative Int32.
    $expected = if ($crash -like 'abort*') { [uint32]3221226505 } else { [uint32]3221225725 }
    if ($code -ne $expected) {
        Fail "$label did not crash as asked (exit code $codeText, expected $('0x{0:X8}' -f $expected))"
    }
    if ($crash -like 'abort*') {
        $log = Join-Path $logDir 'crash.log'
        $text = if (Test-Path $log) { Get-Content -Raw $log } else { '' }
        if ($text -notmatch 'panicked at .*forced crash \(ZMAIL_DEBUG_FORCE_CRASH\)') {
            Fail "$label's crash log has no panic message (log: '$text')"
        }
    }
    return @{ Dumps = $dumps; Reports = $reports }
}

# A registry value, or $null.
function Read-Value([string]$path, [string]$name) {
    $item = Get-ItemProperty -Path $path -Name $name -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }
    return $item.$name
}

# Sets a DWORD, creating its key; returns what to restore.
function Set-Dword([string]$path, [string]$name, [int]$value) {
    $prior = [pscustomobject]@{ Path = $path; Name = $name; Key = (Test-Path $path); Value = (Read-Value $path $name) }
    New-Item -Force -Path $path -ErrorAction SilentlyContinue | Out-Null
    New-ItemProperty -Force -Path $path -Name $name -PropertyType DWord -Value $value | Out-Null
    return $prior
}

function Restore-Dword($prior) {
    if (-not $prior.Key) {
        Remove-Item -Recurse -Force -Path $prior.Path -ErrorAction SilentlyContinue
    } elseif ($null -eq $prior.Value) {
        Remove-ItemProperty -Path $prior.Path -Name $prior.Name -ErrorAction SilentlyContinue
    } else {
        Set-ItemProperty -Path $prior.Path -Name $prior.Name -Value $prior.Value
    }
}

$msiValues = @(Msi-Values)
$created = @()
$restore = @()
try {
    Write-Host 'Diagnostic data at Required, WER reports queued only, local dumps on'
    foreach ($key in $telemetryKeys) {
        $restore += Set-Dword $key 'AllowTelemetry' 1
    }
    $restore += Set-Dword "HKLM:\$werKey" 'ForceQueue' 1
    $restore += Set-Dword $localDumps 'DumpType' 2
    $restore += Set-Dword $localDumps 'DumpCount' 20
    Restart-Service WerSvc -Force -ErrorAction SilentlyContinue
    Remove-MsiValues $msiValues @()
    Remove-ItemProperty -Path $excluded -Name $exeName -ErrorAction SilentlyContinue

    # 1. The control: nothing keeps the dump or the report away.
    $seen = Invoke-Crash 'control' '' 'abort:8000' {
        Remove-ItemProperty -Path $excluded -Name $exeName -ErrorAction SilentlyContinue
        Write-Host '  removed the exclusion'
    }
    if (-not ($seen.Dumps | Where-Object { $_ -like "*\$exeName.*.dmp" })) {
        Fail "the control crash left no local dump, so this check can't see one"
    }
    if (-not ($seen.Reports | Where-Object { $_ -like "*\AppCrash_$exeName*" })) {
        Fail "the control crash queued no WER report, so this check can't see one"
    }

    # 2. A portable copy: the app's own exclusion only.
    $seen = Invoke-Crash 'portable-abort' '' 'abort:8000' $null
    if ($seen.Reports.Count -gt 0) {
        Fail "portable-abort queued a WER report: $($seen.Reports -join ', ')"
    }
    if ($seen.Dumps.Count -gt 0) {
        Write-Host '  (a local dump, as expected without the MSI: see ADR 0005)'
    }
    if ((Get-ItemProperty -Path $excluded -Name $exeName -ErrorAction SilentlyContinue).$exeName -ne 1) {
        Fail "the app did not put $exeName on the user's WER exclusion list"
    }

    # 3. As installed by the MSI.
    Write-Host 'The MSI''s WER values:'
    if ($msiValues.Count -eq 0) {
        Fail "main.wxs installs no values under HKLM\$werKey"
    }
    $created = Apply-MsiValues $msiValues
    $runs = @(
        @{ Label = 'desktop-abort'; Arguments = ''; Crash = 'abort:8000' },
        @{ Label = 'desktop-stack-overflow'; Arguments = ''; Crash = 'stack-overflow:8000' },
        @{ Label = 'tui-abort'; Arguments = 'tui'; Crash = 'abort:5000' },
        @{ Label = 'tui-stack-overflow'; Arguments = 'tui'; Crash = 'stack-overflow:5000' }
    )
    foreach ($run in $runs) {
        $seen = Invoke-Crash $run.Label $run.Arguments $run.Crash $null
        $left = @($seen.Dumps) + @($seen.Reports)
        if ($left.Count -gt 0) {
            Fail "$($run.Label) left a dump or report: $($left -join ', ')"
        }
    }
} finally {
    Get-Process zmail-app -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-MsiValues $msiValues $created
    [array]::Reverse($restore)
    foreach ($prior in $restore) {
        Restore-Dword $prior
    }
}

if ($failures -gt 0) {
    exit 1
}
Write-Host 'A crash of the desktop app or the terminal UI leaves no dump.'
