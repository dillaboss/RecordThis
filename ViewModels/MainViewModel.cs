using System.ComponentModel;
using CommunityToolkit.Mvvm.ComponentModel;
using RecordThis.Models;

namespace RecordThis.ViewModels;

public enum AppScreen
{
    Capture,
    Review,
}

/// <summary>Single-window state machine: Capture ⇄ Review &amp; Export.</summary>
public sealed partial class MainViewModel : ObservableObject, IDisposable
{
    private readonly IDisposable[] _ownedServices;
    private bool _disposed;

    public MainViewModel(CaptureViewModel capture, ReviewViewModel review, params IDisposable[] ownedServices)
    {
        Capture = capture;
        Review = review;
        _ownedServices = ownedServices;

        Capture.RecordingFinished += OnRecordingFinished;
        Capture.PropertyChanged += OnCapturePropertyChanged;
        Review.Closed += OnReviewClosed;
    }

    public CaptureViewModel Capture { get; }

    public ReviewViewModel Review { get; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(WindowTitle))]
    private AppScreen _screen = AppScreen.Capture;

    public string WindowTitle => Screen switch
    {
        AppScreen.Review => "RecordThis — Review & Export",
        _ when Capture.IsRecording => "RecordThis — Recording",
        _ => "RecordThis — Idle",
    };

    private void OnRecordingFinished(object? sender, CaptureResult result)
    {
        Review.Load(result);
        Screen = AppScreen.Review;
    }

    private void OnReviewClosed(object? sender, EventArgs e) => Screen = AppScreen.Capture;

    private void OnCapturePropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(CaptureViewModel.IsRecording))
        {
            OnPropertyChanged(nameof(WindowTitle));
        }
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        Capture.RecordingFinished -= OnRecordingFinished;
        Capture.PropertyChanged -= OnCapturePropertyChanged;
        Review.Closed -= OnReviewClosed;

        Review.Dispose();
        Capture.Dispose();
        foreach (var service in _ownedServices)
        {
            try
            {
                service.Dispose();
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine(ex);
            }
        }
    }
}
