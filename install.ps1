# installs fetchloom from a github release into %LOCALAPPDATA%\Programs\fetchloom\bin, with
# fl.exe as a short name for it there unless another fl would be hidden, and adds that folder to
# your user PATH when it lacks it, unless FETCHLOOM_NO_MODIFY_PATH=1.
#
#   powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
#
# the version is the first argument, else FETCHLOOM_VERSION, else the latest release.
# FETCHLOOM_DOWNLOAD_URL names a folder that holds a release's files, for testing: https, or
# plain http on this machine only.
#
# the archive is checked against the release's SHA256SUMS, which catches a broken download but
# not a release whose files were all replaced. to check that release.yml built the archive from
# the version's tag on github's own runners, download it and run:
#   gh attestation verify <archive> --repo nikhil-yn-aix/fetchloom --source-ref refs/tags/v<version> `
#     --signer-workflow nikhil-yn-aix/fetchloom/.github/workflows/release.yml --deny-self-hosted-runners

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
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or
        $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64' -or
        (('System.Runtime.InteropServices.RuntimeInformation' -as [type]) -and
            [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64')) {
        $target = 'aarch64-pc-windows-msvc'
    }
    $Version = "$Version".Trim().TrimStart('v')
    if ($Version -and $Version -notmatch '^\d[0-9A-Za-z.+-]*$') { throw "$Version is not a version. give one such as 0.1.0" }

    if ($env:FETCHLOOM_DOWNLOAD_URL) {
        $release = $env:FETCHLOOM_DOWNLOAD_URL.TrimEnd('/')
        # read as Invoke-WebRequest reads it, so a name before an @ cannot pass for the host
        $uri = $null
        if (-not [Uri]::TryCreate($release, [UriKind]::Absolute, [ref]$uri) -or
            -not ($uri.Scheme -eq 'https' -or ($uri.Scheme -eq 'http' -and $uri.IsLoopback))) {
            throw 'FETCHLOOM_DOWNLOAD_URL must be https, or plain http to this machine (127.0.0.1, localhost or [::1]): anyone on the way could swap a plain http download, SHA256SUMS and all. nothing was installed'
        }
    } elseif ($Version) {
        $release = "$repository/releases/download/v$Version"
    } else {
        $release = "$repository/releases/latest/download"
    }

    $root = Join-Path $env:LOCALAPPDATA 'Programs\fetchloom'
    $bin = Join-Path $root 'bin'
    $name = 'fetchloom.exe'
    $destination = Join-Path $bin $name
    # Move-Item puts a file into a folder of the destination's name and says nothing
    function Test-Destination {
        if (Test-Path -LiteralPath $destination -PathType Container) {
            throw "$destination is a folder or a link to one. nothing was installed"
        }
    }
    Test-Destination
    # staged beside the destination, so moving each program into place is a rename
    $work = Join-Path $root "install-$PID"
    New-Item -ItemType Directory -Force $bin | Out-Null
    # a run that was killed leaves its staging folder, and a running fl.exe left aside its old
    # link: a folder whose run is gone goes, never through a link, and a link that is free goes
    foreach ($stale in Get-ChildItem -LiteralPath $root -Force -ErrorAction SilentlyContinue) {
        if ($stale.Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
        if ($stale.PSIsContainer -and $stale.Name -match '^install-(\d{1,9})$' -and [int]$Matches[1] -ne $PID -and
            -not (Get-Process -Id ([int]$Matches[1]) -ErrorAction SilentlyContinue)) {
            Remove-Item -LiteralPath $stale.FullName -Recurse -Force -ErrorAction SilentlyContinue
        } elseif (-not $stale.PSIsContainer -and $stale.Name -match '^old-fl-\d+\.exe$') {
            Remove-Item -LiteralPath $stale.FullName -Force -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
    New-Item -ItemType Directory $work | Out-Null
    try {
        Write-Host "downloading $release/SHA256SUMS"
        $sums = Join-Path $work 'SHA256SUMS'
        Get-Download "$release/SHA256SUMS" $sums
        # a plain version, so the archive's name cannot lead outside the staging folder
        $line = Select-String -Path $sums -Pattern "^([0-9a-f]{64}) [ *](fetchloom-(\d[0-9A-Za-z.+-]*)-$target\.zip)$" | Select-Object -First 1
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
        $inside = "$($asset -replace '\.zip$', '')\$name"
        $staged = Join-Path $work $inside
        if (-not (Test-Path -LiteralPath $staged -PathType Leaf)) { throw "$asset holds no $inside. nothing was installed" }
        # run the staged program before it replaces anything, so a build this machine cannot run
        # leaves a working fetchloom in place. security software may take a minute to let it run
        Write-Host "checking that the downloaded $name runs on this machine"
        try {
            $installed = @(& $staged --version)[0]
        } catch {
            $reason = $_.Exception
            if ($reason.InnerException) { $reason = $reason.InnerException }
            $said = "$($reason.Message)".Trim().TrimEnd('.')
            if ($reason -is [ComponentModel.Win32Exception] -and $reason.NativeErrorCode -eq 5) {
                throw "windows refused to run the downloaded $name ($said), as security software such as microsoft defender or app control does with a program it has not seen before. nothing was installed, and an installed fetchloom stays as it was. run the installer again in a few minutes, or ask whoever manages this machine to allow fetchloom"
            }
            throw "the downloaded $name does not run on this machine: $said. nothing was installed"
        }
        if ($LASTEXITCODE) {
            throw "the downloaded $name exited with code $LASTEXITCODE when asked for its version. nothing was installed"
        }
        if ("$installed" -ne "fetchloom $found") {
            $shown = if ("$installed") { "$installed" } else { 'nothing' }
            throw "the downloaded $name says it is $shown, not fetchloom $found. nothing was installed"
        }
        Test-Destination
        Move-Item -Force $staged $destination
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }

    Write-Host "installed $installed in $bin"
    # fl.exe is a hardlink to fetchloom.exe, made again on every install, since a hardlink keeps the
    # old bytes once fetchloom.exe is replaced. none is made where it would hide another fl
    $short = Join-Path $bin 'fl.exe'
    $held = $false
    if (Test-Path -LiteralPath $short) {
        try {
            Remove-Item -LiteralPath $short -Force
        } catch {
            # windows deletes no name of a program that runs, but renames one, so the old link goes
            # aside until a later install finds it free
            try {
                Move-Item -LiteralPath $short -Destination (Join-Path $root "old-fl-$PID.exe") -Force
            } catch {
                $held = $true
            }
        }
    }
    if ($held) {
        Write-Warning "fl.exe still runs the fetchloom installed before, since a running fetchloom or fl holds it; fetchloom.exe runs the new one. close them, then run the installer again"
    } else {
        try {
            $other = Get-Command fl -CommandType Application -All -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($other) {
                Write-Host "left fl alone: it runs $($other.Path) here. for fl in powershell, add Set-Alias fl fetchloom -Force to your profile"
            } else {
                try {
                    New-Item -ItemType HardLink -Path $short -Target $destination | Out-Null
                } catch {
                    Copy-Item -LiteralPath $destination -Destination $short
                }
                # powershell runs an alias before any program, and fl is the alias of Format-List
                if (Get-Alias fl -ErrorAction SilentlyContinue) {
                    Write-Host 'fl.exe runs fetchloom too. in powershell, where fl is Format-List, type fl.exe'
                } else {
                    Write-Host 'fl runs fetchloom too'
                }
            }
        } catch {
            Write-Warning "fl.exe was not made: $("$($_.Exception.Message)".Trim().TrimEnd('.')). fetchloom itself is installed"
        }
    }
    if (($env:Path -split ';' | ForEach-Object { $_.TrimEnd('\') }) -notcontains $bin) { $env:Path = "$env:Path;$bin" }
    if ($env:FETCHLOOM_NO_MODIFY_PATH -eq '1') {
        Write-Host "FETCHLOOM_NO_MODIFY_PATH=1 left your user PATH alone; add $bin to it to run fetchloom from anywhere"
    } else {
        # a policy may keep the user PATH from changing; fetchloom stays installed all the same
        try {
            if (Add-UserPath $bin) { Write-Host "added $bin to your user PATH. open a new terminal to use it" }
        } catch {
            Write-Warning "could not add $bin to your user PATH: $("$($_.Exception.Message)".Trim().TrimEnd('.')). add it yourself to run fetchloom from anywhere"
        }
    }
    Write-Host 'next: run fetchloom to see the first steps'
}

Install-Fetchloom $Version
