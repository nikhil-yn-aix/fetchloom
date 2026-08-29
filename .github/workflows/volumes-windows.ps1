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

$lines = @(
    "FETCHLOOM_TEST_CLONE_VOLUMES=$clone",
    "FETCHLOOM_TEST_CASE_INSENSITIVE_VOLUMES=C:\",
    "FETCHLOOM_TEST_SMALL_VOLUMES=$small",
    "FETCHLOOM_TEST_SECOND_VOLUMES=$second"
)
if ($behaves) {
    New-Item -ItemType Directory -Force -Path $sensitive | Out-Null
    fsutil.exe file setCaseSensitiveInfo $sensitive enable
    $lines += "FETCHLOOM_TEST_CASE_SENSITIVE_VOLUMES=$sensitive"
}
Add-Content -Path $env:GITHUB_ENV -Encoding utf8 -Value $lines

Get-Volume | Format-Table -AutoSize
