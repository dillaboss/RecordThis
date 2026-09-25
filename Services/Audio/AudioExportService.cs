using System.Buffers.Binary;
using NAudio.Wave;
using RecordThis.Models;
using RecordThis.Services.Audio.Pipeline;

namespace RecordThis.Services.Audio;

public sealed record ExportOptions(int SampleRate, int BitDepth);

public sealed record ExportResult(string FilePath, TimeSpan Duration, double GainDb, bool Downmixed, bool Resampled);

/// <summary>
/// Finalization stage: spool → ITU-R BS.775 downmix → clip-safe gain → WdlResampler →
/// 16-bit (TPDF dithered) or 24-bit linear PCM → RIFF/WAV. The whole chain streams from disk,
/// and the WAV is written to a ".partial" file that is only renamed into place once complete,
/// so a failed or cancelled export never leaves a truncated file behind.
/// </summary>
public sealed class AudioExportService
{
    /// <summary>Ceiling used when a downmix would clip: −0.1 dBFS.</summary>
    private const float SafeCeiling = 0.98855f;
    private const int FramesPerChunk = 16384;
    private const long MaxWavDataBytes = uint.MaxValue - 1024L; // RIFF sizes are 32-bit

    /// <summary>
    /// The BS.775 coefficients add up to more than unity, so a loud 5.1/7.1 capture can exceed
    /// 0 dBFS after fold-down. Scale it back just enough to peak at −0.1 dBFS; untouched otherwise.
    /// </summary>
    public static float ClipSafeGain(float peak) => peak > 1f ? SafeCeiling / peak : 1f;

    public Task<ExportResult> ExportAsync(
        SpoolSession session,
        float mixedPeak,
        ExportOptions options,
        string destinationPath,
        IProgress<double>? progress,
        CancellationToken cancellationToken) =>
        Task.Run(() => Export(session, mixedPeak, options, destinationPath, progress, cancellationToken), cancellationToken);

    private static ExportResult Export(
        SpoolSession session,
        float mixedPeak,
        ExportOptions options,
        string destinationPath,
        IProgress<double>? progress,
        CancellationToken cancellationToken)
    {
        if (options.BitDepth is not (16 or 24))
        {
            throw new ArgumentOutOfRangeException(nameof(options), "Bit depth must be 16 or 24.");
        }

        string partialPath = destinationPath + ".partial";
        float gain = ClipSafeGain(mixedPeak);

        using var spool = new SpoolSampleProvider(session);
        var downmix = new DownmixSampleProvider(spool, session.Format.ChannelMask) { Gain = gain };
        var resampler = new ExactLengthResampler(downmix, options.SampleRate, spool.TotalFrames);

        int channels = resampler.WaveFormat.Channels;
        int bytesPerSample = options.BitDepth / 8;
        long dataBytes = resampler.ExpectedFrames * channels * bytesPerSample;
        if (dataBytes > MaxWavDataBytes)
        {
            throw new InvalidOperationException(
                "This recording is too long for a single WAV file (WAV files are limited to 4 GB). " +
                "Try 16-bit or 44.1 kHz to make the file smaller.");
        }

        var outputFormat = new WaveFormat(options.SampleRate, options.BitDepth, channels);
        var encoder = new PcmEncoder(options.BitDepth);
        var floats = new float[FramesPerChunk * channels];
        var bytes = new byte[FramesPerChunk * channels * bytesPerSample];

        try
        {
            using (var writer = new WaveFileWriter(partialPath, outputFormat))
            {
                long written = 0;
                int lastPercent = -1;
                int read;
                while ((read = resampler.Read(floats, 0, floats.Length)) > 0)
                {
                    cancellationToken.ThrowIfCancellationRequested();

                    int byteCount = encoder.Encode(floats.AsSpan(0, read), bytes);
                    writer.Write(bytes, 0, byteCount);

                    written += read / channels;
                    int percent = (int)(written * 100 / Math.Max(1, resampler.ExpectedFrames));
                    if (percent != lastPercent)
                    {
                        lastPercent = percent;
                        progress?.Report(percent / 100.0);
                    }
                }
            }

            File.Move(partialPath, destinationPath, overwrite: true);
        }
        catch
        {
            TryDelete(partialPath);

            // FileSavePicker may have created an empty placeholder at the destination.
            try
            {
                var info = new FileInfo(destinationPath);
                if (info.Exists && info.Length == 0)
                {
                    info.Delete();
                }
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }

            throw;
        }

        progress?.Report(1.0);
        return new ExportResult(
            destinationPath,
            TimeSpan.FromSeconds((double)resampler.ExpectedFrames / options.SampleRate),
            GainDb: gain < 1f ? 20 * Math.Log10(gain) : 0,
            Downmixed: downmix.IsDownmixing,
            Resampled: resampler.IsResampling);
    }

    private static void TryDelete(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }
    }

    /// <summary>Float → little-endian integer PCM. 16-bit output gets TPDF dither; exact digital silence stays silent.</summary>
    private sealed class PcmEncoder(int bitDepth)
    {
        private uint _state = 0x9E3779B9;

        public int Encode(ReadOnlySpan<float> samples, Span<byte> destination)
        {
            if (bitDepth == 16)
            {
                for (int i = 0; i < samples.Length; i++)
                {
                    float s = samples[i];
                    int value = 0;
                    if (s != 0f && !float.IsNaN(s))
                    {
                        // Triangular (TPDF) dither, ±1 LSB, decorrelates quantization error from the signal.
                        double dither = NextUnit() - NextUnit();
                        value = (int)Math.Round(s * 32767.0 + dither);
                        value = Math.Clamp(value, short.MinValue, short.MaxValue);
                    }

                    BinaryPrimitives.WriteInt16LittleEndian(destination.Slice(i * 2, 2), (short)value);
                }

                return samples.Length * 2;
            }

            for (int i = 0, b = 0; i < samples.Length; i++, b += 3)
            {
                float s = samples[i];
                int value = float.IsNaN(s) ? 0 : Math.Clamp((int)Math.Round(s * 8388607.0), -8388608, 8388607);
                destination[b] = (byte)value;
                destination[b + 1] = (byte)(value >> 8);
                destination[b + 2] = (byte)(value >> 16);
            }

            return samples.Length * 3;
        }

        private double NextUnit()
        {
            // xorshift32: fast, allocation-free, plenty for dither noise.
            uint x = _state;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            _state = x;
            return x / 4294967296.0;
        }
    }
}
