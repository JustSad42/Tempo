using System.Reflection;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;

namespace Tempo;

public partial class SettingsWindow : Window
{
    readonly MediaHub _hub;
    readonly AppSettings _s;
    readonly Stats _stats;
    readonly HistoryStore _history;
    bool _loading = true;

    public event Action? PreviewRequested;
    public event Action? QuitRequested;
    public event Action? CheckUpdatesRequested;

    internal SettingsWindow(MediaHub hub, AppSettings settings, Stats stats, HistoryStore history)
    {
        InitializeComponent();
        _hub = hub;
        _s = settings;
        _stats = stats;
        _history = history;

        SourceInitialized += (_, _) =>
        {
            var hwnd = new WindowInteropHelper(this).Handle;
            Native.SetDwm(hwnd, 20, 1);  // dark title bar
            Native.SetDwm(hwnd, 33, 2);  // rounded corners
            if (Environment.OSVersion.Version.Build >= 22621)
            {
                Background = Brushes.Transparent;
                HwndSource.FromHwnd(hwnd).CompositionTarget.BackgroundColor = Colors.Transparent;
                Native.SetDwm(hwnd, 38, 2); // Mica backdrop
                ExtendFrame(hwnd);
            }
        };

        var v = Assembly.GetExecutingAssembly().GetName().Version;
        VersionText.Text = $"Version {v?.Major}.{v?.Minor}.{v?.Build}";
        Welcome.Visibility = _s.WelcomeShown ? Visibility.Collapsed : Visibility.Visible;

        LoadValues();
        Wire();
        _loading = false;

        RefreshStatus();
        RefreshApps();
        RefreshHistory();
        RefreshStats();
        RefreshUpdate();
        _hub.Changed += OnHubChanged;
        _history.Changed += OnHistory;
        _stats.Changed += OnStats;
        _s.Saved += OnSaved;
        Updater.StatusChanged += OnUpdate;
        Closed += (_, _) =>
        {
            _hub.Changed -= OnHubChanged;
            _history.Changed -= OnHistory;
            _stats.Changed -= OnStats;
            _s.Saved -= OnSaved;
            Updater.StatusChanged -= OnUpdate;
            if (!_s.WelcomeShown) { _s.WelcomeShown = true; _s.Save(); }
        };
    }

    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    struct MARGINS { public int L, R, T, B; }
    [System.Runtime.InteropServices.DllImport("dwmapi.dll")] static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref MARGINS m);
    static void ExtendFrame(IntPtr hwnd) { var m = new MARGINS { L = -1, R = -1, T = -1, B = -1 }; DwmExtendFrameIntoClientArea(hwnd, ref m); }

    void OnHubChanged() { RefreshStatus(); RefreshApps(); }
    void OnHistory() => Dispatcher.BeginInvoke(RefreshHistory);
    void OnStats() => Dispatcher.BeginInvoke(RefreshStats);
    void OnUpdate() => Dispatcher.BeginInvoke(RefreshUpdate);
    void OnSaved() => Dispatcher.BeginInvoke(RefreshApps);

    void RefreshUpdate()
    {
        UpdateStatus.Text = string.IsNullOrEmpty(Updater.Status)
            ? (Updater.CanSelfUpdate ? "Checks GitHub every few hours and installs quietly." : "This copy is a development build and doesn't update itself.")
            : Updater.Status;
    }

    void RefreshStats()
    {
        StatsGrid.Children.Clear();
        (string Value, string Label)[] tiles =
        [
            (_stats.KeysToday.ToString("N0"), "key presses today"),
            (_stats.KeysHandled.ToString("N0"), "key presses in total"),
            (_stats.BetterThanWindows.ToString("N0"), "went to a different app than Windows would have"),
            (_stats.Scrubs.ToString("N0"), "scrubs"),
            (_stats.AppSwitches.ToString("N0"), "app switches"),
            (_stats.HookRecoveries.ToString("N0"), "times the key hook was reconnected"),
        ];
        for (int i = 0; i < tiles.Length; i++)
        {
            var panel = new StackPanel { Margin = new Thickness(8, 6, 8, 6) };
            panel.Children.Add(new TextBlock { Text = tiles[i].Value, FontFamily = (FontFamily)FindResource("DisplayFont"), FontSize = 20, FontWeight = FontWeights.SemiBold });
            panel.Children.Add(new TextBlock { Text = tiles[i].Label, FontSize = 11.5, Foreground = (Brush)FindResource("Faint"), TextWrapping = TextWrapping.Wrap });
            Grid.SetColumn(panel, i % 3);
            Grid.SetRow(panel, i / 3);
            StatsGrid.Children.Add(panel);
        }
        StatsSince.Text = $"Counting since {_stats.Since:d MMM yyyy}. {_stats.PassedToOtherPlayers:N0} key presses were handed to players that don't report to Windows.";
    }

    void RefreshHistory()
    {
        HistoryList.Children.Clear();
        var items = _history.Items.Take(20).ToList();
        HistoryEmpty.Visibility = items.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        ClearHistoryBtn.Visibility = items.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        for (int i = 0; i < items.Count; i++)
        {
            var h = items[i];
            var row = new Grid { Margin = new Thickness(16, 8, 10, 8) };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            row.ColumnDefinitions.Add(new ColumnDefinition());
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var app = AppIdentity.For(h.Aumid);
            var icon = new Image { Source = app.Icon, Width = 18, Height = 18, VerticalAlignment = VerticalAlignment.Center, ToolTip = h.App };
            RenderOptions.SetBitmapScalingMode(icon, BitmapScalingMode.HighQuality);
            row.Children.Add(icon);

            var text = new StackPanel { Margin = new Thickness(12, 0, 8, 0), VerticalAlignment = VerticalAlignment.Center };
            text.Children.Add(new TextBlock { Text = h.Title, FontSize = 13, TextTrimming = TextTrimming.CharacterEllipsis });
            string sub = (string.IsNullOrWhiteSpace(h.Artist) ? h.App : h.Artist) + " · " + Ago(h.At);
            text.Children.Add(new TextBlock { Text = sub, FontSize = 11.5, Foreground = (Brush)FindResource("Faint"), TextTrimming = TextTrimming.CharacterEllipsis, Margin = new Thickness(0, 1, 0, 0) });
            Grid.SetColumn(text, 1);
            row.Children.Add(text);

            string query = string.IsNullOrWhiteSpace(h.Artist) ? h.Title : $"{h.Artist} {h.Title}";
            var actions = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
            actions.Children.Add(SmallButton("\uE721", "Search on YouTube", () => Open("https://www.youtube.com/results?search_query=" + Uri.EscapeDataString(query))));
            actions.Children.Add(SmallButton("\uE8D6", "Search on Spotify", () => Open("https://open.spotify.com/search/" + Uri.EscapeDataString(query))));
            actions.Children.Add(SmallButton("\uE8C8", "Copy song name", () =>
            {
                try { Clipboard.SetText(string.IsNullOrWhiteSpace(h.Artist) ? h.Title : $"{h.Artist} \u2013 {h.Title}"); } catch { }
            }));
            Grid.SetColumn(actions, 2);
            row.Children.Add(actions);

            if (i > 0) HistoryList.Children.Add(new Border { Style = (Style)FindResource("Divider") });
            HistoryList.Children.Add(row);
        }
    }

    Button SmallButton(string glyph, string tip, Action action)
    {
        var b = new Button
        {
            Style = (Style)FindResource("IconButton"), Width = 30, Height = 30, ToolTip = tip,
            Content = new TextBlock { Text = glyph, FontFamily = (FontFamily)FindResource("IconFont"), FontSize = 13, Foreground = (Brush)FindResource("Muted") },
        };
        b.Click += (_, _) => action();
        return b;
    }

    static void Open(string url)
    {
        try { System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(url) { UseShellExecute = true }); } catch { }
    }

    static string Ago(DateTime t)
    {
        var d = DateTime.Now - t;
        if (d.TotalMinutes < 1) return "just now";
        if (d.TotalMinutes < 60) return $"{(int)d.TotalMinutes} min ago";
        if (d.TotalHours < 24) return $"{(int)d.TotalHours} h ago";
        if (d.TotalDays < 7) return t.ToString("dddd HH:mm");
        return t.ToString("d MMM");
    }

    void LoadValues()
    {
        ShowPopupBox.IsChecked = _s.ShowPopup;
        (_s.Placement switch
        {
            PopupPlacement.TopLeft => PosTL, PopupPlacement.TopRight => PosTR,
            PopupPlacement.BottomLeft => PosBL, PopupPlacement.BottomCenter => PosBC, PopupPlacement.BottomRight => PosBR,
            _ => PosTC,
        }).IsChecked = true;
        (_s.FollowMouse ? ScreenMouse : ScreenPrimary).IsChecked = true;
        DurationSlider.Value = _s.PopupSeconds;
        DurationText.Text = $"{_s.PopupSeconds:0.#} s";
        FullscreenBox.IsChecked = _s.HideInFullscreen;
        ScrubBox.IsChecked = _s.HoldToScrub;
        SwitchBox.IsChecked = _s.HoldToSwitchApp;
        (_s.SkipSeconds switch { 5 => Skip5, 15 => Skip15, 30 => Skip30, _ => Skip10 }).IsChecked = true;
        PassBox.IsChecked = _s.PassThroughUnknownPlayers;
        DoubleTapBox.IsChecked = _s.DoubleTapToOpen;
        AlwaysBox.IsChecked = _s.AlwaysCapture;
        AutoUpdateBox.IsChecked = _s.AutoUpdate;
        StartupBox.IsChecked = AutoStart.IsEnabled;
    }

    void Wire()
    {
        void Save() { if (!_loading) _s.Save(); }

        ShowPopupBox.Click += (_, _) => { _s.ShowPopup = ShowPopupBox.IsChecked == true; Save(); };
        foreach (var (rb, p) in new[] { (PosTL, PopupPlacement.TopLeft), (PosTC, PopupPlacement.TopCenter), (PosTR, PopupPlacement.TopRight),
                                        (PosBL, PopupPlacement.BottomLeft), (PosBC, PopupPlacement.BottomCenter), (PosBR, PopupPlacement.BottomRight) })
        {
            var place = p;
            rb.Checked += (_, _) => { if (_loading) return; _s.Placement = place; Save(); PreviewRequested?.Invoke(); };
        }
        ScreenMouse.Checked += (_, _) => { _s.FollowMouse = true; Save(); };
        ScreenPrimary.Checked += (_, _) => { _s.FollowMouse = false; Save(); };
        DurationSlider.ValueChanged += (_, e) => { _s.PopupSeconds = e.NewValue; DurationText.Text = $"{e.NewValue:0.#} s"; Save(); };
        FullscreenBox.Click += (_, _) => { _s.HideInFullscreen = FullscreenBox.IsChecked == true; Save(); };
        ScrubBox.Click += (_, _) => { _s.HoldToScrub = ScrubBox.IsChecked == true; Save(); };
        SwitchBox.Click += (_, _) => { _s.HoldToSwitchApp = SwitchBox.IsChecked == true; Save(); };
        foreach (var (rb, sec) in new[] { (Skip5, 5), (Skip10, 10), (Skip15, 15), (Skip30, 30) })
        {
            int n = sec;
            rb.Checked += (_, _) => { _s.SkipSeconds = n; Save(); };
        }
        PassBox.Click += (_, _) => { _s.PassThroughUnknownPlayers = PassBox.IsChecked == true; Save(); };
        DoubleTapBox.Click += (_, _) => { _s.DoubleTapToOpen = DoubleTapBox.IsChecked == true; Save(); };
        AlwaysBox.Click += (_, _) => { _s.AlwaysCapture = AlwaysBox.IsChecked == true; Save(); };
        AutoUpdateBox.Click += (_, _) => { _s.AutoUpdate = AutoUpdateBox.IsChecked == true; Save(); };
        CheckUpdatesBtn.Click += (_, _) => CheckUpdatesRequested?.Invoke();
        ClearHistoryBtn.Click += (_, _) => _history.Clear();
        StartupBox.Click += (_, _) =>
        {
            try { AutoStart.Set(StartupBox.IsChecked == true); }
            catch (Exception ex) { App.Log("Startup toggle failed: " + ex.Message); }
            StartupBox.IsChecked = AutoStart.IsEnabled;
        };
        PreviewBtn.Click += (_, _) => PreviewRequested?.Invoke();
        QuitBtn.Click += (_, _) => QuitRequested?.Invoke();
    }

    void RefreshStatus()
    {
        var t = _hub.Target;
        int n = _hub.Sessions.Count;
        if (t == null)
        {
            StatusArt.Source = null;
            StatusTitle.Text = n == 0 ? "Listening for media keys" : "All media apps are turned off below";
            StatusSub.Text = n == 0 ? "No app is reporting media right now" : "Keys go to Windows as usual";
            return;
        }
        StatusArt.Source = t.Art ?? t.App.Icon;
        StatusTitle.Text = string.IsNullOrWhiteSpace(t.Title) ? t.App.Name : t.Title;
        string state = t.IsPlaying ? "Playing" : "Paused";
        StatusSub.Text = $"Keys control {t.App.Name} · {state}" + (n > 1 ? $" · {n} apps with media" : "");
    }

    bool isPreferredApp(string id) => string.Equals(_s.PreferredApp, id, StringComparison.OrdinalIgnoreCase);

    void RefreshApps()
    {
        AppsList.Children.Clear();
        var live = _hub.Sessions.ToDictionary(s => s.Aumid, StringComparer.OrdinalIgnoreCase);
        var ids = _s.KnownApps.Keys.OrderByDescending(id => live.ContainsKey(id)).ThenBy(id => _s.KnownApps[id]).ToList();
        AppsEmpty.Visibility = ids.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        for (int i = 0; i < ids.Count; i++)
        {
            string id = ids[i];
            var app = AppIdentity.For(id);
            live.TryGetValue(id, out var session);

            var row = new Grid { Margin = new Thickness(16, 10, 16, 10) };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            row.ColumnDefinitions.Add(new ColumnDefinition());
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var iconBox = new Grid { Width = 28, Height = 28 };
            iconBox.Children.Add(new Border { CornerRadius = new CornerRadius(7), Background = new SolidColorBrush(Color.FromArgb(0x14, 255, 255, 255)) });
            if (app.Icon != null)
            {
                var img = new Image { Source = app.Icon, Width = 20, Height = 20 };
                RenderOptions.SetBitmapScalingMode(img, BitmapScalingMode.HighQuality);
                iconBox.Children.Add(img);
            }
            row.Children.Add(iconBox);

            var text = new StackPanel { Margin = new Thickness(12, 0, 0, 0), VerticalAlignment = VerticalAlignment.Center };
            text.Children.Add(new TextBlock { Text = _s.KnownApps[id], FontSize = 13.5, TextTrimming = TextTrimming.CharacterEllipsis });
            string sub = session == null ? "Not running" : session.IsPlaying ? "Playing now" : "Open, paused";
            if (isPreferredApp(id)) sub = "Preferred · " + sub;
            if (session != null && !string.IsNullOrWhiteSpace(session.Title)) sub += " · " + session.Title;
            text.Children.Add(new TextBlock { Text = sub, FontSize = 12, Foreground = (Brush)FindResource("Faint"), TextTrimming = TextTrimming.CharacterEllipsis, Margin = new Thickness(0, 2, 0, 0) });
            Grid.SetColumn(text, 1);
            row.Children.Add(text);

            bool isPreferred = string.Equals(_s.PreferredApp, id, StringComparison.OrdinalIgnoreCase);
            var star = new Button
            {
                Style = (Style)FindResource("IconButton"), Width = 30, Height = 30,
                ToolTip = isPreferred ? "Preferred: gets the keys when nothing is playing. Click to stop preferring." : "Prefer this app when nothing is playing",
                Content = new TextBlock
                {
                    Text = isPreferred ? "\uE735" : "\uE734", FontFamily = (FontFamily)FindResource("IconFont"), FontSize = 14,
                    Foreground = isPreferred ? (Brush)FindResource("Accent") : (Brush)FindResource("Faint"),
                },
            };
            star.Click += (_, _) => { _s.PreferredApp = isPreferred ? null : id; _s.Save(); };
            var right = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
            right.Children.Add(star);

            var toggle = new CheckBox { Style = (Style)FindResource("Switch"), IsChecked = !_s.IgnoredApps.Contains(id), VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(12, 0, 0, 0), ToolTip = "Let the media keys control this app" };
            toggle.Click += (_, _) =>
            {
                if (toggle.IsChecked == true) _s.IgnoredApps.Remove(id); else _s.IgnoredApps.Add(id);
                _s.Save();
            };
            right.Children.Add(toggle);
            Grid.SetColumn(right, 2);
            row.Children.Add(right);

            if (i > 0) AppsList.Children.Add(new Border { Style = (Style)FindResource("Divider") });
            AppsList.Children.Add(row);
        }
    }
}
