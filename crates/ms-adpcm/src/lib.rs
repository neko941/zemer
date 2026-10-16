use zemer_core::AudioError;

const ADAPTATION: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coefficients {
    pub first: i16,
    pub second: i16,
}

pub const STANDARD_COEFFICIENTS: [Coefficients; 7] = [
    Coefficients {
        first: 256,
        second: 0,
    },
    Coefficients {
        first: 512,
        second: -256,
    },
    Coefficients {
        first: 0,
        second: 0,
    },
    Coefficients {
        first: 192,
        second: 64,
    },
    Coefficients {
        first: 240,
        second: 0,
    },
    Coefficients {
        first: 460,
        second: -208,
    },
    Coefficients {
        first: 392,
        second: -232,
    },
];

pub fn samples_per_block(block_align: u16, channels: u16) -> Result<u16, AudioError> {
    if !(1..=2).contains(&channels) {
        return Err(AudioError::Unsupported(
            "Microsoft ADPCM supports mono or stereo",
        ));
    }
    let header = 7_u16.checked_mul(channels).ok_or(AudioError::InvalidData(
        "Microsoft ADPCM header size overflow",
    ))?;
    let payload = block_align
        .checked_sub(header)
        .ok_or(AudioError::InvalidData(
            "Microsoft ADPCM block is smaller than its header",
        ))?;
    if payload == 0 {
        return Err(AudioError::InvalidData(
            "Microsoft ADPCM block has no sample payload",
        ));
    }
    let samples = payload
        .checked_mul(2)
        .and_then(|codes| codes.checked_div(channels))
        .and_then(|codes| codes.checked_add(2))
        .ok_or(AudioError::InvalidData(
            "Microsoft ADPCM samples per block overflow",
        ))?;
    Ok(samples)
}

pub fn frames_in_block(
    data_bytes: u64,
    channels: u16,
    block_align: u16,
) -> Result<u64, AudioError> {
    if !(1..=2).contains(&channels) {
        return Err(AudioError::Unsupported(
            "Microsoft ADPCM supports mono or stereo",
        ));
    }
    let header = u64::from(channels) * 7;
    let align = u64::from(block_align);
    if align <= header {
        return Err(AudioError::InvalidData(
            "Microsoft ADPCM block is smaller than its header",
        ));
    }
    let full_blocks = data_bytes / align;
    let tail = data_bytes % align;
    if tail != 0 && tail < header {
        return Err(AudioError::InvalidData(
            "truncated Microsoft ADPCM block header",
        ));
    }
    let full_frames = u64::from(samples_per_block(block_align, channels)?);
    let tail_frames = if tail == 0 {
        0
    } else {
        2 + (tail - header) * 2 / u64::from(channels)
    };
    full_blocks
        .checked_mul(full_frames)
        .and_then(|frames| frames.checked_add(tail_frames))
        .ok_or(AudioError::InvalidData(
            "Microsoft ADPCM sample count overflow",
        ))
}

pub fn decode_block(
    bytes: &[u8],
    channels: u16,
    coefficients: &[Coefficients],
) -> Result<Vec<i16>, AudioError> {
    validate_coefficients(coefficients)?;
    let channels = usize::from(channels);
    if !(1..=2).contains(&channels) {
        return Err(AudioError::Unsupported(
            "Microsoft ADPCM supports mono or stereo",
        ));
    }
    let header_size = channels * 7;
    if bytes.len() < header_size {
        return Err(AudioError::InvalidFormat(
            "truncated Microsoft ADPCM block header",
        ));
    }
    let mut states = Vec::new();
    states
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("Microsoft ADPCM block is too large"))?;
    for channel in 0..channels {
        let predictor = usize::from(bytes[channel]);
        let delta_offset = channels + channel * 2;
        let sample1_offset = channels * 3 + channel * 2;
        let sample2_offset = channels * 5 + channel * 2;
        let delta = i32::from(i16::from_le_bytes(
            bytes[delta_offset..delta_offset + 2].try_into().unwrap(),
        ));
        if predictor >= coefficients.len() || delta < 16 {
            return Err(AudioError::InvalidFormat(
                "invalid Microsoft ADPCM predictor or initial delta",
            ));
        }
        states.push(State {
            coefficients: coefficients[predictor],
            delta,
            sample1: i32::from(i16::from_le_bytes(
                bytes[sample1_offset..sample1_offset + 2]
                    .try_into()
                    .unwrap(),
            )),
            sample2: i32::from(i16::from_le_bytes(
                bytes[sample2_offset..sample2_offset + 2]
                    .try_into()
                    .unwrap(),
            )),
        });
    }
    let payload = &bytes[header_size..];
    let frames = if channels == 1 {
        payload.len().checked_mul(2).ok_or(AudioError::InvalidData(
            "Microsoft ADPCM block is too large",
        ))?
    } else {
        payload.len()
    };
    let sample_count = frames
        .checked_add(2)
        .and_then(|frames| frames.checked_mul(channels))
        .ok_or(AudioError::InvalidData(
            "Microsoft ADPCM block is too large",
        ))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(sample_count)
        .map_err(|_| AudioError::InvalidData("Microsoft ADPCM block is too large"))?;
    for state in &states {
        output.push(state.sample2 as i16);
    }
    for state in &states {
        output.push(state.sample1 as i16);
    }
    if channels == 1 {
        for byte in payload {
            output.push(states[0].decode(byte >> 4));
            output.push(states[0].decode(byte & 0x0f));
        }
    } else {
        for byte in payload {
            output.push(states[0].decode(byte >> 4));
            output.push(states[1].decode(byte & 0x0f));
        }
    }
    Ok(output)
}

