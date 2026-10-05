using RecordThis.Models;

namespace RecordThis.Services.Audio.Pipeline;

/// <param name="Peaks">Per-bucket absolute peak of the downmixed signal (may exceed 1.0 after a surround fold-down).</param>
/// <param name="MixedPeak">Highest absolute sample of the downmixed signal; drives the clip-safe export gain.</param>
/// <param name="TotalFrames">Length of the recording in frames.</param>
/// <param name="FirstSoundFrame">First frame above the silence threshold, or -1 if the recording is silent.</param>
/// <param name="LastSoundFrame">Last frame above the silence threshold, or -1 if the recording is silent.</param>
public sealed record WaveformData(float[] Peaks, float MixedPeak, long TotalFrames, long FirstSoundFrame, long LastSoundFrame)
{
    public bool HasSound => FirstSoundFrame >= 0;
}

/// <summary>
/// One streaming pass over the spool that yields the review waveform, the post-downmix peak, and
/// where sound starts and ends (for "Trim silence"). Runs on the thread pool and allocates only
/// the bucket array plus one read buffer.
/// </summary>
public static class WaveformAnalyzer
{
    /// <summary>−50 dBFS: quieter than this counts as dead air.</summary>
    private const float SilenceThreshold = 0.00316f;

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
            return new WaveformData(peaks, 0f, 0, -1, -1);
        }

        int channels = mix.WaveFormat.Channels;
        var buffer = new float[8192 * channels];
        long frame = 0;
        long firstSound = -1;
        long lastSound = -1;
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

                if (magnitude > SilenceThreshold)
                {
                    if (firstSound < 0)
                    {
                        firstSound = frame;
                    }

                    lastSound = frame;
                }
            }
        }

        return new WaveformData(peaks, mixedPeak, totalFrames, firstSound, lastSound);
    }
}
