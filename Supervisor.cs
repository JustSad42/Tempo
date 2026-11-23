using System.Diagnostics;
using System.Windows;

namespace Tempo;

/// <summary>
/// Keeps Tempo alive. Started from the Run key / Start menu, the exe first acts as a small supervisor that launches
/// the real app (<c>--child</c>) and restarts it if it ever dies unexpectedly. Quit and updates end it normally.
/// The child runs with .NET crash dumps enabled, so a crash leaves a small dump in %APPDATA%\Tempo to diagnose.
/// </summary>
public partial class App
{
    static readonly string SupervisorMutexName = @"Local\Tempo.Supervisor" + InstanceTag;
    const int MaxCrashes = 5;
    static readonly TimeSpan CrashWindow = TimeSpan.FromMinutes(10);

    /// <summary>Returns true if this process is the supervisor (and has handled everything itself).</summary>
    async Task<bool> RunSupervisorAsync(string[] args)
    {
        if (args.Contains("--child")) return false;

        // After an update, the new version waits for the old one to finish closing.
        int wait = Array.IndexOf(args, "--wait-for");
        if (wait >= 0 && wait + 1 < args.Length && int.TryParse(args[wait + 1], out int oldPid))
        {
            try { using var old = Process.GetProcessById(oldPid); old.WaitForExit(15000); } catch { /* already gone */ }
        }

        var mutex = new Mutex(false, SupervisorMutexName);
        bool owned;
        try { owned = mutex.WaitOne(args.Contains("--updated") ? 10_000 : 0); }
        catch (AbandonedMutexException) { owned = true; } // previous supervisor died; take over
        if (!owned)
        {
            // Already running: ask it to open settings (or show the pop-up), then exit.
            Native.AllowSetForegroundWindow(Native.ASFW_ANY);
            try { EventWaitHandle.OpenExisting(args.Contains("--preview") ? PreviewEventName : ShowEventName).Set(); } catch { }
            mutex.Dispose();
            Shutdown();
            return true;
        }

        string exe = Environment.ProcessPath ?? "";
        var passOn = new List<string>();
        for (int i = 0; i < args.Length; i++)
        {
            if (args[i] == "--wait-for") { i++; continue; }
            passOn.Add(args[i]);
        }
        string childArgs = string.Join(' ', new[] { "--child" }.Concat(passOn));
        var crashes = new Queue<DateTime>();
        PruneDumps();

        while (true)
        {
            var psi = new ProcessStartInfo(exe, childArgs) { UseShellExecute = false };
            psi.Environment["DOTNET_DbgEnableMiniDump"] = "1";
            psi.Environment["DOTNET_DbgMiniDumpType"] = "1"; // small "mini" dump
            psi.Environment["DOTNET_DbgMiniDumpName"] = Path.Combine(AppSettings.Folder, "crash-%p.dmp");
            Process child;
            try { child = Process.Start(psi)!; }
            catch (Exception ex) { Log("Supervisor could not start Tempo: " + ex.Message); break; }

            MemoryTrim.Soon(3);
            await child.WaitForExitAsync();
            int code = child.ExitCode;
            child.Dispose();
            if (code == 0) break; // quit, update hand-over, or Windows signing out

            Log($"Tempo stopped unexpectedly (exit code 0x{code:X8}); restarting it.");
            var now = DateTime.Now;
            crashes.Enqueue(now);
            while (crashes.Count > 0 && now - crashes.Peek() > CrashWindow) crashes.Dequeue();
            if (crashes.Count >= MaxCrashes)
            {
                Log($"Tempo crashed {MaxCrashes} times in {CrashWindow.TotalMinutes:0} minutes; not restarting it again.");
                break;
            }
            childArgs = "--child --background --restarted"; // never reopen settings on a restart
            await Task.Delay(1500);
        }

        try { mutex.ReleaseMutex(); } catch { }
        mutex.Dispose();
        Shutdown();
        return true;
    }

    /// <summary>Keeps only the three newest crash dumps.</summary>
    static void PruneDumps()
    {
        try
        {
            foreach (var old in new DirectoryInfo(AppSettings.Folder).GetFiles("crash-*.dmp").OrderByDescending(f => f.LastWriteTime).Skip(3))
                old.Delete();
        }
        catch { }
    }
}
