using RecordThis.Models;

namespace RecordThis.Services.Audio.Pipeline;

/// <param name="Peaks">Per-bucket absolute peak of the downmixed signal (may exceed 1.0 after a surround fold-down).</param>
/// <param name="MixedPeak">Highest absolute sample of the downmixed signal; drives the clip-safe export gain.</param>
public sealed record WaveformData(float[] Peaks, float MixedPeak);

/// <summary>
/// One streaming pass over the spool that yields both the review waveform and the post-downmix
/// peak. Runs on the thread pool and allocates only the bucket array plus one read buffer.
/// </summary>
public static class WaveformAnalyzer
{
    public static Task<WaveformData> AnalyzeAsync(SpoolSession session, int buckets, CancellationToken cancellationToken) =>
        Task.Run(() => Analyze(session, buckets, cancellationToken), cancellationToken);

    private static WaveformData Analyze(SpoolSession session, int buckets, CancellationToken cancellationToken)
    {
        using var spool = new SpoolSampleProvider(session);
        var mix = new DownmixSampleProvider(spool, session.Format.ChannelMask);

        var peaks = new float[Math.Max(1, buckets)];
        long totalFrames = spool.TotalFrames;
        if (totalFrames == 0)
        {
            return new WaveformData(peaks, 0f);
        }

        int channels = mix.WaveFormat.Channels;
        var buffer = new float[8192 * channels];
        long frame = 0;
        float mixedPeak = 0f;

        int read;
        while ((read = mix.Read(buffer, 0, buffer.Length)) > 0)
        {
            cancellationToken.ThrowIfCancellationRequested();

            for (int i = 0; i < read; i += channels, frame++)
            {
                float magnitude = 0f;
                for (int c = 0; c < channels; c++)
                {
                    float value = MathF.Abs(buffer[i + c]);
                    if (value > magnitude)
                    {
                        magnitude = value;
                    }
                }

                int bucket = (int)(frame * peaks.Length / totalFrames);
                if (magnitude > peaks[bucket])
                {
                    peaks[bucket] = magnitude;
                }

                if (magnitude > mixedPeak)
                {
                    mixedPeak = magnitude;
                }
            }
        }

        return new WaveformData(peaks, mixedPeak);
    }
}
