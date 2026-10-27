using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

// Throwaway GUI-subsystem launcher (no console of its own, like a release CodexBar) that starts one command line with
// a chosen set of creation flags, waits up to 60 s and appends "mode pid exit" to a result file.
//   launch.exe <mode> <result-file> <command line...>
// Modes: plain (0), nowindow (CREATE_NO_WINDOW), contained (CREATE_NO_WINDOW | CREATE_SUSPENDED, job, resume: what
// CodexBar's contained::spawn does), hidden (CREATE_NEW_CONSOLE + STARTF_USESHOWWINDOW/SW_HIDE), newconsole
// (CREATE_NEW_CONSOLE, the positive control), detached (DETACHED_PROCESS).
public static class Launch
{
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct STARTUPINFO
    {
        public int cb; public string lpReserved, lpDesktop, lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }
    [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int pid, tid; }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CreateProcessW(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr h, out uint code);
    [DllImport("kernel32.dll")] static extern uint ResumeThread(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern IntPtr CreateJobObjectW(IntPtr sa, string name);
    [DllImport("kernel32.dll")] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll")] static extern bool TerminateProcess(IntPtr h, uint code);

    const uint CREATE_SUSPENDED = 0x4, DETACHED_PROCESS = 0x8, CREATE_NEW_CONSOLE = 0x10, CREATE_NO_WINDOW = 0x08000000;
    const int STARTF_USESHOWWINDOW = 0x1; const short SW_HIDE = 0;

    public static int Main(string[] args)
    {
        string mode = args[0], result = args[1];
        var line = new StringBuilder();
        for (int i = 2; i < args.Length; i++) { if (i > 2) line.Append(' '); line.Append(args[i]); }
        var si = new STARTUPINFO(); si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
        uint flags = 0;
        switch (mode)
        {
            case "plain": break;
            case "nowindow": flags = CREATE_NO_WINDOW; break;
            case "contained": flags = CREATE_NO_WINDOW | CREATE_SUSPENDED; break;
            case "hidden": flags = CREATE_NEW_CONSOLE; si.dwFlags = STARTF_USESHOWWINDOW; si.wShowWindow = SW_HIDE; break;
            case "newconsole": flags = CREATE_NEW_CONSOLE; break;
            case "detached": flags = DETACHED_PROCESS; break;
            default: File.AppendAllText(result, mode + " unknown-mode\r\n"); return 2;
        }
        PROCESS_INFORMATION pi;
        if (!CreateProcessW(null, line, IntPtr.Zero, IntPtr.Zero, false, flags, IntPtr.Zero, null, ref si, out pi))
        {
            File.AppendAllText(result, mode + " create-failed " + Marshal.GetLastWin32Error() + "\r\n");
            return 1;
        }
        IntPtr job = IntPtr.Zero;
        if (mode == "contained")
        {
            job = CreateJobObjectW(IntPtr.Zero, null);
            AssignProcessToJobObject(job, pi.hProcess);
            ResumeThread(pi.hThread);
        }
        uint code = 259;
        if (WaitForSingleObject(pi.hProcess, 60000) != 0) TerminateProcess(pi.hProcess, 1);
        GetExitCodeProcess(pi.hProcess, out code);
        File.AppendAllText(result, mode + " " + pi.pid + " " + code + "\r\n");
        CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
        if (job != IntPtr.Zero) CloseHandle(job);
        return 0;
    }
}
