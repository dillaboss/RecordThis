using System.Runtime.InteropServices;
using System.Text.Json.Serialization;
using NAudio.Wave;

namespace RecordThis.Models;

public enum SampleEncoding
{
    Pcm = 1,
    IeeeFloat = 3,
}

/// <summary>
/// The raw sample layout written to the spool file. This is the endpoint's WASAPI shared-mode
/// MixFormat captured verbatim; no conversion happens on the real-time capture path.
/// </summary>
public sealed record CaptureFormat(int SampleRate, int Channels, int BitsPerSample, SampleEncoding Encoding, int ChannelMask)
{
    private static readonly Guid SubtypePcm = new("00000001-0000-0010-8000-00aa00389b71");
    private static readonly Guid SubtypeIeeeFloat = new("00000003-0000-0010-8000-00aa00389b71");

    [JsonIgnore] public int BytesPerSample => BitsPerSample / 8;

    [JsonIgnore] public int BlockAlign => Channels * BytesPerSample;

    [JsonIgnore] public string LayoutName => Channels switch
    {
        1 => "mono",
        2 => "stereo",
        6 => "5.1 surround",
        8 => "7.1 surround",
        _ => $"{Channels} channels",
    };

    public string Describe()
    {
        string rate = SampleRate % 1000 == 0 ? $"{SampleRate / 1000} kHz" : $"{SampleRate / 1000.0:0.0} kHz";
        string depth = Encoding == SampleEncoding.IeeeFloat ? $"{BitsPerSample}-bit float" : $"{BitsPerSample}-bit PCM";
        return $"{rate}, {depth}, {LayoutName}";
    }

    /// <summary>
    /// Resolves the true sample encoding. WASAPI mix formats are almost always WAVEFORMATEXTENSIBLE,
    /// whose Encoding tag is "Extensible" — the real type lives in SubFormat.
    /// </summary>
    public static CaptureFormat FromWaveFormat(WaveFormat waveFormat)
    {
        SampleEncoding encoding;
        int channelMask = 0;

        switch (waveFormat.Encoding)
        {
            case WaveFormatEncoding.IeeeFloat:
                encoding = SampleEncoding.IeeeFloat;
                break;
            case WaveFormatEncoding.Pcm:
                encoding = SampleEncoding.Pcm;
                break;
            case WaveFormatEncoding.Extensible:
                (Guid subFormat, channelMask) = ReadExtensibleFields(waveFormat);
                if (subFormat == SubtypeIeeeFloat)
                {
                    encoding = SampleEncoding.IeeeFloat;
                }
                else if (subFormat == SubtypePcm)
                {
                    encoding = SampleEncoding.Pcm;
                }
                else
                {
                    throw new NotSupportedException($"The device uses an unsupported sample sub-format ({subFormat}).");
                }
                break;
            default:
                throw new NotSupportedException($"The device uses an unsupported sample format ({waveFormat.Encoding}).");
        }

        int bits = waveFormat.BitsPerSample;
        bool supported = encoding == SampleEncoding.IeeeFloat ? bits == 32 : bits is 8 or 16 or 24 or 32;
        if (!supported)
        {
            throw new NotSupportedException($"The device uses an unsupported bit depth ({bits}-bit {encoding}).");
        }

        return new CaptureFormat(waveFormat.SampleRate, waveFormat.Channels, bits, encoding, channelMask);
    }

    private static (Guid SubFormat, int ChannelMask) ReadExtensibleFields(WaveFormat waveFormat)
    {
        // WAVEFORMATEXTENSIBLE: WAVEFORMATEX (18 bytes) | wValidBitsPerSample (2) | dwChannelMask (4) | SubFormat (16)
        if (Marshal.SizeOf(waveFormat) < 40)
        {
            return (Guid.Empty, 0);
        }

        nint ptr = WaveFormat.MarshalToPtr(waveFormat);
        try
        {
            short extraSize = Marshal.ReadInt16(ptr, 16);
            if (extraSize < 22)
            {
                return (Guid.Empty, 0);
            }

            int mask = Marshal.ReadInt32(ptr, 20);
            var guid = new byte[16];
            Marshal.Copy(ptr + 24, guid, 0, 16);
            return (new Guid(guid), mask);
        }
        finally
        {
            Marshal.FreeHGlobal(ptr);
        }
    }
}
