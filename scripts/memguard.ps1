<#
.SYNOPSIS
  Run a command under a hard memory cap (a Windows Job Object), the Windows stand-in for
  `systemd-run -p MemoryMax=...`.

.DESCRIPTION
  The child is created suspended, put in a job whose committed memory is capped at -LimitGB, and
  only then resumed, so no allocation escapes the cap. Children it spawns join the same job. When
  the job's commit would exceed the cap the allocation fails, and a Rust binary aborts with
  "memory allocation of N bytes failed". Closing the job kills anything still in it.

  On exit it prints wall time, the job's peak committed memory, the exit code, and GUARD HIT when
  the peak came within 2% of the cap (the signature of a capped run). On a capped run the peak
  can read above the cap: Windows counts the request that was refused. The same summary is
  appended to -LogFile, which also gets the child's stdout and stderr.

  With MEMGUARD_SAMPLE_MS=<n> set, it also appends a line every n ms with the child's committed
  memory, `[memsample] t=12.3s commit=2048 MB`, between the child's own lines. With ctadl's
  CTADL_ITER_LOG on, that gives a memory reading for each iteration of a looping scc.
  (A PowerShell session that already loaded an older copy of this script's type must be
  restarted to load this one.)

.EXAMPLE
  scripts/memguard.ps1 -LimitGB 4 -LogFile run.log -- target/release/ctadl.exe index noto
