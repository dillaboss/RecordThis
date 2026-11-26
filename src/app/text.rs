//! Time and size formatting. Times adapt to length: 0:08, 4:12, 1:02:33.

use std::time::Duration;

pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// m:ss.t — for trim points, where tenths matter.
pub fn precise(d: Duration) -> String {
    let tenths = d.as_millis() / 100;
    let (m, s, t) = (tenths / 600, (tenths / 10) % 60, tenths % 10);
    let h = m / 60;
    if h > 0 {
        format!("{h}:{:02}:{s:02}.{t}", m % 60)
    } else {
        format!("{m}:{s:02}.{t}")
    }
}

pub fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", (bytes / 1024).max(1))
    }
}

pub fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(frames as f64 / rate as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_adapt_to_length() {
        assert_eq!(clock(Duration::from_secs(8)), "0:08");
        assert_eq!(clock(Duration::from_secs(252)), "4:12");
        assert_eq!(clock(Duration::from_secs(3753)), "1:02:33");
        assert_eq!(precise(Duration::from_millis(5640)), "0:05.6");
        assert_eq!(precise(Duration::from_millis(61_050)), "1:01.0");
        assert_eq!(size(422_000), "412 KB");
        assert_eq!(size(3_250_000), "3.1 MB");
    }
}
