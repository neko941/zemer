use std::fmt;

const OPUS_HEAD: &[u8; 8] = b"OpusHead";
const OPUS_TAGS: &[u8; 8] = b"OpusTags";
const MAX_OPUS_TAGS_BYTES: usize = 64 * 1024 * 1024;
// shortcut: one million comments bounds per-entry allocation, raise only with a streaming representation.
const MAX_OPUS_TAGS_COMMENTS: usize = 1_000_000;

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpusTags {
    vendor: String,
    comments: Vec<String>,
    extension_data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpusTagsError {
    Truncated,
    InvalidSignature,
    InvalidUtf8,
    TooLarge,
    AllocationFailed,
}

impl fmt::Display for OpusTagsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated OpusTags packet",
            Self::InvalidSignature => "invalid OpusTags signature",
            Self::InvalidUtf8 => "OpusTags contains invalid UTF-8",
            Self::TooLarge => "OpusTags packet exceeds a configured size limit",
            Self::AllocationFailed => "OpusTags allocation failed",
        })
    }
}

impl std::error::Error for OpusTagsError {}

impl OpusTags {
    pub fn parse(packet: &[u8]) -> Result<Self, OpusTagsError> {
        if packet.len() > MAX_OPUS_TAGS_BYTES {
            return Err(OpusTagsError::TooLarge);
        }
        if packet.len() < OPUS_TAGS.len() {
            return Err(OpusTagsError::Truncated);
        }
        if !packet.starts_with(OPUS_TAGS) {
            return Err(OpusTagsError::InvalidSignature);
        }

        let mut cursor = OPUS_TAGS.len();
        let vendor = read_tag_string(packet, &mut cursor)?;
        let comment_count = read_tag_u32(packet, &mut cursor)? as usize;
        if comment_count > MAX_OPUS_TAGS_COMMENTS {
            return Err(OpusTagsError::TooLarge);
        }
        if comment_count > packet.len().saturating_sub(cursor) / 4 {
            return Err(OpusTagsError::Truncated);
        }
        let mut comments = Vec::new();
        comments
            .try_reserve_exact(comment_count)
            .map_err(|_| OpusTagsError::AllocationFailed)?;
        for _ in 0..comment_count {
            comments.push(read_tag_string(packet, &mut cursor)?);
        }
        let extension_data = copy_tag_bytes(&packet[cursor..])?;
        Ok(Self {
            vendor,
            comments,
            extension_data,
        })
    }

    pub fn new(vendor: impl Into<String>, comments: Vec<String>) -> Self {
        Self {
            vendor: vendor.into(),
            comments,
            extension_data: Vec::new(),
        }
    }

    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    pub fn comments(&self) -> &[String] {
        &self.comments
    }

    pub fn comments_mut(&mut self) -> &mut Vec<String> {
        &mut self.comments
    }

    pub fn extension_data(&self) -> &[u8] {
        &self.extension_data
    }

    pub fn to_packet(&self) -> Result<Vec<u8>, OpusTagsError> {
        let comment_count =
            u32::try_from(self.comments.len()).map_err(|_| OpusTagsError::TooLarge)?;
        let packet_bytes = self
            .comments
            .iter()
            .try_fold(16_usize, |bytes, comment| {
                4_usize
                    .checked_add(comment.len())
                    .and_then(|size| bytes.checked_add(size))
            })
            .and_then(|bytes| bytes.checked_add(self.vendor.len()))
            .and_then(|bytes| bytes.checked_add(self.extension_data.len()))
            .filter(|&bytes| bytes <= MAX_OPUS_TAGS_BYTES)
            .ok_or(OpusTagsError::TooLarge)?;
        let mut packet = Vec::new();
        packet
            .try_reserve_exact(packet_bytes)
            .map_err(|_| OpusTagsError::AllocationFailed)?;
        packet.extend_from_slice(OPUS_TAGS);
        append_tag_string(&mut packet, &self.vendor)?;
        packet.extend_from_slice(&comment_count.to_le_bytes());
        for comment in &self.comments {
            append_tag_string(&mut packet, comment)?;
        }
        packet.extend_from_slice(&self.extension_data);
        Ok(packet)
    }
}

