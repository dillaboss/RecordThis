using System.Runtime.InteropServices;
using RecordThis.Models;

namespace RecordThis.Services.Audio;

/// <summary>Decodes raw interleaved spool bytes (any supported mix format) into 32-bit float.</summary>
internal static class SampleConverter
{
    public static void ToFloat(ReadOnlySpan<byte> source, Span<float> destination, CaptureFormat format)
    {
        switch (format.Encoding, format.BitsPerSample)
        {
            case (SampleEncoding.IeeeFloat, 32):
                MemoryMarshal.Cast<byte, float>(source).CopyTo(destination);
                break;

            case (SampleEncoding.Pcm, 16):
            {
                var samples = MemoryMarshal.Cast<byte, short>(source);
                for (int i = 0; i < samples.Length; i++)
                {
                    destination[i] = samples[i] / 32768f;
                }
                break;
            }

            case (SampleEncoding.Pcm, 24):
            {
                int count = source.Length / 3;
                for (int i = 0, b = 0; i < count; i++, b += 3)
                {
                    int value = (source[b] << 8 | source[b + 1] << 16 | source[b + 2] << 24) >> 8;
                    destination[i] = value / 8388608f;
                }
                break;
            }

            case (SampleEncoding.Pcm, 32):
            {
                var samples = MemoryMarshal.Cast<byte, int>(source);
                for (int i = 0; i < samples.Length; i++)
                {
                    destination[i] = samples[i] / 2147483648f;
                }
                break;
            }

            case (SampleEncoding.Pcm, 8):
                for (int i = 0; i < source.Length; i++)
                {
                    destination[i] = (source[i] - 128) / 128f;
                }
                break;

            default:
                throw new NotSupportedException($"Unsupported sample format: {format.Describe()}.");
        }
    }

    /// <summary>
    /// Absolute peak (0..1) of a raw capture packet. Works for every mix format, including
    /// WAVEFORMATEXTENSIBLE float streams (the Windows default).
    /// </summary>
    public static float Peak(ReadOnlySpan<byte> source, CaptureFormat format)
    {
        float peak = 0f;
        switch (format.Encoding, format.BitsPerSample)
        {
            case (SampleEncoding.IeeeFloat, 32):
                foreach (float sample in MemoryMarshal.Cast<byte, float>(source))
                {
                    float magnitude = MathF.Abs(sample);
                    if (magnitude > peak)
                    {
                        peak = magnitude; // NaN never passes this comparison
                    }
                }
                break;

            case (SampleEncoding.Pcm, 16):
            {
                int max = 0;
                foreach (short sample in MemoryMarshal.Cast<byte, short>(source))
                {
                    max = Math.Max(max, Math.Abs((int)sample));
                }
                peak = max / 32768f;
                break;
            }

            case (SampleEncoding.Pcm, 24):
            {
                int max = 0;
                for (int b = 0; b + 2 < source.Length; b += 3)
                {
                    int value = (source[b] << 8 | source[b + 1] << 16 | source[b + 2] << 24) >> 8;
                    max = Math.Max(max, Math.Abs(value));
                }
                peak = max / 8388608f;
                break;
            }

            case (SampleEncoding.Pcm, 32):
            {
                long max = 0;
                foreach (int sample in MemoryMarshal.Cast<byte, int>(source))
                {
                    max = Math.Max(max, Math.Abs((long)sample));
                }
                peak = max / 2147483648f;
                break;
            }

            case (SampleEncoding.Pcm, 8):
            {
                int max = 0;
                foreach (byte sample in source)
                {
                    max = Math.Max(max, Math.Abs(sample - 128));
                }
                peak = max / 128f;
                break;
            }
        }

        return MathF.Min(peak, 1f);
    }
}
