using System.Runtime.InteropServices;

namespace Tempo;

/// <summary>
/// Per-app volume (the same levels as Windows' Volume Mixer) and "who is making sound right now",
/// read from the audio sessions on every active output device.
/// </summary>
static class AppAudio
{
    public sealed record Session(int Pid, string Exe, string? Aumid, ISimpleAudioVolume Volume, IAudioMeterInformation? Meter);

    /// <summary>All audio sessions with a process behind them (system sounds excluded).</summary>
    public static List<Session> Sessions()
    {
        var list = new List<Session>();
        IMMDeviceEnumerator? en = null;
        try
        {
            en = (IMMDeviceEnumerator)new MMDeviceEnumeratorCom();
            en.EnumAudioEndpoints(0 /*render*/, 1 /*active*/, out var devices);
            devices.GetCount(out int count);
            var exeCache = new Dictionary<int, (string Exe, string? Aumid)>();
            for (int d = 0; d < count; d++)
            {
                devices.Item(d, out var device);
                var iid = typeof(IAudioSessionManager2).GUID;
                device.Activate(ref iid, 0x17 /*CLSCTX_ALL*/, IntPtr.Zero, out object mgrObj);
                var mgr = (IAudioSessionManager2)mgrObj;
                mgr.GetSessionEnumerator(out var sessions);
                sessions.GetCount(out int n);
                for (int i = 0; i < n; i++)
                {
                    sessions.GetSession(i, out var ctl);
                    if (ctl.IsSystemSoundsSession() == 0) continue; // S_OK means it IS the system sounds session
                    ctl.GetProcessId(out uint pid);
                    if (pid == 0) continue;
                    if (!exeCache.TryGetValue((int)pid, out var id))
                    {
                        var (path, aumid) = Native.ProcessIdentity((int)pid);
                        id = (path != null ? Path.GetFileNameWithoutExtension(path) : "", aumid);
                        exeCache[(int)pid] = id;
                    }
                    list.Add(new Session((int)pid, id.Exe, id.Aumid, (ISimpleAudioVolume)ctl, ctl as IAudioMeterInformation));
                }
            }
        }
        catch { /* audio service unavailable */ }
        return list;
    }

    public static bool Belongs(Session s, AppIdentity app) =>
        (s.Aumid != null && string.Equals(s.Aumid, app.Aumid, StringComparison.OrdinalIgnoreCase)) ||
        app.ExeNames.Contains(s.Exe);

    /// <summary>The app's volume (0–1) and mute state, or null if it has no audio session.</summary>
    public static (float Level, bool Muted)? GetVolume(AppIdentity app)
    {
        foreach (var s in Sessions())
        {
            if (!Belongs(s, app)) continue;
            try
            {
                s.Volume.GetMasterVolume(out float v);
                s.Volume.GetMute(out bool m);
                return (v, m);
            }
            catch { }
        }
        return null;
    }

    public static void SetVolume(AppIdentity app, float level, bool? mute = null)
    {
        var ctx = Guid.Empty;
        level = Math.Clamp(level, 0f, 1f);
        foreach (var s in Sessions())
        {
            if (!Belongs(s, app)) continue;
            try
            {
                s.Volume.SetMasterVolume(level, ref ctx);
                if (mute is bool m) s.Volume.SetMute(m, ref ctx);
            }
            catch { }
        }
    }

    /// <summary>
    /// Processes currently making sound that aren't one of the given apps
    /// (e.g. a player that doesn't report to Windows' media controls, like VLC 3).
    /// </summary>
    public static List<(int Pid, string Exe)> AudibleOutside(IReadOnlyList<AppIdentity> apps)
    {
        var result = new List<(int, string)>();
        foreach (var s in Sessions())
        {
            if (s.Meter == null || string.IsNullOrEmpty(s.Exe)) continue;
            if (apps.Any(a => Belongs(s, a))) continue;
            try
            {
                s.Volume.GetMute(out bool muted);
                s.Meter.GetPeakValue(out float peak);
                if (!muted && peak > 0.0005f) result.Add((s.Pid, s.Exe));
            }
            catch { }
        }
        return result;
    }

    // ---------- COM definitions ----------
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumeratorCom { }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IMMDeviceEnumerator
    {
        void EnumAudioEndpoints(int dataFlow, int stateMask, out IMMDeviceCollection devices);
        void GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice device);
    }

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IMMDeviceCollection
    {
        void GetCount(out int count);
        void Item(int index, out IMMDevice device);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IMMDevice
    {
        void Activate(ref Guid iid, int clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface);
    }

    [ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionManager2
    {
        void GetAudioSessionControl(IntPtr guid, int flags, out IntPtr control);
        void GetSimpleAudioVolume(IntPtr guid, int flags, out IntPtr volume);
        void GetSessionEnumerator(out IAudioSessionEnumerator sessions);
    }

    [ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionEnumerator
    {
        void GetCount(out int count);
        void GetSession(int index, out IAudioSessionControl2 session);
    }

    [ComImport, Guid("bfb7ff88-7239-4fc9-8fa2-07c950be9c6d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionControl2
    {
        // IAudioSessionControl
        void GetState(out int state);
        void GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string name);
        void SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string name, ref Guid ctx);
        void GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string path);
        void SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string path, ref Guid ctx);
        void GetGroupingParam(out Guid param);
        void SetGroupingParam(ref Guid param, ref Guid ctx);
        void RegisterAudioSessionNotification(IntPtr client);
        void UnregisterAudioSessionNotification(IntPtr client);
        // IAudioSessionControl2
        void GetSessionIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string id);
        void GetSessionInstanceIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string id);
        void GetProcessId(out uint pid);
        [PreserveSig] int IsSystemSoundsSession();
        void SetDuckingPreference(bool optOut);
    }

    [ComImport, Guid("87CE5498-68D6-44E5-9215-6DA47EF883D8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface ISimpleAudioVolume
    {
        void SetMasterVolume(float level, ref Guid ctx);
        void GetMasterVolume(out float level);
        void SetMute([MarshalAs(UnmanagedType.Bool)] bool mute, ref Guid ctx);
        void GetMute([MarshalAs(UnmanagedType.Bool)] out bool mute);
    }

    [ComImport, Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IAudioMeterInformation
    {
        void GetPeakValue(out float peak);
    }
}
