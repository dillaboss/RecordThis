using System.Diagnostics;
using NAudio.CoreAudioApi;
using NAudio.Wave;
using NAudio.Wave.SampleProviders;
using RecordThis.Models;
using RecordThis.Services.Audio.Pipeline;

namespace RecordThis.Services.Audio;

/// <summary>
/// Review player: streams the spool (downmixed, clip-safe gain) to the default output device.
/// Create and drive it from the UI thread; NAudio then raises PlaybackStopped on that thread too.
/// </summary>
public sealed class ReviewPlaybackService : IDisposable
{
    private SpoolSampleProvider? _spool;
    private DownmixSampleProvider? _mix;
    private IWavePlayer? _output;
    private long _rangeStart;

    /// <summary>Raised when playback ends (reached the end, device error, or output device removed).</summary>
    public event EventHandler<Exception?>? PlaybackStopped;

    public bool IsLoaded => _spool is not null;

    public bool IsPlaying => _output?.PlaybackState == PlaybackState.Playing;

    public TimeSpan Duration => _spool is null ? TimeSpan.Zero : FramesToTime(_spool.TotalFrames);

    public TimeSpan Position => _spool is null ? TimeSpan.Zero : FramesToTime(_spool.PositionFrames);

    public double Progress => _spool is null || _spool.TotalFrames == 0 ? 0 : (double)_spool.PositionFrames / _spool.TotalFrames;

    public long TotalFrames => _spool?.TotalFrames ?? 0;

    /// <summary>Limits playback to the trimmed selection (absolute frames).</summary>
    public void SetRange(long startFrame, long endFrame)
    {
        if (_spool is null)
        {
            return;
        }

        _rangeStart = Math.Clamp(startFrame, 0, _spool.TotalFrames);
        _spool.EndLimitFrames = Math.Clamp(endFrame, _rangeStart, _spool.TotalFrames);
    }

    public void Load(SpoolSession session, float gain)
    {
        Unload();
        _spool = new SpoolSampleProvider(session);
        _rangeStart = 0;
        _mix = new DownmixSampleProvider(_spool, session.Format.ChannelMask) { Gain = gain };
    }

    public void Unload()
    {
        DisposeOutput();
        _spool?.Dispose();
        _spool = null;
        _mix = null;
    }

    public void SetGain(float gain)
    {
        if (_mix is not null)
        {
            _mix.Gain = gain;
        }
    }

    public void Play()
    {
        if (_spool is null || _mix is null)
        {
            return;
        }

        long position = _spool.PositionFrames;
        if (position < _rangeStart || position >= _spool.EndLimitFrames)
        {
            _spool.PositionFrames = _rangeStart; // start (or restart) at the trim-in point
        }

        _output ??= CreateOutput(new SampleToWaveProvider(_mix));
        _output.Play();
    }

    public void Pause() => _output?.Pause();

    public void Seek(double fraction)
    {
        if (_spool is not null)
        {
            _spool.PositionFrames = (long)(Math.Clamp(fraction, 0, 1) * _spool.TotalFrames);
        }
    }

    private IWavePlayer CreateOutput(IWaveProvider provider)
    {
        IWavePlayer output;
        try
        {
            // Shared-mode WASAPI, event-driven, 80 ms latency.
            var wasapi = new WasapiOut(AudioClientShareMode.Shared, true, 80);
            try
            {
                wasapi.Init(provider);
            }
            catch
            {
                wasapi.Dispose();
                throw;
            }

            output = wasapi;
        }
        catch (Exception ex)
        {
            // Some endpoints reject a float stereo stream at a rate they don't mix at.
            // waveOut goes through the audio engine's own converter and accepts anything.
            Debug.WriteLine($"WasapiOut unavailable, falling back to waveOut: {ex.Message}");
            var waveOut = new WaveOutEvent { DesiredLatency = 120 };
            waveOut.Init(provider);
            output = waveOut;
        }

        output.PlaybackStopped += OnOutputStopped;
        return output;
    }

    private void OnOutputStopped(object? sender, StoppedEventArgs e)
    {
        if (!ReferenceEquals(sender, _output))
        {
            return;
        }

        DisposeOutput(); // a fresh output is created on the next Play()
        PlaybackStopped?.Invoke(this, e.Exception);
    }

    private void DisposeOutput()
    {
        if (_output is null)
        {
            return;
        }

        var output = _output;
        _output = null;
        output.PlaybackStopped -= OnOutputStopped;
        try
        {
            output.Stop();
            output.Dispose();
        }
        catch (Exception ex)
        {
            Debug.WriteLine(ex);
        }
    }

    private TimeSpan FramesToTime(long frames) =>
        TimeSpan.FromSeconds((double)frames / _spool!.WaveFormat.SampleRate);

    public void Dispose() => Unload();
}
