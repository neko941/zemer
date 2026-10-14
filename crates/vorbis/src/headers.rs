use crate::bitstream::{BitReader, BitWriter};
use zemer_core::{AudioError, Metadata};

pub(crate) const COMMENT_LIMIT: usize = 16 * 1024 * 1024;
const IDENTIFICATION_PREFIX: &[u8] = b"\x01vorbis";
const COMMENT_PREFIX: &[u8] = b"\x03vorbis";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VorbisIdentificationHeader {
    channels: u8,
    sample_rate: u32,
    bitrate_maximum: Option<i32>,
    bitrate_nominal: Option<i32>,
    bitrate_minimum: Option<i32>,
    block_sizes: [usize; 2],
}

impl VorbisIdentificationHeader {
    pub fn parse(packet: &[u8]) -> Result<Self, AudioError> {
        if packet.len() != 30 || &packet[..7] != IDENTIFICATION_PREFIX {
            return Err(AudioError::InvalidFormat(
                "invalid Vorbis identification header",
            ));
        }
        let version = u32::from_le_bytes(packet[7..11].try_into().unwrap());
        let channels = packet[11];
        let sample_rate = u32::from_le_bytes(packet[12..16].try_into().unwrap());
        let bitrate_maximum = signed_bitrate(&packet[16..20]);
        let bitrate_nominal = signed_bitrate(&packet[20..24]);
        let bitrate_minimum = signed_bitrate(&packet[24..28]);
        let small_block = 1_usize << (packet[28] & 0x0f);
        let large_block = 1_usize << (packet[28] >> 4);
        if version != 0
            || channels == 0
            || sample_rate == 0
            || !(64..=8192).contains(&small_block)
            || !(64..=8192).contains(&large_block)
            || small_block > large_block
            || packet[29] != 1
        {
            return Err(AudioError::InvalidFormat(
                "unsupported or invalid Vorbis identification header",
            ));
        }
        Ok(Self {
            channels,
            sample_rate,
            bitrate_maximum,
            bitrate_nominal,
            bitrate_minimum,
            block_sizes: [small_block, large_block],
        })
    }

    pub const fn channels(self) -> u8 {
        self.channels
    }

    pub const fn sample_rate(self) -> u32 {
        self.sample_rate
    }

    pub const fn bitrate_maximum(self) -> Option<i32> {
        self.bitrate_maximum
    }

    pub const fn bitrate_nominal(self) -> Option<i32> {
        self.bitrate_nominal
    }

    pub const fn bitrate_minimum(self) -> Option<i32> {
        self.bitrate_minimum
    }

