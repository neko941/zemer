#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioSpec {
    sample_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
    speaker_mask: Option<u32>,
}

impl AudioSpec {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        sample_format: SampleFormat,
    ) -> Result<Self, PcmError> {
        Self::new_with_speaker_mask(sample_rate, channels, sample_format, None)
    }

    pub fn new_with_speaker_mask(
        sample_rate: u32,
        channels: u16,
        sample_format: SampleFormat,
        speaker_mask: Option<u32>,
    ) -> Result<Self, PcmError> {
        if sample_rate == 0 || channels == 0 {
            return Err(PcmError(
                "sample rate and channel count must be non-zero",
            ));
        }
        Ok(Self {
            sample_rate,
            channels,
            sample_format,
            speaker_mask,
        })
    }

    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    pub fn channels(self) -> u16 {
        self.channels
    }
    pub fn sample_format(self) -> SampleFormat {
        self.sample_format
    }
    pub fn speaker_mask(self) -> Option<u32> {
        self.speaker_mask
    }
}

