param([ValidateSet('refs', 'exfat', 'fat32')][string]$FileSystem)

$ErrorActionPreference = 'Stop'
if (Test-Path 'R:\') { throw 'R: is already in use' }
$root = Join-Path $env:RUNNER_TEMP "fetchloom-$FileSystem"
New-Item -ItemType Directory $root | Out-Null
$image = Join-Path $root 'volume.vhdx'
$script = Join-Path $root 'diskpart.txt'
$oldTmp, $oldTemp = $env:TMP, $env:TEMP
try {
    Set-Content $script -Encoding ascii -Value @(
        "create vdisk file=`"$image`" maximum=4096 type=expandable"
        "select vdisk file=`"$image`""
        'attach vdisk'
        'create partition primary'
        "format fs=$FileSystem quick label=fetchloom"
        'assign letter=R'
    )
    diskpart /s $script
    if ($LASTEXITCODE) { throw "diskpart could not create $FileSystem" }
    $volume = Get-Volume -DriveLetter R
    if ($volume.FileSystem -ne $FileSystem) { throw "expected $FileSystem, got $($volume.FileSystem)" }
    $volume | Format-Table DriveLetter, FileSystem, Size, HealthStatus
    New-Item -ItemType Directory 'R:\sandboxes' | Out-Null
    $env:TMP = 'R:\sandboxes'
    $env:TEMP = $env:TMP
    $env:FETCHLOOM_CI_VOLUME = $FileSystem
    python python/ci_tests.py run $FileSystem --disk --archive target/ci/windows-tests.tar.zst
    if ($LASTEXITCODE) { throw "tests failed on $FileSystem" }
} finally {
    $env:TMP, $env:TEMP = $oldTmp, $oldTemp
    Remove-Item Env:\FETCHLOOM_CI_VOLUME -ErrorAction SilentlyContinue
    Set-Content $script -Encoding ascii -Value @("select vdisk file=`"$image`"", 'detach vdisk')
    diskpart /s $script
    if ($LASTEXITCODE) { throw "diskpart could not detach $FileSystem" }
    Remove-Item -Recurse -Force $root
}
