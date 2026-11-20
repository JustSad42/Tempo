using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace Tempo;

static class Native
{
    // ---------- Keyboard hook ----------
    public const int WH_KEYBOARD_LL = 13;
    public const int WM_KEYDOWN = 0x0100, WM_KEYUP = 0x0101, WM_SYSKEYDOWN = 0x0104, WM_SYSKEYUP = 0x0105;
    public const int WM_QUIT = 0x0012;
    public const uint LLKHF_INJECTED = 0x10;

    public const int VK_MEDIA_NEXT_TRACK = 0xB0, VK_MEDIA_PREV_TRACK = 0xB1, VK_MEDIA_STOP = 0xB2, VK_MEDIA_PLAY_PAUSE = 0xB3;

    [StructLayout(LayoutKind.Sequential)]
    public struct KBDLLHOOKSTRUCT { public uint vkCode, scanCode, flags, time; public IntPtr dwExtraInfo; }

    [StructLayout(LayoutKind.Sequential)]
    public struct MSG { public IntPtr hwnd; public uint message; public IntPtr wParam, lParam; public uint time; public int x, y; }

    public delegate IntPtr LowLevelKeyboardProc(int nCode, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern IntPtr SetWindowsHookEx(int idHook, LowLevelKeyboardProc lpfn, IntPtr hMod, uint dwThreadId);
    [DllImport("user32.dll")] public static extern bool UnhookWindowsHookEx(IntPtr hhk);
    [DllImport("user32.dll")] public static extern IntPtr CallNextHookEx(IntPtr hhk, int nCode, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetMessage(out MSG msg, IntPtr hWnd, uint min, uint max);
    [DllImport("user32.dll")] public static extern bool PostThreadMessage(uint threadId, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr GetModuleHandle(string? name);

    // ---------- Windows ----------
    public const int GWL_EXSTYLE = -20;
    public const int WS_EX_TOOLWINDOW = 0x80, WS_EX_TOPMOST = 0x8, WS_EX_NOACTIVATE = 0x08000000;
    public static readonly IntPtr HWND_TOPMOST = new(-1);
    public const uint SWP_NOSIZE = 0x1, SWP_NOMOVE = 0x2, SWP_NOACTIVATE = 0x10, SWP_NOZORDER = 0x4;
    public const int ASFW_ANY = -1;

    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr hWnd, int nIndex);
    [DllImport("user32.dll")] public static extern int SetWindowLong(IntPtr hWnd, int nIndex, int value);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool AllowSetForegroundWindow(int processId);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);

    public static void MakeOverlay(IntPtr hwnd)
    {
        int ex = GetWindowLong(hwnd, GWL_EXSTYLE);
        SetWindowLong(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST);
    }

    // ---------- Monitors ----------
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; public int Width => Right - Left; public int Height => Bottom - Top; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct MONITORINFO { public int cbSize; public RECT rcMonitor, rcWork; public uint dwFlags; }

    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT pt);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromPoint(POINT pt, uint flags);
    [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO info);

    /// <summary>Work area (screen minus taskbar) in physical pixels, of the monitor under the mouse or the primary one.</summary>
    public static RECT WorkArea(bool underMouse)
    {
        POINT pt = default;
        if (underMouse) GetCursorPos(out pt);
        var mon = MonitorFromPoint(pt, underMouse ? 2u /*NEAREST*/ : 1u /*PRIMARY*/);
        var info = new MONITORINFO { cbSize = Marshal.SizeOf<MONITORINFO>() };
        GetMonitorInfo(mon, ref info);
        return info.rcWork;
    }

    // ---------- Fullscreen detection ----------
    [DllImport("shell32.dll")] static extern int SHQueryUserNotificationState(out int state);

    /// <summary>True when a fullscreen game, video or presentation owns the screen.</summary>
    public static bool FullscreenAppActive()
    {
        if (SHQueryUserNotificationState(out int s) != 0) return false;
        return s is 2 /*BUSY*/ or 3 /*D3D fullscreen*/ or 4 /*presentation*/;
    }

