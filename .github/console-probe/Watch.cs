using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

// Throwaway observer for chief#43 / codexbar#271: records every top-level window that is created or shown while a
// probe phase runs, from three independent sources: an out-of-context WinEvent hook (catches windows that flash for a
// few milliseconds), a 15 ms EnumWindows poll of the input desktop, and a periodic EnumDesktopWindows sweep of every
// desktop in this window station. Console-like windows are screenshotted the moment the poll sees them visible.
public static class ConsoleWatch
{
    delegate void WinEventProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
    delegate bool EnumProc(IntPtr hwnd, IntPtr l);
    delegate bool EnumNameProc([MarshalAs(UnmanagedType.LPWStr)] string name, IntPtr l);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr hwnd; public uint message; public IntPtr wParam, lParam; public uint time; public int x, y; }

    [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc proc, uint pid, uint tid, uint flags);
    [DllImport("user32.dll")] static extern bool UnhookWinEvent(IntPtr h);
    [DllImport("user32.dll")] static extern int GetMessage(out MSG m, IntPtr h, uint a, uint b);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG m);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref MSG m);
    [DllImport("user32.dll")] static extern bool PostThreadMessage(uint tid, uint msg, IntPtr w, IntPtr l);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumDesktopWindows(IntPtr desk, EnumProc p, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumWindowStations(EnumNameProc p, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDesktops(IntPtr ws, EnumNameProc p, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr OpenDesktop(string name, uint flags, bool inherit, uint access);
    [DllImport("user32.dll")] static extern bool CloseDesktop(IntPtr d);
    [DllImport("user32.dll")] static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr h, uint flags);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);

    const uint EVENT_SYSTEM_FOREGROUND = 0x0003, EVENT_OBJECT_CREATE = 0x8000, EVENT_OBJECT_SHOW = 0x8002;
    const uint WINEVENT_OUTOFCONTEXT = 0, WM_QUIT = 0x0012;
    static readonly string[] ConsoleClasses = { "ConsoleWindowClass", "PseudoConsoleWindow", "CASCADIA_HOSTING_WINDOW_CLASS" };
    static readonly string[] ConsoleProcs = { "gh", "conhost", "openconsole", "windowsterminal", "cmd", "powershell", "pwsh", "git", "node", "agy" };

    static readonly object Gate = new object();
    static readonly List<string> Lines = new List<string>();
    static readonly Dictionary<uint, string> Names = new Dictionary<uint, string>();
    static readonly HashSet<IntPtr> SeenVisible = new HashSet<IntPtr>();
    static readonly HashSet<IntPtr> SeenConsole = new HashSet<IntPtr>();
    static WinEventProc hookProc;
    static Thread hookThread, pollThread;
    static uint hookThreadId;
    static volatile bool running;
    static Stopwatch clock;
    static string shotDir;
    static int shots;
    static string phase = "setup";

    public static long Now { get { return clock == null ? 0 : clock.ElapsedMilliseconds; } }

    public static void Start(string outDir)
    {
        shotDir = outDir;
        Directory.CreateDirectory(shotDir);
        clock = Stopwatch.StartNew();
        running = true;
        // Windows that already exist are the baseline, not new windows.
        EnumWindows(delegate(IntPtr h, IntPtr l) { if (IsWindowVisible(h)) SeenVisible.Add(h); if (IsConsoleClass(ClassOf(h))) SeenConsole.Add(h); return true; }, IntPtr.Zero);
        Add("{\"t\":0,\"source\":\"start\",\"baselineVisible\":" + SeenVisible.Count + ",\"utc\":\"" + DateTime.UtcNow.ToString("o") + "\"}");
        hookThread = new Thread(HookLoop); hookThread.IsBackground = true; hookThread.Start();
        pollThread = new Thread(PollLoop); pollThread.IsBackground = true; pollThread.Start();
        Thread.Sleep(300);
    }

    public static void Mark(string name)
    {
        lock (Gate) { phase = name; }
        Add("{\"t\":" + Now + ",\"source\":\"phase\",\"phase\":" + Q(name) + ",\"utc\":\"" + DateTime.UtcNow.ToString("o") + "\"}");
    }

    public static string[] Stop()
    {
        running = false;
        if (hookThreadId != 0) PostThreadMessage(hookThreadId, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        if (hookThread != null) hookThread.Join(3000);
        if (pollThread != null) pollThread.Join(3000);
        lock (Gate) { return Lines.ToArray(); }
    }

    // Every desktop of every window station this process can open, with its visible and console-class windows.
    public static string Desktops()
    {
        var stations = new List<string>();
        EnumWindowStations(delegate(string n, IntPtr l) { stations.Add(n); return true; }, IntPtr.Zero);
        var desktops = new List<string>();
        EnumDesktops(GetProcessWindowStation(), delegate(string n, IntPtr l) { desktops.Add(n); return true; }, IntPtr.Zero);
        var sb = new StringBuilder();
        sb.Append("{\"t\":" + Now + ",\"source\":\"desktops\",\"phase\":" + Q(phase) + ",\"stations\":[");
        for (int i = 0; i < stations.Count; i++) { if (i > 0) sb.Append(','); sb.Append(Q(stations[i])); }
        sb.Append("],\"desktops\":[");
        for (int i = 0; i < desktops.Count; i++)
        {
            if (i > 0) sb.Append(',');
            int total = 0, visible = 0; var consoles = new List<string>();
            IntPtr d = OpenDesktop(desktops[i], 0, false, 0x0001 | 0x0040);
            if (d != IntPtr.Zero)
            {
                EnumDesktopWindows(d, delegate(IntPtr h, IntPtr l)
                {
                    total++;
                    bool v = IsWindowVisible(h);
                    if (v) visible++;
                    string cls = ClassOf(h);
                    if (IsConsoleClass(cls)) consoles.Add(Describe(h, "desktop-" + desktops[i]));
                    return true;
                }, IntPtr.Zero);
                CloseDesktop(d);
            }
            sb.Append("{\"name\":" + Q(desktops[i]) + ",\"opened\":" + (d != IntPtr.Zero ? "true" : "false") + ",\"windows\":" + total + ",\"visible\":" + visible + ",\"consoleWindows\":[" + string.Join(",", consoles.ToArray()) + "]}");
        }
        sb.Append("]}");
        string line = sb.ToString();
        Add(line);
        return line;
    }

    static void HookLoop()
    {
        hookThreadId = GetCurrentThreadId();
        hookProc = OnEvent;
        IntPtr a = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, IntPtr.Zero, hookProc, 0, 0, WINEVENT_OUTOFCONTEXT);
        IntPtr b = SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, IntPtr.Zero, hookProc, 0, 0, WINEVENT_OUTOFCONTEXT);
        Add("{\"t\":" + Now + ",\"source\":\"hook\",\"installed\":" + (a != IntPtr.Zero && b != IntPtr.Zero ? "true" : "false") + "}");
        MSG m;
        while (GetMessage(out m, IntPtr.Zero, 0, 0) > 0) { TranslateMessage(ref m); DispatchMessage(ref m); }
        UnhookWinEvent(a); UnhookWinEvent(b);
    }

    static void OnEvent(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time)
    {
        if (hwnd == IntPtr.Zero || idObject != 0) return;
        if (GetAncestor(hwnd, 2) != hwnd) return;
        string kind = ev == EVENT_OBJECT_SHOW ? "show" : ev == EVENT_OBJECT_CREATE ? "create" : "foreground";
        // Creation of invisible message/helper windows is noise unless it is a console host window.
        if (ev == EVENT_OBJECT_CREATE && !IsConsoleClass(ClassOf(hwnd))) return;
        Add(Describe(hwnd, "hook-" + kind));
    }

    static void PollLoop()
    {
        int tick = 0;
        while (running)
        {
            EnumWindows(delegate(IntPtr h, IntPtr l)
            {
                string cls = ClassOf(h);
                bool console = IsConsoleClass(cls);
                if (console && !SeenConsole.Contains(h)) { SeenConsole.Add(h); Add(Describe(h, "poll-console-window")); }
                if (IsWindowVisible(h) && !SeenVisible.Contains(h))
                {
                    SeenVisible.Add(h);
                    string line = Describe(h, "poll-new-visible");
                    Add(line);
                    if ((console || IsConsoleProc(ProcOf(h))) && shots < 40) Shoot(h);
                }
                return true;
            }, IntPtr.Zero);
            if (++tick % 70 == 0) Desktops();
            Thread.Sleep(15);
        }
    }

    static void Shoot(IntPtr h)
    {
        try
        {
            var area = System.Windows.Forms.Screen.PrimaryScreen.Bounds;
            using (var bmp = new Bitmap(area.Width, area.Height))
            using (var g = Graphics.FromImage(bmp))
            {
                g.CopyFromScreen(area.X, area.Y, 0, 0, bmp.Size);
                shots++;
                string file = string.Format("console-{0:D2}-{1}.png", shots, Now);
                bmp.Save(Path.Combine(shotDir, file), ImageFormat.Png);
                Add("{\"t\":" + Now + ",\"source\":\"screenshot\",\"phase\":" + Q(phase) + ",\"hwnd\":" + h.ToInt64() + ",\"file\":" + Q(file) + "}");
            }
        }
        catch (Exception e) { Add("{\"t\":" + Now + ",\"source\":\"screenshot-error\",\"error\":" + Q(e.Message) + "}"); }
    }

    public static void ShootTo(string path)
    {
        var area = System.Windows.Forms.Screen.PrimaryScreen.Bounds;
        using (var bmp = new Bitmap(area.Width, area.Height))
        using (var g = Graphics.FromImage(bmp)) { g.CopyFromScreen(area.X, area.Y, 0, 0, bmp.Size); bmp.Save(path, ImageFormat.Png); }
    }

    static string Describe(IntPtr h, string source)
    {
        uint pid; GetWindowThreadProcessId(h, out pid);
        RECT r; GetWindowRect(h, out r);
        int cloaked = 0; DwmGetWindowAttribute(h, 14, out cloaked, 4);
        bool visible = IsWindowVisible(h);
        bool onScreen = visible && cloaked == 0 && !IsIconic(h) && r.Right > r.Left && r.Bottom > r.Top;
        var title = new StringBuilder(256); GetWindowText(h, title, 256);
        string cls = ClassOf(h);
        string p; lock (Gate) { p = phase; }
        return "{\"t\":" + Now + ",\"source\":" + Q(source) + ",\"phase\":" + Q(p) + ",\"hwnd\":" + h.ToInt64() + ",\"class\":" + Q(cls)
            + ",\"title\":" + Q(title.ToString()) + ",\"pid\":" + pid + ",\"process\":" + Q(NameOf(pid)) + ",\"visible\":" + B(visible)
            + ",\"cloaked\":" + cloaked + ",\"onScreen\":" + B(onScreen) + ",\"consoleClass\":" + B(IsConsoleClass(cls))
            + ",\"foreground\":" + B(GetForegroundWindow() == h) + ",\"rect\":[" + r.Left + "," + r.Top + "," + r.Right + "," + r.Bottom + "]}";
    }

    static string ClassOf(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
    static string ProcOf(IntPtr h) { uint pid; GetWindowThreadProcessId(h, out pid); return NameOf(pid); }
    static bool IsConsoleClass(string cls) { return Array.IndexOf(ConsoleClasses, cls) >= 0; }
    static bool IsConsoleProc(string name) { return Array.IndexOf(ConsoleProcs, name.ToLowerInvariant()) >= 0; }

    static string NameOf(uint pid)
    {
        lock (Gate) { string n; if (Names.TryGetValue(pid, out n)) return n; }
        string name = "?";
        try { name = Process.GetProcessById((int)pid).ProcessName; } catch { }
        lock (Gate) { Names[pid] = name; }
        return name;
    }

    static void Add(string line) { lock (Gate) { Lines.Add(line); } }
    static string B(bool b) { return b ? "true" : "false"; }
    static string Q(string s)
    {
        var sb = new StringBuilder("\"");
        foreach (char c in s ?? "")
        {
            if (c == '"' || c == '\\') { sb.Append('\\'); sb.Append(c); }
            else if (c < 0x20) sb.Append(string.Format("\\u{0:x4}", (int)c));
            else sb.Append(c);
        }
        return sb.Append('"').ToString();
    }
}
