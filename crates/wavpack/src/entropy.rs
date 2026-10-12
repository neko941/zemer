use crate::bitstream::BitReader;
use crate::hybrid::HybridState;
use zemer_core::AudioError;

const MEDIAN_DIVISORS: [u64; 3] = [128, 64, 32];

pub(crate) struct EntropyDecoder<'a> {
    bits: BitReader<'a>,
    medians: [[u32; 3]; 2],
    channels: usize,
    holding_zero: bool,
    holding_one: bool,
    zeros_remaining: u32,
    hybrid: Option<HybridState>,
}

impl<'a> EntropyDecoder<'a> {
    pub(crate) fn new(
        bytes: &'a [u8],
        median_logs: [[u16; 3]; 2],
        channels: usize,
        hybrid: Option<HybridState>,
    ) -> Result<Self, AudioError> {
        if !(channels == 1 || channels == 2) {
            return Err(AudioError::Unsupported(
                "WavPack entropy streams must contain one or two channels",
            ));
        }
        let mut medians = [[0; 3]; 2];
        for channel in 0..channels {
            for median in 0..3 {
                let value = exp2s(i32::from(median_logs[channel][median]))?;
                medians[channel][median] = u32::try_from(value)
                    .map_err(|_| AudioError::InvalidData("WavPack median is out of range"))?;
            }
        }
        Ok(Self {
            bits: BitReader::new(bytes),
            medians,
            channels,
            holding_zero: false,
            holding_one: false,
            zeros_remaining: 0,
            hybrid,
        })
    }

    pub(crate) fn read_word(&mut self, channel: usize) -> Result<Option<i32>, AudioError> {
        if channel >= self.channels {
            return Err(AudioError::InvalidData("invalid WavPack entropy channel"));
        }

        if self.medians[0][0] & !1 == 0
            && (self.channels == 1 || self.medians[1][0] & !1 == 0)
            && !self.holding_zero
            && !self.holding_one
        {
            if self.zeros_remaining != 0 {
                self.zeros_remaining -= 1;
                if self.zeros_remaining != 0 {
                    if let Some(hybrid) = self.hybrid.as_mut() {
                        hybrid.record_word(channel, 0);
                    }
                    return Ok(Some(0));
                }
            } else {
                let Some(run) = self.bits.read_zero_run()? else {
                    return Ok(None);
                };
                self.zeros_remaining = run;
                if run != 0 {
                    self.medians = [[0; 3]; 2];
                    if let Some(hybrid) = self.hybrid.as_mut() {
                        hybrid.record_word(channel, 0);
                    }
                    return Ok(Some(0));
                }
            }
        }

        let ones = if self.holding_zero {
            self.holding_zero = false;
            0
        } else {
            let Some(mut value) = read_magnitude_prefix(&mut self.bits)? else {
                return Ok(None);
            };
            if self.holding_one {
                self.holding_one = value & 1 != 0;
                value = (value >> 1) + 1;
            } else {
                self.holding_one = value & 1 != 0;
                value >>= 1;
            }
            self.holding_zero = !self.holding_one;
            value
        };

        if channel == 0
            && let Some(hybrid) = self.hybrid.as_mut()
        {
            hybrid.prepare_frame()?;
        }

        let medians = &mut self.medians[channel];
        let median0 = get_median(medians[0]);
        let (low, width) = match ones {
            0 => {
                let width = median0;
                update_median(&mut medians[0], 0, false)?;
                (0, width)
            }
            1 => {
                let width = get_median(medians[1]);
                update_median(&mut medians[0], 0, true)?;
                update_median(&mut medians[1], 1, false)?;
                (median0, width)
            }
            count => {
                let median1 = get_median(medians[1]);
                let median2 = get_median(medians[2]);
                let low = u64::from(median0)
                    + u64::from(median1)
                    + u64::from(count - 2) * u64::from(median2);
                let width = u64::from(median2);
                update_median(&mut medians[0], 0, true)?;
                update_median(&mut medians[1], 1, true)?;
                update_median(&mut medians[2], 2, count > 2)?;
                (
                    u32::try_from(low)
                        .map_err(|_| AudioError::InvalidData("WavPack word range overflow"))?,
                    u32::try_from(width)
                        .map_err(|_| AudioError::InvalidData("WavPack word range overflow"))?,
                )
            }
        };
        let mut low = low & 0x7fff_ffff;
        let high = low
            .checked_add(width.saturating_sub(1))
            .map(|value| value & 0x7fff_ffff)
            .ok_or(AudioError::InvalidData("WavPack word range overflow"))?;
        let mut high = high.max(low);
        let error_limit = self
            .hybrid
            .as_ref()
            .map_or(0, |hybrid| hybrid.error_limit(channel));
        let magnitude = if error_limit == 0 {
            low.checked_add(self.bits.read_code(high - low)?)
                .ok_or(AudioError::InvalidData("WavPack word overflow"))?
        } else {
            let mut midpoint = (high + low + 1) >> 1;
            while high - low > error_limit {
                if self.bits.read_bit()? {
                    low = midpoint;
                } else {
                    high = midpoint - 1;
                }
                midpoint = (high + low + 1) >> 1;
            }
            midpoint
        };
        let word = if self.bits.read_bit()? {
            !(magnitude as i32)
        } else {
            magnitude as i32
        };
        if let Some(hybrid) = self.hybrid.as_mut() {
            hybrid.record_word(channel, magnitude);
        }
        Ok(Some(word))
    }
}

