using Windows.Storage.Pickers;
using WinRT.Interop;

namespace RecordThis.Services.UI;

public interface IFilePickerService
{
    /// <summary>Shows the Windows save dialog. <paramref name="extension"/> includes the dot, e.g. ".mp3". Returns null if cancelled.</summary>
    Task<string?> PickSavePathAsync(string suggestedFileName, string extension, string fileTypeLabel);
}

public sealed class FilePickerService : IFilePickerService
{
    private readonly Func<nint> _windowHandle;

    public FilePickerService(Func<nint> windowHandle) => _windowHandle = windowHandle;

    public async Task<string?> PickSavePathAsync(string suggestedFileName, string extension, string fileTypeLabel)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.MusicLibrary,
            SuggestedFileName = suggestedFileName,
            DefaultFileExtension = extension,
        };
        picker.FileTypeChoices.Add(fileTypeLabel, new List<string> { extension });

        // Unpackaged WinUI 3 apps must associate pickers with their window handle.
        InitializeWithWindow.Initialize(picker, _windowHandle());

        var file = await picker.PickSaveFileAsync();
        return file?.Path;
    }
}