pub fn encode_block(
    samples: &[i16],
    channels: u16,
    block_align: u16,
) -> Result<Vec<u8>, AudioError> {
    let frames_per_block = usize::from(samples_per_block(block_align, channels)?);
    let channels = usize::from(channels);
    if samples.len() != frames_per_block * channels {
        return Err(AudioError::InvalidData(
            "Microsoft ADPCM encoder requires exactly one complete block",
        ));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(usize::from(block_align))
        .map_err(|_| AudioError::InvalidData("Microsoft ADPCM block is too large"))?;
    let mut states = Vec::new();
    states
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("Microsoft ADPCM block is too large"))?;
    let mut predictors = Vec::new();
    predictors
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("Microsoft ADPCM block is too large"))?;

    let mut initial_deltas = Vec::new();
    initial_deltas
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("Microsoft ADPCM block is too large"))?;
    for channel in 0..channels {
        let (predictor, delta, state) = choose_parameters(samples, channels, channel);
        predictors.push(predictor as u8);
        initial_deltas.push(delta);
        states.push(state);
    }
    output.extend_from_slice(&predictors);
    for delta in initial_deltas {
        output.extend_from_slice(&delta.to_le_bytes());
    }
    for frame in [1, 0] {
        for channel in 0..channels {
            output.extend_from_slice(&samples[frame * channels + channel].to_le_bytes());
        }
    }

    if channels == 1 {
        for (frame, &sample) in samples.iter().enumerate().skip(2) {
            let code = states[0].encode(sample);
            if frame & 1 == 0 {
                output.push(code << 4);
            } else {
                *output.last_mut().unwrap() |= code;
            }
        }
    } else {
        for frame in samples.chunks_exact(2).skip(2) {
            let left = states[0].encode(frame[0]);
            let right = states[1].encode(frame[1]);
            output.push((left << 4) | right);
        }
    }
    output.resize(usize::from(block_align), 0);
    Ok(output)
}

