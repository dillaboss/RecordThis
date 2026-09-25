using NAudio.Wave;

namespace RecordThis.Services.Audio.Pipeline;

/// <summary>
/// Folds any channel layout down to stereo (mono stays mono) using the ITU-R BS.775 matrix from
/// <see cref="DownmixMatrix"/>, then applies a gain. Stereo and mono sources pass straight through
/// the gain stage.
/// </summary>
public sealed class DownmixSampleProvider : ISampleProvider
{
    private readonly ISampleProvider _source;
    private readonly float[][] _matrix;
    private readonly int _inChannels;
    private readonly int _outChannels;
    private float[] _sourceBuffer = [];
    private float _gain = 1f;

    public DownmixSampleProvider(ISampleProvider source, int channelMask)
    {
        _source = source;
        _inChannels = source.WaveFormat.Channels;
        _outChannels = DownmixMatrix.OutputChannels(_inChannels);
        _matrix = DownmixMatrix.Build(_inChannels, channelMask);
        WaveFormat = WaveFormat.CreateIeeeFloatWaveFormat(source.WaveFormat.SampleRate, _outChannels);
    }

    public WaveFormat WaveFormat { get; }

    public bool IsDownmixing => DownmixMatrix.RequiresDownmix(_inChannels);

    /// <summary>Linear gain applied after the matrix. Safe to change while playing.</summary>
    public float Gain
    {
        get => Volatile.Read(ref _gain);
        set => Volatile.Write(ref _gain, value);
    }

    public int Read(float[] buffer, int offset, int count)
    {
        float gain = Gain;

        if (!IsDownmixing)
        {
            int read = _source.Read(buffer, offset, count - count % _outChannels);
            if (gain != 1f)
            {
                var span = buffer.AsSpan(offset, read);
                for (int i = 0; i < span.Length; i++)
                {
                    span[i] *= gain;
                }
            }

            return read;
        }

        int frames = count / _outChannels;
        int needed = frames * _inChannels;
        if (_sourceBuffer.Length < needed)
        {
            _sourceBuffer = new float[needed];
        }

        int framesRead = _source.Read(_sourceBuffer, 0, needed) / _inChannels;
        float[] left = _matrix[0];
        float[] right = _matrix[1];

        for (int f = 0, i = 0, o = offset; f < framesRead; f++, i += _inChannels, o += 2)
        {
            float l = 0f, r = 0f;
            for (int c = 0; c < _inChannels; c++)
            {
                float s = _sourceBuffer[i + c];
                l += s * left[c];
                r += s * right[c];
            }

            buffer[o] = l * gain;
            buffer[o + 1] = r * gain;
        }

        return framesRead * 2;
    }
}
