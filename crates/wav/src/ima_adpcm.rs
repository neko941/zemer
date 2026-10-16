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
            [0, 1, 4, 8, 15, 27, 47, 88, 82]
        );
        let block = [
            0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        ];
        assert_eq!(
            decode_block(&block, 2, 9).unwrap(),
            [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8]
        );
    }

    #[test]
    fn decodes_multichannel_ima_groups_in_channel_order() {
        let mut block = vec![0, 0, 0, 0, 100, 0, 0, 0, 156, 255, 0, 0];
        block.extend_from_slice(&[0x11; 4]);
        block.extend_from_slice(&[0x22; 4]);
        block.extend_from_slice(&[0x33; 4]);

        assert_eq!(
            decode_block(&block, 3, 9).unwrap(),
            [
                0, 100, -100, 1, 103, -96, 2, 106, -92, 3, 109, -88, 4, 112, -84, 5, 115, -80, 6,
                118, -76, 7, 121, -72, 8, 124, -68
            ]
        );
    }

    #[test]
    fn matches_independent_three_channel_ima_wav_decoder_vector() {
        let block = [
            0x18, 0xfc, 0x32, 0x00, 0xdc, 0x05, 0x2d, 0x00, 0x0c, 0xfe, 0x39, 0x00, 0x11, 0x11,
            0x22, 0x31, 0x99, 0xa9, 0x9a, 0xab, 0x00, 0x20, 0x22, 0x44,
        ];
        assert_eq!(
            decode_block(&block, 3, 9).unwrap(),
            [
                -1000, 1500, -500, -672, 1296, -287, -374, 1112, -93, -103, 944, 83, 143, 689, 884,
                516, 458, 1612, 856, 332, 2274, 1040, 65, 3357, 1432, -108, 4668
            ]
        );
    }

    #[test]
    fn encoder_matches_independent_three_channel_decoder_vector() {
        let samples: Vec<i16> = (0..9)
            .flat_map(|frame| {
                [
                    frame * 300 - 1000,
                    1500 - frame * 200,
                    frame * frame * 80 - 500,
                ]
            })
            .collect();
        let block = encode_block(&samples, 3).unwrap();
        assert_eq!(
            block,
            [
                0x18, 0xfc, 0x00, 0x00, 0xdc, 0x05, 0x00, 0x00, 0x0c, 0xfe, 0x00, 0x00, 0x77, 0x77,
                0x77, 0x05, 0xff, 0xff, 0xff, 0x99, 0x77, 0x77, 0x77, 0x67
            ]
        );
        assert_eq!(
            decode_block(&block, 3, 9).unwrap(),
            [
                -1000, 1500, -500, -989, 1489, -489, -959, 1459, -459, -896, 1396, -396, -760,
                1260, -260, -467, 967, 33, 164, 336, 664, 1159, 65, 2021, 1291, -181, 4543
            ]
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