#[derive(Clone, Copy)]
struct State {
    coefficients: Coefficients,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl State {
    fn decode(&mut self, code: u8) -> i16 {
        let signed_code = if code & 8 == 0 {
            i32::from(code)
        } else {
            i32::from(code) - 16
        };
        let prediction = (i64::from(self.sample1) * i64::from(self.coefficients.first)
            + i64::from(self.sample2) * i64::from(self.coefficients.second))
            / 256;
        let sample = (prediction + i64::from(signed_code) * i64::from(self.delta))
            .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i32;
        self.sample2 = self.sample1;
        self.sample1 = sample;
        self.delta = (i64::from(self.delta) * i64::from(ADAPTATION[usize::from(code)]) / 256)
            .clamp(16, i64::from(i32::MAX)) as i32;
        sample as i16
    }

    fn encode(&mut self, target: i16) -> u8 {
        let prediction = (i64::from(self.sample1) * i64::from(self.coefficients.first)
            + i64::from(self.sample2) * i64::from(self.coefficients.second))
            / 256;
        let error = i64::from(target) - prediction;
        let magnitude = (error.unsigned_abs() + self.delta as u64 / 2) / self.delta as u64;
        let signed_code = if error < 0 {
            -(magnitude.min(8) as i32)
        } else {
            magnitude.min(7) as i32
        };
        let code = (signed_code as u8) & 0x0f;
        let sample = (prediction + i64::from(signed_code) * i64::from(self.delta))
            .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i32;
        self.sample2 = self.sample1;
        self.sample1 = sample;
        self.delta = (i64::from(self.delta) * i64::from(ADAPTATION[usize::from(code)]) / 256)
            .clamp(16, i64::from(i32::MAX)) as i32;
        code
    }
}

fn choose_parameters(samples: &[i16], channels: usize, channel: usize) -> (usize, i16, State) {
    let sample = |frame: usize| samples[frame * channels + channel];
    let mut best: Option<(u64, usize, i16, Coefficients)> = None;
    for (predictor, coefficients) in STANDARD_COEFFICIENTS.iter().copied().enumerate() {
        let initial_prediction = (i64::from(sample(1)) * i64::from(coefficients.first)
            + i64::from(sample(0)) * i64::from(coefficients.second))
            / 256;
        let first_error = (i64::from(sample(2)) - initial_prediction).unsigned_abs();
        let initial_delta = ((first_error + 3) / 4).clamp(16, i16::MAX as u64) as i16;
        let mut state = State {
            coefficients,
            delta: i32::from(initial_delta),
            sample1: i32::from(sample(1)),
            sample2: i32::from(sample(0)),
        };
        let mut cost = 0_u64;
        for frame in 2..samples.len() / channels {
            let target = sample(frame);
            state.encode(target);
            let difference = i64::from(target) - i64::from(state.sample1);
            cost = cost.saturating_add((difference * difference) as u64);
        }
        if best
            .as_ref()
            .is_none_or(|(best_cost, ..)| cost < *best_cost)
        {
            best = Some((cost, predictor, initial_delta, coefficients));
        }
    }
    let (_, predictor, delta, coefficients) = best.expect("standard coefficient table is nonempty");
    (
        predictor,
        delta,
        State {
            coefficients,
            delta: i32::from(delta),
            sample1: i32::from(sample(1)),
            sample2: i32::from(sample(0)),
        },
    )
}

fn validate_coefficients(coefficients: &[Coefficients]) -> Result<(), AudioError> {
    if coefficients.is_empty() || coefficients.len() > usize::from(u8::MAX) + 1 {
        return Err(AudioError::InvalidFormat(
            "invalid Microsoft ADPCM coefficient table",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{decode_block, encode_block, frames_in_block, samples_per_block};

    #[test]
    fn standard_mono_and_stereo_geometry_and_roundtrip() {
        assert_eq!(samples_per_block(256, 1).unwrap(), 500);
        assert_eq!(samples_per_block(512, 2).unwrap(), 500);
        for (channels, block_align) in [(1, 256), (2, 512)] {
            let samples: Vec<i16> = (0..500 * channels)
                .map(|index| ((index * 977 + index * index * 13) % 60_000 - 30_000) as i16)
                .collect();
            let encoded = encode_block(&samples, channels as u16, block_align).unwrap();
            assert_eq!(encoded.len(), usize::from(block_align));
            assert_eq!(
                frames_in_block(encoded.len() as u64, channels as u16, block_align).unwrap(),
                500
            );
            let decoded =
                decode_block(&encoded, channels as u16, &super::STANDARD_COEFFICIENTS).unwrap();
            assert_eq!(decoded.len(), samples.len());
            assert!(
                samples
                    .iter()
                    .zip(decoded)
                    .all(|(a, b)| (i32::from(*a) - i32::from(b)).abs() < 20_000)
            );
        }
    }

    #[test]
    fn known_mono_predictor_and_adaptation_vector() {
        let mut block = vec![0; 8];
        block[0] = 0;
        block[1..3].copy_from_slice(&16_i16.to_le_bytes());
        block[3..5].copy_from_slice(&100_i16.to_le_bytes());
        block[5..7].copy_from_slice(&50_i16.to_le_bytes());
        block[7] = 0x11;
        assert_eq!(
            decode_block(&block, 1, &super::STANDARD_COEFFICIENTS).unwrap(),
            [50, 100, 116, 132]
        );
    }

    #[test]
    fn known_stereo_predictors_and_nibble_order() {
        let block = [0, 0, 16, 0, 16, 0, 100, 0, 200, 0, 50, 0, 60, 0, 0x10];
        assert_eq!(
            decode_block(&block, 2, &super::STANDARD_COEFFICIENTS).unwrap(),
            [50, 60, 100, 200, 116, 200]
        );
    }

    #[test]
    fn extreme_adaptation_does_not_overflow() {
        let mut block = vec![0; 256];
        block[1..3].copy_from_slice(&i16::MAX.to_le_bytes());
        block[3..5].copy_from_slice(&i16::MAX.to_le_bytes());
        block[5..7].copy_from_slice(&i16::MAX.to_le_bytes());
        block[7..].fill(0x77);
        assert_eq!(
            decode_block(&block, 1, &super::STANDARD_COEFFICIENTS)
                .unwrap()
                .len(),
            500
        );
    }

    #[test]
    fn rejects_invalid_headers_coefficients_and_geometry() {
        assert!(decode_block(&[0; 7], 1, &[]).is_err());
        assert!(decode_block(&[0; 8], 1, &super::STANDARD_COEFFICIENTS[..6]).is_err());
        let mut invalid = [0_u8; 8];
        invalid[0] = 7;
        assert!(decode_block(&invalid, 1, &super::STANDARD_COEFFICIENTS).is_err());
        assert!(frames_in_block(6, 1, 256).is_err());
    }
}
