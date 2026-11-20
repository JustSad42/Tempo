using System.Windows.Interop;

namespace Tempo;

/// <summary>
/// Second pair of eyes on the media keys. Windows silently disconnects keyboard hooks that ever answer too slowly,
/// and then keys quietly go back to Windows. Raw input sees every key press regardless of the hook;
/// if a media key shows up here and the hook never saw it, the hook is reconnected immediately.
/// </summary>
sealed class HookWatch : IDisposable
{
    readonly HwndSource _window;
    readonly Func<long> _hookLastSeen;
    readonly Action _hookDied;
    long _lastRecovery;
    static readonly bool TestMode = !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("TEMPO_INSTANCE"));

    public HookWatch(Func<long> hookLastSeen, Action hookDied)
    {
        _hookLastSeen = hookLastSeen;
        _hookDied = hookDied;
        var p = new HwndSourceParameters("Tempo.HookWatch") { Width = 0, Height = 0, WindowStyle = 0, ParentWindow = new IntPtr(-3) /*HWND_MESSAGE*/ };
        _window = new HwndSource(p);
        _window.AddHook(WndProc);
        var dev = new[] { new Native.RAWINPUTDEVICE { UsagePage = 1, Usage = 6, Flags = Native.RIDEV_INPUTSINK, Target = _window.Handle } };
        if (!Native.RegisterRawInputDevices(dev, 1, (uint)System.Runtime.InteropServices.Marshal.SizeOf<Native.RAWINPUTDEVICE>()))
            App.Log("Raw input watch unavailable.");
    }

    IntPtr WndProc(IntPtr hwnd, int msg, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (msg != Native.WM_INPUT) return IntPtr.Zero;
        var key = Native.ReadRawKey(lParam);
        if (TestMode && key is { Down: true, Vk: 0x87 /*F24*/ }) App.Log("Test: raw input saw F24.");
        // In test mode F24 stands in for "a media key the hook never saw" (a real dropped hook can't be faked safely).
        if (key is { Down: true } k && (k.Vk is >= Native.VK_MEDIA_NEXT_TRACK and <= Native.VK_MEDIA_PLAY_PAUSE || (TestMode && k.Vk == 0x87)))
        {
            long seenAt = Environment.TickCount64;
            // Give the hook a moment (raw input and the hook can arrive in either order).
            var check = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromMilliseconds(250) };
            check.Tick += (_, _) =>
            {
                check.Stop();
                long last = _hookLastSeen();
                if (seenAt - last > 600 && seenAt - _lastRecovery > 2000)
                {
                    _lastRecovery = seenAt;
                    _hookDied();
                }
            };
            check.Start();
        }
        return IntPtr.Zero;
    }

    public void Dispose() => _window.Dispose();
}
