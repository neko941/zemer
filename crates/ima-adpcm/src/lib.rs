const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX_TABLE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

#[derive(Clone, Copy, Debug)]
pub struct ImaAdpcmState {
    predictor: i32,
    index: i32,
}

impl ImaAdpcmState {
    pub fn new(predictor: i16, index: u8) -> Option<Self> {
        (index <= 88).then_some(Self {
            predictor: i32::from(predictor),
            index: i32::from(index),
        })
    }

    pub fn from_sample(sample: i16) -> Self {
        Self {
            predictor: i32::from(sample),
            index: 0,
        }
    }

    pub fn predictor(self) -> i16 {
        self.predictor as i16
    }

    pub fn index(self) -> u8 {
        self.index as u8
    }

    pub fn set_index(&mut self, index: u8) -> bool {
        if index > 88 {
            return false;
        }
        self.index = i32::from(index);
        true
    }

    pub fn decode(&mut self, code: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut difference = step >> 3;
        if code & 1 != 0 {
            difference += step >> 2;
        }
        if code & 2 != 0 {
            difference += step >> 1;
        }
        if code & 4 != 0 {
            difference += step;
        }
        self.predictor = (self.predictor
            + if code & 8 != 0 {
                -difference
            } else {
                difference
            })
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + INDEX_TABLE[(code & 7) as usize]).clamp(0, 88);
        self.predictor as i16
    }

    pub fn decode_wav(&mut self, code: u8) -> i16 {
        self.decode(code)
    }

    pub fn encode(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let mut difference = i32::from(sample) - self.predictor;
        let mut code: i32 = 0;
        if difference < 0 {
            code = 8;
            difference = -difference;
        }
        let mut quantized = step >> 3;
        for (mask, threshold) in [(4, step), (2, step >> 1), (1, step >> 2)] {
            if difference >= threshold {
                code |= mask;
                difference -= threshold;
                quantized += threshold;
            }
        }
        self.predictor = (self.predictor + if code & 8 != 0 { -quantized } else { quantized })
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + INDEX_TABLE[(code & 7) as usize]).clamp(0, 88);
        code as u8
    }

    pub fn encode_wav(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let difference = i32::from(sample) - self.predictor;
        let magnitude = difference.abs();
        let delta = ((magnitude << 2) / step).min(7);
        let code = delta | if difference < 0 { 8 } else { 0 };
        self.decode(code as u8);
        code as u8
    }
}

/// Decodes one Apple IMA4 packet (64 mono samples in 34 bytes).
pub fn decode_ima4_packet(
    packet: &[u8; 34],
    previous: Option<ImaAdpcmState>,
) -> Result<([i16; 64], ImaAdpcmState), &'static str> {
    let header = u16::from_be_bytes([packet[0], packet[1]]);
    let index = (header & 0x7f) as u8;
    if index > 88 {
        return Err("IMA4 step index is out of range");
    }
    let mut state = if let Some(mut state) = previous {
        state.set_index(index);
        state
    } else {
        ImaAdpcmState::new((header as i16 >> 7) << 7, index)
            .ok_or("IMA4 step index is out of range")?
    };
    let mut samples = [0; 64];
    for (index, byte) in packet[2..].iter().enumerate() {
        samples[index * 2] = state.decode(byte & 0x0f);
        samples[index * 2 + 1] = state.decode(byte >> 4);
    }
    Ok((samples, state))
}

/// Encodes one Apple IMA4 packet (64 mono samples in 34 bytes).
pub fn encode_ima4_packet(
    samples: &[i16; 64],
    previous: Option<ImaAdpcmState>,
) -> ([u8; 34], ImaAdpcmState) {
    let mut state = previous.unwrap_or_else(|| {
        ImaAdpcmState::new((samples[0] >> 7) << 7, 0).expect("fixed IMA4 index is valid")
    });
    let predictor = (state.predictor() >> 7).clamp(-256, 255);
    let header = (((predictor as u16) & 0x01ff) << 7) | u16::from(state.index());
    let mut packet = [0; 34];
    packet[..2].copy_from_slice(&header.to_be_bytes());
    for (index, pair) in samples.chunks_exact(2).enumerate() {
        let low = state.encode(pair[0]);
        let high = state.encode(pair[1]);
        packet[index + 2] = low | (high << 4);
    }
    (packet, state)
}

#[cfg(test)]
mod tests {
    use super::ImaAdpcmState;

    #[test]
    fn reset_checkpoint_and_extreme_nibbles_are_bounded() {
        let mut state = ImaAdpcmState::new(i16::MAX, 88).unwrap();
        let maximum = state.decode(7);
        assert_eq!(maximum, i16::MAX);
        assert!(state.decode(15) < maximum);
        assert!(!state.set_index(89));
        let mut encoder = ImaAdpcmState::from_sample(i16::MIN);
        let code = encoder.encode(i16::MAX);
        assert_eq!(code & 0x0f, 7);
        assert!(ImaAdpcmState::new(0, 89).is_none());
    }

    #[test]
    fn wav_encoder_state_matches_reference_decoder() {
        for index in 0..=88 {
            for predictor in [i16::MIN, -1, 0, 1, i16::MAX] {
                for sample in [i16::MIN, -1, 0, 1, i16::MAX] {
                    let start = ImaAdpcmState::new(predictor, index).unwrap();
                    let mut encoder = start;
                    let code = encoder.encode_wav(sample);
                    let mut decoder = start;
                    decoder.decode_wav(code);
                    assert_eq!(encoder.predictor(), decoder.predictor());
                    assert_eq!(encoder.index(), decoder.index());
                }
            }
        }
    }

    #[test]
    fn wav_decode_uses_reference_shift_accumulation() {
        let mut state = ImaAdpcmState::new(0, 0).unwrap();
        assert_eq!(state.decode_wav(0x07), 11);
    }

    #[test]
    fn ima4_packets_round_trip_and_reject_invalid_step_indices() {
        let samples = std::array::from_fn(|index| ((index as f32 * 0.31).sin() * 3_000.0) as i16);
        let (packet, _) = super::encode_ima4_packet(&samples, None);
        let decoded = super::decode_ima4_packet(&packet, None).unwrap().0;
        let max_error = samples
            .iter()
            .zip(decoded)
            .map(|(source, decoded)| (i32::from(*source) - i32::from(decoded)).abs())
            .max()
            .unwrap();
        assert!(max_error < 3_000);

        let mut invalid = [0; 34];
        invalid[..2].copy_from_slice(&89_u16.to_be_bytes());
        assert!(super::decode_ima4_packet(&invalid, None).is_err());
    }
}
