param([string]$EnvFile = (Join-Path $env:TEMP 'fetchloom-volumes.env'))

$ErrorActionPreference = 'Stop'

New-Item -ItemType Directory -Force -Path C:\fetchloom-images | Out-Null

function New-TestVolume {
    param([string]$Path, [long]$Size, [switch]$DevDrive)

    $partition = New-VHD -Path $Path -SizeBytes $Size -Dynamic |
        Mount-VHD -Passthru |
        Initialize-Disk -Passthru |
        New-Partition -AssignDriveLetter -UseMaximumSize
    if ($DevDrive) {
        $partition | Format-Volume -DevDrive -Confirm:$false -Force | Out-Null
    }
    else {
        $partition | Format-Volume -FileSystem NTFS -Confirm:$false -Force | Out-Null
    }
    return ($partition.DriveLetter + ':\')
}

$clone = New-TestVolume -Path C:\fetchloom-images\refs.vhdx -Size 20GB -DevDrive
$small = New-TestVolume -Path C:\fetchloom-images\small.vhdx -Size 32MB
$second = New-TestVolume -Path C:\fetchloom-images\second.vhdx -Size 64MB

$sensitive = 'C:\fetchloom-case-sensitive'
New-Item -ItemType Directory -Force -Path $sensitive | Out-Null
fsutil.exe file setCaseSensitiveInfo $sensitive enable
New-Item -ItemType File -Force -Path (Join-Path $sensitive 'Probe') | Out-Null
New-Item -ItemType File -Force -Path (Join-Path $sensitive 'probe') | Out-Null
$behaves = (Get-ChildItem -Path $sensitive -Force).Count -eq 2
Remove-Item -Path $sensitive -Recurse -Force
Write-Output "case sensitive directory behaves: $behaves"


# The trust taxonomy rests on a cache being refused on a network volume, so the
# row needs a real one. A share this machine serves to itself, mapped to a
# drive letter, is remote by every answer Windows gives about it. Best effort:
# a runner with no SMB server leaves the variable unset and the tests that need
# it decline by name.
$network = $null
try {
    $shared = 'C:\fetchloom-share'
    New-Item -ItemType Directory -Force -Path $shared | Out-Null
    if (-not (Get-SmbShare -Name fetchloom -ErrorAction SilentlyContinue)) {
        New-SmbShare -Name fetchloom -Path $shared -FullAccess Everyone | Out-Null
    }
    net.exe use N: /delete /y 2>&1 | Out-Null
    net.exe use N: \localhost\fetchloom | Out-Null
    if (Test-Path N:\) {
        $network = 'N:\'
    }
}
catch {
    Write-Output "no network volume here: $_"
}
$lines = @(
    "FETCHLOOM_TEST_CLONE_VOLUMES=$clone",
    "FETCHLOOM_TEST_CASE_INSENSITIVE_VOLUMES=C:\",
    "FETCHLOOM_TEST_SMALL_VOLUMES=$small",
    "FETCHLOOM_TEST_SECOND_VOLUMES=$second"
)
if ($network) {
    $lines += "FETCHLOOM_TEST_NETWORK_VOLUMES=$network"
}
if ($behaves) {
    New-Item -ItemType Directory -Force -Path $sensitive | Out-Null
    fsutil.exe file setCaseSensitiveInfo $sensitive enable
    $lines += "FETCHLOOM_TEST_CASE_SENSITIVE_VOLUMES=$sensitive"
}
Set-Content -Path $EnvFile -Encoding utf8 -Value $lines

Get-Volume | Format-Table -AutoSize
