# runs every lab plan, 5 blocks each, one profile at a time and one system at a time:
# windows first, then WSL. run from powershell in the repository with the lab server started.
# stops when WSL's eth0 no longer has the address the windows plans name.
param([string[]]$Profiles = @('lan', 'home', 'far', 'poor'))
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$wheels = "$env:LOCALAPPDATA\fetchloom-work\wheels"
$wslRepo = (wsl -d Ubuntu-24.04 -u fl -e wslpath -u $repo).Trim()
Set-Location $repo

$address = (wsl -d Ubuntu-24.04 -u fl -e sh -c "ip -4 -br addr show eth0 | awk '{print `$3}' | cut -d/ -f1").Trim()
$named = Select-String -Path bench\plans\lab-lan-windows.toml -Pattern '^lab = "https://([0-9.]+):8443"' | ForEach-Object { $_.Matches[0].Groups[1].Value }
if ($address -ne $named) {
    throw "WSL's eth0 is $address but the windows plans name $named; update lab in their [vars]"
}

foreach ($shape in $Profiles) {
    wsl -d Ubuntu-24.04 -u root -e bash bench/lab/net.sh $shape
    if ($LASTEXITCODE) { throw "could not shape the lab to $shape" }
    foreach ($plan in Get-ChildItem bench\plans -Filter "lab-$shape-windows*.toml") {
        uv run --no-config --no-index --find-links $wheels --python 3.13 bench/harness.py "bench/plans/$($plan.Name)"
        if ($LASTEXITCODE) { throw "$($plan.Name) stopped with $LASTEXITCODE" }
    }
    foreach ($plan in Get-ChildItem bench\plans -Filter "lab-$shape-linux*.toml") {
        wsl -d Ubuntu-24.04 -u fl --cd $wslRepo -e env "PATH=/home/fl/.local/bin:/usr/local/bin:/usr/bin:/bin" /home/fl/bench-venv/bin/python bench/harness.py "bench/plans/$($plan.Name)"
        if ($LASTEXITCODE) { throw "$($plan.Name) stopped with $LASTEXITCODE" }
    }
}
wsl -d Ubuntu-24.04 -u root -e bash bench/lab/net.sh clear
