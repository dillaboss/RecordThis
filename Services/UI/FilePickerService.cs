using Windows.Storage.Pickers;
using WinRT.Interop;

namespace RecordThis.Services.UI;

public interface IFilePickerService
{
    /// <summary>Shows the Windows save dialog for a .wav file. Returns null if the user cancels.</summary>
    Task<string?> PickSaveWavPathAsync(string suggestedFileName);
}

public sealed class FilePickerService : IFilePickerService
{
    private readonly Func<nint> _windowHandle;

    public FilePickerService(Func<nint> windowHandle) => _windowHandle = windowHandle;

    public async Task<string?> PickSaveWavPathAsync(string suggestedFileName)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.MusicLibrary,
            SuggestedFileName = suggestedFileName,
            DefaultFileExtension = ".wav",
        };
        picker.FileTypeChoices.Add("WAV audio", new List<string> { ".wav" });

        // Unpackaged WinUI 3 apps must associate pickers with their window handle.
        InitializeWithWindow.Initialize(picker, _windowHandle());

        var file = await picker.PickSaveFileAsync();
        return file?.Path;
    }
}