fn read_magnitude_prefix(bits: &mut BitReader<'_>) -> Result<Option<u32>, AudioError> {
    let mut ones = 0;
    while bits.read_bit()? {
        ones += 1;
        if ones > 16 {
            return Ok(None);
        }
    }
    if ones == 16 {
        let Some(extension) = bits.read_zero_run()? else {
            return Err(AudioError::InvalidData("truncated WavPack magnitude"));
        };
        ones += extension;
    }
    Ok(Some(ones))
}

fn get_median(value: u32) -> u32 {
    (value >> 4) + 1
}

fn update_median(value: &mut u32, index: usize, increment: bool) -> Result<(), AudioError> {
    let divisor = MEDIAN_DIVISORS[index];
    let delta = if increment {
        (u64::from(*value) + divisor) / divisor * 5
    } else {
        (u64::from(*value) + divisor - 2) / divisor * 2
    };
    let next = if increment {
        u64::from(*value) + delta
    } else {
        u64::from(*value).saturating_sub(delta)
    };
    *value = u32::try_from(next).map_err(|_| AudioError::InvalidData("WavPack median overflow"))?;
    Ok(())
}

pub(crate) fn exp2s(log: i32) -> Result<i64, AudioError> {
    let negative = log < 0;
    let magnitude = log.unsigned_abs();
    let exponent = magnitude >> 8;
    let fraction = magnitude & 0xff;
    let mantissa = (256.0 * 2.0_f64.powf(f64::from(fraction) / 256.0)).round() as u64;
    let value = if exponent <= 9 {
        mantissa >> (9 - exponent)
    } else {
        let shift = exponent - 9;
        let factor = 1_u64
            .checked_shl(shift)
            .ok_or(AudioError::InvalidData("WavPack logarithm is out of range"))?;
        mantissa
            .checked_mul(factor)
            .ok_or(AudioError::InvalidData("WavPack logarithm is out of range"))?
    };
    let value = i64::try_from(value)
        .map_err(|_| AudioError::InvalidData("WavPack logarithm is out of range"))?;
    Ok(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_scale_converts_zero_and_integer_powers_of_two() {
        for (log, expected) in [(0, 0), (256, 1), (512, 2), (768, 4), (1024, 8), (-256, -1)] {
            assert_eq!(exp2s(log).unwrap(), expected);
        }
        assert!(exp2s(i32::MAX).is_err());
    }

    #[test]
    fn rejects_invalid_channel_counts_and_indices() {
        assert!(EntropyDecoder::new(&[], [[0; 3]; 2], 0, None).is_err());
        assert!(EntropyDecoder::new(&[], [[0; 3]; 2], 3, None).is_err());
        let mut decoder = EntropyDecoder::new(&[], [[0; 3]; 2], 1, None).unwrap();
        assert!(decoder.read_word(1).is_err());
    }

    #[test]
    fn zero_run_metadata_decodes_silence_without_allocating_samples() {
        let mut decoder = EntropyDecoder::new(&[0xff, 0], [[0; 3]; 2], 1, None).unwrap();
        for _ in 0..128 {
            assert_eq!(decoder.read_word(0).unwrap(), Some(0));
        }
    }

    #[test]
    fn entropy_metadata_logs_initialize_fixed_point_medians() {
        let decoder =
            EntropyDecoder::new(&[], [[0x0f0e, 0x13ce, 0x1170], [0; 3]], 1, None).unwrap();
        assert_eq!(decoder.medians[0][0], 17_024);
        assert_ne!(decoder.medians[0][0], 17_024 * 16);
    }
}
