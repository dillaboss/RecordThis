using NAudio.Wave;
using RecordThis.Models;

namespace RecordThis.Services.Audio.Pipeline;

/// <summary>
/// Streams a raw spool file as 32-bit float samples in the capture's native channel layout.
/// Nothing is held in memory beyond one read buffer, so hour-long recordings preview and export
/// with the same small footprint as a ten-second one.
/// </summary>
public sealed class SpoolSampleProvider : ISampleProvider, IDisposable
{
    private readonly FileStream _stream;
    private readonly CaptureFormat _format;
    private readonly object _gate = new();
    private byte[] _bytes = [];
    private long _positionFrames;
    private bool _disposed;

    public SpoolSampleProvider(SpoolSession session)
    {
        _format = session.Format;
        _stream = new FileStream(
            session.RawPath,
            FileMode.Open,
            FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete,
            bufferSize: 1 << 16,
            FileOptions.SequentialScan);

        // A crash can leave a trailing partial frame; ignore it.
        TotalFrames = _stream.Length / _format.BlockAlign;
        WaveFormat = WaveFormat.CreateIeeeFloatWaveFormat(_format.SampleRate, _format.Channels);
    }

    public WaveFormat WaveFormat { get; }

    public CaptureFormat SourceFormat => _format;

    public long TotalFrames { get; }

    /// <summary>Thread-safe: may be set from the UI thread while the audio thread is reading.</summary>
    public long PositionFrames
    {
        get
        {
            lock (_gate)
            {
                return _positionFrames;
            }
        }
        set
        {
            lock (_gate)
            {
                _positionFrames = Math.Clamp(value, 0, TotalFrames);
            }
        }
    }

    public int Read(float[] buffer, int offset, int count)
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return 0;
            }

            int channels = _format.Channels;
            long frames = Math.Min(count / channels, TotalFrames - _positionFrames);
            if (frames <= 0)
            {
                return 0;
            }

            int byteCount = (int)frames * _format.BlockAlign;
            if (_bytes.Length < byteCount)
            {
                _bytes = new byte[byteCount];
            }

            _stream.Position = _positionFrames * _format.BlockAlign;
            int read = 0;
            while (read < byteCount)
            {
                int n = _stream.Read(_bytes, read, byteCount - read);
                if (n == 0)
                {
                    break;
                }

                read += n;
            }

            int framesRead = read / _format.BlockAlign;
            int samples = framesRead * channels;
            SampleConverter.ToFloat(
                _bytes.AsSpan(0, framesRead * _format.BlockAlign),
                buffer.AsSpan(offset, samples),
                _format);

            _positionFrames += framesRead;
            return samples;
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
            _stream.Dispose();
        }
    }
}
