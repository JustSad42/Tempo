using System.Text.Json;

namespace Tempo;

public sealed class HistoryItem
{
    public string Title { get; set; } = "";
    public string Artist { get; set; } = "";
    public string App { get; set; } = "";
    public string Aumid { get; set; } = "";
    public DateTime At { get; set; }
}

/// <summary>Recently played tracks, newest first (%APPDATA%\Tempo\history.json).</summary>
sealed class HistoryStore
{
    const int Max = 50;
    static string FilePath => Path.Combine(AppSettings.Folder, "history.json");
    public List<HistoryItem> Items { get; private set; } = [];
    public event Action? Changed;

    public static HistoryStore Load()
    {
        var h = new HistoryStore();
        try { if (File.Exists(FilePath)) h.Items = JsonSerializer.Deserialize<List<HistoryItem>>(File.ReadAllText(FilePath)) ?? []; }
        catch { }
        return h;
    }

    public void Add(MediaSession s)
    {
        if (string.IsNullOrWhiteSpace(s.Title)) return;
        // Same track again within half an hour (pause/resume, switching between apps, the app re-announcing it): not a new entry.
        if (Items.Any(h => h.Title == s.Title && h.Artist == s.Artist && (DateTime.Now - h.At).TotalMinutes < 30)) return;
        Items.Insert(0, new HistoryItem { Title = s.Title, Artist = s.Artist, App = s.App.Name, Aumid = s.Aumid, At = DateTime.Now });
        if (Items.Count > Max) Items.RemoveRange(Max, Items.Count - Max);
        Save();
        Changed?.Invoke();
    }

    public void Clear() { Items.Clear(); Save(); Changed?.Invoke(); }

    void Save()
    {
        try
        {
            Directory.CreateDirectory(AppSettings.Folder);
            File.WriteAllText(FilePath, JsonSerializer.Serialize(Items, new JsonSerializerOptions { WriteIndented = true }));
        }
        catch { }
    }
}

/// <summary>Counters shown in settings (%APPDATA%\Tempo\stats.json).</summary>
public sealed class Stats
{
    public DateTime Since { get; set; } = DateTime.Now;
    public long KeysHandled { get; set; }
    public long KeysToday { get; set; }
    public DateTime Today { get; set; } = DateTime.Today;
    /// <summary>Key presses that went to a different app than Windows would have chosen.</summary>
    public long BetterThanWindows { get; set; }
    public long PassedToOtherPlayers { get; set; }
    public long AppSwitches { get; set; }
    public long Scrubs { get; set; }
    public long HookRecoveries { get; set; }

    static string FilePath => Path.Combine(AppSettings.Folder, "stats.json");
    bool _dirty;
    public event Action? Changed;

    public static Stats Load()
    {
        try { if (File.Exists(FilePath)) return JsonSerializer.Deserialize<Stats>(File.ReadAllText(FilePath)) ?? new Stats(); }
        catch { }
        return new Stats();
    }

    public void Key(bool betterThanWindows)
    {
        if (Today != DateTime.Today) { Today = DateTime.Today; KeysToday = 0; }
        KeysHandled++; KeysToday++;
        if (betterThanWindows) BetterThanWindows++;
        Touch();
    }

    public void Touch() { _dirty = true; Changed?.Invoke(); }

    /// <summary>Writes to disk if anything changed (called on a timer and at exit).</summary>
    public void Flush()
    {
        if (!_dirty) return;
        _dirty = false;
        try
        {
            Directory.CreateDirectory(AppSettings.Folder);
            File.WriteAllText(FilePath, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
        }
        catch { }
    }
}
