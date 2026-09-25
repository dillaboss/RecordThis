using Microsoft.UI.Xaml.Controls;
using RecordThis.Models;
using RecordThis.ViewModels;

namespace RecordThis.Views;

public sealed partial class CaptureView : UserControl
{
    public CaptureView(CaptureViewModel viewModel)
    {
        ViewModel = viewModel;
        InitializeComponent();
    }

    public CaptureViewModel ViewModel { get; }

    // SelectedItem is bound OneWay; user picks flow back here (avoids an object→model two-way cast).
    private void OnSourceSelectionChanged(object sender, SelectionChangedEventArgs e) =>
        ViewModel.SelectedDevice = SourceCombo.SelectedItem as AudioDeviceModel;

    private void OnNoticeClosed(InfoBar sender, InfoBarClosedEventArgs args)
    {
        // The InfoBar also closes itself when IsOpen goes false; only clear on a user dismissal.
        if (args.Reason == InfoBarCloseReason.CloseButton)
        {
            ViewModel.Notice = null;
        }
    }
}
