# installs fetchloom and fl from a github release into %LOCALAPPDATA%\Programs\fetchloom\bin, and
# adds that folder to your user PATH when it lacks it.
#
#   powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
#
# the version is the first argument, else FETCHLOOM_VERSION, else the latest release.
# FETCHLOOM_DOWNLOAD_URL names a folder that holds a release's files, for testing.

param([string]$Version = $env:FETCHLOOM_VERSION)

# everything runs inside a function, so irm | iex leaves the caller's preferences alone and an
# error never closes the caller's window
function Install-Fetchloom([string]$Version) {
    $ErrorActionPreference = 'Stop'
    # windows powershell draws a progress bar that slows downloads
    $ProgressPreference = 'SilentlyContinue'
    # windows powershell may not offer tls 1.2 unless asked
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repository = 'https://github.com/nikhil-yn-aix/fetchloom'
    $target = 'x86_64-pc-windows-msvc'

    function Get-Download([string]$Url, [string]$File) {
        try {
            Invoke-WebRequest -Uri $Url -OutFile $File -UseBasicParsing
        } catch {
            throw "could not download ${Url}: $($_.Exception.Message)"
        }
    }

    function Add-UserPath([string]$Folder) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        try {
            # read the value unexpanded and write it back as the same kind, so entries such as
            # %USERPROFILE%\bin keep working
            $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            if ($key.GetValueNames() -contains 'Path') { $kind = $key.GetValueKind('Path') }
            foreach ($entry in $raw -split ';') {
                if ([Environment]::ExpandEnvironmentVariables($entry).TrimEnd('\') -eq $Folder) { return $false }
            }
            if ($raw -and -not $raw.EndsWith(';')) { $raw += ';' }
            $key.SetValue('Path', "$raw$Folder", $kind)
        } finally {
            $key.Close()
        }
        # setting a variable through .NET tells explorer and other running programs to read the
        # environment again. powershell 7 passes $null as an empty string, which .NET keeps
        [Environment]::SetEnvironmentVariable('FETCHLOOM_INSTALL', '1', 'User')
        [Environment]::SetEnvironmentVariable('FETCHLOOM_INSTALL', [NullString]::Value, 'User')
        return $true
    }

    if (-not [Environment]::Is64BitOperatingSystem) {
        throw "there is no fetchloom build for 32-bit windows. build it from source: $repository#install"
    }
    $Version = "$Version".Trim().TrimStart('v')
    if ($Version -and $Version -notmatch '^\d') { throw "$Version is not a version. give one such as 0.1.0" }

    if ($env:FETCHLOOM_DOWNLOAD_URL) {
        $release = $env:FETCHLOOM_DOWNLOAD_URL.TrimEnd('/')
    } elseif ($Version) {
        $release = "$repository/releases/download/v$Version"
    } else {
        $release = "$repository/releases/latest/download"
    }

    $root = Join-Path $env:LOCALAPPDATA 'Programs\fetchloom'
    $bin = Join-Path $root 'bin'
    # staged beside the destination, so moving each program into place is a rename
    $work = Join-Path $root "install-$PID"
    New-Item -ItemType Directory -Force $bin | Out-Null
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
    New-Item -ItemType Directory $work | Out-Null
    try {
        Write-Host "downloading $release/SHA256SUMS"
        $sums = Join-Path $work 'SHA256SUMS'
        Get-Download "$release/SHA256SUMS" $sums
        $line = Select-String -Path $sums -Pattern "^([0-9a-f]{64}) [ *](fetchloom-(\S+)-$target\.zip)$" | Select-Object -First 1
        if (-not $line) { throw "$release/SHA256SUMS lists no fetchloom archive for $target" }
        $groups = $line.Matches[0].Groups
        $expected = $groups[1].Value.ToLowerInvariant()
        $asset = $groups[2].Value
        $found = $groups[3].Value
        if ($Version -and $found -ne $Version) { throw "$release holds fetchloom $found, not $Version" }
        if (-not $env:FETCHLOOM_DOWNLOAD_URL) { $release = "$repository/releases/download/v$found" }

        Write-Host "downloading $release/$asset"
        $archive = Join-Path $work $asset
        Get-Download "$release/$asset" $archive
        $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "$asset has SHA-256 $actual, but SHA256SUMS lists $expected. nothing was installed"
        }

        Expand-Archive -Path $archive -DestinationPath $work
        $folder = Join-Path $work ($asset -replace '\.zip$', '')
        foreach ($name in 'fetchloom.exe', 'fl.exe') {
            if (-not (Test-Path (Join-Path $folder $name))) { throw "$asset holds no $name. nothing was installed" }
        }
        foreach ($name in 'fetchloom.exe', 'fl.exe') {
            Move-Item -Force (Join-Path $folder $name) (Join-Path $bin $name)
        }
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }

    $installed = & (Join-Path $bin 'fetchloom.exe') --version
    Write-Host "installed $installed and fl in $bin"
    if (($env:Path -split ';' | ForEach-Object { $_.TrimEnd('\') }) -notcontains $bin) { $env:Path = "$env:Path;$bin" }
    if (Add-UserPath $bin) {
        Write-Host "added $bin to your user PATH. open a new terminal to use it"
    }
    Write-Host 'next: run fetchloom to see the first steps. in powershell type fetchloom, since fl means Format-List there'
}

Install-Fetchloom $Version
