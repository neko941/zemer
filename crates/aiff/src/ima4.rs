use ima_adpcm::ImaAdpcmState;
use zemer_core::AudioError;

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
    let mut state = if let Some(mut state) = previous {
        if !state.set_index((header & 0x7f) as u8) {
            return Err(AudioError::InvalidFormat(
                "AIFC IMA4 step index is out of range",
            ));
        }
        state
    } else {
        ImaAdpcmState::new((header as i16 >> 7) << 7, (header & 0x7f) as u8).ok_or(
            AudioError::InvalidFormat("AIFC IMA4 step index is out of range"),
        )?
    };
    let mut samples = [0; 64];
    for (index, byte) in packet[2..].iter().enumerate() {
        samples[index * 2] = state.decode(byte & 0x0f);
        samples[index * 2 + 1] = state.decode(byte >> 4);
    }
    Ok((samples, state))
}

pub(super) fn encode_packet(
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
