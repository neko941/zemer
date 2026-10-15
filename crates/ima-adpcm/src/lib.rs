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
        let step = STEP_TABLE[self.index as usize];
        let delta = i32::from(code & 7);
        let difference = (step * (2 * delta + 1)) >> 3;
        self.predictor = (self.predictor
            + if code & 8 != 0 {
                -difference
            } else {
                difference
            })
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + INDEX_TABLE[delta as usize]).clamp(0, 88);
        self.predictor as i16
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
        let reconstructed = (step * (2 * delta + 1)) >> 3;
        self.predictor = (self.predictor
            + if difference < 0 {
                -reconstructed
            } else {
                reconstructed
            })
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + INDEX_TABLE[delta as usize]).clamp(0, 88);
        code as u8
    }
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
}
