param(
    [Parameter(Mandatory)] [string] $Name,
    [Parameter(Mandatory)] [string] $A,
    [Parameter(Mandatory)] [string] $B,
    [Parameter(Mandatory)] [string[]] $Arguments,
    [int] $Runs = 10,
    [string] $Profile = '',
    [hashtable] $EnvA = @{},
    [hashtable] $EnvB = @{},
    [string] $Out = (Join-Path $PSScriptRoot 'results\ab')
)
# paired runs of two fetchloom builds, interleaved a b, b a, a b, ... so drift hits both alike.
# each run gets an empty cache and work folder. `{work}` in the arguments becomes the work folder.
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$csv = Join-Path $Out "$Name.csv"
if (-not (Test-Path $csv)) { 'name,pair,side,exe,seconds,exit,started,requests,netem,profile' | Set-Content $csv }
$scratch = Join-Path ([IO.Path]::GetTempPath()) "fetchloom-ab\$Name"
function Netem { if ($Profile) { (wsl -d Ubuntu-24.04 -u root -- tc qdisc show dev eth0 | Select-String 'netem').Count } else { '' } }
for ($pair = 1; $pair -le $Runs; $pair++) {
    $order = if ($pair % 2) { @('a', 'b') } else { @('b', 'a') }
    foreach ($side in $order) {
        $exe = if ($side -eq 'a') { $A } else { $B }
        $extra = if ($side -eq 'a') { $EnvA } else { $EnvB }
        if (Test-Path $scratch) { Remove-Item -Recurse -Force $scratch }
        New-Item -ItemType Directory -Force "$scratch\cache", "$scratch\work" | Out-Null
        $saved = @{}
        foreach ($key in @('FETCHLOOM_CACHE') + $extra.Keys) { $saved[$key] = [Environment]::GetEnvironmentVariable($key) }
        $env:FETCHLOOM_CACHE = "$scratch\cache"
        foreach ($key in $extra.Keys) { [Environment]::SetEnvironmentVariable($key, $extra[$key]) }
        $args2 = $Arguments | ForEach-Object { $_.Replace('{work}', "$scratch\work") }
        $netem = Netem
        $started = Get-Date -Format 'HH:mm:ss'
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process -FilePath $exe -ArgumentList $args2 -NoNewWindow -Wait -PassThru -RedirectStandardError "$scratch\stderr.txt" -RedirectStandardOutput "$scratch\stdout.txt"
        $watch.Stop()
        foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key]) }
        $requests = ''
        $summary = Get-Content "$scratch\stderr.txt" | Select-String '(\d+) requests?' | Select-Object -Last 1
        if ($summary) { $requests = $summary.Matches[0].Groups[1].Value }
        $seconds = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        "$Name,$pair,$side,$(Split-Path $exe -Leaf),$seconds,$($process.ExitCode),$started,$requests,$netem,$Profile" | Add-Content $csv
        if ($process.ExitCode -ne 0) { Copy-Item "$scratch\stderr.txt" (Join-Path $Out "$Name-$pair-$side.stderr.txt") -Force }
        Start-Sleep -Seconds 2
    }
}
if (Test-Path $scratch) { Remove-Item -Recurse -Force $scratch }
