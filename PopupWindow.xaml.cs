using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Shapes;
using System.Windows.Threading;

namespace Tempo;

/// <summary>The overlay that slides in on a media key press and fades away on its own.</summary>
public partial class PopupWindow : Window
{
    readonly MediaHub _hub;
    readonly Func<AppSettings> _settings;
    readonly DispatcherTimer _hideTimer = new();
    readonly DispatcherTimer _tick = new() { Interval = TimeSpan.FromMilliseconds(250) };
    SolidColorBrush _accent = new(ArtColor.Default);
    Color _accentTarget;

    // Resource dictionaries freeze brushes, so the accent animates as a color property and each frame swaps in a new brush.
    public static readonly DependencyProperty AccentColorProperty = DependencyProperty.Register(
        nameof(AccentColor), typeof(Color), typeof(PopupWindow),
        new PropertyMetadata(ArtColor.Default, (d, e) => ((PopupWindow)d).ApplyAccent((Color)e.NewValue)));
    public Color AccentColor { get => (Color)GetValue(AccentColorProperty); set => SetValue(AccentColorProperty, value); }
    IntPtr _hwnd;
    bool _shown, _hiding, _dragging, _scrubbing, _volumeChanging;
    MediaSession? _rendered;
    object? _renderedArt;
    bool _artOnA = true;
    DateTime _noticeUntil;
    string _renderedTrack = "";

    public event Action? SettingsRequested;

    internal PopupWindow(MediaHub hub, Func<AppSettings> settings)
    {
        InitializeComponent();
        _hub = hub;
        _settings = settings;
        ApplyAccent(ArtColor.Default);

        SourceInitialized += (_, _) =>
        {
            _hwnd = new WindowInteropHelper(this).Handle;
            Native.MakeOverlay(_hwnd);
        };
        Card.SizeChanged += (_, _) =>
            Card.Clip = new RectangleGeometry(new Rect(Card.RenderSize), 20, 20);
        ArtClip.Clip = new RectangleGeometry(new Rect(0, 0, 88, 88), 12, 12);
        SizeChanged += (_, _) => { if (_shown) Place(); };
        DpiChanged += (_, _) => Dispatcher.BeginInvoke(Place, DispatcherPriority.Loaded);

        _hideTimer.Tick += (_, _) => { if (!Root.IsMouseOver && !_dragging && Menu.Visibility != Visibility.Visible) HideAnimated(); else RestartHide(1.2); };
        _tick.Tick += (_, _) => UpdateProgress();

        _hub.Changed += () => { if (_shown) Render(); };

        PlayBtn.Click += async (_, _) => { var t = _hub.Target; await _hub.PlayPauseAsync(t); Flash(PopupAction.PlayPause, fromMouse: true); };
        NextBtn.Click += async (_, _) => { await _hub.NextAsync(); Flash(PopupAction.Next, fromMouse: true); };
        PrevBtn.Click += async (_, _) => { await _hub.PreviousAsync(); Flash(PopupAction.Previous, fromMouse: true); };
        BackBtn.Click += async (_, _) => { await _hub.SeekByAsync(TimeSpan.FromSeconds(-_settings().SkipSeconds)); UpdateProgress(); };
        FwdBtn.Click += async (_, _) => { await _hub.SeekByAsync(TimeSpan.FromSeconds(_settings().SkipSeconds)); UpdateProgress(); };
        SettingsBtn.Click += (_, _) => { HideAnimated(); SettingsRequested?.Invoke(); };
        MuteBtn.Click += (_, _) => ToggleMute();
        CopyBtn.Click += (_, _) => CopySong(_rendered);
        ArtBox.MouseLeftButtonUp += (_, e) => { OpenApp(_rendered); e.Handled = true; };
        ArtBox.ToolTip = "Open the app";
        Root.MouseRightButtonUp += (_, e) => { ShowMenu(); e.Handled = true; };
        MenuShade.MouseLeftButtonDown += (_, e) => { HideMenu(); e.Handled = true; };
        VolSlider.ValueChanged += (_, e) =>
        {
            if (_volumeChanging || _rendered == null) return;
            AppAudio.SetVolume(_rendered.App, (float)(e.NewValue / 100), false);
            UpdateVolumeGlyph(e.NewValue, false);
        };

        Root.MouseEnter += (_, _) => { Fade(SettingsBtn, 1, 150); Fade(CopyBtn, 1, 150); };
        Root.MouseLeave += (_, _) => { Fade(SettingsBtn, 0, 250); Fade(CopyBtn, 0, 250); HideMenu(); RestartHide(Math.Min(1.5, _settings().PopupSeconds)); };
        Root.MouseWheel += OnWheel;

        Progress.MouseEnter += (_, _) => Fade(ProgressThumb, 1, 120);
        Progress.MouseLeave += (_, _) => { if (!_dragging) Fade(ProgressThumb, 0, 200); };
        Progress.MouseLeftButtonDown += (_, e) => { if (_rendered?.CanSeek != true) return; _dragging = true; Progress.CaptureMouse(); DragTo(e); e.Handled = true; };
        Progress.MouseMove += (_, e) => { if (_dragging) DragTo(e); };
        Progress.MouseLeftButtonUp += async (_, e) =>
        {
            if (!_dragging) return;
            _dragging = false;
            Progress.ReleaseMouseCapture();
            var pos = PositionAt(e);
            HideScrubBadge();
            if (_rendered != null) await _hub.SeekToAsync(pos, _rendered);
            UpdateProgress();
        };
    }

