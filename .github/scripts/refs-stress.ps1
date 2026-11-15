param(
    [Parameter(Mandatory)][string]$Out,
    [string]$Arms = 'link,clone,rename,fix',
    [int]$Threads = 16,
    [int]$Seconds = 40,
    [int]$Minutes = 20,
    [int]$Job = 1
)

# runs refs-stress.exe, one arm per new refs volume made as the windows refs lane makes it, the
# arms taking turns. a run where some thread makes no progress for 15 s is a hang: the volume is
# left to its stuck process, a short plain run on a new volume checks that refs still works
# elsewhere, and the loop goes on there. a hang outside the link arm saves kernel stacks.

$ErrorActionPreference = 'Stop'
$Root = (Get-Location).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$Symbols = Join-Path $env:RUNNER_TEMP 'symbols'
$Debuggers = 'C:\Program Files (x86)\Windows Kits\10\Debuggers\x64'
$SymbolPath = "srv*$Symbols*https://msdl.microsoft.com/download/symbols"
$Started = Get-Date
$Summary = Join-Path $Out 'runs.csv'
$Work = Join-Path $env:RUNNER_TEMP 'refs-stress'
New-Item -ItemType Directory -Force $Work, $Symbols | Out-Null
$script:Iteration = 0
$script:Volumes = 0
$script:Capture = $Out

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;

public static class RefsHang
{
    [StructLayout(LayoutKind.Sequential)]
    struct Luid { public uint Low; public int High; }

    [StructLayout(LayoutKind.Sequential)]
    struct Privileges { public uint Count; public Luid Luid; public uint Attributes; }

    [StructLayout(LayoutKind.Sequential)]
    struct LiveDump
    {
        public uint Version;
        public uint BugCheckCode;
        public UIntPtr Parameter1, Parameter2, Parameter3, Parameter4;
        public IntPtr File;
        public IntPtr Cancel;
        public uint Flags;
        public uint AddPages;
    }

    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool LookupPrivilegeValue(string system, string name, out Luid luid);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll, ref Privileges state, uint length, IntPtr previous, IntPtr returned);
    [DllImport("ntdll.dll")]
    static extern int NtSystemDebugControl(int command, ref LiveDump input, int inputLength, IntPtr output, int outputLength, out int returned);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr GetStdHandle(int which);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, int id);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);
    [DllImport("ntdll.dll")]
    static extern int NtSuspendProcess(IntPtr process);
    [DllImport("ntdll.dll")]
    static extern int NtResumeProcess(IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);

    // a kernel memory dump of the running system, as task manager's live dump makes; the ntstatus
    public static int WriteLiveDump(string path)
    {
        IntPtr token;
        if (!OpenProcessToken(System.Diagnostics.Process.GetCurrentProcess().Handle, 0x28, out token)) throw new Win32Exception();
        var privileges = new Privileges { Count = 1, Attributes = 2 };
        if (!LookupPrivilegeValue(null, "SeDebugPrivilege", out privileges.Luid)) throw new Win32Exception();
        if (!AdjustTokenPrivileges(token, false, ref privileges, 0, IntPtr.Zero, IntPtr.Zero)) throw new Win32Exception();
        using (var file = new FileStream(path, FileMode.Create, FileAccess.ReadWrite))
        {
            var control = new LiveDump { Version = 1, BugCheckCode = 0x161, File = file.SafeFileHandle.DangerousGetHandle() };
            int returned;
            return NtSystemDebugControl(37, ref control, Marshal.SizeOf(typeof(LiveDump)), IntPtr.Zero, 0, out returned);
        }
    }

    // stops or restarts every thread of process `id`; the ntstatus, or the win32 error opening it
    public static int Hold(int id, bool hold)
    {
        IntPtr process = OpenProcess(0x0800, false, id);
        if (process == IntPtr.Zero) return Marshal.GetLastWin32Error();
        try { return hold ? NtSuspendProcess(process) : NtResumeProcess(process); }
        finally { CloseHandle(process); }
    }

    // whether processes started next inherit this one's standard handles, the step's pipes
    public static void InheritStandardHandles(bool inherit)
    {
        foreach (var which in new[] { -10, -11, -12 })
        {
            SetHandleInformation(GetStdHandle(which), 1, inherit ? 1u : 0u);
        }
    }
}
'@