#>
# A plain (non-advanced) param block, so everything after the two named options -- including
# arguments that look like switches, such as python's `-c` -- lands in $args untouched.
param([double] $LimitGB, [string] $LogFile)
# An environment variable rather than a parameter: any parameter here would also bind positionally.
$SampleMs = [int]$env:MEMGUARD_SAMPLE_MS
$Command = $args
if (-not $LimitGB -or -not $LogFile -or -not $Command) {
    throw 'usage: memguard.ps1 -LimitGB <gb> -LogFile <path> <command> [args...]'
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class MemGuard {
    [StructLayout(LayoutKind.Sequential)]
    struct BASIC_LIMIT {
        public long PerProcessUserTimeLimit, PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct IO_COUNTERS { public ulong a, b, c, d, e, f; }
    [StructLayout(LayoutKind.Sequential)]
    struct EXTENDED_LIMIT {
        public BASIC_LIMIT Basic;
        public IO_COUNTERS Io;
        public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct STARTUPINFO {
        public int cb;
        public string lpReserved, lpDesktop, lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2;
        public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
    [StructLayout(LayoutKind.Sequential)]
    struct SECURITY_ATTRIBUTES { public int nLength; public IntPtr lpSecurityDescriptor; public bool bInheritHandle; }

    [DllImport("kernel32", SetLastError = true)] static extern IntPtr CreateJobObject(IntPtr a, string n);
    [DllImport("kernel32", SetLastError = true)] static extern bool SetInformationJobObject(IntPtr j, int c, ref EXTENDED_LIMIT i, int l);
    [DllImport("kernel32", SetLastError = true)] static extern bool QueryInformationJobObject(IntPtr j, int c, out EXTENDED_LIMIT i, int l, IntPtr r);
    [DllImport("kernel32", SetLastError = true)] static extern bool AssignProcessToJobObject(IntPtr j, IntPtr p);
    [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateProcess(string app, string cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags,
        IntPtr env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr CreateFile(string name, uint access, uint share, ref SECURITY_ATTRIBUTES sa, uint disp, uint flags, IntPtr t);
    [DllImport("kernel32")] static extern uint ResumeThread(IntPtr t);
    [DllImport("kernel32")] static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32")] static extern bool GetExitCodeProcess(IntPtr h, out uint code);
    [DllImport("kernel32")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32")] static extern bool WriteFile(IntPtr h, byte[] b, int n, out int w, IntPtr o);
    [StructLayout(LayoutKind.Sequential)]
    struct PMC_EX { public int cb, PageFaultCount; public UIntPtr PeakWS, WS, a, b, c, d, PagefileUsage, PeakPagefileUsage, PrivateUsage; }
    [DllImport("kernel32")] static extern bool K32GetProcessMemoryInfo(IntPtr p, out PMC_EX c, int cb);

    const uint CREATE_SUSPENDED = 0x4, STARTF_USESTDHANDLES = 0x100;
    const uint LIMIT_JOB_MEMORY = 0x200, LIMIT_KILL_ON_JOB_CLOSE = 0x2000;

    // Returns { exitCode, peakJobBytes }.
    public static ulong[] Run(string cmdLine, ulong limitBytes, string logPath, string dir, uint sampleMs) {
        IntPtr job = CreateJobObject(IntPtr.Zero, null);
        var lim = new EXTENDED_LIMIT();
        lim.Basic.LimitFlags = LIMIT_JOB_MEMORY | LIMIT_KILL_ON_JOB_CLOSE;
        lim.JobMemoryLimit = (UIntPtr)limitBytes;
        if (!SetInformationJobObject(job, 9, ref lim, Marshal.SizeOf(lim)))
            throw new Exception("SetInformationJobObject failed: " + Marshal.GetLastWin32Error());

        // Child stdout and stderr both append to the log file.
        var sa = new SECURITY_ATTRIBUTES { nLength = Marshal.SizeOf(typeof(SECURITY_ATTRIBUTES)), bInheritHandle = true };
        IntPtr log = CreateFile(logPath, 0x4 /* FILE_APPEND_DATA */, 0x3, ref sa, 4 /* OPEN_ALWAYS */, 0x80, IntPtr.Zero);
        if (log == new IntPtr(-1)) throw new Exception("cannot open log: " + Marshal.GetLastWin32Error());

        var si = new STARTUPINFO { cb = Marshal.SizeOf(typeof(STARTUPINFO)), dwFlags = (int)STARTF_USESTDHANDLES,
                                   hStdOutput = log, hStdError = log };
        PROCESS_INFORMATION pi;
        if (!CreateProcess(null, cmdLine, IntPtr.Zero, IntPtr.Zero, true, CREATE_SUSPENDED, IntPtr.Zero, dir, ref si, out pi))
            throw new Exception("CreateProcess failed: " + Marshal.GetLastWin32Error());
        if (!AssignProcessToJobObject(job, pi.hProcess))
            throw new Exception("AssignProcessToJobObject failed: " + Marshal.GetLastWin32Error());
        ResumeThread(pi.hThread);
        if (sampleMs == 0) WaitForSingleObject(pi.hProcess, 0xFFFFFFFF);
        else {
            // One "[memsample]" line per period, appended to the log between the child's own
            // lines, so each can be placed against the [iter] and [mem cp] lines around it.
            var sw = System.Diagnostics.Stopwatch.StartNew();
            while (WaitForSingleObject(pi.hProcess, sampleMs) == 0x102 /* WAIT_TIMEOUT */) {
                PMC_EX c; c.cb = Marshal.SizeOf(typeof(PMC_EX));
                if (!K32GetProcessMemoryInfo(pi.hProcess, out c, c.cb)) continue;
                var line = System.Text.Encoding.UTF8.GetBytes(string.Format(
                    "[memsample] t={0:F1}s commit={1} MB\n", sw.Elapsed.TotalSeconds, (ulong)c.PrivateUsage >> 20));
                int w; WriteFile(log, line, line.Length, out w, IntPtr.Zero);
            }
        }

        uint code; GetExitCodeProcess(pi.hProcess, out code);
        EXTENDED_LIMIT q;
        QueryInformationJobObject(job, 9, out q, Marshal.SizeOf(typeof(EXTENDED_LIMIT)), IntPtr.Zero);
        CloseHandle(pi.hThread); CloseHandle(pi.hProcess); CloseHandle(log);
        CloseHandle(job); // kills any stragglers
        return new ulong[] { code, (ulong)q.PeakJobMemoryUsed };
    }
}
'@

# CreateProcess resolves a relative program, and starts the child, in this process's working
# directory, which `cd` in PowerShell does not move. Resolve both against PowerShell's location.
$here = (Get-Location).ProviderPath
if (Test-Path -LiteralPath $Command[0] -PathType Leaf) { $Command[0] = (Resolve-Path -LiteralPath $Command[0]).ProviderPath }
# Quote each argument for CreateProcess.
$cmdLine = ($Command | ForEach-Object { if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
$limit = [uint64]($LimitGB * 1GB)
# Against PowerShell's location, not .NET's working directory (which `cd` does not move).
$LogFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($LogFile)
$ErrorActionPreference = 'Stop'

$sw = [Diagnostics.Stopwatch]::StartNew()
$r = [MemGuard]::Run($cmdLine, $limit, $LogFile, $here, [uint32]$SampleMs)
$sw.Stop()

$peakGB = $r[1] / 1GB
$hit = $r[1] -ge 0.98 * $limit
$summary = "[memguard] cmd: $cmdLine`n[memguard] limit {0:N2} GB  peak {1:N2} GB  wall {2:N1} s  exit 0x{3:X8}{4}" -f `
    $LimitGB, $peakGB, $sw.Elapsed.TotalSeconds, $r[0], $(if ($hit) { '  GUARD HIT' } else { '' })
Add-Content -Path $LogFile -Value $summary -Encoding utf8
Write-Output $summary
exit [int]($r[0] -band 0x7FFFFFFF)