    // ---------- Showing and hiding ----------

    /// <summary>
    /// Shows the (invisible) window once off-screen so it has its real size from the start;
    /// otherwise the first show briefly opens a screen-sized transparent window.
    /// </summary>
    internal void Warmup()
    {
        Left = -32000; Top = -32000;
        Render();
        Show();
        UpdateLayout();
        Hide();
    }

    /// <summary>Shows the pop-up for a key press or click and pulses the matching control.</summary>
    internal void Flash(PopupAction action, bool fromMouse = false)
    {
        var s = _settings();
        if (!fromMouse && !PopupAllowed()) return;
        ShowNow();
        Render();
        Pulse(action);
        RestartHide(s.PopupSeconds);
    }

    internal void ShowScrub(MediaSession target, TimeSpan pos, TimeSpan from)
    {
        var s = _settings();
        if (!PopupAllowed()) return;
        _scrubbing = true;
        ShowNow();
        Render();
        DrawProgress(pos, target.Duration, preview: true);
        var delta = pos - from;
        ScrubText.Text = (delta < TimeSpan.Zero ? "−" : "+") + Format(delta.Duration());
        Fade(ScrubBadge, 1, 100);
        RestartHide(s.PopupSeconds);
    }

    internal void EndScrub()
    {
        _scrubbing = false;
        HideScrubBadge();
        UpdateProgress();
    }

    void HideScrubBadge() => Fade(ScrubBadge, 0, 250);

    bool PopupAllowed()
    {
        var s = _settings();
        if (!s.ShowPopup || DateTime.Now < s.SnoozeUntil) return false;
        return !(s.HideInFullscreen && Native.FullscreenAppActive());
    }

    /// <summary>Brings the app's window forward (the browser window showing the playing tab, if it can tell).</summary>
    internal void OpenApp(MediaSession? t)
    {
        if (t == null) return;
        HideMenu();
        bool ok = Native.FocusAppWindow(t.App.ExeNames, t.Aumid, t.Title);
        if (_shown || PopupAllowed())
        {
            ShowNow();
            Render();
            Notice(ok ? $"Opened {t.App.Name}" : $"{t.App.Name} has no window");
            RestartHide(Math.Min(_settings().PopupSeconds, 1.6));
        }
    }

    internal void CopySong(MediaSession? t)
    {
        if (t == null || string.IsNullOrWhiteSpace(t.Title)) return;
        string text = string.IsNullOrWhiteSpace(t.Artist) ? t.Title : $"{t.Artist} \u2013 {t.Title}";
        for (int i = 0; i < 5; i++)
        {
            try { Clipboard.SetText(text); Notice("Copied song name"); return; }
            catch { Thread.Sleep(40); }
        }
        Notice("Couldn't copy (clipboard busy)");
    }