function Invoke-Bounded {
    param([string]$Name, [string]$File, [string[]]$Arguments = @(), [int]$Seconds = 120, [string]$Folder = $script:Capture)
    $path = Join-Path $Folder "$Name.txt"
    $began = Get-Date
    try {
        $info = [System.Diagnostics.ProcessStartInfo]::new($File)
        foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
        $info.UseShellExecute = $false
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $info.RedirectStandardInput = $true
        $info.WorkingDirectory = $env:RUNNER_TEMP
        $process = [System.Diagnostics.Process]::Start($info)
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $ended = $process.WaitForExit($Seconds * 1000)
        if (-not $ended) {
            try { $process.Kill($true) } catch { }
        }
        $null = $stdout.Wait(15000)
        $null = $stderr.Wait(15000)
        $status = if ($ended) { "exit $($process.ExitCode)" } else { "still running after $Seconds s, killed" }
        $text = "# $File $($Arguments -join ' ')`n# $status, $([int]((Get-Date) - $began).TotalSeconds) s`n"
        if ($stdout.IsCompleted) { $text += $stdout.Result }
        if ($stderr.IsCompleted -and $stderr.Result) { $text += "`n# stderr`n" + $stderr.Result }
    } catch {
        $text = "# $File $($Arguments -join ' ')`n# failed: $_"
    }
    Set-Content -Path $path -Value $text -Encoding utf8
    Write-Host ("  {0}: {1} s" -f $Name, [int]((Get-Date) - $began).TotalSeconds)
}

function Save-Text {
    param([string]$Name, [scriptblock]$Make)
    $path = Join-Path $script:Capture "$Name.txt"
    try {
        & $Make | Out-String -Width 4096 | Set-Content -Path $path -Encoding utf8
    } catch {
        "failed: $_" | Set-Content -Path $path -Encoding utf8
    }
}

