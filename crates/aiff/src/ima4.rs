use zemer_core::AudioError;

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX_TABLE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

pub(super) fn decode_packet(
    packet: &[u8; 34],
    previous: Option<State>,
) -> Result<([i16; 64], State), AudioError> {
    let header = u16::from_be_bytes([packet[0], packet[1]]);
    if header & 0x7f > 88 {
        return Err(AudioError::InvalidFormat(
            "AIFC IMA4 step index is out of range",
        ));
    }
    // Sequential decoding keeps the full predictor; the packet header is its rounded seek checkpoint.
    let mut state = previous.unwrap_or_else(|| State::from_header(header));
    state.index = i32::from(header & 0x7f);
    let mut samples = [0; 64];
    for (index, byte) in packet[2..].iter().enumerate() {
        samples[index * 2] = state.decode(byte & 0x0f);
        samples[index * 2 + 1] = state.decode(byte >> 4);
    }
    Ok((samples, state))
}

pub(super) fn encode_packet(samples: &[i16; 64], previous: Option<State>) -> ([u8; 34], State) {
    let mut state = previous.unwrap_or_else(|| State::from_sample(samples[0]));
    let predictor = (state.predictor >> 7).clamp(-256, 255);
    let header = (((predictor as u16) & 0x01ff) << 7) | state.index as u16;
    let mut packet = [0; 34];
    packet[..2].copy_from_slice(&header.to_be_bytes());
    for (index, pair) in samples.chunks_exact(2).enumerate() {
        let low = state.encode(pair[0]);
        let high = state.encode(pair[1]);
        packet[index + 2] = low | (high << 4);
    }
    (packet, state)
}

#[derive(Clone, Copy)]
pub(super) struct State {
    predictor: i32,
    index: i32,
}

impl State {
    fn from_header(header: u16) -> Self {
        Self {
            predictor: i32::from((header as i16) >> 7) << 7,
            index: i32::from(header & 0x7f),
        }
    }

    fn from_sample(sample: i16) -> Self {
        Self {
            predictor: (i32::from(sample) >> 7) << 7,
            index: 0,
        }
    }

    fn decode(&mut self, code: u8) -> i16 {
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

    fn encode(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let mut difference = i32::from(sample) - self.predictor;
        let mut code = 0;
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
}

#[cfg(test)]
mod tests {
    use super::{decode_packet, encode_packet};

    #[test]
    fn zero_packet_decodes_to_silence() {
        assert_eq!(decode_packet(&[0; 34], None).unwrap().0, [0; 64]);
    }

    #[test]
    fn encoded_packet_round_trips_with_bounded_adpcm_error() {
        let samples =
            std::array::from_fn(|index| ((index as f32 * 0.31).sin() * 3_000.0).round() as i16);
        let (packet, _) = encode_packet(&samples, None);
        let decoded = decode_packet(&packet, None).unwrap().0;
        let max_error = samples
            .iter()
            .zip(decoded)
            .map(|(source, decoded)| (i32::from(*source) - i32::from(decoded)).abs())
            .max()
            .unwrap();
        assert!(max_error < 3_000, "maximum IMA4 error was {max_error}");
    }

    #[test]
    fn rejects_step_indices_outside_the_ima_table() {
        let mut packet = [0; 34];
        packet[..2].copy_from_slice(&89_u16.to_be_bytes());
        assert!(decode_packet(&packet, None).is_err());
    }
}