    pub const fn block_sizes(self) -> [usize; 2] {
        self.block_sizes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisCommentHeader {
    vendor: String,
    metadata: Metadata,
}

impl VorbisCommentHeader {
    pub fn parse(packet: &[u8]) -> Result<Self, AudioError> {
        if packet.len() < COMMENT_PREFIX.len() + 9 || !packet.starts_with(COMMENT_PREFIX) {
            return Err(AudioError::InvalidFormat("invalid Vorbis comment header"));
        }
        if packet.len() > COMMENT_LIMIT {
            return Err(AudioError::InvalidData(
                "Vorbis comment header exceeds the 16 MiB limit",
            ));
        }
        let mut cursor = COMMENT_PREFIX.len();
        let vendor = read_utf8(packet, &mut cursor)?.to_owned();
        let count = read_u32(packet, &mut cursor)? as usize;
        if cursor == packet.len() || count > (packet.len() - cursor - 1) / 4 {
            return Err(AudioError::InvalidData("too many Vorbis comments"));
        }
        let mut metadata = Metadata::default();
        for _ in 0..count {
            let comment = read_utf8(packet, &mut cursor)?;
            let Some((key, value)) = comment.split_once('=') else {
                continue;
            };
            if key.is_empty()
                || key
                    .bytes()
                    .any(|byte| !(0x20..=0x7d).contains(&byte) || byte == b'=')
            {
                continue;
            }
            metadata.insert(key, value.to_owned());
        }
        let mut framing = BitReader::new(&packet[cursor..]);
        if framing.read_bits(1)? != 1 {
            return Err(AudioError::InvalidFormat(
                "invalid Vorbis comment framing bit",
            ));
        }
        Ok(Self { vendor, metadata })
    }

    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

pub(crate) fn encode_comment_header(
    vendor: &str,
    metadata: &Metadata,
) -> Result<Vec<u8>, AudioError> {
    let comment_count = metadata
        .iter()
        .try_fold(0_usize, |count, (_, values)| {
            count.checked_add(values.len())
        })
        .filter(|&count| count <= u32::MAX as usize)
        .ok_or(AudioError::InvalidData("too many Vorbis comments"))?;
    let mut packet = Vec::new();
    packet
        .try_reserve_exact(11)
        .map_err(|_| AudioError::InvalidData("Vorbis comment allocation failed"))?;
    packet.extend_from_slice(COMMENT_PREFIX);
    append_string(&mut packet, vendor)?;
    append_u32(&mut packet, comment_count)?;
    for (key, values) in metadata.iter() {
        if key.is_empty()
            || key
                .bytes()
                .any(|byte| !(0x20..=0x7d).contains(&byte) || byte == b'=')
        {
            return Err(AudioError::InvalidData("invalid Vorbis comment name"));
        }
        for value in values {
            let field_size = key
                .len()
                .checked_add(1)
                .and_then(|size| size.checked_add(value.len()))
                .ok_or(AudioError::InvalidData("Vorbis comment length overflow"))?;
            append_length(&mut packet, field_size)?;
            append_bytes(&mut packet, key.as_bytes())?;
            append_bytes(&mut packet, b"=")?;
            append_bytes(&mut packet, value.as_bytes())?;
        }
    }
    let mut framing = BitWriter::new();
    framing.write_bit(true)?;
    append_bytes(&mut packet, &framing.into_bytes())?;
    Ok(packet)
}

fn append_string(packet: &mut Vec<u8>, value: &str) -> Result<(), AudioError> {
    append_length(packet, value.len())?;
    append_bytes(packet, value.as_bytes())
}

fn append_length(packet: &mut Vec<u8>, length: usize) -> Result<(), AudioError> {
    let length = u32::try_from(length)
        .map_err(|_| AudioError::InvalidData("Vorbis comment field is too large"))?;
    append_bytes(packet, &length.to_le_bytes())
}

fn append_u32(packet: &mut Vec<u8>, value: usize) -> Result<(), AudioError> {
    let value =
        u32::try_from(value).map_err(|_| AudioError::InvalidData("too many Vorbis comments"))?;
    append_bytes(packet, &value.to_le_bytes())
}

fn append_bytes(packet: &mut Vec<u8>, bytes: &[u8]) -> Result<(), AudioError> {
    let new_len = packet
        .len()
        .checked_add(bytes.len())
        .filter(|&length| length <= COMMENT_LIMIT)
        .ok_or(AudioError::InvalidData(
            "Vorbis comment header exceeds the 16 MiB limit",
        ))?;
    packet
        .try_reserve(new_len - packet.len())
        .map_err(|_| AudioError::InvalidData("Vorbis comment allocation failed"))?;
    packet.extend_from_slice(bytes);
    Ok(())
}

fn signed_bitrate(bytes: &[u8]) -> Option<i32> {
    let bitrate = i32::from_le_bytes(bytes.try_into().unwrap());
    (bitrate >= 0).then_some(bitrate)
}

fn read_u32(packet: &[u8], cursor: &mut usize) -> Result<u32, AudioError> {
    let end = cursor
        .checked_add(4)
        .filter(|&end| end <= packet.len())
        .ok_or(AudioError::InvalidFormat("truncated Vorbis comment header"))?;
    let value = u32::from_le_bytes(packet[*cursor..end].try_into().unwrap());
    *cursor = end;
    Ok(value)
}

fn read_utf8<'a>(packet: &'a [u8], cursor: &mut usize) -> Result<&'a str, AudioError> {
    let length = usize::try_from(read_u32(packet, cursor)?)
        .map_err(|_| AudioError::InvalidFormat("Vorbis comment length overflow"))?;
    let end = cursor
        .checked_add(length)
        .filter(|&end| end <= packet.len())
        .ok_or(AudioError::InvalidFormat("truncated Vorbis comment field"))?;
    let value = std::str::from_utf8(&packet[*cursor..end])
        .map_err(|_| AudioError::InvalidFormat("Vorbis comment field is not UTF-8"))?;
    *cursor = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use zemer_core::Metadata;

    use super::{VorbisCommentHeader, VorbisIdentificationHeader, encode_comment_header};

    fn identification() -> [u8; 30] {
        let mut packet = [0; 30];
        packet[..7].copy_from_slice(b"\x01vorbis");
        packet[11] = 2;
        packet[12..16].copy_from_slice(&44_100_u32.to_le_bytes());
        packet[20..24].copy_from_slice(&128_000_i32.to_le_bytes());
        packet[28] = 0xb6;
        packet[29] = 1;
        packet
    }

    fn comment(comments: &[&str]) -> Vec<u8> {
        let mut packet = b"\x03vorbis".to_vec();
        packet.extend_from_slice(&5_u32.to_le_bytes());
        packet.extend_from_slice(b"zemer");
        packet.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for value in comments {
            packet.extend_from_slice(&(value.len() as u32).to_le_bytes());
            packet.extend_from_slice(value.as_bytes());
        }
        packet.push(1);
        packet
    }

    #[test]
    fn parses_identification_header() {
        let header = VorbisIdentificationHeader::parse(&identification()).unwrap();
        assert_eq!(header.channels(), 2);
        assert_eq!(header.sample_rate(), 44_100);
        assert_eq!(header.bitrate_nominal(), Some(128_000));
        assert_eq!(header.block_sizes(), [64, 2048]);
    }

    #[test]
    fn rejects_identification_header_with_bad_framing_or_block_sizes() {
        let mut packet = identification();
        packet[29] = 0;
        assert!(VorbisIdentificationHeader::parse(&packet).is_err());
        packet = identification();
        packet[28] = 0x65;
        assert!(VorbisIdentificationHeader::parse(&packet).is_err());
        packet = identification();
        packet[28] = 0xe6;
        assert!(VorbisIdentificationHeader::parse(&packet).is_err());
    }

    #[test]
    fn parses_comment_header_and_preserves_repeated_fields() {
        let header =
            VorbisCommentHeader::parse(&comment(&["TITLE=Song", "ARTIST=Band", "TITLE=Alt"]))
                .unwrap();
        assert_eq!(header.vendor(), "zemer");
        assert_eq!(header.metadata().values("title"), ["Song", "Alt"]);
        assert_eq!(header.metadata().values("artist"), ["Band"]);
    }

    #[test]
    fn ignores_unstructured_fields_but_rejects_truncated_and_unframed_comments() {
        assert!(VorbisCommentHeader::parse(b"\x03vorbis").is_err());
        let header =
            VorbisCommentHeader::parse(&comment(&["INVALID", "TITLE=Song", "=empty"])).unwrap();
        assert_eq!(header.metadata().values("TITLE"), ["Song"]);
        assert!(header.metadata().values("").is_empty());
        let mut packet = comment(&[]);
        *packet.last_mut().unwrap() = 0;
        assert!(VorbisCommentHeader::parse(&packet).is_err());
    }

    #[test]
    fn reads_only_the_low_framing_bit_and_keeps_repeated_values() {
        let mut packet = comment(&["TITLE=Song"]);
        *packet.last_mut().unwrap() |= 0xfe;
        assert_eq!(
            VorbisCommentHeader::parse(&packet)
                .unwrap()
                .metadata()
                .values("TITLE"),
            ["Song"]
        );
    }

    #[test]
    fn writes_utf8_and_repeated_comment_fields() {
        let mut metadata = Metadata::default();
        metadata.insert("title", "Song 🎵");
        metadata.insert("artist", "Band");
        metadata.insert("artist", "Guest");

        let encoded = encode_comment_header("Zemer", &metadata).unwrap();
        let parsed = VorbisCommentHeader::parse(&encoded).unwrap();

        assert_eq!(parsed.vendor(), "Zemer");
        assert_eq!(parsed.metadata().values("TITLE"), ["Song 🎵"]);
        assert_eq!(parsed.metadata().values("artist"), ["Band", "Guest"]);
    }

    #[test]
    fn rejects_unrepresentable_comment_names() {
        let mut metadata = Metadata::default();
        metadata.insert("BAD=KEY", "value");
        assert!(encode_comment_header("Zemer", &metadata).is_err());
    }
}
