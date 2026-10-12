use zemer_core::AudioError;

use crate::entropy::exp2s;

const SLOW_LEVEL_SHIFT: u32 = 8;
const SLOW_LEVEL_OFFSET: i64 = 1 << (SLOW_LEVEL_SHIFT - 1);

pub(crate) struct HybridState {
    bitrate: [u32; 2],
    bitrate_delta: [u32; 2],
    slow_level: [i64; 2],
    slow_level_profile: [i16; 2],
    error_limit: [u32; 2],
    channels: usize,
    bitrate_mode: bool,
    balance: bool,
}

pub(crate) struct ShapingState {
    accumulator: [i32; 2],
    delta: [i32; 2],
    error: [i32; 2],
    channels: usize,
    iir: bool,
}

impl ShapingState {
    pub(crate) fn parse(bytes: &[u8], channels: usize, flags: u32) -> Result<Self, AudioError> {
        if !(channels == 1 || channels == 2)
            || (bytes.len() != channels * 4 && bytes.len() != channels * 6)
        {
            return Err(AudioError::InvalidData(
                "invalid WavPack shaping weights size",
            ));
        }
        let mut accumulator = [0; 2];
        let mut delta = [0; 2];
        let mut error = [0; 2];
        for channel in 0..channels {
            let offset = channel * 4;
            error[channel] = i32::try_from(exp2s(i32::from(i16::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
            ])))?)
            .map_err(|_| AudioError::InvalidData("WavPack shaping error is out of range"))?;
            accumulator[channel] = i32::try_from(exp2s(i32::from(i16::from_le_bytes([
                bytes[offset + 2],
                bytes[offset + 3],
            ])))?)
            .map_err(|_| AudioError::InvalidData("WavPack shaping accumulator is out of range"))?;
        }
        if bytes.len() == channels * 6 {
            for (channel, value) in delta.iter_mut().enumerate().take(channels) {
                let offset = channels * 4 + channel * 2;
                *value = i32::try_from(exp2s(i32::from(i16::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                ])))?)
                .map_err(|_| AudioError::InvalidData("WavPack shaping delta is out of range"))?;
            }
        }
        Ok(Self {
            accumulator,
            delta,
            error,
            channels,
            iir: flags & (1 << 29) != 0,
        })
    }

    pub(crate) fn restore_sample(
        &mut self,
        sample: i32,
        correction: i32,
        channel: usize,
    ) -> Result<i32, AudioError> {
        if channel >= self.channels {
            return Err(AudioError::InvalidData("invalid WavPack shaping channel"));
        }
        self.accumulator[channel] = self.accumulator[channel].wrapping_add(self.delta[channel]);
        let weight = self.accumulator[channel] >> 16;
        let mut filtered = i32::try_from(apply_shaping_weight(weight, self.error[channel]))
            .map_err(|_| AudioError::InvalidData("WavPack shaping filter overflow"))?;
        filtered = filtered
            .checked_neg()
            .ok_or(AudioError::InvalidData("WavPack shaping filter overflow"))?;
        if self.iir && weight < 0 && filtered != 0 {
            if filtered == self.error[channel] {
                filtered = filtered
                    .checked_add(if filtered < 0 { 1 } else { -1 })
                    .ok_or(AudioError::InvalidData("WavPack shaping filter overflow"))?;
            }
            self.error[channel] = filtered
                .checked_sub(correction)
                .ok_or(AudioError::InvalidData("WavPack shaping error overflow"))?;
        } else {
            self.error[channel] = correction
                .checked_neg()
                .ok_or(AudioError::InvalidData("WavPack shaping error overflow"))?;
        }
        i32::try_from(i64::from(sample) - i64::from(filtered))
            .map_err(|_| AudioError::InvalidData("WavPack shaped sample overflow"))
    }
}

fn apply_shaping_weight(weight: i32, sample: i32) -> i64 {
    if sample == i32::from(sample as i16) {
        (i64::from(weight) * i64::from(sample) + 512) >> 10
    } else {
        let low = (i64::from(sample & 0xffff) * i64::from(weight)) >> 9;
        let high = i64::from((sample & !0xffff) >> 9) * i64::from(weight);
        (low + high + 1) >> 1
    }
}

