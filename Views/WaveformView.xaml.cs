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

/// <summary>
/// Peak-envelope waveform with a playhead and trim handles. Click or drag the body to scrub; drag a
/// bracket to trim. Keyboard: arrows seek 5 %, Home jumps to the start, [ and ] set the trim points
/// at the playhead.
/// </summary>
public sealed partial class WaveformView : UserControl
{
    private const double KeyboardStep = 0.05;
    private const double HandleWidth = 10;
    private const double HandleGrabDistance = 12;
    private const VirtualKey OpenBracketKey = (VirtualKey)219;  // VK_OEM_4
    private const VirtualKey CloseBracketKey = (VirtualKey)221; // VK_OEM_6

    public static readonly DependencyProperty WaveformProperty = DependencyProperty.Register(
        nameof(Waveform), typeof(object), typeof(WaveformView),
        new PropertyMetadata(null, (d, _) => ((WaveformView)d).RedrawEnvelope()));

    public static readonly DependencyProperty ProgressProperty = DependencyProperty.Register(
        nameof(Progress), typeof(double), typeof(WaveformView),
        new PropertyMetadata(0.0, (d, _) => ((WaveformView)d).UpdatePlayhead()));

    public static readonly DependencyProperty SeekCommandProperty = DependencyProperty.Register(
        nameof(SeekCommand), typeof(ICommand), typeof(WaveformView), new PropertyMetadata(null));

    public static readonly DependencyProperty TrimStartProperty = DependencyProperty.Register(
        nameof(TrimStart), typeof(double), typeof(WaveformView),
        new PropertyMetadata(0.0, (d, _) => ((WaveformView)d).UpdateTrimVisuals()));

    public static readonly DependencyProperty TrimEndProperty = DependencyProperty.Register(
        nameof(TrimEnd), typeof(double), typeof(WaveformView),
        new PropertyMetadata(1.0, (d, _) => ((WaveformView)d).UpdateTrimVisuals()));

    public static readonly DependencyProperty MinimumSelectionProperty = DependencyProperty.Register(
        nameof(MinimumSelection), typeof(double), typeof(WaveformView), new PropertyMetadata(0.01));

    private static readonly InputCursor ResizeCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeWestEast);
    private static readonly InputCursor ArrowCursor = InputSystemCursor.Create(InputSystemCursorShape.Arrow);

    private DragMode _drag = DragMode.None;

    public WaveformView()
    {
        InitializeComponent();
        KeyDown += OnKeyDown;
    }

    private enum DragMode
    {
        None,
        Scrub,
        TrimStart,
        TrimEnd,
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

    /// <summary>Trim-in point, 0..1. Bind TwoWay.</summary>
    public double TrimStart
    {
        get => (double)GetValue(TrimStartProperty);
        set => SetValue(TrimStartProperty, value);
    }

    /// <summary>Trim-out point, 0..1. Bind TwoWay.</summary>
    public double TrimEnd
    {
        get => (double)GetValue(TrimEndProperty);
        set => SetValue(TrimEndProperty, value);
    }

    /// <summary>Smallest allowed gap between the handles, 0..1.</summary>
    public double MinimumSelection
    {
        get => (double)GetValue(MinimumSelectionProperty);
        set => SetValue(MinimumSelectionProperty, value);
    }

    private void OnSurfaceSizeChanged(object sender, SizeChangedEventArgs e)
    {
        Surface.Clip = new RectangleGeometry { Rect = new Rect(0, 0, e.NewSize.Width, e.NewSize.Height) };
        CenterLine.X2 = e.NewSize.Width;
        RedrawEnvelope();
        UpdatePlayhead();
        UpdateTrimVisuals();
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
            double half = mid - 4; // room for the handle brackets
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

    private void UpdateTrimVisuals()
    {
        double width = Surface.ActualWidth;
        if (width <= 0)
        {
            return;
        }

        double startX = Math.Clamp(TrimStart, 0, 1) * width;
        double endX = Math.Clamp(TrimEnd, 0, 1) * width;

        LeftShade.Width = startX;
        RightShade.Width = Math.Max(0, width - endX);
        StartHandleTransform.X = Math.Min(startX, Math.Max(0, width - HandleWidth));
        EndHandleTransform.X = Math.Max(0, endX - HandleWidth);
    }

    // ---------------------------------------------------------------- Pointer

    private DragMode HitTest(double x)
    {
        double width = Surface.ActualWidth;
        double toStart = Math.Abs(x - TrimStart * width);
        double toEnd = Math.Abs(x - TrimEnd * width);

        if (toStart <= HandleGrabDistance || toEnd <= HandleGrabDistance)
        {
            return toStart <= toEnd ? DragMode.TrimStart : DragMode.TrimEnd;
        }

        return DragMode.Scrub;
    }

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        Focus(FocusState.Pointer);
        double x = e.GetCurrentPoint(Surface).Position.X;
        _drag = HitTest(x);
        Surface.CapturePointer(e.Pointer);
        ApplyDrag(x);
        e.Handled = true;
    }

    private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        double x = e.GetCurrentPoint(Surface).Position.X;
        if (_drag != DragMode.None)
        {
            ApplyDrag(x);
            e.Handled = true;
            return;
        }

        ProtectedCursor = HitTest(x) == DragMode.Scrub ? ArrowCursor : ResizeCursor;
    }

    private void OnPointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (_drag != DragMode.None)
        {
            Surface.ReleasePointerCapture(e.Pointer);
            _drag = DragMode.None;
            e.Handled = true;
        }
    }

    private void OnPointerCaptureLost(object sender, PointerRoutedEventArgs e) => _drag = DragMode.None;

    private void OnPointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (_drag == DragMode.None)
        {
            ProtectedCursor = ArrowCursor;
        }
    }

    private void ApplyDrag(double x)
    {
        double width = Surface.ActualWidth;
        if (width <= 0)
        {
            return;
        }

        double fraction = Math.Clamp(x / width, 0, 1);
        switch (_drag)
        {
            case DragMode.Scrub:
                SeekTo(fraction);
                break;
            case DragMode.TrimStart:
                SetTrimStart(fraction);
                break;
            case DragMode.TrimEnd:
                SetTrimEnd(fraction);
                break;
        }
    }

    private void SetTrimStart(double fraction) =>
        TrimStart = Math.Clamp(fraction, 0, Math.Max(0, TrimEnd - MinimumSelection));

    private void SetTrimEnd(double fraction) =>
        TrimEnd = Math.Clamp(fraction, Math.Min(1, TrimStart + MinimumSelection), 1);

    private void SeekTo(double fraction)
    {
        fraction = Math.Clamp(fraction, 0, 1);
        if (SeekCommand?.CanExecute(fraction) == true)
        {
            SeekCommand.Execute(fraction);
        }
    }

    // ---------------------------------------------------------------- Keyboard

    private void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case VirtualKey.Left:
                SeekTo(Progress - KeyboardStep);
                break;
            case VirtualKey.Right:
                SeekTo(Progress + KeyboardStep);
                break;
            case VirtualKey.Home:
                SeekTo(TrimStart);
                break;
            case OpenBracketKey:
                SetTrimStart(Progress);
                break;
            case CloseBracketKey:
                SetTrimEnd(Progress);
                break;
            default:
                return;
        }

        e.Handled = true;
    }
}