    /// <summary>Briefly replaces the app name line with a short message.</summary>
    void Notice(string text)
    {
        _noticeUntil = DateTime.Now.AddSeconds(1.6);
        AppName.Text = text;
        AppName.Foreground = _accent;
        AppLineShift.BeginAnimation(TranslateTransform.XProperty,
            new DoubleAnimation(-8, 0, TimeSpan.FromMilliseconds(260)) { EasingFunction = new QuinticEase { EasingMode = EasingMode.EaseOut } });
        var restore = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1.7) };
        restore.Tick += (_, _) => { restore.Stop(); if (DateTime.Now >= _noticeUntil && _shown) Render(); };
        restore.Start();
    }

    // ---------- Right-click menu ----------

    void ShowMenu()
    {
        var t = _rendered;
        MenuItems.Children.Clear();
        var s = _settings();
        if (t != null)
        {
            AddMenuItem("\uE8A7", $"Open {t.App.Name}", () => OpenApp(t));
            AddMenuItem("\uE8C8", "Copy song name", () => { HideMenu(); CopySong(t); }, !string.IsNullOrWhiteSpace(t.Title));
            bool preferred = string.Equals(s.PreferredApp, t.Aumid, StringComparison.OrdinalIgnoreCase);
            AddMenuItem(preferred ? "\uE735" : "\uE734", preferred ? $"Stop preferring {t.App.Name}" : $"Prefer {t.App.Name} when nothing plays", () =>
            {
                s.PreferredApp = preferred ? null : t.Aumid;
                s.Save();
                HideMenu();
                Notice(preferred ? "No preferred app" : $"{t.App.Name} is now preferred");
            });
        }
        AddMenuItem("\uE708", "Hide pop-ups for an hour", () =>
        {
            s.SnoozeUntil = DateTime.Now.AddHours(1);
            HideMenu();
            HideAnimated();
        });
        AddMenuItem("\uE713", "Tempo settings", () => { HideMenu(); HideAnimated(); SettingsRequested?.Invoke(); });

        MenuShade.Visibility = Menu.Visibility = Visibility.Visible;
        Menu.Opacity = 0;
        Fade(Menu, 1, 120);
        _hideTimer.Stop();
    }

    void AddMenuItem(string glyph, string text, Action action, bool enabled = true)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        row.Children.Add(new TextBlock { Text = glyph, FontFamily = (FontFamily)FindResource("IconFont"), FontSize = 13, Width = 26, VerticalAlignment = VerticalAlignment.Center, Foreground = (Brush)FindResource("Muted") });
        row.Children.Add(new TextBlock { Text = text, FontSize = 12.5, VerticalAlignment = VerticalAlignment.Center });
        var btn = new Button { Content = row, Style = (Style)FindResource("MenuItem"), IsEnabled = enabled };
        btn.Click += (_, _) => action();
        MenuItems.Children.Add(btn);
    }

    void HideMenu()
    {
        if (Menu.Visibility != Visibility.Visible) return;
        Menu.Visibility = MenuShade.Visibility = Visibility.Collapsed;
        RestartHide(Math.Min(1.5, _settings().PopupSeconds));
    }

    void ShowNow()
    {
        if (_shown && !_hiding) return;
        bool wasHidden = !_shown;
        _hiding = false;
        _shown = true;
        if (wasHidden)
        {
            Show();
            UpdateLayout();
            Place();
            _tick.Start();
        }
        Native.SetWindowPos(_hwnd, Native.HWND_TOPMOST, 0, 0, 0, 0, Native.SWP_NOMOVE | Native.SWP_NOSIZE | Native.SWP_NOACTIVATE);

        bool top = _settings().Placement is PopupPlacement.TopLeft or PopupPlacement.TopCenter or PopupPlacement.TopRight;
        var ease = new QuinticEase { EasingMode = EasingMode.EaseOut };
        Root.BeginAnimation(OpacityProperty, new DoubleAnimation(1, TimeSpan.FromMilliseconds(170)));
        RootShift.BeginAnimation(TranslateTransform.YProperty,
            new DoubleAnimation(wasHidden ? (top ? -14 : 14) : RootShift.Y, 0, TimeSpan.FromMilliseconds(320)) { EasingFunction = ease });
        var scale = new DoubleAnimation(wasHidden ? 0.96 : RootScale.ScaleX, 1, TimeSpan.FromMilliseconds(320)) { EasingFunction = ease };
        RootScale.BeginAnimation(ScaleTransform.ScaleXProperty, scale);
        RootScale.BeginAnimation(ScaleTransform.ScaleYProperty, scale);
    }

    void HideAnimated()
    {
        if (!_shown || _hiding) return;
        _hiding = true;
        _hideTimer.Stop();
        bool top = _settings().Placement is PopupPlacement.TopLeft or PopupPlacement.TopCenter or PopupPlacement.TopRight;
        var fade = new DoubleAnimation(0, TimeSpan.FromMilliseconds(200)) { EasingFunction = new CubicEase { EasingMode = EasingMode.EaseIn } };
        fade.Completed += (_, _) =>
        {
            if (!_hiding) return; // shown again meanwhile
            _hiding = false;
            _shown = false;
            _tick.Stop();
            Menu.Visibility = MenuShade.Visibility = Visibility.Collapsed;
            Hide();
        };
        RootShift.BeginAnimation(TranslateTransform.YProperty, new DoubleAnimation(top ? -8 : 8, TimeSpan.FromMilliseconds(200)));
        Root.BeginAnimation(OpacityProperty, fade);
    }

    void RestartHide(double seconds)
    {
        _hideTimer.Stop();
        _hideTimer.Interval = TimeSpan.FromSeconds(Math.Max(0.6, seconds));
        _hideTimer.Start();
    }

    /// <summary>Puts the card in the chosen corner/edge of the screen, in physical pixels.</summary>
    void Place()
    {
        if (_hwnd == IntPtr.Zero) return;
        var s = _settings();
        var area = Native.WorkArea(s.FollowMouse);
        Native.GetWindowRect(_hwnd, out var r);
        double dpi = VisualTreeHelper.GetDpi(this).DpiScaleX;
        int margin = (int)(Root.Margin.Left * dpi);   // transparent shadow room around the card
        int gap = (int)(14 * dpi);                    // visible gap between card and screen edge
        int w = r.Width, h = r.Height;
        if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("TEMPO_INSTANCE"))) App.Log($"Popup window {w}x{h} px");
        int left = area.Left + gap - margin, right = area.Right - w - gap + margin;
        int top = area.Top + gap - margin, bottom = area.Bottom - h - gap + margin;
        int cx = area.Left + (area.Width - w) / 2;
        (int x, int y) = s.Placement switch
        {
            PopupPlacement.TopLeft => (left, top),
            PopupPlacement.TopRight => (right, top),
            PopupPlacement.BottomLeft => (left, bottom),
            PopupPlacement.BottomCenter => (cx, bottom),
            PopupPlacement.BottomRight => (right, bottom),
            _ => (cx, top + (int)(10 * dpi)),
        };
        if (r.Left != x || r.Top != y)
            Native.SetWindowPos(_hwnd, Native.HWND_TOPMOST, x, y, 0, 0, Native.SWP_NOSIZE | Native.SWP_NOACTIVATE);
    }

    // ---------- Content ----------

    void Render()
    {
        var t = _hub.Target;
        var sessions = _hub.ControllableSessions;
        if (t == null)
        {
            _rendered = null;
            AppIcon.Source = null;
            AppName.Text = "Tempo";
            AppName.Foreground = (Brush)FindResource("Muted");
            TitleText.Text = "Nothing to control";
            ArtistText.Text = "Open Spotify, a video or a player first";
            SetArt(null);
            Sources.Children.Clear();
            LiveDot.Visibility = Visibility.Collapsed;
            DrawProgress(TimeSpan.Zero, TimeSpan.Zero, false);
            SetPlayGlyph(false);
            VolumeBox.Visibility = Visibility.Hidden;
            return;
        }

        bool switched = _rendered != null && _rendered != t;
        _rendered = t;
        AppIcon.Source = t.App.Icon;
        AppIcon.Visibility = t.App.Icon != null ? Visibility.Visible : Visibility.Collapsed;
        if (DateTime.Now >= _noticeUntil)
        {
            AppName.Text = t.App.Name;
            AppName.Foreground = (Brush)FindResource("Muted");
        }
        LiveDot.Visibility = t.IsPlaying ? Visibility.Visible : Visibility.Collapsed;
        TitleText.Text = string.IsNullOrWhiteSpace(t.Title) ? t.App.Name : t.Title;
        ArtistText.Text = string.IsNullOrWhiteSpace(t.Artist) ? (t.IsPlaying ? "Playing" : "Paused") : t.Artist;
        SetArt(t.Art);
        SetAccent(t.Accent);
        SetPlayGlyph(t.IsPlaying);

        PrevBtn.IsEnabled = t.CanPrevious;
        NextBtn.IsEnabled = t.CanNext;
        BackBtn.IsEnabled = FwdBtn.IsEnabled = t.CanSeek && t.Duration > TimeSpan.Zero;
        BackLabel.Text = FwdLabel.Text = _settings().SkipSeconds.ToString();
        BackBtn.ToolTip = $"Back {_settings().SkipSeconds} seconds";
        FwdBtn.ToolTip = $"Forward {_settings().SkipSeconds} seconds";
        Progress.Cursor = t.CanSeek ? Cursors.Hand : Cursors.Arrow;
        ProgressRow.Opacity = t.Duration > TimeSpan.Zero ? 1 : 0.35;

        RenderSources(sessions, t);
        RenderVolume(t);
        if (!_scrubbing && !_dragging) UpdateProgress();

        string track = t.Aumid + "|" + t.Title + "|" + t.Artist;
        if (switched)
        {
            AppLineShift.BeginAnimation(TranslateTransform.XProperty,
                new DoubleAnimation(-10, 0, TimeSpan.FromMilliseconds(300)) { EasingFunction = new QuinticEase { EasingMode = EasingMode.EaseOut } });
        }
        if (track != _renderedTrack && _renderedTrack != "")
        {
            TitleText.BeginAnimation(OpacityProperty, new DoubleAnimation(0.2, 1, TimeSpan.FromMilliseconds(320)));
            ArtistText.BeginAnimation(OpacityProperty, new DoubleAnimation(0.2, 1, TimeSpan.FromMilliseconds(420)));
        }
        _renderedTrack = track;
    }

    void RenderSources(IReadOnlyList<MediaSession> sessions, MediaSession target)
    {
        Sources.Children.Clear();
        if (sessions.Count < 2) return;
        foreach (var s in sessions)
        {
            bool current = s == target;
            var icon = new Image { Source = s.App.Icon, Width = 14, Height = 14 };
            RenderOptions.SetBitmapScalingMode(icon, BitmapScalingMode.HighQuality);
            var content = new Grid();
            if (s.App.Icon != null) content.Children.Add(icon);
            else content.Children.Add(new TextBlock { Text = s.App.Name[..1], FontSize = 10, FontWeight = FontWeights.Bold, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center });
            if (s.IsPlaying)
                content.Children.Add(new Ellipse { Width = 5, Height = 5, Fill = _accent, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, -3, -3) });

            var btn = new Button
            {
                Style = (Style)FindResource("IconButton"),
                Width = 24, Height = 24, Margin = new Thickness(2, 0, 0, 0),
                Content = content,
                Opacity = current ? 1 : 0.55,
                Background = current ? new SolidColorBrush(Color.FromArgb(0x2E, 255, 255, 255)) : Brushes.Transparent,
                ToolTip = (current ? "Controlling " : "Switch to ") + s.App.Name + (string.IsNullOrEmpty(s.Title) ? "" : " — " + s.Title),
            };
            var session = s;
            btn.Click += (_, _) => { _hub.Pin(session); Pulse(PopupAction.Switched); RestartHide(_settings().PopupSeconds); };
            Sources.Children.Add(btn);
        }
    }

    void SetArt(System.Windows.Media.ImageSource? art)
    {
        if (ReferenceEquals(art, _renderedArt)) return;
        _renderedArt = art;
        var incoming = _artOnA ? ArtB : ArtA;
        var outgoing = _artOnA ? ArtA : ArtB;
        _artOnA = !_artOnA;
        incoming.Source = art;
        var dur = TimeSpan.FromMilliseconds(_shown ? 280 : 0);
        incoming.BeginAnimation(OpacityProperty, new DoubleAnimation(art != null ? 1 : 0, dur));
        outgoing.BeginAnimation(OpacityProperty, new DoubleAnimation(0, dur));
        if (art != null)
        {
            var brush = new ImageBrush(art) { Stretch = Stretch.UniformToFill };
            brush.Freeze();
            Backdrop.Fill = brush;
        }
        else Backdrop.Fill = null;
        ArtPlaceholder.Visibility = Visibility.Visible;
    }

    void SetAccent(Color c)
    {
        var dur = TimeSpan.FromMilliseconds(_shown ? 400 : 0);
        if (c == _accentTarget) return;
        _accentTarget = c;
        BeginAnimation(AccentColorProperty, new ColorAnimation(c, dur));
    }

    void ApplyAccent(Color c)
    {
        _accent = new SolidColorBrush(c);
        _accent.Freeze();
        Resources["Accent"] = _accent;
        var glow = new RadialGradientBrush(Color.FromArgb(0x4D, c.R, c.G, c.B), Color.FromArgb(0, c.R, c.G, c.B));
        glow.Freeze();
        Glow.Fill = glow;
    }

    void SetPlayGlyph(bool playing)
    {
        PlayGlyph.Data = (Geometry)Resources[playing ? "PauseGeo" : "PlayGeo"];
        PlayGlyph.Margin = playing ? new Thickness(0) : new Thickness(0.5, 0, 0, 0);
    }

    void Pulse(PopupAction action)
    {
        var t = _rendered;
        Button? btn = action switch
        {
            PopupAction.PlayPause => PlayBtn,
            PopupAction.Next => NextBtn,
            PopupAction.Previous => PrevBtn,
            PopupAction.Stop => PlayBtn,
            _ => null,
        };
        if (btn != null)
        {
            var st = btn.RenderTransform as ScaleTransform;
            if (st == null || st.IsFrozen) btn.RenderTransform = st = new ScaleTransform();
            var a = new DoubleAnimation(0.86, 1, TimeSpan.FromMilliseconds(380)) { EasingFunction = new BackEase { EasingMode = EasingMode.EaseOut, Amplitude = 0.6 } };
            st.BeginAnimation(ScaleTransform.ScaleXProperty, a);
            st.BeginAnimation(ScaleTransform.ScaleYProperty, a);
        }

        if (action == PopupAction.PlayPause || action == PopupAction.Stop)
        {
            PlayRingScale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(1, 1.45, TimeSpan.FromMilliseconds(500)));
            PlayRingScale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(1, 1.45, TimeSpan.FromMilliseconds(500)));
            PlayRing.BeginAnimation(OpacityProperty, new DoubleAnimation(0.8, 0, TimeSpan.FromMilliseconds(500)));
        }

        // Big glyph over the artwork: what just happened.
        string? geo = action switch
        {
            PopupAction.PlayPause => t?.IsPlaying == true ? "PlayGeo" : "PauseGeo",
            PopupAction.Next => "NextGeo",
            PopupAction.Previous => "NextGeo",
            PopupAction.Stop => "PauseGeo",
            _ => null,
        };
        if (geo != null)
        {
            ActionGlyph.Data = (Geometry)Resources[geo];
            ActionGlyph.RenderTransformOrigin = new Point(0.5, 0.5);
            ActionGlyph.RenderTransform = action == PopupAction.Previous ? new ScaleTransform(-1, 1) : Transform.Identity;
            var badge = new DoubleAnimationUsingKeyFrames();
            badge.KeyFrames.Add(new EasingDoubleKeyFrame(1, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(90))));
            badge.KeyFrames.Add(new EasingDoubleKeyFrame(1, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(550))));
            badge.KeyFrames.Add(new EasingDoubleKeyFrame(0, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(850))));
            ActionBadge.BeginAnimation(OpacityProperty, badge);
        }

        if (action is PopupAction.Next or PopupAction.Previous or PopupAction.Switched)
        {
            var a = new DoubleAnimation(0.92, 1, TimeSpan.FromMilliseconds(420)) { EasingFunction = new BackEase { EasingMode = EasingMode.EaseOut, Amplitude = 0.5 } };
            ArtScale.BeginAnimation(ScaleTransform.ScaleXProperty, a);
            ArtScale.BeginAnimation(ScaleTransform.ScaleYProperty, a);
        }
        if (action == PopupAction.Switched)
        {
            AppLineShift.BeginAnimation(TranslateTransform.XProperty,
                new DoubleAnimation(-12, 0, TimeSpan.FromMilliseconds(320)) { EasingFunction = new QuinticEase { EasingMode = EasingMode.EaseOut } });
        }
    }

    // ---------- Progress ----------

    void UpdateProgress()
    {
        if (_dragging || _scrubbing) return;
        var t = _rendered;
        if (t == null) return;
        DrawProgress(t.Position, t.Duration, false);
        LiveDot.Visibility = t.IsPlaying ? Visibility.Visible : Visibility.Collapsed;
    }

    void DrawProgress(TimeSpan pos, TimeSpan dur, bool preview)
    {
        double width = Progress.ActualWidth;
        double frac = dur > TimeSpan.Zero ? Math.Clamp(pos.TotalSeconds / dur.TotalSeconds, 0, 1) : 0;
        ProgressFill.Width = Math.Max(0, width * frac);
        ProgressThumb.Margin = new Thickness(Math.Clamp(width * frac - 6, -6, width - 6), 0, 0, 0);
        PosText.Text = dur > TimeSpan.Zero ? Format(pos) : "";
        DurText.Text = dur > TimeSpan.Zero ? Format(dur) : (_rendered?.IsPlaying == true ? "Live" : "");
        PosText.Foreground = preview ? _accent : (Brush)FindResource("Muted");
        if (preview)
        {
            ProgressThumb.Opacity = 1;
            ScrubBadge.UpdateLayout();
            ScrubBadge.Margin = new Thickness(Math.Clamp(width * frac - ScrubBadge.ActualWidth / 2, 0, Math.Max(0, width - ScrubBadge.ActualWidth)), -34, 0, 0);
        }
    }

    TimeSpan PositionAt(MouseEventArgs e)
    {
        var t = _rendered;
        if (t == null || Progress.ActualWidth <= 0) return TimeSpan.Zero;
        double frac = Math.Clamp(e.GetPosition(Progress).X / Progress.ActualWidth, 0, 1);
        return TimeSpan.FromSeconds(t.Duration.TotalSeconds * frac);
    }

    void DragTo(MouseEventArgs e)
    {
        var t = _rendered;
        if (t == null) return;
        var pos = PositionAt(e);
        DrawProgress(pos, t.Duration, true);
        ScrubText.Text = Format(pos);
        ScrubBadge.Opacity = 1;
        RestartHide(_settings().PopupSeconds);
    }

    static string Format(TimeSpan t) =>
        t.TotalHours >= 1 ? $"{(int)t.TotalHours}:{t.Minutes:00}:{t.Seconds:00}" : $"{(int)t.TotalMinutes}:{t.Seconds:00}";

    // ---------- Volume ----------

    void RenderVolume(MediaSession t)
    {
        var v = AppAudio.GetVolume(t.App);
        if (v == null) { VolumeBox.Visibility = Visibility.Hidden; return; }
        VolumeBox.Visibility = Visibility.Visible;
        _volumeChanging = true;
        VolSlider.Value = v.Value.Level * 100;
        _volumeChanging = false;
        UpdateVolumeGlyph(v.Value.Level * 100, v.Value.Muted);
        MuteBtn.ToolTip = (v.Value.Muted ? "Unmute " : "Mute ") + t.App.Name;
    }

    void UpdateVolumeGlyph(double level, bool muted)
    {
        bool silent = muted || level < 0.5;
        VolWaves.Visibility = silent ? Visibility.Collapsed : Visibility.Visible;
        VolMuteX.Visibility = silent ? Visibility.Visible : Visibility.Collapsed;
        VolSlider.Opacity = muted ? 0.4 : 1;
    }

    void ToggleMute()
    {
        var t = _rendered;
        if (t == null) return;
        var v = AppAudio.GetVolume(t.App);
        if (v == null) return;
        AppAudio.SetVolume(t.App, v.Value.Level, !v.Value.Muted);
        RenderVolume(t);
    }

    void OnWheel(object sender, MouseWheelEventArgs e)
    {
        var t = _rendered;
        if (t == null || VolumeBox.Visibility != Visibility.Visible) return;
        VolSlider.Value = Math.Clamp(VolSlider.Value + (e.Delta > 0 ? 4 : -4), 0, 100);
        RestartHide(_settings().PopupSeconds);
        e.Handled = true;
    }

    static void Fade(UIElement el, double to, int ms) =>
        el.BeginAnimation(OpacityProperty, new DoubleAnimation(to, TimeSpan.FromMilliseconds(ms)));
}