fn read_tag_u32(packet: &[u8], cursor: &mut usize) -> Result<u32, OpusTagsError> {
    let end = cursor.checked_add(4).ok_or(OpusTagsError::Truncated)?;
    let bytes = packet.get(*cursor..end).ok_or(OpusTagsError::Truncated)?;
    *cursor = end;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_tag_string(packet: &[u8], cursor: &mut usize) -> Result<String, OpusTagsError> {
    let length = usize::try_from(read_tag_u32(packet, cursor)?).unwrap();
    let end = cursor
        .checked_add(length)
        .filter(|&end| end <= packet.len())
        .ok_or(OpusTagsError::Truncated)?;
    let text =
        std::str::from_utf8(&packet[*cursor..end]).map_err(|_| OpusTagsError::InvalidUtf8)?;
    let mut value = String::new();
    value
        .try_reserve_exact(text.len())
        .map_err(|_| OpusTagsError::AllocationFailed)?;
    value.push_str(text);
    *cursor = end;
    Ok(value)
}

fn copy_tag_bytes(bytes: &[u8]) -> Result<Vec<u8>, OpusTagsError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| OpusTagsError::AllocationFailed)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn append_tag_string(packet: &mut Vec<u8>, value: &str) -> Result<(), OpusTagsError> {
    let length = u32::try_from(value.len()).map_err(|_| OpusTagsError::TooLarge)?;
    packet.extend_from_slice(&length.to_le_bytes());
    packet.extend_from_slice(value.as_bytes());
    Ok(())
}

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
    use super::{OpusHead, OpusHeadError, OpusTags, OpusTagsError};

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

    #[test]
    fn parses_and_rewrites_opus_tags_with_opaque_trailing_data() {
        let mut tags = OpusTags::new(
            "Zemer test",
            vec![
                "TITLE=Sample".into(),
                "ARTIST=One".into(),
                "ARTIST=Two".into(),
            ],
        );
        let mut encoded = tags.to_packet().unwrap();
        encoded.extend_from_slice(&[0x81, 0x02]);
        tags = OpusTags::parse(&encoded).unwrap();
        assert_eq!(tags.vendor(), "Zemer test");
        assert_eq!(
            tags.comments(),
            ["TITLE=Sample", "ARTIST=One", "ARTIST=Two"]
        );
        assert_eq!(tags.extension_data(), [0x81, 0x02]);

        tags.comments_mut().push("ALBUM=Record".into());
        let rewritten = tags.to_packet().unwrap();
        let parsed = OpusTags::parse(&rewritten).unwrap();
        assert_eq!(parsed.extension_data(), [0x81, 0x02]);
        assert_eq!(parsed.comments().last().unwrap(), "ALBUM=Record");
    }

    #[test]
    fn rejects_truncated_or_invalid_tag_packets() {
        assert_eq!(
            OpusTags::parse(b"OpusTag").unwrap_err(),
            OpusTagsError::Truncated
        );
        assert_eq!(
            OpusTags::parse(b"Vorbis!!").unwrap_err(),
            OpusTagsError::InvalidSignature
        );
        assert_eq!(
            OpusTags::parse(b"OpusTags\0\0\0\0").unwrap_err(),
            OpusTagsError::Truncated
        );
        let mut excessive_count = b"OpusTags".to_vec();
        excessive_count.extend_from_slice(&0_u32.to_le_bytes());
        excessive_count.extend_from_slice(&1_000_001_u32.to_le_bytes());
        assert_eq!(
            OpusTags::parse(&excessive_count).unwrap_err(),
            OpusTagsError::TooLarge
        );

        let mut invalid_utf8 = b"OpusTags".to_vec();
        invalid_utf8.extend_from_slice(&1_u32.to_le_bytes());
        invalid_utf8.push(0xff);
        invalid_utf8.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            OpusTags::parse(&invalid_utf8).unwrap_err(),
            OpusTagsError::InvalidUtf8
        );
    }
}
