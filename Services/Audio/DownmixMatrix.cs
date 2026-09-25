using System.Numerics;

namespace RecordThis.Services.Audio;

/// <summary>
/// Builds ITU-R BS.775-style fold-down coefficients from the WAVEFORMATEXTENSIBLE channel mask:
/// Left = L + 0.707·C + 0.707·LFE + 0.707·Ls,  Right = R + 0.707·C + 0.707·LFE + 0.707·Rs.
/// These coefficients can sum above 0 dBFS; clipping is prevented downstream by a peak-aware
/// gain stage (see <see cref="AudioExportService.ClipSafeGain"/>), not by the matrix itself.
/// </summary>
public static class DownmixMatrix
{
    private const float MinusThreeDb = 0.70710678f;

    [Flags]
    private enum Speaker
    {
        None = 0,
        FrontLeft = 0x1,
        FrontRight = 0x2,
        FrontCenter = 0x4,
        LowFrequency = 0x8,
        BackLeft = 0x10,
        BackRight = 0x20,
        FrontLeftOfCenter = 0x40,
        FrontRightOfCenter = 0x80,
        BackCenter = 0x100,
        SideLeft = 0x200,
        SideRight = 0x400,
        TopCenter = 0x800,
        TopFrontLeft = 0x1000,
        TopFrontCenter = 0x2000,
        TopFrontRight = 0x4000,
        TopBackLeft = 0x8000,
        TopBackCenter = 0x10000,
        TopBackRight = 0x20000,
    }

    public static bool RequiresDownmix(int channels) => channels > 2;

    /// <summary>Mono stays mono; everything else becomes stereo.</summary>
    public static int OutputChannels(int inputChannels) => inputChannels == 1 ? 1 : 2;

    /// <summary>Returns matrix[outputChannel][inputChannel].</summary>
    public static float[][] Build(int channels, int channelMask)
    {
        int outputs = OutputChannels(channels);
        var matrix = new float[outputs][];
        for (int o = 0; o < outputs; o++)
        {
            matrix[o] = new float[channels];
        }

        if (channels == 1)
        {
            matrix[0][0] = 1f;
            return matrix;
        }

        Speaker[] positions = ResolvePositions(channels, channelMask);
        for (int c = 0; c < channels; c++)
        {
            (float left, float right) = positions[c] switch
            {
                Speaker.FrontLeft or Speaker.FrontLeftOfCenter => (1f, 0f),
                Speaker.FrontRight or Speaker.FrontRightOfCenter => (0f, 1f),
                Speaker.FrontCenter or Speaker.LowFrequency or Speaker.BackCenter => (MinusThreeDb, MinusThreeDb),
                Speaker.BackLeft or Speaker.SideLeft or Speaker.TopFrontLeft or Speaker.TopBackLeft => (MinusThreeDb, 0f),
                Speaker.BackRight or Speaker.SideRight or Speaker.TopFrontRight or Speaker.TopBackRight => (0f, MinusThreeDb),
                Speaker.TopCenter or Speaker.TopFrontCenter or Speaker.TopBackCenter => (0.5f, 0.5f),
                _ => c % 2 == 0 ? (MinusThreeDb, 0f) : (0f, MinusThreeDb),
            };
            matrix[0][c] = left;
            matrix[1][c] = right;
        }

        return matrix;
    }

    private static Speaker[] ResolvePositions(int channels, int channelMask)
    {
        int mask = BitOperations.PopCount((uint)channelMask) >= channels ? channelMask : DefaultMask(channels);
        var positions = new Speaker[channels];
        int channel = 0;
        for (int bit = 0; bit < 31 && channel < channels; bit++)
        {
            if ((mask & (1 << bit)) != 0)
            {
                positions[channel++] = (Speaker)(1 << bit);
            }
        }

        return positions; // any channels beyond the mask stay None and are split L/R
    }

    private static int DefaultMask(int channels) => channels switch
    {
        1 => 0x4,   // C
        2 => 0x3,   // L R
        3 => 0x7,   // L R C
        4 => 0x33,  // L R Ls Rs
        5 => 0x37,  // L R C Ls Rs
        6 => 0x3F,  // 5.1
        7 => 0x13F, // 6.1
        _ => 0x63F, // 7.1
    };
}
