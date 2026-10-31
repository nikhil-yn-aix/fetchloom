# runs every lab plan, 5 blocks each, one profile at a time and one system at a time:
# windows first, then WSL. run from powershell in the repository with the lab server started.
# stops when FETCHLOOM_LAB no longer names WSL's eth0 or loopback.
param(
    [string[]]$Profiles = @('lan', 'home', 'far', 'poor'),
    [string]$Distro = 'Ubuntu-24.04',
    [Parameter(Mandatory = $true)][string]$User,
    [string]$Wheels = "$env:LOCALAPPDATA\fetchloom-work\wheels",
    [string]$Python = $env:FETCHLOOM_BENCH_PY,
    [string]$LinuxPython
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Python) { throw 'set FETCHLOOM_BENCH_PY or pass -Python with the windows benchmark python' }
$env:FETCHLOOM_BENCH_PY = $Python
foreach ($name in 'FETCHLOOM_LAB', 'FETCHLOOM_LAB_CA') {
    if (-not [Environment]::GetEnvironmentVariable($name)) { throw "environment variable $name is not set" }
}
$wslRepo = (wsl -d $Distro -u $User -e wslpath -u $repo).Trim()
if ($LASTEXITCODE) { throw 'could not resolve the repository path in WSL' }
$linuxHome = (wsl -d $Distro -u $User -e printenv HOME).Trim()
if ($LASTEXITCODE -or -not $linuxHome) { throw "could not find the home for $User in $Distro" }
if (-not $LinuxPython) { $LinuxPython = "$linuxHome/bench-venv/bin/python" }
Set-Location $repo

$address = (wsl -d $Distro -u $User -e sh -c "ip -4 -br addr show eth0 | awk '{print `$3}' | cut -d/ -f1").Trim()
if ($LASTEXITCODE -or -not $address) { throw "could not find WSL's eth0 address" }
$lab = [uri]$env:FETCHLOOM_LAB
if (-not $lab.IsAbsoluteUri -or $lab.Scheme -ne 'https') { throw 'FETCHLOOM_LAB must be an absolute HTTPS base URL' }
if (-not $lab.IsLoopback -and $address -ne $lab.Host) {
    throw "WSL's eth0 is $address but FETCHLOOM_LAB names $($lab.Host); update FETCHLOOM_LAB"
}

foreach ($shape in $Profiles) {
    wsl -d $Distro -u root --cd $wslRepo -e bash bench/lab/net.sh $shape
    if ($LASTEXITCODE) { throw "could not shape the lab to $shape" }
    foreach ($plan in Get-ChildItem bench\plans -Filter "lab-$shape-windows*.toml") {
        uv run --no-config --no-index --find-links $Wheels --python $Python bench/harness.py "bench/plans/$($plan.Name)"
        if ($LASTEXITCODE) { throw "$($plan.Name) stopped with $LASTEXITCODE" }
    }
    foreach ($plan in Get-ChildItem bench\plans -Filter "lab-$shape-linux*.toml") {
        wsl -d $Distro -u $User --cd $wslRepo -e env "PATH=$linuxHome/.local/bin:/usr/local/bin:/usr/bin:/bin" "FETCHLOOM_BENCH_PY=$LinuxPython" 'FETCHLOOM_LAB=https://localhost:8443' 'FETCHLOOM_LAB_CA=/etc/ssl/certs/ca-certificates.crt' $LinuxPython bench/harness.py "bench/plans/$($plan.Name)"
        if ($LASTEXITCODE) { throw "$($plan.Name) stopped with $LASTEXITCODE" }
    }
}
wsl -d $Distro -u root --cd $wslRepo -e bash bench/lab/net.sh clear
