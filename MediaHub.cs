using System.Collections.ObjectModel;
using Windows.Media.Control;
using GSession = Windows.Media.Control.GlobalSystemMediaTransportControlsSession;
using PlaybackStatus = Windows.Media.Control.GlobalSystemMediaTransportControlsSessionPlaybackStatus;

namespace Tempo;

/// <summary>
/// Every app that reports media to Windows, plus the rule for which one the media keys control:
///   1. an app you picked by hand, until some other app starts playing;
///   2. otherwise the app that most recently started playing;
///   3. if nothing is playing, the app that played last (not whichever tab Windows happens to prefer).
/// </summary>
sealed class MediaHub
{
    readonly System.Windows.Threading.Dispatcher _ui;
    readonly Func<AppSettings> _settings;
    GlobalSystemMediaTransportControlsSessionManager? _mgr;
    MediaSession? _pinned;
    DateTime _pinnedAt;

    public ObservableCollection<MediaSession> Sessions { get; } = [];

    /// <summary>Raised whenever the controlled app or its state might have changed.</summary>
    public event Action? Changed;
    /// <summary>Raised when any app starts a track (for the history).</summary>
    public event Action<MediaSession>? TrackStarted;

    /// <summary>Snapshot read by the keyboard hook thread: true when a key press has an app to go to.</summary>
    public volatile bool HasTarget;
    /// <summary>Snapshot for the hook thread: identities of the controllable apps.</summary>
    public volatile AppIdentity[] Apps = [];
    /// <summary>Snapshot for the hook thread: whether the current target is playing.</summary>
    public volatile bool TargetPlaying;

    public MediaHub(System.Windows.Threading.Dispatcher ui, Func<AppSettings> settings)
    {
        _ui = ui;
        _settings = settings;
    }

    public async Task StartAsync()
    {
        _mgr = await GlobalSystemMediaTransportControlsSessionManager.RequestAsync();
        _mgr.SessionsChanged += (_, _) => _ui.BeginInvoke(() => _ = SyncAsync());
        _mgr.CurrentSessionChanged += (_, _) => _ui.BeginInvoke(Publish);
        await SyncAsync();
        var now = DateTime.Now;
        // Sessions already playing at start-up: prefer the one Windows considers current.
        var current = _mgr.GetCurrentSession()?.SourceAppUserModelId;
        foreach (var s in Sessions) s.SeedPlaying(s.Aumid == current ? now : now.AddMilliseconds(-1));
        Publish();
    }

    async Task SyncAsync()
    {
        if (_mgr == null) return;
        IReadOnlyList<GSession> live;
        try { live = _mgr.GetSessions(); } catch { return; }

        var seen = new HashSet<MediaSession>();
        var fresh = new List<MediaSession>();
        var counts = new Dictionary<string, int>();
        foreach (var g in live)
        {
            string aumid = g.SourceAppUserModelId ?? "";
            counts[aumid] = counts.GetValueOrDefault(aumid) + 1;
            // Match existing entries by app ID (and order, if an app has several sessions).
            var existing = Sessions.Where(s => s.Aumid == aumid && !seen.Contains(s)).FirstOrDefault();
            if (existing != null) { existing.Rebind(g); seen.Add(existing); continue; }
            var made = new MediaSession(g, _ui);
            made.PlaybackChanged += OnPlaybackChanged;
            made.TrackStarted += t => TrackStarted?.Invoke(t);
            made.PropertyChanged += (_, _) => PublishSoon();
            seen.Add(made);
            fresh.Add(made);
            Sessions.Add(made);
            _settings().Remember(made.App);
        }
        foreach (var gone in Sessions.Where(s => !seen.Contains(s)).ToList())
        {
            gone.Detach();
            Sessions.Remove(gone);
            if (_pinned == gone) _pinned = null;
        }
        foreach (var s in fresh) await s.RefreshAllAsync();
        Publish();
    }

    void OnPlaybackChanged(MediaSession s) => Publish();

    bool _publishQueued;
    void PublishSoon()
    {
        if (_publishQueued) return;
        _publishQueued = true;
        _ui.BeginInvoke(() => { _publishQueued = false; Publish(); }, System.Windows.Threading.DispatcherPriority.Background);
    }

    /// <summary>Re-evaluates the target (e.g. after settings changed).</summary>
    public void Refresh() => Publish();

    void Publish()
    {
        var controllable = Controllable().ToList();
        Apps = controllable.Select(s => s.App).ToArray();
        var target = Target;
        HasTarget = target != null;
        TargetPlaying = target?.IsPlaying == true;
        Changed?.Invoke();
    }

    IEnumerable<MediaSession> Controllable()
    {
        var ignored = _settings().IgnoredApps;
        return Sessions.Where(s => !ignored.Contains(s.Aumid));
    }

