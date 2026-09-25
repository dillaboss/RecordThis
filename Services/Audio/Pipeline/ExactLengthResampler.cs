using NAudio.Dsp;
using NAudio.Wave;

namespace RecordThis.Services.Audio.Pipeline;

/// <summary>
/// Sample-rate converter built on NAudio's <see cref="WdlResampler"/> (windowed-sinc mode) that
/// produces exactly round(sourceFrames × outRate / inRate) frames. When the source runs dry the
/// resampler is fed zeros to drain its filter tail, so the exported timeline never loses its end
/// and never grows. When the rates already match, samples pass through untouched.
/// </summary>
public sealed class ExactLengthResampler : ISampleProvider
{
    private const int MaxIdleIterations = 8;

    private readonly ISampleProvider _source;
    private readonly WdlResampler? _resampler;
    private readonly int _channels;
    private long _producedFrames;
    private bool _sourceDone;

    public ExactLengthResampler(ISampleProvider source, int outputSampleRate, long sourceFrames)
    {
        _source = source;
        _channels = source.WaveFormat.Channels;
        int inputRate = source.WaveFormat.SampleRate;
        WaveFormat = WaveFormat.CreateIeeeFloatWaveFormat(outputSampleRate, _channels);

        if (inputRate == outputSampleRate)
        {
            ExpectedFrames = sourceFrames;
            return;
        }

        ExpectedFrames = (long)Math.Round(sourceFrames * (double)outputSampleRate / inputRate);
        _resampler = new WdlResampler();
        _resampler.SetMode(true, 0, true, 64, 32); // interpolate, sinc filter: 64 taps, 32× oversampled
        _resampler.SetFilterParms();
        _resampler.SetFeedMode(false);             // output-driven: we ask for N output frames
        _resampler.SetRates(inputRate, outputSampleRate);
    }

    public WaveFormat WaveFormat { get; }

    public long ExpectedFrames { get; }

    public bool IsResampling => _resampler is not null;

    public int Read(float[] buffer, int offset, int count)
    {
        int wanted = (int)Math.Min(count / _channels, ExpectedFrames - _producedFrames);
        if (wanted <= 0)
        {
            return 0;
        }

        int total = 0;
        int idle = 0;
        while (total < wanted)
        {
            int need = wanted - total;
            int outOffset = offset + total * _channels;
            int produced = _resampler is null ? ReadDirect(buffer, outOffset, need) : ReadResampled(buffer, outOffset, need);

            if (produced <= 0)
            {
                if (++idle >= MaxIdleIterations)
                {
                    // Defensive: never stall the export. Pad the remainder with silence.
                    Array.Clear(buffer, outOffset, need * _channels);
                    produced = need;
                }
            }
            else
            {
                idle = 0;
            }

            total += Math.Max(0, produced);
        }

        _producedFrames += total;
        return total * _channels;
    }

    private int ReadDirect(float[] buffer, int offset, int frames)
    {
        int read = _sourceDone ? 0 : ReadFully(buffer, offset, frames * _channels) / _channels;
        if (read < frames)
        {
            _sourceDone = true;
            Array.Clear(buffer, offset + read * _channels, (frames - read) * _channels);
        }

        return frames;
    }

    private int ReadResampled(float[] buffer, int offset, int frames)
    {
        int inNeeded = _resampler!.ResamplePrepare(frames, _channels, out float[] inBuffer, out int inOffset);
        int inRead = 0;
        if (!_sourceDone && inNeeded > 0)
        {
            inRead = ReadFully(inBuffer, inOffset, inNeeded * _channels) / _channels;
            if (inRead < inNeeded)
            {
                _sourceDone = true;
            }
        }

        if (inRead < inNeeded)
        {
            // Zero-feed to flush the sinc filter's delay line at the end of the recording.
            Array.Clear(inBuffer, inOffset + inRead * _channels, (inNeeded - inRead) * _channels);
        }

        return _resampler.ResampleOut(buffer, offset, inNeeded, frames, _channels);
    }

    private int ReadFully(float[] buffer, int offset, int count)
    {
        int total = 0;
        while (total < count)
        {
            int n = _source.Read(buffer, offset + total, count - total);
            if (n <= 0)
            {
                break;
            }

            total += n;
        }

        return total;
    }
}
