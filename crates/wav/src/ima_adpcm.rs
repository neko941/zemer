use ima_adpcm::ImaAdpcmState;
use zemer_core::AudioError;

pub(super) fn frames_in_block(
    data_size: u64,
    channels: u16,
    block_align: u16,
    samples_per_block: u16,
) -> Result<u64, AudioError> {
    let header_size = u64::from(channels) * 4;
    let block_align = u64::from(block_align);
    if data_size == 0 {
        return Ok(0);
    }
    let full_blocks = data_size / block_align;
    let tail = data_size % block_align;
    if tail != 0 && tail < header_size {
        return Err(AudioError::InvalidFormat(
            "truncated IMA ADPCM block header",
        ));
    }
    let per_group = u64::from(channels) * 4;
    if tail != 0 && (tail - header_size) % per_group != 0 {
        return Err(AudioError::InvalidFormat("partial IMA ADPCM channel group"));
    }
    let tail_frames = if tail == 0 {
        0
    } else {
        1 + (tail - header_size) / per_group * 8
    };
    full_blocks
        .checked_mul(u64::from(samples_per_block))
        .and_then(|frames| frames.checked_add(tail_frames))
        .ok_or(AudioError::InvalidFormat("IMA ADPCM sample count overflow"))
}

pub(super) fn decode_block(
    bytes: &[u8],
    channels: u16,
    samples_per_block: u16,
) -> Result<Vec<i16>, AudioError> {
    let channels = usize::from(channels);
    let header_size = channels * 4;
    if bytes.len() < header_size || (bytes.len() - header_size) % (channels * 4) != 0 {
        return Err(AudioError::InvalidFormat("invalid IMA ADPCM block length"));
    }
    let groups = (bytes.len() - header_size) / (channels * 4);
    let frames = 1 + groups * 8;
    if frames > usize::from(samples_per_block) {
        return Err(AudioError::InvalidFormat(
            "IMA ADPCM block exceeds samples per block",
        ));
    }
    let mut states = Vec::new();
    states
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("IMA ADPCM channel count is too large"))?;
    for channel_header in bytes[..header_size].chunks_exact(4) {
        let predictor = i16::from_le_bytes(channel_header[..2].try_into().unwrap());
        let index = channel_header[2];
        if channel_header[3] != 0 {
            return Err(AudioError::InvalidFormat(
                "IMA ADPCM reserved header byte is nonzero",
            ));
        }
        states.push(
            ImaAdpcmState::new(predictor, index).ok_or(AudioError::InvalidFormat(
                "IMA ADPCM step index is out of range",
            ))?,
        );
    }
    let sample_count = frames.checked_mul(channels).ok_or(AudioError::InvalidData(
        "IMA ADPCM output block is too large",
    ))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(sample_count)
        .map_err(|_| AudioError::InvalidData("IMA ADPCM output block is too large"))?;
    output.extend(states.iter().map(|state| state.predictor()));
    let payload = &bytes[header_size..];
    for group in payload.chunks_exact(channels * 4) {
        for sample in 0..8 {
            for (channel, state) in states.iter_mut().enumerate() {
                let byte = group[channel * 4 + sample / 2];
                let code = if sample & 1 == 0 {
                    byte & 0x0f
                } else {
                    byte >> 4
                };
                output.push(state.decode_wav(code));
            }
        }
    }
    Ok(output)
}

pub(super) fn encode_block(samples: &[i16], channels: u16) -> Result<Vec<u8>, AudioError> {
    let channels = usize::from(channels);
    if channels == 0 || samples.len() % channels != 0 || samples.is_empty() {
        return Err(AudioError::InvalidData(
            "IMA ADPCM input must contain complete sample frames",
        ));
    }
    let frames = samples.len() / channels;
    if (frames - 1) % 8 != 0 {
        return Err(AudioError::InvalidData(
            "IMA ADPCM blocks require groups of eight encoded frames",
        ));
    }
    let mut states = Vec::new();
    states
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("IMA ADPCM channel count is too large"))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(channels * 4 + (frames - 1) * channels / 2)
        .map_err(|_| AudioError::InvalidData("IMA ADPCM output block is too large"))?;
    for sample in samples.iter().take(channels) {
        let predictor = *sample;
        states.push(ImaAdpcmState::from_sample(predictor));
        output.extend_from_slice(&predictor.to_le_bytes());
        output.extend_from_slice(&[0, 0]);
    }
    let mut frame = 1;
    while frame < frames {
        let group_start = output.len();
        output.resize(group_start + channels * 4, 0);
        for (channel, state) in states.iter_mut().enumerate() {
            for offset in 0..4 {
                let first = state.encode_wav(samples[(frame + offset * 2) * channels + channel]);
                let second =
                    state.encode_wav(samples[(frame + offset * 2 + 1) * channels + channel]);
                output[group_start + channel * 4 + offset] = first | (second << 4);
            }
        }
        frame += 8;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{decode_block, encode_block, frames_in_block};

    #[test]
    fn decodes_known_mono_and_stereo_nibble_order() {
        assert_eq!(
            decode_block(&[0, 0, 0, 0, 0x21, 0x43, 0x65, 0x87], 1, 9).unwrap(),
            [0, 2, 6, 12, 19, 31, 52, 95, 89]
        );
        let block = [
            0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        ];
        assert_eq!(
            decode_block(&block, 2, 9).unwrap(),
            [0, 0, 2, 2, 4, 4, 6, 6, 8, 8, 10, 10, 12, 12, 14, 14, 16, 16]
        );
    }

    #[test]
    fn encoder_blocks_decode_and_partial_frames_are_counted() {
        let source: Vec<i16> = (0..18).map(|i| i * 200 - 1600).collect();
        let encoded = encode_block(&source, 2).unwrap();
        assert_eq!(frames_in_block(encoded.len() as u64, 2, 16, 9).unwrap(), 9);
        let decoded = decode_block(&encoded, 2, 9).unwrap();
        let max_error = source
            .iter()
            .zip(decoded)
            .map(|(left, right)| (i32::from(*left) - i32::from(right)).abs())
            .max()
            .unwrap();
        assert!(max_error < 2_000);
        assert!(frames_in_block(15, 2, 16, 9).is_err());
        assert!(decode_block(&[0, 0, 89, 0], 1, 9).is_err());
    }
}
