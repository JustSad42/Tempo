using System.Runtime.InteropServices;

namespace Tempo;

public enum MediaKey { PlayPause, Next, Previous, Stop }

/// <summary>
/// Catches the keyboard's media keys before Windows does. Runs on its own thread with its own message loop,
/// so the hook always answers instantly (Windows silently removes hooks that are slow).
/// </summary>
sealed class MediaKeyHook : IDisposable
{
    /// <summary>Called on the hook thread when a key goes down. Return true to keep the key from Windows. Must be fast.</summary>
    public Func<MediaKey, bool>? ShouldCapture;
    /// <summary>Called on the hook thread for captured keys: (key, isDown, isRepeat).</summary>
    public Action<MediaKey, bool, bool>? Pressed;

    readonly Native.LowLevelKeyboardProc _proc;
    readonly bool[] _captured = new bool[4];
    readonly bool[] _down = new bool[4];
    Thread? _thread;
    /// <summary>Tick count of the last media key event the hook received (any key, captured or not).</summary>
    public long LastSeen;
    volatile bool _blind;
    /// <summary>
    /// Test only: the hook stops noticing keys, as if Windows had dropped it, but still keeps them from Windows
    /// (so a test can't end up controlling whatever the user is playing).
    /// </summary>
    public void SimulateDrop() => _blind = true;
    uint _threadId;
    IntPtr _hook;

    public MediaKeyHook() => _proc = Callback;

    public void Start()
    {
        var ready = new ManualResetEventSlim();
        _thread = new Thread(() =>
        {
            _threadId = Native.GetCurrentThreadId();
            _hook = Native.SetWindowsHookEx(Native.WH_KEYBOARD_LL, _proc, Native.GetModuleHandle(null), 0);
            ready.Set();
            while (Native.GetMessage(out var msg, IntPtr.Zero, 0, 0) > 0) { }
            if (_hook != IntPtr.Zero) Native.UnhookWindowsHookEx(_hook);
            _hook = IntPtr.Zero;
        }) { IsBackground = true, Name = "Media key hook", Priority = ThreadPriority.Highest };
        _thread.SetApartmentState(ApartmentState.MTA);
        _thread.Start();
        ready.Wait(3000);
        if (_hook == IntPtr.Zero) throw new InvalidOperationException("Could not install the keyboard hook.");
    }

    static MediaKey? Map(uint vk) => vk switch
    {
        Native.VK_MEDIA_PLAY_PAUSE => MediaKey.PlayPause,
        Native.VK_MEDIA_NEXT_TRACK => MediaKey.Next,
        Native.VK_MEDIA_PREV_TRACK => MediaKey.Previous,
        Native.VK_MEDIA_STOP => MediaKey.Stop,
        _ => null,
    };

    IntPtr Callback(int nCode, IntPtr wParam, IntPtr lParam)
    {
        if (nCode >= 0)
        {
            var info = Marshal.PtrToStructure<Native.KBDLLHOOKSTRUCT>(lParam);
            if (Map(info.vkCode) is MediaKey key)
            {
                if (_blind) return 1;
                Interlocked.Exchange(ref LastSeen, Environment.TickCount64);
                int msg = (int)wParam;
                bool isDown = msg is Native.WM_KEYDOWN or Native.WM_SYSKEYDOWN;
                int k = (int)key;
                bool repeat = isDown && _down[k];
                if (isDown && !repeat)
                {
                    bool capture = false;
                    try { capture = ShouldCapture?.Invoke(key) ?? false; } catch { }
                    _captured[k] = capture;
                }
                _down[k] = isDown;
                if (_captured[k])
                {
                    try { Pressed?.Invoke(key, isDown, repeat); } catch { }
                    return 1; // swallow: Windows' own flyout and other apps never see it
                }
            }
        }
        return Native.CallNextHookEx(_hook, nCode, wParam, lParam);
    }

    public void Dispose()
    {
        if (_threadId != 0) Native.PostThreadMessage(_threadId, Native.WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        _thread?.Join(1000);
    }
}
