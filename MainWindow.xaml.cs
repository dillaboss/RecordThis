using System.ComponentModel;
using System.Runtime.InteropServices;
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

        viewModel.PropertyChanged += OnViewModelPropertyChanged;
        RootGrid.Loaded += (_, _) => _dialogs.XamlRoot = RootGrid.XamlRoot;
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

    private void UpdateScreen() =>
        ContentHost.Content = _viewModel.Screen == AppScreen.Review ? _reviewView : _captureView;

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
