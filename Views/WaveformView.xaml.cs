using System.Windows.Input;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using RecordThis.Services.Audio.Pipeline;
using Windows.Foundation;
using Windows.System;

namespace RecordThis.Views;

/// <summary>Peak-envelope waveform with a playhead. Click or drag to scrub; arrow keys step 5 %.</summary>
public sealed partial class WaveformView : UserControl
{
    private const double KeyboardStep = 0.05;

    public static readonly DependencyProperty WaveformProperty = DependencyProperty.Register(
        nameof(Waveform), typeof(object), typeof(WaveformView),
        new PropertyMetadata(null, (d, _) => ((WaveformView)d).RedrawEnvelope()));

    public static readonly DependencyProperty ProgressProperty = DependencyProperty.Register(
        nameof(Progress), typeof(double), typeof(WaveformView),
        new PropertyMetadata(0.0, (d, _) => ((WaveformView)d).UpdatePlayhead()));

    public static readonly DependencyProperty SeekCommandProperty = DependencyProperty.Register(
        nameof(SeekCommand), typeof(ICommand), typeof(WaveformView), new PropertyMetadata(null));

    private bool _scrubbing;

    public WaveformView()
    {
        InitializeComponent();
        KeyDown += OnKeyDown;
    }

    /// <summary>A <see cref="WaveformData"/> (typed as object so the DP stores the managed instance as-is).</summary>
    public WaveformData? Waveform
    {
        get => GetValue(WaveformProperty) as WaveformData;
        set => SetValue(WaveformProperty, value);
    }

    public double Progress
    {
        get => (double)GetValue(ProgressProperty);
        set => SetValue(ProgressProperty, value);
    }

    public ICommand? SeekCommand
    {
        get => (ICommand?)GetValue(SeekCommandProperty);
        set => SetValue(SeekCommandProperty, value);
    }

    private void OnSurfaceSizeChanged(object sender, SizeChangedEventArgs e)
    {
        Surface.Clip = new RectangleGeometry { Rect = new Rect(0, 0, e.NewSize.Width, e.NewSize.Height) };
        CenterLine.X2 = e.NewSize.Width;
        RedrawEnvelope();
        UpdatePlayhead();
    }

    private void RedrawEnvelope()
    {
        double width = Surface.ActualWidth;
        double height = Surface.ActualHeight;
        var points = new PointCollection();

        if (Waveform is { Peaks.Length: > 0 } data && width > 0 && height > 0)
        {
            float[] peaks = data.Peaks;
            double mid = height / 2;
            double half = mid - 2; // breathing room top and bottom
            double step = width / peaks.Length;

            // Top edge left→right, then bottom edge right→left. Keep a 1px minimum so silence reads as a line.
            for (int i = 0; i < peaks.Length; i++)
            {
                double y = Math.Max(0.5, peaks[i] * half);
                points.Add(new Point(i * step, mid - y));
                points.Add(new Point((i + 1) * step, mid - y));
            }

            for (int i = peaks.Length - 1; i >= 0; i--)
            {
                double y = Math.Max(0.5, peaks[i] * half);
                points.Add(new Point((i + 1) * step, mid + y));
                points.Add(new Point(i * step, mid + y));
            }
        }

        Envelope.Points = points;
    }

    private void UpdatePlayhead()
    {
        double width = Surface.ActualWidth;
        double x = Math.Clamp(Progress, 0, 1) * width;
        PlayheadTransform.X = Math.Min(x, Math.Max(0, width - Playhead.Width));
        PlayedOverlay.Width = x;
    }

    private void SeekTo(double fraction)
    {
        fraction = Math.Clamp(fraction, 0, 1);
        if (SeekCommand?.CanExecute(fraction) == true)
        {
            SeekCommand.Execute(fraction);
        }
    }

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        Focus(FocusState.Pointer);
        _scrubbing = Surface.CapturePointer(e.Pointer);
        SeekToPointer(e);
        e.Handled = true;
    }

    private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (_scrubbing)
        {
            SeekToPointer(e);
            e.Handled = true;
        }
    }

    private void OnPointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (_scrubbing)
        {
            Surface.ReleasePointerCapture(e.Pointer);
            _scrubbing = false;
            e.Handled = true;
        }
    }

    private void OnPointerCaptureLost(object sender, PointerRoutedEventArgs e) => _scrubbing = false;

    private void SeekToPointer(PointerRoutedEventArgs e)
    {
        double width = Surface.ActualWidth;
        if (width > 0)
        {
            SeekTo(e.GetCurrentPoint(Surface).Position.X / width);
        }
    }

    private void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case VirtualKey.Left:
                SeekTo(Progress - KeyboardStep);
                e.Handled = true;
                break;
            case VirtualKey.Right:
                SeekTo(Progress + KeyboardStep);
                e.Handled = true;
                break;
            case VirtualKey.Home:
                SeekTo(0);
                e.Handled = true;
                break;
        }
    }
}
