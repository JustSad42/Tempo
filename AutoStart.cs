using Microsoft.Win32;

namespace Tempo;

/// <summary>Toggles the "start with Windows" entry under HKCU\...\Run.</summary>
static class AutoStart
{
    const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    const string ValueName = "Tempo";

    public static bool IsEnabled
    {
        get
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKey);
            return key?.GetValue(ValueName) != null;
        }
    }

    public static void Set(bool enabled)
    {
        using var key = Registry.CurrentUser.CreateSubKey(RunKey);
        if (enabled) key.SetValue(ValueName, $"\"{Environment.ProcessPath}\" --background");
        else key.DeleteValue(ValueName, false);
    }

    /// <summary>Keeps the entry pointing at this exe if the app was moved.</summary>
    public static void Repair()
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey, true);
        if (key?.GetValue(ValueName) is string v && !v.Contains(Environment.ProcessPath ?? "?", StringComparison.OrdinalIgnoreCase))
            Set(true);
    }
}
