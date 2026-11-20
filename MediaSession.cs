using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Windows.Media.Control;
using GSession = Windows.Media.Control.GlobalSystemMediaTransportControlsSession;
using PlaybackStatus = Windows.Media.Control.GlobalSystemMediaTransportControlsSessionPlaybackStatus;

namespace Tempo;

/// <summary>One app that reports media to Windows (Spotify, a browser, a player…), kept up to date on the UI thread.</summary>
sealed class MediaSession : INotifyPropertyChanged
{
    readonly System.Windows.Threading.Dispatcher _ui;
    int _propsVersion;

    public GSession Session { get; private set; }
    public string Aumid { get; }
    public AppIdentity App { get; }

    public string Title { get; private set; } = "";
    public string Artist { get; private set; } = "";
    public string Album { get; private set; } = "";
    public ImageSource? Art { get; private set; }
    public Color Accent { get; private set; } = ArtColor.Default;

    public PlaybackStatus Status { get; private set; } = PlaybackStatus.Closed;
    public bool IsPlaying => Status == PlaybackStatus.Playing;
    public bool CanPrevious { get; private set; }
    public bool CanNext { get; private set; }
    public bool CanSeek { get; private set; }
    public bool CanStop { get; private set; }

    /// <summary>When this app last started playing (null = never seen starting).</summary>
    public DateTime? StartedAt { get; private set; }
    /// <summary>The last moment this app was seen playing.</summary>
    public DateTime? LastPlayedAt { get; private set; }

    TimeSpan _position, _start, _end;
    DateTimeOffset _positionAt;
    double _rate = 1;

    public TimeSpan Duration => _end > _start ? _end - _start : TimeSpan.Zero;

