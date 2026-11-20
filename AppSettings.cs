using System.Text.Json;
using System.Text.Json.Serialization;

namespace Tempo;

public enum PopupPlacement { TopLeft, TopCenter, TopRight, BottomLeft, BottomCenter, BottomRight }

/// <summary>Saved in %APPDATA%\Tempo\settings.json.</summary>
public sealed class AppSettings
{
    public PopupPlacement Placement { get; set; } = PopupPlacement.TopCenter;
    /// <summary>Show on the screen under the mouse (otherwise the primary screen).</summary>
    public bool FollowMouse { get; set; } = true;
    /// <summary>How long the pop-up stays after a key press, in seconds.</summary>
    public double PopupSeconds { get; set; } = 2.5;
    public bool ShowPopup { get; set; } = true;
    public bool HideInFullscreen { get; set; } = false;
    public bool HoldToScrub { get; set; } = true;
    public bool HoldToSwitchApp { get; set; } = true;
    public int SkipSeconds { get; set; } = 10;
    /// <summary>Let the key through to Windows when the only thing making sound is a player Tempo can't control.</summary>
    public bool PassThroughUnknownPlayers { get; set; } = true;
    public bool WelcomeShown { get; set; }
    /// <summary>App ID that gets the keys when nothing is playing (instead of whatever played last).</summary>
    public string? PreferredApp { get; set; }
    /// <summary>Double-tap Play/Pause to bring the playing app to the front.</summary>
    public bool DoubleTapToOpen { get; set; } = true;
    /// <summary>Keep the keys from Windows even when no media app is open.</summary>
    public bool AlwaysCapture { get; set; } = true;
    public bool AutoUpdate { get; set; } = true;
    /// <summary>Pop-ups hidden until this time ("Hide for an hour"). Not saved.</summary>
    [JsonIgnore] public DateTime SnoozeUntil { get; set; }

    /// <summary>App IDs the media keys should never control.</summary>
    public HashSet<string> IgnoredApps { get; set; } = new(StringComparer.OrdinalIgnoreCase);
    /// <summary>Every app seen so far (app ID → name), for the settings list.</summary>
    public Dictionary<string, string> KnownApps { get; set; } = new(StringComparer.OrdinalIgnoreCase);

    [JsonIgnore] public static string Folder { get; } =
        Environment.GetEnvironmentVariable("TEMPO_DATA_DIR") is { Length: > 0 } dir ? dir :
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "Tempo");
    static string FilePath => Path.Combine(Folder, "settings.json");
    static readonly JsonSerializerOptions Json = new() { WriteIndented = true, Converters = { new JsonStringEnumConverter() } };

    public event Action? Saved;

    public static AppSettings Load()
    {
        try
        {
            if (File.Exists(FilePath))
            {
                var s = JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(FilePath), Json);
                if (s != null)
                {
                    s.IgnoredApps = new HashSet<string>(s.IgnoredApps, StringComparer.OrdinalIgnoreCase);
                    s.KnownApps = new Dictionary<string, string>(s.KnownApps, StringComparer.OrdinalIgnoreCase);
                    return s;
                }
            }
        }
        catch { }
        return new AppSettings();
    }

    public void Save()
    {
        try
        {
            Directory.CreateDirectory(Folder);
            var tmp = FilePath + ".tmp";
            File.WriteAllText(tmp, JsonSerializer.Serialize(this, Json));
            File.Move(tmp, FilePath, true);
        }
        catch { }
        Saved?.Invoke();
    }

    internal void Remember(AppIdentity app)
    {
        if (string.IsNullOrEmpty(app.Aumid)) return;
        if (KnownApps.TryGetValue(app.Aumid, out var name) && name == app.Name) return;
        KnownApps[app.Aumid] = app.Name;
        Save();
    }
}
