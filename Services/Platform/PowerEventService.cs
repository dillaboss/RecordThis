using Microsoft.Win32;

namespace RecordThis.Services.Platform;

/// <summary>
/// Wraps SystemEvents.PowerModeChanged. Subscribing from a thread-pool (MTA) thread makes
/// SystemEvents create its own hidden-window thread, so handlers run off the UI thread and can
/// block briefly to finalize the spool before Windows suspends the CPU.
/// </summary>
public sealed class PowerEventService : IDisposable
{
    private bool _disposed;

    public PowerEventService()
    {
        Task.Run(() => SystemEvents.PowerModeChanged += OnPowerModeChanged).GetAwaiter().GetResult();
    }

    /// <summary>Raised synchronously on the SystemEvents thread before sleep/hibernate. Handlers may block (keep it under ~2 s).</summary>
    public event EventHandler? Suspending;

    public event EventHandler? Resumed;

    private void OnPowerModeChanged(object sender, PowerModeChangedEventArgs e)
    {
        switch (e.Mode)
        {
            case PowerModes.Suspend:
                Suspending?.Invoke(this, EventArgs.Empty);
                break;
            case PowerModes.Resume:
                Resumed?.Invoke(this, EventArgs.Empty);
                break;
        }
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        SystemEvents.PowerModeChanged -= OnPowerModeChanged;
    }
}
