use crate::DsdError;

/// Native DSD stream properties. `bit_rate` counts one-bit samples per second
/// for each channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsdSpec {
    bit_rate: u32,
    channels: u16,
    speaker_mask: Option<u32>,
}

impl DsdSpec {
    pub fn new(bit_rate: u32, channels: u16) -> Result<Self, DsdError> {
        Self::new_with_speaker_mask(bit_rate, channels, None)
    }

    pub fn new_with_speaker_mask(
        bit_rate: u32,
        channels: u16,
        speaker_mask: Option<u32>,
    ) -> Result<Self, DsdError> {
        if bit_rate == 0 || channels == 0 {
            return Err(DsdError("DSD bit rate and channel count must be non-zero"));
        }
        Ok(Self {
            bit_rate,
            channels,
            speaker_mask,
        })
    }

    pub fn bit_rate(self) -> u32 {
        self.bit_rate
    }

    pub fn channels(self) -> u16 {
        self.channels
    }

    pub fn speaker_mask(self) -> Option<u32> {
        self.speaker_mask
    }
}
