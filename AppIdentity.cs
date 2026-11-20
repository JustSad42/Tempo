using System.Diagnostics;

using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace Tempo;

/// <summary>
/// Turns a media session's app ID ("Spotify.exe", "Chrome", "308046B0AF4A39CB", "SpotifyAB.SpotifyMusic_…!Spotify")
/// into a friendly name, an icon and the exe names its audio comes from.
/// </summary>
sealed class AppIdentity
{
    static readonly Dictionary<string, AppIdentity> Cache = new(StringComparer.OrdinalIgnoreCase);

    // App IDs that don't name their exe.
    static readonly Dictionary<string, (string Name, string Exe)> Known = new(StringComparer.OrdinalIgnoreCase)
    {
        ["Chrome"] = ("Google Chrome", "chrome"),
        ["MSEdge"] = ("Microsoft Edge", "msedge"),
        ["308046B0AF4A39CB"] = ("Firefox", "firefox"),
        ["6F193CCC56814779"] = ("Firefox Nightly", "firefox"),
        ["E7CF176E110C211B"] = ("Firefox", "firefox"),
        ["Brave"] = ("Brave", "brave"),
        ["Opera"] = ("Opera", "opera"),
        ["Vivaldi"] = ("Vivaldi", "vivaldi"),
    };

    public string Aumid { get; }
    public string Name { get; }
    public ImageSource? Icon { get; }
    /// <summary>Lower-case process names without ".exe" that play this app's audio.</summary>
    public IReadOnlySet<string> ExeNames { get; }

    AppIdentity(string aumid, string name, ImageSource? icon, HashSet<string> exes)
    {
        Aumid = aumid; Name = name; Icon = icon; ExeNames = exes;
    }

    public static AppIdentity For(string aumid)
    {
        lock (Cache)
        {
            if (Cache.TryGetValue(aumid, out var hit)) return hit;
        }
        var made = Resolve(aumid);
        lock (Cache) Cache[aumid] = made;
        return made;
    }

    static AppIdentity Resolve(string aumid)
    {
        var exes = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        string? name = null;
        ImageSource? icon = null;

        if (Known.TryGetValue(aumid, out var known)) { name = known.Name; exes.Add(known.Exe); }

        // "Spotify.exe" style IDs name the exe directly.
        if (aumid.EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
            exes.Add(Path.GetFileNameWithoutExtension(aumid));

        // Store apps: "Publisher.App_hash!AppName" -> "AppName" is often the process name.
        int bang = aumid.IndexOf('!');
        if (bang > 0) exes.Add(aumid[(bang + 1)..]);

        // The Start menu entry has the proper name and icon, and for desktop apps the exe path.
        var (shellName, shellIcon, target) = Native.AppsFolderItem(aumid, 64);
        if (!string.IsNullOrWhiteSpace(shellName) && !shellName.Contains('\\')) name ??= shellName;
        icon = shellIcon;
        if (target != null && target.EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
            exes.Add(Path.GetFileNameWithoutExtension(target));

        // Fall back to a running process with a matching name.
        if (name == null || icon == null)
        {
            foreach (var exe in exes)
            {
                string? path = FindProcessPath(exe);
                if (path == null) continue;
                if (name == null)
                {
                    try
                    {
                        var desc = FileVersionInfo.GetVersionInfo(path).FileDescription;
                        if (!string.IsNullOrWhiteSpace(desc)) name = desc.Trim();
                    }
                    catch { }
                }
                icon ??= ExeIcon(path);
                break;
            }
        }

        name ??= Prettify(aumid);
        return new AppIdentity(aumid, name, icon, exes);
    }

    static string Prettify(string aumid)
    {
        string s = aumid;
        int bang = s.IndexOf('!');
        if (bang > 0) s = s[(bang + 1)..];
        if (s.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)) s = s[..^4];
        int dot = s.LastIndexOf('.');
        if (dot > 0 && dot < s.Length - 1) s = s[(dot + 1)..];
        return s.Length > 0 ? char.ToUpper(s[0]) + s[1..] : aumid;
    }

    static string? FindProcessPath(string exe)
    {
        foreach (var p in Process.GetProcessesByName(exe))
        {
            try
            {
                var (path, _) = Native.ProcessIdentity(p.Id);
                if (path != null) return path;
            }
            catch { }
            finally { p.Dispose(); }
        }
        return null;
    }

    static ImageSource? ExeIcon(string path)
    {
        try
        {
            using var ico = System.Drawing.Icon.ExtractAssociatedIcon(path);
            if (ico == null) return null;
            var src = Imaging.CreateBitmapSourceFromHIcon(ico.Handle, System.Windows.Int32Rect.Empty, BitmapSizeOptions.FromEmptyOptions());
            src.Freeze();
            return src;
        }
        catch { return null; }
    }
}
