using System.Diagnostics;
using System.Text.Json;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Threading;

namespace Tempo;

public partial class App : Application
{
    static readonly string InstanceTag = Environment.GetEnvironmentVariable("TEMPO_INSTANCE") is { Length: > 0 } t ? "." + t : "";
    static readonly string MutexName = @"Local\Tempo.SingleInstance" + InstanceTag;
    static readonly string ShowEventName = @"Local\Tempo.ShowSettings" + InstanceTag;
    static readonly string PreviewEventName = @"Local\Tempo.Preview" + InstanceTag;
    static readonly string DropHookEventName = @"Local\Tempo.TestDropHook" + InstanceTag;

    // Players that don't report to Windows' media controls; while one of them is making sound, keys go to it.
    static readonly HashSet<string> NonReportingPlayers = new(StringComparer.OrdinalIgnoreCase)
    {
        "vlc", "mpc-hc", "mpc-hc64", "mpc-be", "mpc-be64", "PotPlayer", "PotPlayer64", "PotPlayerMini", "PotPlayerMini64",
        "winamp", "AIMP", "kmplayer", "KMPlayer64", "GOM", "SMPlayer", "mpv", "foobar2000", "Plex", "PlexMediaPlayer", "Kodi",
    };

    Mutex? _mutex;
    MediaHub? _hub;
    MediaKeyHook? _hook;
    PopupWindow? _popup;
    KeyGestures? _gestures;
    SettingsWindow? _settingsWindow;
    DispatcherTimer? _rehook;
    HookWatch? _hookWatch;
    readonly Stats _stats = Stats.Load();
    readonly HistoryStore _history = HistoryStore.Load();

    // Pass-through state. _audiblePlayerPid is refreshed in the background so the hook never waits on audio queries.
    volatile int _audiblePlayerPid;
    int _passPid;
    long _passAtTicks;
    long _lastSessionStartTicks;

    public AppSettings Settings { get; private set; } = new();

    protected override async void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        DispatcherUnhandledException += (_, ex) => { Log("UI error: " + ex.Exception); ex.Handled = true; };
        AppDomain.CurrentDomain.UnhandledException += (_, ex) => Log("Fatal: " + ex.ExceptionObject);
        TaskScheduler.UnobservedTaskException += (_, ex) => { Log("Task error: " + ex.Exception.Message); ex.SetObserved(); };

        int dump = Array.IndexOf(e.Args, "--dump");
        if (dump >= 0)
        {
            await DumpAsync(dump + 1 < e.Args.Length ? e.Args[dump + 1] : Path.Combine(AppSettings.Folder, "sessions.json"));
            Shutdown();
            return;
        }

        if (InstanceTag != "" && Environment.GetEnvironmentVariable("TEMPO_RENDER") == "software") // test only
            System.Windows.Media.RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly;
        bool preview = e.Args.Contains("--preview");
        if (e.Args.Contains("--test-drop-hook") && InstanceTag != "")
        {
            try { EventWaitHandle.OpenExisting(DropHookEventName).Set(); } catch { }
            Shutdown();
            return;
        }
        if (await RunSupervisorAsync(e.Args)) return;

        // Test only: crash on purpose to check that the supervisor brings Tempo back.
        if (InstanceTag != "" && int.TryParse(Environment.GetEnvironmentVariable("TEMPO_TEST_CRASH_AFTER"), out int crashAfter) && !e.Args.Contains("--restarted"))
            new Thread(() => { Thread.Sleep(crashAfter * 1000); System.Runtime.InteropServices.Marshal.ReadInt32(IntPtr.Zero); }) { IsBackground = true }.Start();

        Updater.CleanUp();
        _mutex = new Mutex(true, MutexName, out bool firstInstance);
        if (!firstInstance)
        {
            // Already running: ask it to open settings (or show the pop-up), then exit.
            Native.AllowSetForegroundWindow(Native.ASFW_ANY);
            try { EventWaitHandle.OpenExisting(preview ? PreviewEventName : ShowEventName).Set(); } catch { }
            _mutex = null;
            Shutdown();
            return;
        }

