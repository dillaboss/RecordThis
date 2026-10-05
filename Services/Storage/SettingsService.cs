using System.Diagnostics;
using System.Text.Json;

namespace RecordThis.Services.Storage;

/// <summary>Choices RecordThis remembers between launches.</summary>
public sealed class AppSettings
{
    public string ExportFormat { get; set; } = "wav";

    public int BitDepth { get; set; } = 24;

    public int Mp3BitRate { get; set; } = 192;

    /// <summary>True once the user's last share action was Copy; makes Copy the highlighted button.</summary>
    public bool PreferCopy { get; set; }

    public string SourceKind { get; set; } = "Output";

    public string? LastOutputDeviceId { get; set; }

    public string? LastInputDeviceId { get; set; }
}

/// <summary>Small JSON file in %LOCALAPPDATA%\RecordThis. Failures are logged and ignored; settings are a convenience.</summary>
public sealed class SettingsService
{
    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = true };
    private readonly string _path;

    public SettingsService()
    {
        string folder = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "RecordThis");
        _path = Path.Combine(folder, "settings.json");
        Current = Load();
    }

    public AppSettings Current { get; }

    public void Save()
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_path)!);
            File.WriteAllText(_path, JsonSerializer.Serialize(Current, JsonOptions));
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            Debug.WriteLine($"Couldn't save settings: {ex.Message}");
        }
    }

    private AppSettings Load()
    {
        try
        {
            if (File.Exists(_path))
            {
                return JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(_path), JsonOptions) ?? new AppSettings();
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            Debug.WriteLine($"Couldn't read settings, using defaults: {ex.Message}");
        }

        return new AppSettings();
    }
}
