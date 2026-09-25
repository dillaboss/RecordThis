using RecordThis.Models;

namespace RecordThis.Services.Audio;

/// <summary>
/// Generates digital-silence frames in the exact format of the active endpoint so the watchdog
/// can pad the spool during WASAPI loopback gaps. A single zeroed chunk is reused, so injecting
/// minutes of silence allocates nothing.
/// </summary>
public sealed class SilenceInjectionBuffer
{
    private readonly byte[] _chunk;
    private readonly int _blockAlign;

    public SilenceInjectionBuffer(CaptureFormat format, int chunkMilliseconds = 100)
    {
        _blockAlign = format.BlockAlign;
        int frames = Math.Max(1, format.SampleRate * chunkMilliseconds / 1000);
        _chunk = new byte[frames * _blockAlign];

        // 8-bit PCM is unsigned: silence is 0x80, not 0x00.
        if (format.Encoding == SampleEncoding.Pcm && format.BitsPerSample == 8)
        {
            Array.Fill(_chunk, (byte)0x80);
        }
    }

    public void Write(Stream destination, long frames)
    {
        long remaining = frames * _blockAlign;
        while (remaining > 0)
        {
            int count = (int)Math.Min(remaining, _chunk.Length);
            destination.Write(_chunk, 0, count);
            remaining -= count;
        }
    }
}
