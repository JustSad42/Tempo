using System.Windows.Threading;

namespace Tempo;

public enum PopupAction { None, PlayPause, Next, Previous, Stop, Switched, Scrub, Volume, Opened }

/// <summary>
/// Turns key presses into actions:
///   tap Play/Pause, Next, Previous, Stop → the usual;
///   double-tap Play/Pause → bring the playing app to the front (playback stays as it was);
///   hold Next / Previous → scrub forward / back, the app follows along live;
///   hold Play/Pause → switch to the next app (keeps switching while held).
/// </summary>
sealed class KeyGestures
{
    const int DoubleTapMs = 400;

    readonly MediaHub _hub;
    readonly PopupWindow _popup;
    readonly Func<AppSettings> _settings;
    readonly Stats _stats;
    readonly DispatcherTimer _hold;
    MediaKey? _heldKey;
    bool _holdFired;
    int _ticks;
    TimeSpan _scrubPos, _scrubFrom;
    long _lastLiveSeek;
    MediaSession? _scrubTarget;
    long _lastPlayTap;
    MediaSession? _lastPlayTarget;

    public KeyGestures(MediaHub hub, PopupWindow popup, Func<AppSettings> settings, Stats stats)
    {
        _hub = hub; _popup = popup; _settings = settings; _stats = stats;
        _hold = new DispatcherTimer(DispatcherPriority.Input);
        _hold.Tick += OnHoldTick;
    }

    public bool IsHolding => _heldKey != null;

    public void OnKey(MediaKey key, bool down, bool repeat)
    {
        if (repeat) return; // holds are timed here, not by key repeat
        if (down) Down(key); else Up(key);
    }

    void Down(MediaKey key)
    {
        var s = _settings();
        var target = _hub.Target;
        if (_heldKey != null) Up(_heldKey.Value); // another key while holding: finish the first

        switch (key)
        {
            case MediaKey.PlayPause when s.HoldToSwitchApp && _hub.ControllableSessions.Count > 1:
                StartHold(key, 450);
                break;
            case MediaKey.Next or MediaKey.Previous when s.HoldToScrub && target is { CanSeek: true, Duration.Ticks: > 0 }:
                _scrubTarget = target;
                _scrubPos = _scrubFrom = target.Position;
                StartHold(key, 380);
                break;
            default:
                Tap(key);
                break;
        }
    }

    void StartHold(MediaKey key, int ms)
    {
        _heldKey = key;
        _holdFired = false;
        _ticks = 0;
        _hold.Interval = TimeSpan.FromMilliseconds(ms);
        _hold.Start();
    }

    void Up(MediaKey key)
    {
        if (_heldKey != key) return;
        _hold.Stop();
        _heldKey = null;
        if (!_holdFired) { Tap(key); return; }
        if (key is MediaKey.Next or MediaKey.Previous && _scrubTarget != null)
        {
            var t = _scrubTarget;
            _scrubTarget = null;
            _ = _hub.SeekToAsync(_scrubPos, t);
            _popup.EndScrub();
            _stats.Scrubs++;
            _stats.Key(false);
        }
    }

    void OnHoldTick(object? sender, EventArgs e)
    {
        if (_heldKey is not MediaKey key) { _hold.Stop(); return; }
        _holdFired = true;
        _ticks++;
        if (key == MediaKey.PlayPause)
        {
            _hub.CycleTarget();
            _popup.Flash(PopupAction.Switched);
            _stats.AppSwitches++;
            _stats.Key(false);
            _hold.Interval = TimeSpan.FromMilliseconds(750);
            return;
        }

        var t = _scrubTarget;
        if (t == null) { _hold.Stop(); return; }
        // Speeds up the longer the key is held: ~2 s of media per step at first, up to 15 s.
        _hold.Interval = TimeSpan.FromMilliseconds(80);
        double step = Math.Min(2 + _ticks * 0.25, 15);
        _scrubPos += TimeSpan.FromSeconds(key == MediaKey.Next ? step : -step);
        if (_scrubPos < TimeSpan.Zero) _scrubPos = TimeSpan.Zero;
        if (_scrubPos > t.Duration) _scrubPos = t.Duration;
        _popup.ShowScrub(t, _scrubPos, _scrubFrom);

        // Move the app along while scrubbing so its own picture/timeline follows (not just at release).
        long now = Environment.TickCount64;
        if (now - _lastLiveSeek >= 350)
        {
            _lastLiveSeek = now;
            _ = _hub.SeekToAsync(_scrubPos, t);
        }
    }

    void Tap(MediaKey key)
    {
        var target = _hub.Target;
        if (target == null)
        {
            _popup.Flash(PopupAction.None);
            return;
        }

        if (key == MediaKey.PlayPause && _settings().DoubleTapToOpen)
        {
            long now = Environment.TickCount64;
            if (now - _lastPlayTap < DoubleTapMs && _lastPlayTarget == target)
            {
                // Second tap: undo the first tap's play/pause and bring the app forward instead.
                _lastPlayTap = 0;
                _ = _hub.PlayPauseAsync(target);
                _popup.OpenApp(target);
                return;
            }
            _lastPlayTap = now;
            _lastPlayTarget = target;
        }

        bool better = !string.Equals(_hub.WindowsChoice, target.Aumid, StringComparison.OrdinalIgnoreCase);
        _stats.Key(better);
        switch (key)
        {
            case MediaKey.PlayPause: _ = _hub.PlayPauseAsync(target); break;
            case MediaKey.Next: _ = _hub.NextAsync(target); break;
            case MediaKey.Previous: _ = _hub.PreviousAsync(target); break;
            case MediaKey.Stop: _ = _hub.StopAsync(target); break;
        }
        _popup.Flash(key switch
        {
            MediaKey.PlayPause => PopupAction.PlayPause,
            MediaKey.Next => PopupAction.Next,
            MediaKey.Previous => PopupAction.Previous,
            _ => PopupAction.Stop,
        });
    }
}