impl HybridState {
    pub(crate) fn for_encoding(
        channels: usize,
        bitrate: u16,
        samples: &[i32],
    ) -> Result<Self, AudioError> {
        if !(channels == 1 || channels == 2) {
            return Err(AudioError::Unsupported(
                "WavPack hybrid channel groups must contain one or two channels",
            ));
        }
        let mut slow_level = [0; 2];
        for (index, &sample) in samples.iter().enumerate() {
            let magnitude = if sample < 0 {
                !(sample as u32)
            } else {
                sample as u32
            };
            let channel = index % channels;
            slow_level[channel] -=
                (slow_level[channel] + SLOW_LEVEL_OFFSET) >> SLOW_LEVEL_SHIFT;
            slow_level[channel] += i64::from(log2(magnitude));
        }
        let mut slow_level_profile = [0; 2];
        for channel in 0..channels {
            slow_level_profile[channel] = if slow_level[channel] > 0 {
                log2(slow_level[channel] as u32) as i16
            } else {
                0
            };
            slow_level[channel] = exp2s(i32::from(slow_level_profile[channel]))?;
        }
        Ok(Self {
            bitrate: [u32::from(bitrate) << 16; 2],
            bitrate_delta: [0; 2],
            slow_level,
            slow_level_profile,
            error_limit: [0; 2],
            channels,
            bitrate_mode: true,
            balance: false,
        })
    }

    pub(crate) fn profile(&self) -> Vec<u8> {
        let mut profile = Vec::with_capacity(self.channels * 4);
        for &encoded in self.slow_level_profile.iter().take(self.channels) {
            profile.extend_from_slice(&encoded.to_le_bytes());
        }
        for &bitrate in self.bitrate.iter().take(self.channels) {
            profile.extend_from_slice(&((bitrate >> 16) as u16).to_le_bytes());
        }
        profile
    }

    pub(crate) fn parse(bytes: &[u8], channels: usize, flags: u32) -> Result<Self, AudioError> {
        let bitrate_mode = flags & (1 << 9) != 0;
        let balance = flags & (1 << 10) != 0;
        let values = channels * if bitrate_mode { 2 } else { 1 };
        let expected = values * 2;
        if bytes.len() != expected && bytes.len() != expected + channels * 2 {
            return Err(AudioError::InvalidData(
                "invalid WavPack hybrid profile size",
            ));
        }

        let mut cursor = 0;
        let mut slow_level = [0; 2];
        let mut slow_level_profile = [0; 2];
        if bitrate_mode {
            for (channel, value) in slow_level.iter_mut().enumerate().take(channels) {
                let log = i16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
                *value = exp2s(i32::from(log))?;
                slow_level_profile[channel] = log;
                cursor += 2;
            }
        }

        let mut bitrate = [0; 2];
        for value in bitrate.iter_mut().take(channels) {
            *value = u32::from(u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]])) << 16;
            cursor += 2;
        }

        let mut bitrate_delta = [0; 2];
        if cursor < bytes.len() {
            for value in bitrate_delta.iter_mut().take(channels) {
                let log = i16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
                *value = exp2s(i32::from(log))? as u32;
                cursor += 2;
            }
        }

        Ok(Self {
            bitrate,
            bitrate_delta,
            slow_level,
            slow_level_profile,
            error_limit: [0; 2],
            channels,
            bitrate_mode,
            balance,
        })
    }

    pub(crate) fn prepare_frame(&mut self) -> Result<(), AudioError> {
        let mut rate = [0_i32; 2];
        for (channel, current) in rate.iter_mut().enumerate().take(self.channels) {
            self.bitrate[channel] = self.bitrate[channel].wrapping_add(self.bitrate_delta[channel]);
            *current = (self.bitrate[channel] >> 16) as i32;
        }

        let mut target = rate;
        if self.bitrate_mode {
            let mut slow_log = [0_i64; 2];
            for (channel, value) in slow_log.iter_mut().enumerate().take(self.channels) {
                *value = (self.slow_level[channel] + SLOW_LEVEL_OFFSET) >> SLOW_LEVEL_SHIFT;
            }
            if self.channels == 2 && self.balance {
                let balance = (slow_log[1] - slow_log[0] + i64::from(rate[1]) + 1) >> 1;
                if balance > i64::from(rate[0]) {
                    target = [0, rate[0].saturating_mul(2)];
                } else if -balance > i64::from(rate[0]) {
                    target = [rate[0].saturating_mul(2), 0];
                } else {
                    target[0] = (i64::from(rate[0]) - balance) as i32;
                    target[1] = (i64::from(rate[0]) + balance) as i32;
                }
            }
            for (channel, (&slow, &target)) in
                slow_log.iter().zip(&target).enumerate().take(self.channels)
            {
                let delta = slow - i64::from(target);
                self.error_limit[channel] = if delta > -0x100 {
                    u32::try_from(exp2s((delta + 0x100) as i32)?).map_err(|_| {
                        AudioError::InvalidData("WavPack hybrid error limit overflow")
                    })?
                } else {
                    0
                };
            }
        } else {
            for (channel, &rate) in rate.iter().enumerate().take(self.channels) {
                self.error_limit[channel] = u32::try_from(exp2s(rate)?)
                    .map_err(|_| AudioError::InvalidData("WavPack hybrid error limit overflow"))?;
            }
        }
        Ok(())
    }

    pub(crate) fn error_limit(&self, channel: usize) -> u32 {
        self.error_limit[channel]
    }

    pub(crate) fn record_word(&mut self, channel: usize, magnitude: u32) {
        if self.bitrate_mode {
            let level = log2(magnitude);
            self.slow_level[channel] -=
                (self.slow_level[channel] + SLOW_LEVEL_OFFSET) >> SLOW_LEVEL_SHIFT;
            self.slow_level[channel] += i64::from(level);
        }
    }
}

