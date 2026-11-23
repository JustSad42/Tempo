using System.Diagnostics;
using System.Net.Http;
using System.Reflection;
using System.Security.Cryptography;
using System.Text.Json;

namespace Tempo;

/// <summary>
/// Keeps Tempo up to date from GitHub releases (github.com/JustSad42/Tempo).
/// A release carries Tempo.exe and Tempo.exe.sha256. A newer version is downloaded next to
/// the running exe, checked against its SHA-256, and swapped in while the pop-up is hidden and no key is held.
/// </summary>
static class Updater
{
    public const string Repo = "JustSad42/Tempo";
    public static string ReleasesPage => $"https://github.com/{Repo}/releases";

    static readonly HttpClient Http = new() { Timeout = TimeSpan.FromSeconds(60) };

    static Updater()
    {
        Http.DefaultRequestHeaders.UserAgent.ParseAdd("Tempo-updater");
        Http.DefaultRequestHeaders.Accept.ParseAdd("application/vnd.github+json");
    }

    public static Version Current { get; } = Assembly.GetExecutingAssembly().GetName().Version ?? new Version(0, 0, 0);
    public static string CurrentText => $"{Current.Major}.{Current.Minor}.{Math.Max(0, Current.Build)}";

    /// <summary>Set when a verified new version is downloaded and waiting to be swapped in.</summary>
    public static Version? Ready { get; private set; }
    public static string Status { get; private set; } = "";
    public static event Action? StatusChanged;

    static string ExePath => Environment.ProcessPath ?? "";
    static string NewPath => ExePath + ".new";
    static string OldPath => ExePath + ".old";
    static bool _busy;

    /// <summary>Dev builds (run from bin\) never update themselves.</summary>
    public static bool CanSelfUpdate =>
        ExePath.EndsWith("Tempo.exe", StringComparison.OrdinalIgnoreCase) &&
        !ExePath.Contains(@"\bin\", StringComparison.OrdinalIgnoreCase);

    static void Set(string status) { Status = status; StatusChanged?.Invoke(); }

    /// <summary>Looks for a newer release and downloads it. Safe to call often; does nothing while busy.</summary>
    public static async Task CheckAsync(bool manual = false)
    {
        if (_busy || Ready != null) return;
        _busy = true;
        try
        {
            Set("Checking for updates…");
            using var doc = JsonDocument.Parse(await Http.GetStringAsync($"https://api.github.com/repos/{Repo}/releases/latest"));
            var root = doc.RootElement;
            var tag = root.GetProperty("tag_name").GetString() ?? "";
            if (!Version.TryParse(tag.TrimStart('v', 'V'), out var latest)) { Set("Couldn't read the latest version."); return; }
            latest = new Version(latest.Major, latest.Minor, Math.Max(0, latest.Build));
            var current = new Version(Current.Major, Current.Minor, Math.Max(0, Current.Build));
            if (latest <= current) { Set($"You have the latest version ({CurrentText})."); return; }

            string? exeUrl = null, shaUrl = null, digest = null;
            foreach (var a in root.GetProperty("assets").EnumerateArray())
            {
                var name = a.GetProperty("name").GetString();
                var url = a.GetProperty("browser_download_url").GetString();
                if (name == "Tempo.exe")
                {
                    exeUrl = url;
                    if (a.TryGetProperty("digest", out var d) && d.ValueKind == JsonValueKind.String) digest = d.GetString();
                }
                else if (name == "Tempo.exe.sha256") shaUrl = url;
            }
            if (exeUrl == null) { Set($"Version {latest} is out, but has no download yet."); return; }
            if (!CanSelfUpdate) { Set($"Version {latest} is available on GitHub."); return; }

            // Expected hash: the .sha256 file, else GitHub's own digest. Never install an unverified file.
            string? expected = null;
            if (shaUrl != null) expected = (await Http.GetStringAsync(shaUrl)).Trim().Split(' ', '\t', '\n')[0];
            else if (digest?.StartsWith("sha256:") == true) expected = digest[7..];
            if (string.IsNullOrEmpty(expected)) { Set($"Version {latest} has no checksum, so it wasn't installed."); return; }

            Set($"Downloading version {latest}…");
            var bytes = await Http.GetByteArrayAsync(exeUrl);
            var actual = Convert.ToHexString(SHA256.HashData(bytes));
            if (!actual.Equals(expected, StringComparison.OrdinalIgnoreCase))
            {
                Set("The download didn't match its checksum, so it wasn't installed.");
                return;
            }
            await File.WriteAllBytesAsync(NewPath, bytes);
            Ready = latest;
            Set($"Version {latest} is ready. It installs in a moment.");
        }
        catch (HttpRequestException ex) when (ex.StatusCode == System.Net.HttpStatusCode.NotFound)
        {
            Set("No releases published yet.");
        }
        catch (Exception ex)
        {
            Set(manual ? "Couldn't check for updates: " + ex.Message : "Couldn't check for updates.");
        }
        finally { _busy = false; }
    }

    /// <summary>Swaps in the downloaded version and restarts. Returns false if it couldn't.</summary>
    public static bool ApplyAndRestart()
    {
        if (Ready == null || !File.Exists(NewPath)) return false;
        try
        {
            File.Delete(OldPath);
            File.Move(ExePath, OldPath);   // Windows allows renaming a running exe
            File.Move(NewPath, ExePath);
        }
        catch
        {
            try { if (!File.Exists(ExePath) && File.Exists(OldPath)) File.Move(OldPath, ExePath); } catch { }
            Set("Couldn't install the update (is the folder read-only?).");
            Ready = null;
            return false;
        }
        Process.Start(new ProcessStartInfo(ExePath, $"--background --updated --wait-for {Environment.ProcessId}") { UseShellExecute = false });
        System.Windows.Application.Current.Shutdown();
        return true;
    }

    /// <summary>Called on startup: removes the previous version left behind by an update.</summary>
    public static void CleanUp()
    {
        // Off the UI thread: the old exe may still be closing, and retrying must not hold up start-up.
        Task.Run(() =>
        {
            for (int i = 0; i < 20; i++)
            {
                try
                {
                    if (File.Exists(OldPath)) File.Delete(OldPath);
                    if (File.Exists(NewPath) && Ready == null) File.Delete(NewPath);
                    return;
                }
                catch { Thread.Sleep(500); }
            }
        });
    }
}