function New-Volume([string]$Letter) {
    if (Test-Path "${Letter}:\") { throw "${Letter}: is already in use" }
    $script:Volumes++
    $folder = Join-Path $env:RUNNER_TEMP "fetchloom-refs-$script:Volumes"
    New-Item -ItemType Directory -Force $folder | Out-Null
    $script:Image = Join-Path $folder 'volume.vhdx'
    $script:Diskpart = Join-Path $folder 'diskpart.txt'
    Set-Content $script:Diskpart -Encoding ascii -Value @(
        "create vdisk file=`"$script:Image`" maximum=4096 type=expandable"
        "select vdisk file=`"$script:Image`""
        'attach vdisk'
        'create partition primary'
        'format fs=refs quick label=fetchloom'
        "assign letter=$Letter"
    )
    diskpart /s $script:Diskpart | Out-Host
    if ($LASTEXITCODE) { throw 'diskpart could not create refs' }
    $volume = Get-Volume -DriveLetter $Letter
    if ($volume.FileSystem -ne 'refs') { throw "expected refs, got $($volume.FileSystem)" }
    $volume | Format-Table DriveLetter, FileSystem, Size, HealthStatus
    New-Item -ItemType Directory "${Letter}:\sandboxes" | Out-Null
    $script:Drive = $Letter
}

# detaches the volume and deletes its image, true when it went
function Remove-Volume {
    Set-Content $script:Diskpart -Encoding ascii -Value @("select vdisk file=`"$script:Image`"", 'detach vdisk')
    diskpart /s $script:Diskpart | Out-Host
    if ($LASTEXITCODE) { return $false }
    Remove-Item -Recurse -Force (Split-Path $script:Image) -ErrorAction SilentlyContinue
    return $true
}

function Read-Log([string]$Log) {
    if (-not (Test-Path $Log)) { return '' }
    $stream = [System.IO.File]::Open($Log, 'Open', 'Read', 'ReadWrite')
    try {
        return [System.IO.StreamReader]::new($stream).ReadToEnd()
    } finally {
        $stream.Dispose()
    }
}


function Save-KernelDump([string]$Path) {
    try {
        $status = [RefsHang]::WriteLiveDump($Path)
        'NtSystemDebugControl: 0x{0:x8}' -f $status | Set-Content (Join-Path $script:Capture 'native-live-dump.txt')
    } catch {
        "native live dump failed: $_" | Set-Content (Join-Path $script:Capture 'native-live-dump.txt')
    }
    Test-Path $Path
}

# the stuck process's threads, kernel stacks and refs's locks, for a hang outside the link arm
function Save-Hang([string]$Label, $Wrapper) {
    $script:Capture = Join-Path $Out "hang-$Label"
    New-Item -ItemType Directory -Force $script:Capture | Out-Null
    $Process = Get-CimInstance Win32_Process -Filter "ParentProcessId = $($Wrapper.Id)" |
        Where-Object Name -EQ 'refs-stress.exe' | Select-Object -First 1 @{ n = 'Id'; e = { [int]$_.ProcessId } }
    if (-not $Process) { Write-Host '::warning::the stuck refs-stress.exe is gone'; return }
    Save-Text 'threads' {
        $(foreach ($thread in (Get-Process -Id $Process.Id).Threads) {
                $wait = ''
                try { if ($thread.ThreadState -eq 'Wait') { $wait = $thread.WaitReason } } catch { }
                [pscustomobject]@{ Tid = $thread.Id; State = $thread.ThreadState; WaitReason = $wait }
            }) | Format-Table -AutoSize
    }
    New-Item -ItemType Directory -Force 'C:\refs-stress' | Out-Null
    $dump = "C:\refs-stress\kernel-$Label.dmp"
    if (Save-KernelDump $dump) {
        $commands = @('.echo "=== locks"', '!locks', '.echo "=== refs"', '!stacks 2 refs',
            '.echo "=== stress"', ('!process 0x{0:x} 1f' -f $Process.Id), 'q')
        Invoke-Bounded 'kernel' (Join-Path $Debuggers 'kd.exe') @('-z', $dump, '-y', $SymbolPath, '-c', ($commands -join '; ')) 1200
        Invoke-Bounded 'compress' 'C:\Program Files\7-Zip\7z.exe' @('a', '-mx=3', (Join-Path $script:Capture 'kernel.dmp.7z'), $dump) 900
        Remove-Item $dump -ErrorAction SilentlyContinue
    }
}

# starts refs-stress.exe in a console of its own, so a stuck one holds none of the step's pipes
function Start-Stress([string]$Arm, [int]$For, [string]$Progress) {
    $folder = "$($script:Drive):\stress"
    $batch = "$Progress.cmd"
    Set-Content -Path $batch -Encoding ascii -Value @(
        '@echo off'
        ('"{0}" {1} "{2}" {3} {4} "{5}" > "{5}.out" 2>&1' -f $script:Exe, $Arm, $folder, $Threads, $For, $Progress)
        'exit /b %ERRORLEVEL%'
    )
    $process = Start-Process -FilePath 'cmd.exe' -ArgumentList "/d /c `"$batch`"" -WindowStyle Hidden -PassThru
    $null = $process.Handle
    $process
}

# runs one arm for $For seconds on the current volume: ops, the longest stall, and whether it hung
function Invoke-Arm([string]$Arm, [int]$For) {
    $progress = Join-Path $Work ("run-{0}-{1}.txt" -f $script:Iteration, $Arm)
    $process = Start-Stress $Arm $For $progress
    $deadline = (Get-Date).AddSeconds($For + 45)
    $hung = $false
    $line = ''
    while (-not $process.HasExited) {
        Start-Sleep -Milliseconds 500
        $lines = @((Read-Log $progress) -split "`n" | Where-Object { $_ -match '^\d' })
        if ($lines.Count) { $line = $lines[-1] }
        $stalled = if ($line) { [double]($line -split ' ')[2] } else { 0 }
        if ($stalled -ge 15 -or (Get-Date) -gt $deadline) {
            $hung = $true
            break
        }
    }
    $text = Read-Log $progress
    $ops = if ($text -match '(?m)^end (\d+)') { [int64]$Matches[1] } elseif ($line) { [int64]($line -split ' ')[1] } else { 0 }
    $failed = if ($text -match '(?m)^failed (.*)$') { $Matches[1].Trim() } else { '' }
    if (-not $hung -and $process.ExitCode -ne 0 -and -not $failed) {
        $failed = "exit $($process.ExitCode): $(Read-Log "$progress.out")"
    }
    [pscustomobject]@{ Ops = $ops; Hung = $hung; Line = $line; Failed = $failed; Process = $process }
}

# the dotnet framework's own compiler, on every windows runner
$script:Exe = Join-Path $Work 'refs-stress.exe'
& 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe' /nologo /optimize "/out:$script:Exe" (Join-Path $PSScriptRoot 'refs-stress.cs') | Out-Host
if ($LASTEXITCODE) { throw 'refs-stress.cs did not compile' }
[RefsHang]::InheritStandardHandles($false)
Set-Content $Summary 'job,run,arm,drive,started,seconds,ops,hang,stalled,failed'
$arms = @($Arms -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$deadline = $Started.AddMinutes($Minutes)
$counts = @{}
foreach ($arm in $arms) { $counts[$arm] = @{ Runs = 0; Hangs = 0; Ops = 0 } }
$fresh = $false
New-Volume 'R'
while ((Get-Date) -lt $deadline) {
    $script:Iteration++
    if ($fresh) {
        $letter = $script:Drive
        if (-not (Remove-Volume)) { $letter = [string][char]([int][char]$script:Drive + 1) }
        New-Volume $letter
    }
    $fresh = $true
    $arm = $arms[($script:Iteration + $Job) % $arms.Count]
    $began = Get-Date
    $run = Invoke-Arm $arm $Seconds
    $seconds = [int]((Get-Date) - $began).TotalSeconds
    $stall = if ($run.Line) { ($run.Line -split ' ')[2] } else { '' }
    $failed = $run.Failed -replace '[,\r\n]', ' '
    Add-Content $Summary "$Job,$script:Iteration,$arm,$($script:Drive),$($began.ToString('o')),$seconds,$($run.Ops),$([int]$run.Hung),$stall,$failed"
    $counts[$arm].Runs++
    $counts[$arm].Ops += $run.Ops
    Write-Host ('run {0} {1} on {2}: {3} ops in {4} s, hang {5}, longest stall {6} s {7}' -f $script:Iteration, $arm, $script:Drive, $run.Ops, $seconds, $run.Hung, $stall, $failed)
    if ($run.Failed -and -not $run.Hung) { Write-Host "::warning::run $script:Iteration ${arm}: $($run.Failed)" }
    if (-not $run.Hung) { continue }
    $counts[$arm].Hangs++
    Write-Host "::warning::job ${Job}: $arm hung on run $script:Iteration on $($script:Drive):"
    if ($arm -ne 'link') { Save-Hang "$script:Iteration-$arm" $run.Process }
    # the stuck process keeps its volume; refs must still work on a new one for the loop to go on
    $next = [string][char]([int][char]$script:Drive + 1)
    if ($next -gt 'Z') { break }
    New-Volume $next
    $fresh = $false
    $probe = Invoke-Arm 'plain' 10
    Write-Host ('probe on {0}: {1} ops, hang {2}' -f $script:Drive, $probe.Ops, $probe.Hung)
    if ($probe.Hung) {
        Write-Host "::warning::job ${Job}: after the hang refs hung on a new volume too, so the job ends"
        Add-Content $Summary "$Job,$script:Iteration,probe,$($script:Drive),,10,$($probe.Ops),1,,"
        break
    }
}
$summary = ($arms | ForEach-Object { '{0} {1}/{2} hung ({3} ops)' -f $_, $counts[$_].Hangs, $counts[$_].Runs, $counts[$_].Ops }) -join ', '
Write-Host "::notice::job ${Job}: $summary"
