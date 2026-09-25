using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace RecordThis.Services.UI;

public enum DialogChoice
{
    Primary,
    Secondary,
    None,
}

public interface IDialogService
{
    Task<DialogChoice> ShowAsync(
        string title,
        string message,
        string? primaryText = null,
        string? secondaryText = null,
        string closeText = "OK");
}

/// <summary>Fluent ContentDialog wrapper. WinUI allows only one open dialog per window, so overlapping requests return None.</summary>
public sealed class DialogService : IDialogService
{
    private bool _isOpen;

    public XamlRoot? XamlRoot { get; set; }

    public async Task<DialogChoice> ShowAsync(
        string title,
        string message,
        string? primaryText = null,
        string? secondaryText = null,
        string closeText = "OK")
    {
        if (XamlRoot is null || _isOpen)
        {
            return DialogChoice.None;
        }

        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Style = (Style)Application.Current.Resources["DefaultContentDialogStyle"],
            Title = title,
            Content = new TextBlock { Text = message, TextWrapping = TextWrapping.WrapWholeWords },
            PrimaryButtonText = primaryText ?? string.Empty,
            SecondaryButtonText = secondaryText ?? string.Empty,
            CloseButtonText = closeText,
            DefaultButton = primaryText is null ? ContentDialogButton.Close : ContentDialogButton.Primary,
        };

        _isOpen = true;
        try
        {
            return await dialog.ShowAsync() switch
            {
                ContentDialogResult.Primary => DialogChoice.Primary,
                ContentDialogResult.Secondary => DialogChoice.Secondary,
                _ => DialogChoice.None,
            };
        }
        finally
        {
            _isOpen = false;
        }
    }
}