fn log2(value: u32) -> u32 {
    if value == 0 {
        0
    } else {
        let adjusted = value.saturating_add(value >> 9);
        let exponent = u32::BITS - adjusted.leading_zeros();
        let normalized = if exponent <= 9 {
            adjusted << (9 - exponent)
        } else {
            adjusted >> (exponent - 9)
        };
        let fraction = (f64::from(256 + (normalized & 0xff)) / 256.0).log2() * 256.0;
        exponent * 256 + fraction.round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stereo_bitrate_profile_and_computes_error_bounds() {
        let profile = [0x00, 0x08, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00];
        let mut state = HybridState::parse(&profile, 2, 1 << 9).unwrap();
        state.prepare_frame().unwrap();
        assert!(state.error_limit(0) > 0);
        assert!(state.error_limit(1) > 0);
    }

    #[test]
    fn rejects_truncated_or_extra_hybrid_profile_data() {
        assert!(HybridState::parse(&[0; 7], 2, 1 << 9).is_err());
        assert!(HybridState::parse(&[0; 10], 2, 1 << 9).is_err());
    }

    #[test]
    fn validates_shaping_weight_metadata_lengths() {
        assert!(ShapingState::parse(&[0; 8], 2, 0).is_ok());
        assert!(ShapingState::parse(&[0; 12], 2, 0).is_ok());
        assert!(ShapingState::parse(&[0; 7], 2, 0).is_err());
        assert!(ShapingState::parse(&[0; 6], 2, 0).is_err());
    }

    #[test]
    fn iir_shaping_applies_the_adjusted_filter_to_the_output_sample() {
        let weights = [150, 2, 0, 230, 0, 255, 0, 230, 207, 15, 207, 15];
        let mut shaping = ShapingState::parse(&weights, 2, 1 << 29).unwrap();

        assert_eq!(shaping.restore_sample(80, -2, 1).unwrap(), 80);
    }

    #[test]
    fn fixed_point_log2_keeps_only_the_fractional_table_component() {
        for (value, expected) in [(0, 0), (1, 256), (2, 512), (3, 662), (256, 2304)] {
            assert_eq!(log2(value), expected);
        }
    }
}
