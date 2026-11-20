using System.Runtime;
using System.Runtime.InteropServices;
using System.Windows.Threading;

namespace Tempo;

/// <summary>
/// Tempo is hidden almost all the time, so once the pop-up or settings window goes away it hands
/// memory back: a full compacting GC (which also releases COM/WinRT objects) and a working-set trim.
/// Debounced so a burst of key presses doesn't trigger repeated collections.
/// </summary>
static class MemoryTrim
{
    [DllImport("psapi.dll")] static extern bool EmptyWorkingSet(IntPtr process);
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();

    static DispatcherTimer? _timer;

    /// <summary>Trims after <paramref name="seconds"/> of quiet. Calling again restarts the wait.</summary>
    public static void Soon(double seconds = 4)
    {
        if (_timer == null)
        {
            _timer = new DispatcherTimer(DispatcherPriority.ApplicationIdle);
            _timer.Tick += (_, _) => { _timer.Stop(); Now(); };
        }
        _timer.Stop();
        _timer.Interval = TimeSpan.FromSeconds(seconds);
        _timer.Start();
    }

    public static void Now()
    {
        try
        {
            GCSettings.LargeObjectHeapCompactionMode = GCLargeObjectHeapCompactionMode.CompactOnce;
            GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true);
            GC.WaitForPendingFinalizers();
            GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true);
            EmptyWorkingSet(GetCurrentProcess());
            if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("TEMPO_INSTANCE")))
            {
                var info = GC.GetGCMemoryInfo();
                App.Log($"Trim: GC heap {info.HeapSizeBytes / 1048576.0:N1} MB, GC committed {info.TotalCommittedBytes / 1048576.0:N1} MB, " +
                        $"private {System.Diagnostics.Process.GetCurrentProcess().PrivateMemorySize64 / 1048576.0:N1} MB");
            }
        }
        catch { }
    }
}
