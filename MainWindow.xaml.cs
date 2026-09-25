using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using RecordThis.Services.UI;
using RecordThis.ViewModels;
using RecordThis.Views;
using Windows.Graphics;
using WinRT.Interop;

namespace RecordThis;

public sealed partial class MainWindow : Window
{
    private const int DefaultWidth = 640;
    private const int DefaultHeight = 560;

    private readonly MainViewModel _viewModel;
    private readonly DialogService _dialogs;
    private readonly CaptureView _captureView;
    private readonly ReviewView _reviewView;
    private bool _closeConfirmed;
    private bool _closeInProgress;

    public MainWindow(MainViewModel viewModel, DialogService dialogs)
    {
        _viewModel = viewModel;
        _dialogs = dialogs;
        InitializeComponent();

        // Windows 11 Fluent chrome: Mica Alt backdrop + content extended into the title bar.
        SystemBackdrop = new MicaBackdrop { Kind = MicaKind.BaseAlt };
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;

        SizeAndCenter(DefaultWidth, DefaultHeight);

        _captureView = new CaptureView(viewModel.Capture);
        _reviewView = new ReviewView(viewModel.Review);
        ContentHost.Children.Add(_captureView);
        ContentHost.Children.Add(_reviewView);

        // Minimize / maximize / close glyphs follow the app theme (white in dark mode).
        ApplyCaptionButtonColors();
        RootGrid.ActualThemeChanged += (_, _) => ApplyCaptionButtonColors();

        viewModel.PropertyChanged += OnViewModelPropertyChanged;
        RootGrid.Loaded += (_, _) =>
        {
            _dialogs.XamlRoot = RootGrid.XamlRoot;
            ApplyCaptionButtonColors();
        };
        AppWindow.Closing += OnAppWindowClosing;
        Closed += (_, _) => _viewModel.Dispose();

        UpdateScreen();
        UpdateTitle();
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(MainViewModel.Screen))
        {
            UpdateScreen();
        }

        if (e.PropertyName is nameof(MainViewModel.WindowTitle) or nameof(MainViewModel.Screen))
        {
            UpdateTitle();
        }
    }

    private void UpdateScreen()
    {
        bool review = _viewModel.Screen == AppScreen.Review;
        _reviewView.Visibility = review ? Visibility.Visible : Visibility.Collapsed;
        _captureView.Visibility = review ? Visibility.Collapsed : Visibility.Visible;
    }

    private void ApplyCaptionButtonColors()
    {
        bool dark = RootGrid.ActualTheme == ElementTheme.Dark;
        byte shade = dark ? (byte)0xFF : (byte)0x00;
        var titleBar = AppWindow.TitleBar;

        titleBar.ButtonBackgroundColor = Colors.Transparent;
        titleBar.ButtonInactiveBackgroundColor = Colors.Transparent;
        titleBar.ButtonForegroundColor = ColorHelper.FromArgb(0xFF, shade, shade, shade);
        titleBar.ButtonHoverForegroundColor = ColorHelper.FromArgb(0xFF, shade, shade, shade);
        titleBar.ButtonPressedForegroundColor = ColorHelper.FromArgb(0xFF, shade, shade, shade);
        titleBar.ButtonInactiveForegroundColor = ColorHelper.FromArgb(0x80, shade, shade, shade);
        titleBar.ButtonHoverBackgroundColor = ColorHelper.FromArgb(0x1A, shade, shade, shade);
        titleBar.ButtonPressedBackgroundColor = ColorHelper.FromArgb(0x33, shade, shade, shade);
    }

    private void UpdateTitle()
    {
        TitleText.Text = _viewModel.WindowTitle;
        AppWindow.Title = _viewModel.WindowTitle;
        bool recording = _viewModel.Capture.IsRecording;
        TitleIcon.Glyph = recording ? "\uE7C8" : "\uE720";
        TitleIcon.Foreground = recording
            ? new SolidColorBrush(Microsoft.UI.Colors.Red)
            : (Brush)Application.Current.Resources["AccentTextFillColorPrimaryBrush"];
    }

    /// <summary>
    /// Never lose audio on close: finalize an active recording, then offer to save an unsaved one.
    /// </summary>
    private async void OnAppWindowClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_closeConfirmed)
        {
            return;
        }

        args.Cancel = true;
        if (_closeInProgress)
        {
            return;
        }

        _closeInProgress = true;
        try
        {
            if (_viewModel.Capture.IsRecording)
            {
                await _viewModel.Capture.StopRecordingAsync();
            }

            if (_viewModel.Review.HasUnsavedRecording)
            {
                var choice = await _dialogs.ShowAsync(
                    "Save your recording before closing?",
                    "If you don't save it, the captured audio will be deleted.",
                    primaryText: "Save",
                    secondaryText: "Don't save",
                    closeText: "Cancel");

                switch (choice)
                {
                    case DialogChoice.Primary:
                        await _viewModel.Review.SaveCommand.ExecuteAsync(null);
                        if (_viewModel.Review.HasUnsavedRecording)
                        {
                            return; // Save was cancelled or failed; keep the window open.
                        }
                        break;
                    case DialogChoice.Secondary:
                        _viewModel.Review.CloseSession();
                        break;
                    default:
                        return;
                }
            }

            _closeConfirmed = true;
            Close();
        }
        finally
        {
            _closeInProgress = false;
        }
    }

    private void SizeAndCenter(int width, int height)
    {
        nint hwnd = WindowNative.GetWindowHandle(this);
        double scale = GetDpiForWindow(hwnd) / 96.0;
        var size = new SizeInt32((int)(width * scale), (int)(height * scale));
        AppWindow.Resize(size);

        var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest);
        if (area is not null)
        {
            var work = area.WorkArea;
            AppWindow.Move(new PointInt32(
                work.X + Math.Max(0, (work.Width - size.Width) / 2),
                work.Y + Math.Max(0, (work.Height - size.Height) / 2)));
        }
    }

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hwnd);
}
