use std::fmt;

const OPUS_HEAD: &[u8; 8] = b"OpusHead";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpusHead {
    pub version: u8,
    pub channels: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain_q8: i16,
    pub mapping_family: u8,
    pub stream_count: u8,
    pub coupled_count: u8,
    pub channel_mapping: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpusHeadError {
    Truncated,
    InvalidSignature,
    IncompatibleVersion,
    InvalidChannelCount,
    InvalidMapping,
}

impl fmt::Display for OpusHeadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated OpusHead packet",
            Self::InvalidSignature => "invalid OpusHead signature",
            Self::IncompatibleVersion => "incompatible OpusHead version",
            Self::InvalidChannelCount => "invalid OpusHead channel count",
            Self::InvalidMapping => "invalid OpusHead channel mapping",
        })
    }
}

impl std::error::Error for OpusHeadError {}

impl OpusHead {
    pub fn parse(packet: &[u8]) -> Result<Self, OpusHeadError> {
        if packet.len() < 19 {
            return Err(OpusHeadError::Truncated);
        }
        if packet.get(..8) != Some(OPUS_HEAD) {
            return Err(OpusHeadError::InvalidSignature);
        }

        let version = packet[8];
        if version > 15 {
            return Err(OpusHeadError::IncompatibleVersion);
        }
        let channels = packet[9];
        if channels == 0 {
            return Err(OpusHeadError::InvalidChannelCount);
        }

        let pre_skip = u16::from_le_bytes([packet[10], packet[11]]);
        let input_sample_rate = u32::from_le_bytes(packet[12..16].try_into().unwrap());
        let output_gain_q8 = i16::from_le_bytes([packet[16], packet[17]]);
        let mapping_family = packet[18];
        let (stream_count, coupled_count, channel_mapping, header_len) = if mapping_family == 0 {
            if channels > 2 {
                return Err(OpusHeadError::InvalidChannelCount);
            }
            (1, channels - 1, (0..channels).collect(), 19)
        } else {
            if mapping_family == 1 && channels > 8 {
                return Err(OpusHeadError::InvalidChannelCount);
            }
            let header_len = 21_usize
                .checked_add(usize::from(channels))
                .ok_or(OpusHeadError::InvalidMapping)?;
            if packet.len() < header_len {
                return Err(OpusHeadError::Truncated);
            }
            let stream_count = packet[19];
            let coupled_count = packet[20];
            let mapping = packet[21..header_len].to_vec();
            let decoded_channels = u16::from(stream_count) + u16::from(coupled_count);
            if stream_count == 0
                || coupled_count > stream_count
                || decoded_channels > u8::MAX.into()
                || mapping
                    .iter()
                    .any(|&index| index != u8::MAX && u16::from(index) >= decoded_channels)
            {
                return Err(OpusHeadError::InvalidMapping);
            }
            (stream_count, coupled_count, mapping, header_len)
        };

        // Minor-version extensions may append fields; version 1 has an exact layout.
        if version <= 1 && packet.len() != header_len {
            return Err(OpusHeadError::InvalidMapping);
        }

        Ok(Self {
            version,
            channels,
            pre_skip,
            input_sample_rate,
            output_gain_q8,
            mapping_family,
            stream_count,
            coupled_count,
            channel_mapping,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{OpusHead, OpusHeadError};

    fn header(channels: u8, family: u8) -> Vec<u8> {
        let mut packet = b"OpusHead".to_vec();
        packet.extend_from_slice(&[1, channels]);
        packet.extend_from_slice(&312_u16.to_le_bytes());
        packet.extend_from_slice(&48_000_u32.to_le_bytes());
        packet.extend_from_slice(&(-256_i16).to_le_bytes());
        packet.push(family);
        packet
    }

    #[test]
    fn parses_family_zero_defaults_and_little_endian_fields() {
        for (channels, expected_map, coupled) in [(1, vec![0], 0), (2, vec![0, 1], 1)] {
            let parsed = OpusHead::parse(&header(channels, 0)).unwrap();
            assert_eq!(parsed.channels, channels);
            assert_eq!(parsed.pre_skip, 312);
            assert_eq!(parsed.input_sample_rate, 48_000);
            assert_eq!(parsed.output_gain_q8, -256);
            assert_eq!(parsed.stream_count, 1);
            assert_eq!(parsed.coupled_count, coupled);
            assert_eq!(parsed.channel_mapping, expected_map);
        }
    }

    #[test]
    fn parses_generic_mapping_and_silence_entries() {
        let mut packet = header(3, 1);
        packet.extend_from_slice(&[2, 1, 0, 1, 255]);
        let parsed = OpusHead::parse(&packet).unwrap();
        assert_eq!(parsed.stream_count, 2);
        assert_eq!(parsed.coupled_count, 1);
        assert_eq!(parsed.channel_mapping, [0, 1, 255]);
    }

    #[test]
    fn rejects_invalid_headers_and_mapping_indices() {
        assert_eq!(
            OpusHead::parse(b"OpusHead").unwrap_err(),
            OpusHeadError::Truncated
        );
        let mut packet = header(3, 0);
        assert_eq!(
            OpusHead::parse(&packet).unwrap_err(),
            OpusHeadError::InvalidChannelCount
        );
        packet = header(1, 1);
        packet.extend_from_slice(&[1, 0, 1]);
        assert_eq!(
            OpusHead::parse(&packet).unwrap_err(),
            OpusHeadError::InvalidMapping
        );
        packet = header(9, 1);
        packet.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            OpusHead::parse(&packet).unwrap_err(),
            OpusHeadError::InvalidChannelCount
        );
        packet = header(1, 1);
        packet.extend_from_slice(&[1, 0, 0]);
        packet[8] = 16;
        assert_eq!(
            OpusHead::parse(&packet).unwrap_err(),
            OpusHeadError::IncompatibleVersion
        );
    }

    #[test]
    fn accepts_compatible_minor_extensions() {
        let mut packet = header(1, 0);
        packet[8] = 2;
        packet.extend_from_slice(&[0xA5, 0x5A]);
        assert_eq!(OpusHead::parse(&packet).unwrap().version, 2);
    }
}
