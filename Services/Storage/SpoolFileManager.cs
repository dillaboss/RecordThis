using System.Diagnostics;
using System.Text.Json;
using RecordThis.Models;

namespace RecordThis.Services.Storage;

/// <summary>
/// Owns the scratch files in %LOCALAPPDATA%\Temp: RecordThis_{Guid}.raw (raw PCM in the
/// device's mix format) plus a small .json sidecar with the format. Together they are enough to
/// rebuild a RIFF header after a crash, power loss or forced shutdown.
/// </summary>
public sealed class SpoolFileManager
{
    public const string FilePrefix = "RecordThis_";
    private const int MetadataVersion = 1;

    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = true };

    public SpoolFileManager(string? directory = null)
    {
        SpoolDirectory = directory ?? Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Temp");
        Directory.CreateDirectory(SpoolDirectory);
    }

    public string SpoolDirectory { get; }

    /// <summary>Writes the metadata sidecar first, so any raw file on disk is always interpretable.</summary>
    public SpoolSession CreateSession(CaptureFormat format, string deviceName)
    {
        var id = Guid.NewGuid();
        string rawPath = Path.Combine(SpoolDirectory, $"{FilePrefix}{id:N}.raw");
        var session = new SpoolSession
        {
            Id = id,
            RawPath = rawPath,
            MetadataPath = Path.ChangeExtension(rawPath, ".json"),
            Format = format,
            DeviceName = deviceName,
            StartedAt = DateTimeOffset.Now,
        };

        var metadata = new SpoolMetadata(MetadataVersion, id, deviceName, session.StartedAt, format);
        File.WriteAllText(session.MetadataPath, JsonSerializer.Serialize(metadata, JsonOptions));
        return session;
    }

    /// <summary>
    /// Opens the spool for the capture thread. bufferSize 0 disables FileStream's managed buffer so
    /// every packet reaches the OS immediately (the spec's "unbuffered" scratch file); the exclusive
    /// share mode also marks the file as in use for other instances' orphan scans.
    /// </summary>
    public FileStream OpenForWriting(SpoolSession session) =>
        new(session.RawPath, FileMode.CreateNew, FileAccess.Write, FileShare.None, bufferSize: 0);

    /// <summary>
    /// Keeps a spool marked as "in use" while it is being reviewed, so a second running instance
    /// won't offer it for recovery. Readers opened with FileShare.ReadWrite | Delete still work.
    /// </summary>
    public IDisposable? Hold(SpoolSession session)
    {
        try
        {
            return new FileStream(session.RawPath, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            Debug.WriteLine($"Couldn't hold spool {session.RawPath}: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Spools left behind by a previous run that crashed or lost power, newest first.
    /// Files that are still open (another instance is using them) are skipped; empty ones are deleted.
    /// </summary>
    public IReadOnlyList<SpoolSession> FindOrphans()
    {
        var sessions = new List<SpoolSession>();
        IEnumerable<string> sidecars;
        try
        {
            sidecars = Directory.EnumerateFiles(SpoolDirectory, FilePrefix + "*.json");
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return sessions;
        }

        foreach (string metadataPath in sidecars)
        {
            SpoolSession? session = TryReadSession(metadataPath);
            if (session is null)
            {
                continue;
            }

            if (!File.Exists(session.RawPath))
            {
                TryDeleteFile(metadataPath);
                continue;
            }

            if (IsInUse(session.RawPath))
            {
                continue;
            }

            if (session.FrameCount == 0)
            {
                Delete(session);
                continue;
            }

            sessions.Add(session);
        }

        return sessions.OrderByDescending(s => s.StartedAt).ToList();
    }

    public void Delete(SpoolSession session)
    {
        TryDeleteFile(session.RawPath);
        TryDeleteFile(session.MetadataPath);
    }

    private static SpoolSession? TryReadSession(string metadataPath)
    {
        try
        {
            var metadata = JsonSerializer.Deserialize<SpoolMetadata>(File.ReadAllText(metadataPath), JsonOptions);
            if (metadata is null || metadata.Format.BlockAlign <= 0 || metadata.Format.SampleRate <= 0)
            {
                return null;
            }

            return new SpoolSession
            {
                Id = metadata.Id,
                RawPath = Path.ChangeExtension(metadataPath, ".raw"),
                MetadataPath = metadataPath,
                Format = metadata.Format,
                DeviceName = metadata.DeviceName,
                StartedAt = metadata.StartedAt,
            };
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException or NotSupportedException)
        {
            Debug.WriteLine($"Ignoring unreadable spool metadata {metadataPath}: {ex.Message}");
            return null;
        }
    }

    private static bool IsInUse(string path)
    {
        try
        {
            using var probe = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.None);
            return false;
        }
        catch (IOException)
        {
            return true;
        }
        catch (UnauthorizedAccessException)
        {
            return true;
        }
    }

    private static void TryDeleteFile(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            Debug.WriteLine($"Couldn't delete {path}: {ex.Message}");
        }
    }
}