    // ---------- DWM ----------
    [DllImport("dwmapi.dll")] static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);
    public static void SetDwm(IntPtr hwnd, int attr, int value) => DwmSetWindowAttribute(hwnd, attr, ref value, sizeof(int));

    // ---------- Processes ----------
    [DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern int GetApplicationUserModelId(IntPtr hProcess, ref int length, char[]? buffer);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool QueryFullProcessImageName(IntPtr hProcess, int flags, char[] buffer, ref int size);
    const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;

    /// <summary>Exe path and (for Store apps) the app ID of a process. Either may be null.</summary>
    public static (string? Path, string? Aumid) ProcessIdentity(int pid)
    {
        var h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        if (h == IntPtr.Zero) return (null, null);
        try
        {
            string? path = null, aumid = null;
            var buf = new char[1024];
            int size = buf.Length;
            if (QueryFullProcessImageName(h, 0, buf, ref size)) path = new string(buf, 0, size);
            int len = 0;
            if (GetApplicationUserModelId(h, ref len, null) == 122 /*ERROR_INSUFFICIENT_BUFFER*/ && len > 0)
            {
                var ab = new char[len];
                if (GetApplicationUserModelId(h, ref len, ab) == 0) aumid = new string(ab, 0, Math.Max(0, len - 1));
            }
            return (path, aumid);
        }
        finally { CloseHandle(h); }
    }

    // ---------- Shell items (app names and icons from an app ID) ----------
    [ComImport, Guid("43826d1e-e718-42ee-bc55-a1e261c37bfe"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellItem
    {
        void BindToHandler(IntPtr pbc, ref Guid bhid, ref Guid riid, out IntPtr ppv);
        void GetParent(out IShellItem ppsi);
        void GetDisplayName(uint sigdnName, [MarshalAs(UnmanagedType.LPWStr)] out string ppszName);
        void GetAttributes(uint sfgaoMask, out uint psfgaoAttribs);
        void Compare(IShellItem psi, uint hint, out int piOrder);
    }

    [StructLayout(LayoutKind.Sequential)]
    struct PROPERTYKEY { public Guid fmtid; public uint pid; }

    [ComImport, Guid("7e9fb0d3-919f-4307-ab2e-9b1860310c93"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellItem2
    {
        // IShellItem
        void BindToHandler(IntPtr pbc, ref Guid bhid, ref Guid riid, out IntPtr ppv);
        void GetParent(out IntPtr ppsi);
        void GetDisplayName(uint sigdnName, [MarshalAs(UnmanagedType.LPWStr)] out string ppszName);
        void GetAttributes(uint sfgaoMask, out uint psfgaoAttribs);
        void Compare(IntPtr psi, uint hint, out int piOrder);
        // IShellItem2 (only GetString is used; the others keep the vtable order)
        void GetPropertyStore(int flags, ref Guid riid, out IntPtr ppv);
        void GetPropertyStoreWithCreateObject(int flags, IntPtr punk, ref Guid riid, out IntPtr ppv);
        void GetPropertyStoreForKeys(IntPtr keys, uint cKeys, int flags, ref Guid riid, out IntPtr ppv);
        void GetPropertyDescriptionList(ref PROPERTYKEY key, ref Guid riid, out IntPtr ppv);
        void Update(IntPtr pbc);
        void GetProperty(ref PROPERTYKEY key, IntPtr ppropvar);
        void GetCLSID(ref PROPERTYKEY key, out Guid pclsid);
        void GetFileTime(ref PROPERTYKEY key, out long pft);
        void GetInt32(ref PROPERTYKEY key, out int pi);
        void GetString(ref PROPERTYKEY key, [MarshalAs(UnmanagedType.LPWStr)] out string ppsz);
    }

    [ComImport, Guid("bcc18b79-ba16-442f-80c4-8a59c30c463b"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellItemImageFactory
    {
        void GetImage(SIZE size, int flags, out IntPtr phbm);
    }

    [StructLayout(LayoutKind.Sequential)] struct SIZE { public int cx, cy; }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
    static extern void SHCreateItemFromParsingName(string path, IntPtr pbc, ref Guid riid, [MarshalAs(UnmanagedType.Interface)] out object item);

    /// <summary>Name, icon and target exe of an app ID as the Start menu knows it ("shell:AppsFolder\{id}").</summary>
    public static (string? Name, ImageSource? Icon, string? TargetPath) AppsFolderItem(string aumid, int iconPx)
    {
        string? name = null, target = null;
        ImageSource? icon = null;
        try
        {
            var iid = typeof(IShellItem2).GUID;
            SHCreateItemFromParsingName(@"shell:AppsFolder\" + aumid, IntPtr.Zero, ref iid, out object obj);
            var item = (IShellItem2)obj;
            try { item.GetDisplayName(0 /*NORMALDISPLAY*/, out name); } catch { }
            try
            {
                // PKEY_Link_TargetParsingPath
                var key = new PROPERTYKEY { fmtid = new Guid("B9B4B3FC-2B51-4A42-B5D8-324146AFCF25"), pid = 2 };
                item.GetString(ref key, out target);
            }
            catch { }
            try
            {
                var factory = (IShellItemImageFactory)obj;
                factory.GetImage(new SIZE { cx = iconPx, cy = iconPx }, 0x4 /*ICONONLY*/, out IntPtr hbm);
                icon = BitmapFromHBitmap(hbm);
            }
            catch { }
            Marshal.ReleaseComObject(obj);
        }
        catch { /* not a known app ID */ }
        return (name, icon, target);
    }

    // ---------- HBITMAP -> WPF with alpha ----------
    [StructLayout(LayoutKind.Sequential)]
    struct BITMAP { public int bmType, bmWidth, bmHeight, bmWidthBytes; public ushort bmPlanes, bmBitsPixel; public IntPtr bmBits; }
    [StructLayout(LayoutKind.Sequential)]
    struct BITMAPINFOHEADER { public int biSize, biWidth, biHeight; public short biPlanes, biBitCount; public int biCompression, biSizeImage, biXPelsPerMeter, biYPelsPerMeter, biClrUsed, biClrImportant; }

    [DllImport("gdi32.dll")] static extern int GetObject(IntPtr h, int size, out BITMAP bmp);
    [DllImport("gdi32.dll")] static extern int GetDIBits(IntPtr hdc, IntPtr hbm, uint start, uint lines, byte[] bits, ref BITMAPINFOHEADER bmi, uint usage);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr hwnd, IntPtr hdc);

    static BitmapSource? BitmapFromHBitmap(IntPtr hbm)
    {
        if (hbm == IntPtr.Zero) return null;
        try
        {
            GetObject(hbm, Marshal.SizeOf<BITMAP>(), out var bm);
            int w = bm.bmWidth, h = bm.bmHeight;
            var bmi = new BITMAPINFOHEADER { biSize = Marshal.SizeOf<BITMAPINFOHEADER>(), biWidth = w, biHeight = -h, biPlanes = 1, biBitCount = 32 };
            var bits = new byte[w * h * 4];
            var dc = GetDC(IntPtr.Zero);
            GetDIBits(dc, hbm, 0, (uint)h, bits, ref bmi, 0);
            ReleaseDC(IntPtr.Zero, dc);
            var src = BitmapSource.Create(w, h, 96, 96, PixelFormats.Pbgra32, null, bits, w * 4);
            src.Freeze();
            return src;
        }
        finally { DeleteObject(hbm); }
    }

    // ---------- Raw input (independent check that the hook is still alive) ----------
    [StructLayout(LayoutKind.Sequential)]
    public struct RAWINPUTDEVICE { public ushort UsagePage, Usage; public uint Flags; public IntPtr Target; }
    public const uint RIDEV_INPUTSINK = 0x100;
    public const int WM_INPUT = 0x00FF;
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool RegisterRawInputDevices(RAWINPUTDEVICE[] devices, uint count, uint size);
    [DllImport("user32.dll")]
    public static extern uint GetRawInputData(IntPtr hRawInput, uint command, IntPtr data, ref uint size, uint headerSize);

    /// <summary>Reads a WM_INPUT keyboard message: (virtual key, key down). Null if it isn't a keyboard event.</summary>
    public static (int Vk, bool Down)? ReadRawKey(IntPtr lParam)
    {
        uint header = (uint)(8 + 2 * IntPtr.Size);
        uint size = 0;
        GetRawInputData(lParam, 0x10000003, IntPtr.Zero, ref size, header);
        if (size == 0 || size > 1024) return null;
        var buf = Marshal.AllocHGlobal((int)size);
        try
        {
            if (GetRawInputData(lParam, 0x10000003, buf, ref size, header) != size) return null;
            if (Marshal.ReadInt32(buf) != 1 /*RIM_TYPEKEYBOARD*/) return null;
            int off = (int)header;
            ushort flags = (ushort)Marshal.ReadInt16(buf, off + 2);
            ushort vk = (ushort)Marshal.ReadInt16(buf, off + 6);
            return (vk, (flags & 1) == 0);
        }
        finally { Marshal.FreeHGlobal(buf); }
    }

    // ---------- Finding and focusing an app's window ----------
    delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hwnd, uint cmd);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out int pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder sb, int max);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();

    /// <summary>
    /// Brings forward a top-level window of one of the given processes, preferring one whose title contains <paramref name="titleHint"/>
    /// (a browser window showing the playing tab). Returns false if none was found.
    /// </summary>
    public static bool FocusAppWindow(IReadOnlySet<string> exeNames, string? aumid, string? titleHint)
    {
        IntPtr best = IntPtr.Zero; int bestScore = -1;
        var pidCache = new Dictionary<int, bool>();
        EnumWindows((h, _) =>
        {
            if (!IsWindowVisible(h) || GetWindow(h, 4 /*GW_OWNER*/) != IntPtr.Zero) return true;
            GetWindowThreadProcessId(h, out int pid);
            if (!pidCache.TryGetValue(pid, out bool match))
            {
                var (path, procAumid) = ProcessIdentity(pid);
                match = (path != null && exeNames.Contains(Path.GetFileNameWithoutExtension(path))) ||
                        (aumid != null && procAumid != null && string.Equals(aumid, procAumid, StringComparison.OrdinalIgnoreCase));
                pidCache[pid] = match;
            }
            if (!match) return true;
            var sb = new System.Text.StringBuilder(512);
            GetWindowText(h, sb, sb.Capacity);
            string title = sb.ToString();
            if (title.Length == 0) return true;
            int score = 1;
            if (!string.IsNullOrWhiteSpace(titleHint) && title.Contains(titleHint, StringComparison.OrdinalIgnoreCase)) score = 3;
            if (score > bestScore) { best = h; bestScore = score; }
            return true;
        }, IntPtr.Zero);
        if (best == IntPtr.Zero) return false;
        if (IsIconic(best)) ShowWindow(best, 9 /*SW_RESTORE*/);
        // Windows only lets the app with the latest input take the foreground; a tap of Alt counts as that.
        keybd_event(0x12, 0, 0, UIntPtr.Zero);
        keybd_event(0x12, 0, 2, UIntPtr.Zero);
        SetForegroundWindow(best);
        return GetForegroundWindow() == best;
    }
}