    /// <summary>Playback position, moved forward since the app last reported it.</summary>
    public TimeSpan Position
    {
        get
        {
            var p = _position - _start;
            if (IsPlaying && _positionAt != default)
                p += TimeSpan.FromTicks((long)((DateTimeOffset.Now - _positionAt).Ticks * _rate));
            if (p < TimeSpan.Zero) p = TimeSpan.Zero;
            if (Duration > TimeSpan.Zero && p > Duration) p = Duration;
            return p;
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    /// <summary>Raised when the app starts or stops playing.</summary>
    public event Action<MediaSession>? PlaybackChanged;
    /// <summary>Raised when a track starts playing (new track while playing, or playback starting).</summary>
    public event Action<MediaSession>? TrackStarted;

    public MediaSession(GSession session, System.Windows.Threading.Dispatcher ui)
    {
        _ui = ui;
        Session = session;
        Aumid = session.SourceAppUserModelId ?? "";
        App = AppIdentity.For(Aumid);
        Hook(session);
    }

    /// <summary>Windows sometimes hands out a fresh session object for the same app.</summary>
    public void Rebind(GSession session)
    {
        if (ReferenceEquals(session, Session)) return;
        Unhook(Session);
        Session = session;
        Hook(session);
    }

    void Hook(GSession s)
    {
        s.MediaPropertiesChanged += OnProps;
        s.PlaybackInfoChanged += OnPlayback;
        s.TimelinePropertiesChanged += OnTimeline;
    }

    void Unhook(GSession s)
    {
        try
        {
            s.MediaPropertiesChanged -= OnProps;
            s.PlaybackInfoChanged -= OnPlayback;
            s.TimelinePropertiesChanged -= OnTimeline;
        }
        catch { }
    }

    public void Detach() => Unhook(Session);

    void OnProps(GSession s, MediaPropertiesChangedEventArgs e) => _ui.BeginInvoke(() => _ = RefreshPropsAsync());
    void OnPlayback(GSession s, PlaybackInfoChangedEventArgs e) => _ui.BeginInvoke(RefreshPlayback);
    void OnTimeline(GSession s, TimelinePropertiesChangedEventArgs e) => _ui.BeginInvoke(RefreshTimeline);

    public async Task RefreshAllAsync()
    {
        RefreshPlayback();
        RefreshTimeline();
        await RefreshPropsAsync();
    }

    public async Task RefreshPropsAsync()
    {
        int version = ++_propsVersion;
        try
        {
            var p = await Session.TryGetMediaPropertiesAsync();
            if (version != _propsVersion || p == null) return;
            string title = p.Title ?? "", artist = p.Artist ?? "", album = p.AlbumTitle ?? "";
            if (string.IsNullOrEmpty(artist)) artist = p.AlbumArtist ?? "";
            bool trackChanged = title != Title || artist != Artist;

            ImageSource? art = Art;
            Color accent = Accent;
            if (trackChanged || Art == null)
            {
                art = null;
                if (p.Thumbnail != null)
                {
                    var bmp = await LoadArtAsync(p.Thumbnail);
                    if (version != _propsVersion) return;
                    art = bmp;
                    accent = bmp != null ? ArtColor.From(bmp) : ArtColor.Default;
                }
                else accent = ArtColor.Default;
            }

            Title = title; Artist = artist; Album = album; Art = art; Accent = accent;
            Raise(nameof(Title)); Raise(nameof(Artist)); Raise(nameof(Album)); Raise(nameof(Art)); Raise(nameof(Accent));
            if (trackChanged && IsPlaying) TrackStarted?.Invoke(this);
        }
        catch { /* the app went away mid-read */ }
    }

    static async Task<BitmapSource?> LoadArtAsync(Windows.Storage.Streams.IRandomAccessStreamReference thumb)
    {
        try
        {
            using var ras = await thumb.OpenReadAsync();
            using var stream = ras.AsStreamForRead();
            var ms = new MemoryStream();
            await stream.CopyToAsync(ms);
            if (ms.Length == 0) return null;
            ms.Position = 0;
            var img = new BitmapImage();
            img.BeginInit();
            img.CacheOption = BitmapCacheOption.OnLoad;
            img.DecodePixelWidth = 320;
            img.StreamSource = ms;
            img.EndInit();
            img.Freeze();
            return ArtColor.TrimLetterbox(img);
        }
        catch { return null; }
    }

    public void RefreshPlayback()
    {
        try
        {
            var info = Session.GetPlaybackInfo();
            if (info == null) return;
            bool wasPlaying = IsPlaying;
            Status = info.PlaybackStatus;
            var c = info.Controls;
            CanPrevious = c.IsPreviousEnabled;
            CanNext = c.IsNextEnabled;
            CanSeek = c.IsPlaybackPositionEnabled;
            CanStop = c.IsStopEnabled;
            if (info.PlaybackRate is double r && r > 0) _rate = r;

            if (IsPlaying && !wasPlaying) StartedAt = DateTime.Now;
            if (IsPlaying || wasPlaying) LastPlayedAt = DateTime.Now;

            Raise(nameof(Status)); Raise(nameof(IsPlaying));
            Raise(nameof(CanPrevious)); Raise(nameof(CanNext)); Raise(nameof(CanSeek));
            if (IsPlaying != wasPlaying)
            {
                // Freeze the extrapolated position at the moment playback stopped.
                if (!IsPlaying) { _position = _start + PositionWhilePlayingUntilNow(); _positionAt = DateTimeOffset.Now; }
                else _positionAt = DateTimeOffset.Now;
                PlaybackChanged?.Invoke(this);
                if (IsPlaying) TrackStarted?.Invoke(this);
            }
        }
        catch { }
    }

    TimeSpan PositionWhilePlayingUntilNow()
    {
        var p = _position - _start;
        if (_positionAt != default) p += TimeSpan.FromTicks((long)((DateTimeOffset.Now - _positionAt).Ticks * _rate));
        if (Duration > TimeSpan.Zero && p > Duration) p = Duration;
        return p < TimeSpan.Zero ? TimeSpan.Zero : p;
    }

    public void RefreshTimeline()
    {
        try
        {
            var t = Session.GetTimelineProperties();
            if (t == null) return;
            _start = t.StartTime;
            _end = t.EndTime > TimeSpan.Zero ? t.EndTime : t.MaxSeekTime;
            _position = t.Position;
            // Some apps report a stale or zero timestamp; then trust our own clock.
            var at = t.LastUpdatedTime;
            _positionAt = at.Year < 2000 || at > DateTimeOffset.Now.AddSeconds(5) || at < DateTimeOffset.Now.AddHours(-12)
                ? DateTimeOffset.Now : at;
            Raise(nameof(Duration)); Raise(nameof(Position));
        }
        catch { }
    }

    /// <summary>Marks a session found playing when Tempo starts.</summary>
    public void SeedPlaying(DateTime when)
    {
        if (IsPlaying) { StartedAt ??= when; LastPlayedAt = when; }
    }

    /// <summary>Shows the new state right away, before the app confirms it.</summary>
    public void Assume(PlaybackStatus status)
    {
        if (status == Status) return;
        bool wasPlaying = IsPlaying;
        if (wasPlaying) { _position = _start + PositionWhilePlayingUntilNow(); }
        _positionAt = DateTimeOffset.Now;
        Status = status;
        if (IsPlaying && !wasPlaying) StartedAt = DateTime.Now;
        LastPlayedAt = DateTime.Now;
        Raise(nameof(Status)); Raise(nameof(IsPlaying));
    }

    public void AssumePosition(TimeSpan pos)
    {
        _position = _start + pos;
        _positionAt = DateTimeOffset.Now;
        Raise(nameof(Position));
    }

    void Raise([CallerMemberName] string? name = null) => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
