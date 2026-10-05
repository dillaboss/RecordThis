namespace RecordThis.ViewModels;

internal static class TimeFormat
{
    /// <summary>hh:mm:ss, matching the spec's "Stop Recording (00:04:12)".</summary>
    public static string Clock(TimeSpan time)
    {
        if (time < TimeSpan.Zero)
        {
            time = TimeSpan.Zero;
        }

        return $"{(int)time.TotalHours:00}:{time.Minutes:00}:{time.Seconds:00}";
    }

    /// <summary>m:ss.t, for trim selections where tenths of a second matter.</summary>
    public static string Precise(TimeSpan time)
    {
        if (time < TimeSpan.Zero)
        {
            time = TimeSpan.Zero;
        }

        return $"{(int)time.TotalMinutes}:{time.Seconds:00}.{time.Milliseconds / 100}";
    }

    public static string Friendly(TimeSpan time) =>
        time.TotalMinutes >= 1 ? $"{(int)time.TotalMinutes} min {time.Seconds} s" : $"{time.TotalSeconds:0.#} s";
}