        bool firstRun = !File.Exists(Path.Combine(AppSettings.Folder, "settings.json"));
        Settings = AppSettings.Load();
        if (firstRun)
        {
            Settings.Save();
            if (string.IsNullOrEmpty(InstanceTag))
                try { AutoStart.Set(true); } catch (Exception ex) { Log("Could not enable start-up: " + ex.Message); }
        }
        else if (string.IsNullOrEmpty(InstanceTag)) { try { AutoStart.Repair(); } catch { } }

        _hub = new MediaHub(Dispatcher, () => Settings);
        _hub.Changed += () =>
        {
            var newest = _hub.Sessions.Where(s => s.IsPlaying).Select(s => s.StartedAt ?? DateTime.MinValue).DefaultIfEmpty(DateTime.MinValue).Max();
            if (newest != DateTime.MinValue) Interlocked.Exchange(ref _lastSessionStartTicks, newest.Ticks);
        };
        Settings.Saved += () => Dispatcher.BeginInvoke(_hub.Refresh);
        _hub.TrackStarted += s => _history.Add(s);
        try { await _hub.StartAsync(); }
        catch (Exception ex) { Log("Media sessions unavailable: " + ex); }

        _popup = new PopupWindow(_hub, () => Settings);
        new WindowInteropHelper(_popup).EnsureHandle();
        _popup.Warmup();
        _popup.SettingsRequested += ShowSettings;
        _gestures = new KeyGestures(_hub, _popup, () => Settings, _stats);

        StartHook();
        // Windows silently drops keyboard hooks that ever answer too slowly (e.g. under heavy load); renew it now and then.
        _rehook = new DispatcherTimer { Interval = TimeSpan.FromMinutes(30) };
        _rehook.Tick += (_, _) => { _hook?.Dispose(); StartHook(); };
        _rehook.Start();
        _hookWatch = new HookWatch(() => _hook == null ? 0 : Interlocked.Read(ref _hook.LastSeen), () =>
        {
            Log("Keyboard hook stopped receiving keys; reconnected it.");
            _stats.HookRecoveries++;
            _stats.Touch();
            _hook?.Dispose();
            StartHook();
        });
        StartAudioWatch();

        var flush = new DispatcherTimer { Interval = TimeSpan.FromSeconds(30) };
        flush.Tick += (_, _) => _stats.Flush();
        flush.Start();
        StartUpdates(e.Args.Contains("--updated"));
        if (InstanceTag == "") EnsureStartMenuShortcut();

        Microsoft.Win32.SystemEvents.PowerModeChanged += (_, pm) =>
        {
            if (pm.Mode == Microsoft.Win32.PowerModes.Resume) Dispatcher.BeginInvoke(() => { _hook?.Dispose(); StartHook(); });
        };

        ListenForSignals();

