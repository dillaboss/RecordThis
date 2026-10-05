using System.Buffers.Binary;
using NAudio.MediaFoundation;
using NAudio.Wave;
using NAudio.Wave.SampleProviders;
using RecordThis.Models;
using RecordThis.Services.Audio.Pipeline;

namespace RecordThis.Services.Audio;

public enum ExportFormat
{
    Wav,
    Mp3,
}

/// <param name="Format">WAV (lossless) or MP3 (compressed).</param>
/// <param name="SampleRate">44100 or 48000.</param>
/// <param name="BitDepth">WAV only: 16 (TPDF dithered) or 24.</param>
/// <param name="Mp3BitRate">MP3 only, in kbps: 128, 192 or 320.</param>
/// <param name="StartFrame">Trim-in point, in source frames.</param>
/// <param name="EndFrame">Trim-out point (exclusive), in source frames; null = end of recording.</param>
public sealed record ExportOptions(
    ExportFormat Format,
    int SampleRate,
    int BitDepth = 24,
    int Mp3BitRate = 192,
    long StartFrame = 0,
    long? EndFrame = null)
{
    public string Extension => Format == ExportFormat.Mp3 ? ".mp3" : ".wav";
}

public sealed record ExportResult(string FilePath, TimeSpan Duration, double GainDb, bool Downmixed, bool Resampled);

/// <summary>This copy of Windows has no MP3 encoder (e.g. an "N" edition without the Media Feature Pack).</summary>
public sealed class Mp3EncoderUnavailableException() : Exception("This PC doesn't have an MP3 encoder installed.");

/// <summary>
/// Finalization stage: spool (trimmed) → ITU-R BS.775 downmix → clip-safe gain → WdlResampler →
/// either 16-bit TPDF-dithered / 24-bit PCM in a RIFF/WAV, or Windows' Media Foundation MP3
/// encoder. Everything streams from disk. WAVs are written to a ".partial" file and renamed when
/// complete, so a failed or cancelled export never leaves a truncated file behind.
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
        Task.Run(
            () => options.Format == ExportFormat.Mp3
                ? ExportMp3(session, mixedPeak, options, destinationPath, progress, cancellationToken)
                : ExportWav(session, mixedPeak, options, destinationPath, progress, cancellationToken),
            cancellationToken);

    /// <summary>Opens the spool at the trim range and builds downmix → gain → resampler.</summary>
    private static Chain BuildChain(SpoolSession session, float mixedPeak, ExportOptions options)
    {
        var spool = new SpoolSampleProvider(session);
        long start = Math.Clamp(options.StartFrame, 0, spool.TotalFrames);
        long end = Math.Clamp(options.EndFrame ?? spool.TotalFrames, start, spool.TotalFrames);
        spool.PositionFrames = start;
        spool.EndLimitFrames = end;

        float gain = ClipSafeGain(mixedPeak);
        var downmix = new DownmixSampleProvider(spool, session.Format.ChannelMask) { Gain = gain };
        var resampler = new ExactLengthResampler(downmix, options.SampleRate, end - start);
        return new Chain(spool, downmix, resampler, gain);
    }

    private static ExportResult ExportWav(
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

        using var chain = BuildChain(session, mixedPeak, options);
        var resampler = chain.Resampler;

        int channels = resampler.WaveFormat.Channels;
        int bytesPerSample = options.BitDepth / 8;
        long dataBytes = resampler.ExpectedFrames * channels * bytesPerSample;
        if (dataBytes > MaxWavDataBytes)
        {
            throw new InvalidOperationException(
                "This recording is too long for a single WAV file (WAV files are limited to 4 GB). " +
                "Try MP3, 16-bit or 44.1 kHz to make the file smaller.");
        }

        string partialPath = destinationPath + ".partial";
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
            DeleteEmptyPlaceholder(destinationPath);
            throw;
        }

        progress?.Report(1.0);
        return chain.ToResult(destinationPath, options.SampleRate);
    }

    private static ExportResult ExportMp3(
        SpoolSession session,
        float mixedPeak,
        ExportOptions options,
        string destinationPath,
        IProgress<double>? progress,
        CancellationToken cancellationToken)
    {
        MediaFoundationApi.Startup();

        using var chain = BuildChain(session, mixedPeak, options);
        var pcm16 = new SampleToWaveProvider16(chain.Resampler);
        int bitRate = options.Mp3BitRate * 1000;

        if (MediaFoundationEncoder.SelectMediaType(AudioSubtypes.MFAudioFormat_MP3, pcm16.WaveFormat, bitRate) is null)
        {
            throw new Mp3EncoderUnavailableException();
        }

        long totalBytes = chain.Resampler.ExpectedFrames * pcm16.WaveFormat.BlockAlign;
        var tracked = new ProgressWaveProvider(pcm16, totalBytes, progress, cancellationToken);

        // Media Foundation picks the container from the file extension, so the temp file must end in .mp3.
        string partialPath = Path.Combine(
            Path.GetDirectoryName(destinationPath) ?? ".",
            $".{Path.GetFileNameWithoutExtension(destinationPath)}.partial.mp3");

        try
        {
            MediaFoundationEncoder.EncodeToMp3(tracked, partialPath, bitRate);
            File.Move(partialPath, destinationPath, overwrite: true);
        }
        catch
        {
            TryDelete(partialPath);
            DeleteEmptyPlaceholder(destinationPath);
            throw;
        }

        progress?.Report(1.0);
        return chain.ToResult(destinationPath, options.SampleRate);
    }

    private static void DeleteEmptyPlaceholder(string destinationPath)
    {
        // FileSavePicker may have created an empty placeholder at the destination.
        try
        {
            var info = new FileInfo(destinationPath);
            if (info.Exists && info.Length == 0)
            {
                info.Delete();
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }
    }

    private static void TryDelete(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }
    }

    private sealed record Chain(
        SpoolSampleProvider Spool,
        DownmixSampleProvider Downmix,
        ExactLengthResampler Resampler,
        float Gain) : IDisposable
    {
        public ExportResult ToResult(string path, int sampleRate) => new(
            path,
            TimeSpan.FromSeconds((double)Resampler.ExpectedFrames / sampleRate),
            GainDb: Gain < 1f ? 20 * Math.Log10(Gain) : 0,
            Downmixed: Downmix.IsDownmixing,
            Resampled: Resampler.IsResampling);

        public void Dispose() => Spool.Dispose();
    }

    /// <summary>Pass-through that reports progress and honours cancellation while the MP3 encoder pulls audio.</summary>
    private sealed class ProgressWaveProvider(
        IWaveProvider source,
        long totalBytes,
        IProgress<double>? progress,
        CancellationToken cancellationToken) : IWaveProvider
    {
        private long _read;
        private int _lastPercent = -1;

        public WaveFormat WaveFormat => source.WaveFormat;

        public int Read(byte[] buffer, int offset, int count)
        {
            cancellationToken.ThrowIfCancellationRequested();
            int read = source.Read(buffer, offset, count);
            _read += read;

            int percent = (int)(_read * 100 / Math.Max(1, totalBytes));
            if (percent != _lastPercent)
            {
                _lastPercent = percent;
                progress?.Report(Math.Min(1.0, percent / 100.0));
            }

            return read;
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