    public IReadOnlyList<MediaSession> ControllableSessions => Controllable().ToList();

    public MediaSession? Target
    {
        get
        {
            var list = Controllable().ToList();
            if (list.Count == 0) return null;

            if (_pinned != null && list.Contains(_pinned) &&
                !list.Any(s => s != _pinned && s.IsPlaying && s.StartedAt > _pinnedAt))
                return _pinned;

            var playing = list.Where(s => s.IsPlaying).OrderByDescending(s => s.StartedAt ?? DateTime.MinValue).FirstOrDefault();
            if (playing != null) return playing;

            // Nothing playing: your preferred app wins over whatever played last.
            var preferred = _settings().PreferredApp;
            if (!string.IsNullOrEmpty(preferred))
            {
                var p = list.FirstOrDefault(s => string.Equals(s.Aumid, preferred, StringComparison.OrdinalIgnoreCase));
                if (p != null) return p;
            }

            var recent = list.Where(s => s.LastPlayedAt != null).OrderByDescending(s => s.LastPlayedAt).FirstOrDefault();
            if (recent != null) return recent;

            string? current = null;
            try { current = _mgr?.GetCurrentSession()?.SourceAppUserModelId; } catch { }
            return list.FirstOrDefault(s => s.Aumid == current) ?? list[0];
        }
    }

    /// <summary>The app Windows itself would have sent the key to.</summary>
    public string? WindowsChoice
    {
        get { try { return _mgr?.GetCurrentSession()?.SourceAppUserModelId; } catch { return null; } }
    }

    /// <summary>Control this app until another one starts playing.</summary>
    public void Pin(MediaSession s)
    {
        _pinned = s;
        _pinnedAt = DateTime.Now;
        Publish();
    }

    /// <summary>Switches to the next controllable app (in the order they appear on the pop-up).</summary>
    public MediaSession? CycleTarget()
    {
        var list = Controllable().ToList();
        if (list.Count < 2) return Target;
        int i = list.IndexOf(Target!);
        var next = list[(i + 1) % list.Count];
        Pin(next);
        return next;
    }

    // ---------- Commands ----------
    public async Task PlayPauseAsync(MediaSession? s = null)
    {
        s ??= Target;
        if (s == null) return;
        try
        {
            if (s.IsPlaying)
            {
                s.Assume(PlaybackStatus.Paused);
                if (!await s.Session.TryPauseAsync()) await s.Session.TryTogglePlayPauseAsync();
            }
            else
            {
                s.Assume(PlaybackStatus.Playing);
                if (!await s.Session.TryPlayAsync()) await s.Session.TryTogglePlayPauseAsync();
            }
        }
        catch { }
        Publish();
        Resync(s);
    }

    public async Task NextAsync(MediaSession? s = null)
    {
        s ??= Target;
        if (s == null) return;
        try { await s.Session.TrySkipNextAsync(); } catch { }
        Resync(s);
    }

    public async Task PreviousAsync(MediaSession? s = null)
    {
        s ??= Target;
        if (s == null) return;
        try { await s.Session.TrySkipPreviousAsync(); } catch { }
        Resync(s);
    }

    public async Task StopAsync(MediaSession? s = null)
    {
        s ??= Target;
        if (s == null) return;
        try
        {
            s.Assume(PlaybackStatus.Paused);
            if (!(s.CanStop && await s.Session.TryStopAsync())) await s.Session.TryPauseAsync();
        }
        catch { }
        Publish();
        Resync(s);
    }

    public async Task SeekToAsync(TimeSpan pos, MediaSession? s = null)
    {
        s ??= Target;
        if (s == null || !s.CanSeek) return;
        if (pos < TimeSpan.Zero) pos = TimeSpan.Zero;
        if (s.Duration > TimeSpan.Zero && pos > s.Duration) pos = s.Duration - TimeSpan.FromMilliseconds(500);
        s.AssumePosition(pos);
        try
        {
            var timeline = s.Session.GetTimelineProperties();
            await s.Session.TryChangePlaybackPositionAsync((timeline.StartTime + pos).Ticks);
        }
        catch { }
    }

    public Task SeekByAsync(TimeSpan delta, MediaSession? s = null)
    {
        s ??= Target;
        return s == null ? Task.CompletedTask : SeekToAsync(s.Position + delta, s);
    }

    /// <summary>Some apps are slow to report the new state; read it again shortly after a command.</summary>
    void Resync(MediaSession s)
    {
        _ = Task.Delay(350).ContinueWith(_ => _ui.BeginInvoke(() => { _ = s.RefreshAllAsync(); Publish(); }));
        _ = Task.Delay(1200).ContinueWith(_ => _ui.BeginInvoke(() => { _ = s.RefreshAllAsync(); Publish(); }));
    }
}
