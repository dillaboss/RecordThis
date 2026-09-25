using Microsoft.UI.Xaml.Controls;
using RecordThis.Models;
using RecordThis.ViewModels;

namespace RecordThis.Views;

public sealed partial class ReviewView : UserControl
{
    public ReviewView(ReviewViewModel viewModel)
    {
        ViewModel = viewModel;
        InitializeComponent();
    }

    public ReviewViewModel ViewModel { get; }

    private void OnSampleRateChanged(object sender, SelectionChangedEventArgs e)
    {
        if (SampleRateCombo.SelectedItem is ExportChoice choice)
        {
            ViewModel.SelectedSampleRate = choice;
        }
    }

    private void OnBitDepthChanged(object sender, SelectionChangedEventArgs e)
    {
        if (BitDepthCombo.SelectedItem is ExportChoice choice)
        {
            ViewModel.SelectedBitDepth = choice;
        }
    }

    private void OnNoticeClosed(InfoBar sender, InfoBarClosedEventArgs args)
    {
        if (args.Reason == InfoBarCloseReason.CloseButton)
        {
            ViewModel.Notice = null;
        }
    }
}
