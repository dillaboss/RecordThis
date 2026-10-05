using System.ComponentModel;
using Microsoft.UI.Xaml;
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

        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ApplyPrimaryAction();
    }

    public ReviewViewModel ViewModel { get; }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ReviewViewModel.PreferCopy))
        {
            ApplyPrimaryAction();
        }
    }

    /// <summary>Whichever of Copy / Save the user reached for last is highlighted as the primary action.</summary>
    private void ApplyPrimaryAction()
    {
        var accent = (Style)Application.Current.Resources["AccentButtonStyle"];
        var standard = (Style)Application.Current.Resources["DefaultButtonStyle"];
        CopyButton.Style = ViewModel.PreferCopy ? accent : standard;
        SaveButton.Style = ViewModel.PreferCopy ? standard : accent;
    }

    // Combo boxes are bound OneWay; user picks flow back here (avoids object→model two-way casts).
    private void OnFormatChanged(object sender, SelectionChangedEventArgs e)
    {
        if (FormatCombo.SelectedItem is ExportChoice choice)
        {
            ViewModel.SelectedFormat = choice;
        }
    }

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

    private void OnBitRateChanged(object sender, SelectionChangedEventArgs e)
    {
        if (BitRateCombo.SelectedItem is ExportChoice choice)
        {
            ViewModel.SelectedBitRate = choice;
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
