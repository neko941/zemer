use ima_adpcm::{ImaAdpcmState, decode_ima4_packet, encode_ima4_packet};
use zemer_core::AudioError;

pub(super) fn decode_packet(
    packet: &[u8; 34],
    previous: Option<State>,
) -> Result<([i16; 64], State), AudioError> {
    decode_ima4_packet(packet, previous).map_err(AudioError::InvalidFormat)
}

pub(super) fn encode_packet(samples: &[i16; 64], previous: Option<State>) -> ([u8; 34], State) {
    encode_ima4_packet(samples, previous)
}

pub(super) type State = ImaAdpcmState;

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
