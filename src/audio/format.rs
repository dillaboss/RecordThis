use serde::{Deserialize, Serialize};

/// Stored as the WAVE_FORMAT tag number (1 = PCM, 3 = IEEE float), which is also what the C#
/// version wrote, so spools left behind by it can still be recovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleEncoding {
    Pcm,
    IeeeFloat,
}

impl Serialize for SampleEncoding {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(match self {
            SampleEncoding::Pcm => 1,
            SampleEncoding::IeeeFloat => 3,
        })
    }
}

impl<'de> Deserialize<'de> for SampleEncoding {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u8),
            Name(String),
        }
        match Raw::deserialize(d)? {
            Raw::Number(1) => Ok(SampleEncoding::Pcm),
            Raw::Number(3) => Ok(SampleEncoding::IeeeFloat),
            Raw::Name(n) if n.eq_ignore_ascii_case("pcm") => Ok(SampleEncoding::Pcm),
            Raw::Name(n) if n.eq_ignore_ascii_case("ieeefloat") => Ok(SampleEncoding::IeeeFloat),
            _ => Err(serde::de::Error::custom("unknown sample encoding")),
        }
    }
}

/// The raw sample layout written to the spool file: the endpoint's WASAPI shared-mode mix
/// format, captured verbatim. Nothing is converted on the real-time capture path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CaptureFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub encoding: SampleEncoding,
    /// WAVEFORMATEXTENSIBLE dwChannelMask (0 when unknown).
    pub channel_mask: u32,
}

impl CaptureFormat {
    pub fn new(sample_rate: u32, channels: u16, bits_per_sample: u16, encoding: SampleEncoding, channel_mask: u32) -> Result<Self, String> {
        let supported = match encoding {
            SampleEncoding::IeeeFloat => bits_per_sample == 32,
            SampleEncoding::Pcm => matches!(bits_per_sample, 8 | 16 | 24 | 32),
        };
        if !supported {
            return Err(format!("The device uses an unsupported bit depth ({bits_per_sample}-bit {encoding:?})."));
        }
        if channels == 0 || sample_rate == 0 {
            return Err("The device reported an empty audio format.".into());
        }
        Ok(Self { sample_rate, channels, bits_per_sample, encoding, channel_mask })
    }

    pub fn bytes_per_sample(&self) -> usize {
        self.bits_per_sample as usize / 8
    }

    pub fn block_align(&self) -> usize {
        self.channels as usize * self.bytes_per_sample()
    }

    pub fn layout_name(&self) -> String {
        match self.channels {
            1 => "mono".into(),
            2 => "stereo".into(),
            6 => "5.1 surround".into(),
            8 => "7.1 surround".into(),
            n => format!("{n} channels"),
        }
    }

    /// "48 kHz · 32-bit float · stereo"
    pub fn describe(&self) -> String {
        let depth = match self.encoding {
            SampleEncoding::IeeeFloat => format!("{}-bit float", self.bits_per_sample),
            SampleEncoding::Pcm => format!("{}-bit", self.bits_per_sample),
        };
        format!("{} · {} · {}", rate_label(self.sample_rate), depth, self.layout_name())
    }

    /// Bytes of digital silence for one frame. 8-bit PCM is unsigned, so its silence is 0x80.
    pub fn silence_byte(&self) -> u8 {
        if self.encoding == SampleEncoding::Pcm && self.bits_per_sample == 8 {
            0x80
        } else {
            0
        }
    }
}

pub fn rate_label(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_common_formats() {
        let f = CaptureFormat::new(44100, 2, 32, SampleEncoding::IeeeFloat, 3).unwrap();
        assert_eq!(f.describe(), "44.1 kHz · 32-bit float · stereo");
        assert_eq!(f.block_align(), 8);
        let f = CaptureFormat::new(48000, 6, 24, SampleEncoding::Pcm, 0x3f).unwrap();
        assert_eq!(f.describe(), "48 kHz · 24-bit · 5.1 surround");
        assert!(CaptureFormat::new(48000, 2, 64, SampleEncoding::IeeeFloat, 0).is_err());
    }
}
