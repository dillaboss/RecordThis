using System.Diagnostics;
using NAudio.CoreAudioApi;
using NAudio.CoreAudioApi.Interfaces;
using RecordThis.Models;

namespace RecordThis.Services.Devices;

/// <summary>
/// Enumerates active render (loopback) and capture endpoints and tracks hot-plug changes through
/// IMMNotificationClient. Notifications arrive on MMDevAPI worker threads; handlers must not call
/// back into the enumerator synchronously, so this class only raises events and consumers
/// debounce and re-query on their own thread.
/// </summary>
public sealed class WindowsDeviceManager : IDisposable
{
    private readonly MMDeviceEnumerator _enumerator;
    private readonly NotificationClient _notifications;
    private bool _disposed;

    public WindowsDeviceManager()
    {
        _enumerator = new MMDeviceEnumerator();
        _notifications = new NotificationClient(this);
        _enumerator.RegisterEndpointNotificationCallback(_notifications);
    }

    /// <summary>Something about the endpoint list changed (added, removed, state, default, name).</summary>
    public event EventHandler? DevicesChanged;

    /// <summary>A specific endpoint was removed or is no longer active. Argument: endpoint ID.</summary>
    public event EventHandler<string>? DeviceUnavailable;

    /// <summary>Output devices (recorded via loopback) first, then inputs; Windows defaults lead each group.</summary>
    public IReadOnlyList<AudioDeviceModel> GetDevices()
    {
        var devices = new List<AudioDeviceModel>();
        AddDevices(devices, DataFlow.Render, DeviceKind.Output);
        AddDevices(devices, DataFlow.Capture, DeviceKind.Input);
        return devices;
    }

    /// <summary>The engine's shared-mode mix format for an endpoint, or null if it can't be read.</summary>
    public CaptureFormat? TryGetMixFormat(string deviceId)
    {
        MMDevice? device = null;
        AudioClient? client = null;
        try
        {
            device = _enumerator.GetDevice(deviceId);
            client = device.AudioClient;
            return CaptureFormat.FromWaveFormat(client.MixFormat);
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"Mix format unavailable for {deviceId}: {ex.Message}");
            return null;
        }
        finally
        {
            SafeDispose(client);
            SafeDispose(device);
        }
    }

    private void AddDevices(List<AudioDeviceModel> devices, DataFlow flow, DeviceKind kind)
    {
        string? defaultId = null;
        MMDevice? defaultDevice = null;
        try
        {
            defaultDevice = _enumerator.GetDefaultAudioEndpoint(flow, Role.Multimedia);
            defaultId = defaultDevice.ID;
        }
        catch (Exception)
        {
            // No default endpoint for this flow (e.g. no microphone installed).
        }
        finally
        {
            SafeDispose(defaultDevice);
        }

        var group = new List<AudioDeviceModel>();
        try
        {
            foreach (var endpoint in _enumerator.EnumerateAudioEndPoints(flow, DeviceState.Active))
            {
                try
                {
                    group.Add(new AudioDeviceModel(endpoint.ID, endpoint.FriendlyName, kind, endpoint.ID == defaultId));
                }
                catch (Exception ex)
                {
                    Debug.WriteLine($"Skipping endpoint: {ex.Message}"); // vanished mid-enumeration
                }
                finally
                {
                    SafeDispose(endpoint);
                }
            }
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"Endpoint enumeration failed for {flow}: {ex.Message}");
        }

        devices.AddRange(group
            .OrderByDescending(d => d.IsDefault)
            .ThenBy(d => d.Name, StringComparer.CurrentCultureIgnoreCase));
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        try
        {
            _enumerator.UnregisterEndpointNotificationCallback(_notifications);
        }
        catch (Exception ex)
        {
            Debug.WriteLine(ex);
        }

        SafeDispose(_enumerator);
    }

    private static void SafeDispose(object? value)
    {
        try
        {
            (value as IDisposable)?.Dispose();
        }
        catch (Exception ex)
        {
            Debug.WriteLine(ex);
        }
    }

    private void RaiseChanged() => DevicesChanged?.Invoke(this, EventArgs.Empty);

    private void RaiseUnavailable(string deviceId)
    {
        DeviceUnavailable?.Invoke(this, deviceId);
        RaiseChanged();
    }

    /// <summary>COM callback sink. Must be public for the runtime-callable wrapper.</summary>
    public sealed class NotificationClient : IMMNotificationClient
    {
        private readonly WindowsDeviceManager _owner;

        internal NotificationClient(WindowsDeviceManager owner) => _owner = owner;

        public void OnDeviceStateChanged(string deviceId, DeviceState newState)
        {
            if (newState == DeviceState.Active)
            {
                _owner.RaiseChanged();
            }
            else
            {
                _owner.RaiseUnavailable(deviceId);
            }
        }

        public void OnDeviceAdded(string pwstrDeviceId) => _owner.RaiseChanged();

        public void OnDeviceRemoved(string deviceId) => _owner.RaiseUnavailable(deviceId);

        public void OnDefaultDeviceChanged(DataFlow flow, Role role, string defaultDeviceId)
        {
            if (role == Role.Multimedia)
            {
                _owner.RaiseChanged();
            }
        }

        public void OnPropertyValueChanged(string pwstrDeviceId, PropertyKey key)
        {
            // Fires constantly (volume, jack state...). Only a rename changes what we show.
            if (key.formatId == PropertyKeys.PKEY_Device_FriendlyName.formatId &&
                key.propertyId == PropertyKeys.PKEY_Device_FriendlyName.propertyId)
            {
                _owner.RaiseChanged();
            }
        }
    }
}
