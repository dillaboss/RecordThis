namespace RecordThis.Models;

/// <summary>A raw PCM scratch file plus the metadata needed to rebuild a RIFF header from it.</summary>
public sealed class SpoolSession
{
    public required Guid Id { get; init; }

    public required string RawPath { get; init; }

    public required string MetadataPath { get; init; }

    public required CaptureFormat Format { get; init; }

    public required string DeviceName { get; init; }

    public required DateTimeOffset StartedAt { get; init; }

    /// <summary>Whole frames on disk (a crash can leave a trailing partial frame, which is ignored).</summary>
    public long FrameCount
    {
        get
        {
            try
            {
                var info = new FileInfo(RawPath);
                return info.Exists ? info.Length / Format.BlockAlign : 0;
            }
            catch (IOException)
            {
                return 0;
            }
        }
    }

    public TimeSpan Duration => TimeSpan.FromSeconds((double)FrameCount / Format.SampleRate);
}

/// <summary>Sidecar JSON written next to every spool file.</summary>
public sealed record SpoolMetadata(int Version, Guid Id, string DeviceName, DateTimeOffset StartedAt, CaptureFormat Format);