        if (preview) _popup.Flash(PopupAction.None, fromMouse: true);
        else if (!e.Args.Contains("--background")) ShowSettings();
        Log($"Started {(firstRun ? "(first run) " : "")}{(e.Args.Contains("--restarted") ? "(restarted after a crash) " : "")}with {_hub.Sessions.Count} media session(s).");
        MemoryTrim.Soon(15);
    }

    void StartHook()
    {
        // Test only: measure or preview without touching the keyboard.
        if (InstanceTag != "" && Environment.GetEnvironmentVariable("TEMPO_NO_HOOK") == "1") return;
        try
        {
            _hook = new MediaKeyHook
            {
                ShouldCapture = ShouldCapture,
                Pressed = (key, down, repeat) => Dispatcher.BeginInvoke(() => _gestures?.OnKey(key, down, repeat), DispatcherPriority.Input),
            };
            _hook.Start();
        }
        catch (Exception ex) { Log("Keyboard hook failed: " + ex.Message); }
    }

    /// <summary>Hook thread: should Tempo take this key press, or leave it to Windows?</summary>
    bool ShouldCapture(MediaKey key)
    {
        var hub = _hub;
        if (hub == null) return false;
        if (!hub.HasTarget) return Settings.AlwaysCapture; // nothing to control: Tempo still keeps the key (and says so)
        if (!Settings.PassThroughUnknownPlayers || hub.TargetPlaying) return true;

        // A player that Windows doesn't know about is making sound: leave the key to it.
        int audible = _audiblePlayerPid;
        if (audible != 0)
        {
            _passPid = audible;
            _passAtTicks = DateTime.Now.Ticks;
            _stats.PassedToOtherPlayers++;
            return false;
        }
        // Still the last player we gave the keys to (it's paused now), and no reporting app has started since.
        if (_passPid != 0 && Interlocked.Read(ref _lastSessionStartTicks) < _passAtTicks && ProcessAlive(_passPid))
            return false;
        _passPid = 0;
        return true;
    }

    /// <summary>Once a second, notes whether a player that doesn't report to Windows (VLC 3…) is making sound.</summary>
    void StartAudioWatch()
    {
        new Thread(() =>
        {
            bool playerOpen = false;
            long nextProcessScan = 0;
            while (true)
            {
                try
                {
                    var hub = _hub;
                    bool wanted = hub != null && Settings.PassThroughUnknownPlayers && !hub.TargetPlaying;
                    // Listing processes is cheap; querying every audio session is not. Only do the latter
                    // while one of those players is open (checked every 10 s).
                    if (wanted && Environment.TickCount64 >= nextProcessScan)
                    {
                        nextProcessScan = Environment.TickCount64 + 10_000;
                        playerOpen = NonReportingPlayers.Any(n => { var ps = Process.GetProcessesByName(n); bool any = ps.Length > 0; foreach (var x in ps) x.Dispose(); return any; });
                    }
                    _audiblePlayerPid = wanted && playerOpen
                        ? AppAudio.AudibleOutside(hub!.Apps).FirstOrDefault(a => NonReportingPlayers.Contains(a.Exe)).Pid
                        : 0;
                }
                catch { _audiblePlayerPid = 0; }
                Thread.Sleep(1000);
            }
        }) { IsBackground = true, Name = "Audio watch", Priority = ThreadPriority.BelowNormal }.Start();
    }

    void StartUpdates(bool justUpdated)
    {
        if (justUpdated) Log($"Updated to {Updater.CurrentText}.");
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(30) };
        timer.Tick += async (_, _) =>
        {
            timer.Interval = TimeSpan.FromHours(6);
            if (Settings.AutoUpdate) await CheckForUpdateAsync(false);
        };
        timer.Start();
    }

    public async Task CheckForUpdateAsync(bool manual)
    {
        await Updater.CheckAsync(manual);
        if (Updater.Ready == null) return;
        // Install when nothing is on screen and no key is being held.
        var wait = new DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
        wait.Tick += (_, _) =>
        {
            if (_popup?.IsVisible == true || _gestures?.IsHolding == true || _settingsWindow != null) return;
            wait.Stop();
            Log($"Installing version {Updater.Ready}.");
            _stats.Flush();
            _hook?.Dispose();
            if (!Updater.ApplyAndRestart()) StartHook();
        };
        wait.Start();
    }

    /// <summary>Start menu entry, so Tempo (and its settings) can be found without a tray icon.</summary>
    static void EnsureStartMenuShortcut()
    {
        try
        {
            string exe = Environment.ProcessPath ?? "";
            if (exe.Contains(@"\bin\", StringComparison.OrdinalIgnoreCase)) return;
            string lnk = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Programs), "Tempo.lnk");
            var shellType = Type.GetTypeFromProgID("WScript.Shell");
            if (shellType == null) return;
            dynamic shell = Activator.CreateInstance(shellType)!;
            if (File.Exists(lnk))
            {
                dynamic existing = shell.CreateShortcut(lnk);
                if (string.Equals((string)existing.TargetPath, exe, StringComparison.OrdinalIgnoreCase)) return;
            }
            dynamic sc = shell.CreateShortcut(lnk);
            sc.TargetPath = exe;
            sc.WorkingDirectory = Path.GetDirectoryName(exe);
            sc.Description = "Tempo media keys - opens settings";
            sc.Save();
        }
        catch (Exception ex) { Log("Start menu shortcut failed: " + ex.Message); }
    }

    static bool ProcessAlive(int pid)
    {
        try { using var p = Process.GetProcessById(pid); return !p.HasExited; }
        catch { return false; }
    }

    void ListenForSignals()
    {
        foreach (var (name, action) in new (string, Action)[]
        {
            (ShowEventName, ShowSettings),
            (PreviewEventName, () => _popup?.Flash(PopupAction.None, fromMouse: true)),
            (DropHookEventName, () => { _hook?.SimulateDrop(); Log("Test: hook dropped."); }),
        })
        {
            var ev = new EventWaitHandle(false, EventResetMode.AutoReset, name);
            new Thread(() => { while (ev.WaitOne()) Dispatcher.BeginInvoke(action); }) { IsBackground = true }.Start();
        }
    }

    void ShowSettings()
    {
        if (_hub == null) return;
        if (_settingsWindow == null)
        {
            _settingsWindow = new SettingsWindow(_hub, Settings, _stats, _history);
            _settingsWindow.CheckUpdatesRequested += () => _ = CheckForUpdateAsync(true);
            _settingsWindow.PreviewRequested += () => _popup?.Flash(PopupAction.None, fromMouse: true);
            _settingsWindow.QuitRequested += Quit;
            _settingsWindow.Closed += (_, _) => { _settingsWindow = null; MemoryTrim.Soon(2); };
            _settingsWindow.Show();
        }
        if (_settingsWindow.WindowState == WindowState.Minimized) _settingsWindow.WindowState = WindowState.Normal;
        _settingsWindow.Activate();
        Native.SetForegroundWindow(new WindowInteropHelper(_settingsWindow).Handle);
    }

    void Quit()
    {
        Log("Quit from settings.");
        _hook?.Dispose();
        _hook = null;
        Shutdown();
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _stats.Flush();
        _hookWatch?.Dispose();
        _hook?.Dispose();
        _mutex?.ReleaseMutex();
        base.OnExit(e);
    }

    /// <summary>Writes the current media sessions to a JSON file (diagnostics: Tempo.exe --dump [file]).</summary>
    async Task DumpAsync(string file)
    {
        var settings = AppSettings.Load();
        var hub = new MediaHub(Dispatcher, () => settings);
        await hub.StartAsync();
        await Task.Delay(400);
        var data = new
        {
            target = hub.Target?.Aumid,
            sessions = hub.Sessions.Select(s => new
            {
                s.Aumid, app = s.App.Name, exes = s.App.ExeNames, hasIcon = s.App.Icon != null,
                s.Title, s.Artist, status = s.Status.ToString(), hasArt = s.Art != null,
                accent = s.Accent.ToString(), position = s.Position.ToString(@"m\:ss"), duration = s.Duration.ToString(@"m\:ss"),
                s.CanNext, s.CanPrevious, s.CanSeek,
                volume = AppAudio.GetVolume(s.App)?.Level,
            }),
            audio = AppAudio.Sessions().Select(a => new { a.Pid, a.Exe, a.Aumid }),
        };
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(file))!);
        File.WriteAllText(file, JsonSerializer.Serialize(data, new JsonSerializerOptions { WriteIndented = true }));
    }

    static readonly object LogLock = new();
    public static void Log(string message)
    {
        try
        {
            lock (LogLock)
            {
                Directory.CreateDirectory(AppSettings.Folder);
                var file = Path.Combine(AppSettings.Folder, "log.txt");
                if (File.Exists(file) && new FileInfo(file).Length > 512 * 1024) File.Delete(file);
                File.AppendAllText(file, $"{DateTime.Now:yyyy-MM-dd HH:mm:ss}  {message}{Environment.NewLine}");
            }
        }
        catch { }
    }
}
