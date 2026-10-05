using System.Diagnostics;
using Microsoft.UI.Xaml;
using RecordThis.Services.Audio;
using RecordThis.Services.Devices;
using RecordThis.Services.Platform;
using RecordThis.Services.Storage;
using RecordThis.Services.UI;
using RecordThis.ViewModels;
using WinRT.Interop;

namespace RecordThis;

/// <summary>Application lifecycle and composition root.</summary>
public partial class App : Application
{
    private MainWindow? _window;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) => Debug.WriteLine($"Unhandled exception: {e.Exception}");
    }

    protected override void OnLaunched(Microsoft.UI.Xaml.LaunchActivatedEventArgs args)
    {
        // Services
        var settings = new SettingsService();
        var spools = new SpoolFileManager();
        var capture = new WasapiAudioCaptureService(spools);
        var devices = new WindowsDeviceManager();
        var power = new PowerEventService();
        var export = new AudioExportService();
        var dialogs = new DialogService();
        var picker = new FilePickerService(() => _window is null ? 0 : WindowNative.GetWindowHandle(_window));
        var clipboard = new ClipboardService(() => _window is null ? 0 : WindowNative.GetWindowHandle(_window));

        // View models
        var captureVm = new CaptureViewModel(capture, devices, spools, power, settings);
        var reviewVm = new ReviewViewModel(export, spools, picker, clipboard, settings);
        var mainVm = new MainViewModel(captureVm, reviewVm, capture, devices, power);

        _window = new MainWindow(mainVm, dialogs);
        _window.Activate();
    }
}
