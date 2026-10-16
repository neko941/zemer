use crate::bitstream::BitWriter;
use zemer_core::{AudioError, Metadata, Picture};

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VorbisCommentHeader {
    vendor: String,
    metadata: Metadata,
}

enum CommentHeaderParseError {
    Truncated,
    Audio(AudioError),
}

impl From<AudioError> for CommentHeaderParseError {
    fn from(error: AudioError) -> Self {
        Self::Audio(error)
    }
}

impl CommentHeaderParseError {
    fn into_audio_error(self) -> AudioError {
        match self {
            Self::Truncated => AudioError::InvalidFormat("truncated Vorbis comment header"),
            Self::Audio(error) => error,
        }
    }
}

impl VorbisCommentHeader {
    pub fn parse(packet: &[u8]) -> Result<Self, AudioError> {
        Self::parse_packet(packet).map_err(CommentHeaderParseError::into_audio_error)
    }

    pub(crate) fn parse_for_stream(packet: &[u8]) -> Result<Self, AudioError> {
        match Self::parse_packet(packet) {
            Ok(comments) => Ok(comments),
            Err(CommentHeaderParseError::Truncated) => Ok(Self::default()),
            Err(error) => Err(error.into_audio_error()),
        }
    }

    fn parse_packet(packet: &[u8]) -> Result<Self, CommentHeaderParseError> {
        if packet.len() < COMMENT_PREFIX.len() && COMMENT_PREFIX.starts_with(packet) {
            return Err(CommentHeaderParseError::Truncated);
        }
        if !packet.starts_with(COMMENT_PREFIX) {
            return Err(CommentHeaderParseError::Audio(AudioError::InvalidFormat(
                "invalid Vorbis comment header",
            )));
        }
        if packet.len() > COMMENT_LIMIT {
            return Err(CommentHeaderParseError::Audio(AudioError::InvalidData(
                "Vorbis comment header exceeds the 16 MiB limit",
            )));
        }
        let mut cursor = COMMENT_PREFIX.len();
        let vendor = read_utf8(packet, &mut cursor)?.to_owned();
        let count = read_u32(packet, &mut cursor)? as usize;
        if cursor == packet.len() || count > (packet.len() - cursor - 1) / 4 {
            return Err(CommentHeaderParseError::Truncated);
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
            if key.eq_ignore_ascii_case("METADATA_BLOCK_PICTURE") {
                metadata.add_picture(parse_picture(&decode_base64(value)?)?);
            } else {
                metadata.insert(key, value.to_owned());
            }
        }
        let framing = packet
            .get(cursor)
            .ok_or(CommentHeaderParseError::Truncated)?;
        if framing & 1 != 1 {
            return Err(CommentHeaderParseError::Audio(AudioError::InvalidFormat(
                "invalid Vorbis comment framing bit",
            )));
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
        .and_then(|count| count.checked_add(metadata.pictures().len()))
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
        if key.eq_ignore_ascii_case("METADATA_BLOCK_PICTURE") {
            return Err(AudioError::InvalidData(
                "METADATA_BLOCK_PICTURE must use the embedded picture API",
            ));
        }
        if key.is_empty()
            || key
                .bytes()
                .any(|byte| !(0x20..=0x7d).contains(&byte) || byte == b'=')
        {
            return Err(AudioError::InvalidData("invalid Vorbis comment name"));
        }
        for value in values {
            append_comment(&mut packet, key, value)?;
        }
    }
    for picture in metadata.pictures() {
        let value = encode_base64(&encode_picture(picture)?)?;
        append_comment(&mut packet, "METADATA_BLOCK_PICTURE", &value)?;
    }
    let mut framing = BitWriter::new();
    framing.write_bit(true)?;
    append_bytes(&mut packet, &framing.into_bytes())?;
    Ok(packet)
}

fn append_comment(packet: &mut Vec<u8>, key: &str, value: &str) -> Result<(), AudioError> {
    let field_size = key
        .len()
        .checked_add(1)
        .and_then(|size| size.checked_add(value.len()))
        .ok_or(AudioError::InvalidData("Vorbis comment length overflow"))?;
    append_length(packet, field_size)?;
    append_bytes(packet, key.as_bytes())?;
    append_bytes(packet, b"=")?;
    append_bytes(packet, value.as_bytes())
}

fn encode_picture(picture: &Picture) -> Result<Vec<u8>, AudioError> {
    let media_type = picture.media_type().as_bytes();
    if media_type
        .iter()
        .any(|&byte| !(0x20..=0x7e).contains(&byte))
    {
        return Err(AudioError::InvalidData(
            "FLAC picture media type must be ASCII",
        ));
    }
    let media_len = u32::try_from(media_type.len())
        .map_err(|_| AudioError::InvalidData("FLAC picture media type is too large"))?;
    let description_len = u32::try_from(picture.description().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture description is too large"))?;
    let data_len = u32::try_from(picture.data().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture data is too large"))?;
    let size = 32_usize
        .checked_add(media_type.len())
        .and_then(|size| size.checked_add(picture.description().len()))
        .and_then(|size| size.checked_add(picture.data().len()))
        .filter(|&size| size <= 0x00ff_ffff)
        .ok_or(AudioError::InvalidData("FLAC picture block is too large"))?;
    let mut block = Vec::new();
    block
        .try_reserve_exact(size)
        .map_err(|_| AudioError::InvalidData("FLAC picture allocation failed"))?;
    block.extend_from_slice(&picture.picture_type().to_be_bytes());
    block.extend_from_slice(&media_len.to_be_bytes());
    block.extend_from_slice(media_type);
    block.extend_from_slice(&description_len.to_be_bytes());
    block.extend_from_slice(picture.description().as_bytes());
    for value in [
        picture.dimensions().0,
        picture.dimensions().1,
        picture.color_depth(),
        picture.colors(),
        data_len,
    ] {
        block.extend_from_slice(&value.to_be_bytes());
    }
    block.extend_from_slice(picture.data());
    Ok(block)
}

fn parse_picture(block: &[u8]) -> Result<Picture, AudioError> {
    let mut cursor = 0;
    let picture_type = read_be_u32(block, &mut cursor)?;
    let media_type = read_flac_string(block, &mut cursor)?;
    if media_type
        .bytes()
        .any(|byte| !(0x20..=0x7e).contains(&byte))
    {
        return Err(AudioError::InvalidData(
            "FLAC picture media type is not printable ASCII",
        ));
    }
    let description = read_flac_string(block, &mut cursor)?;
    let width = read_be_u32(block, &mut cursor)?;
    let height = read_be_u32(block, &mut cursor)?;
    let color_depth = read_be_u32(block, &mut cursor)?;
    let colors = read_be_u32(block, &mut cursor)?;
    let data_len = usize::try_from(read_be_u32(block, &mut cursor)?)
        .map_err(|_| AudioError::InvalidFormat("FLAC picture data length overflow"))?;
    let end = cursor
        .checked_add(data_len)
        .filter(|&end| end == block.len())
        .ok_or(AudioError::InvalidData("invalid FLAC picture data length"))?;
    let mut picture_data = Vec::new();
    picture_data
        .try_reserve_exact(data_len)
        .map_err(|_| AudioError::InvalidData("picture allocation failed"))?;
    picture_data.extend_from_slice(&block[cursor..end]);
    Ok(Picture::new(
        picture_type,
        media_type,
        description,
        width,
        height,
        color_depth,
        colors,
        picture_data,
    ))
}

fn read_flac_string<'a>(block: &'a [u8], cursor: &mut usize) -> Result<&'a str, AudioError> {
    let length = usize::try_from(read_be_u32(block, cursor)?)
        .map_err(|_| AudioError::InvalidFormat("FLAC picture string length overflow"))?;
    let end = cursor
        .checked_add(length)
        .filter(|&end| end <= block.len())
        .ok_or(AudioError::InvalidData("truncated FLAC picture string"))?;
    let value = std::str::from_utf8(&block[*cursor..end])
        .map_err(|_| AudioError::InvalidData("FLAC picture string is not UTF-8"))?;
    *cursor = end;
    Ok(value)
}

fn read_be_u32(block: &[u8], cursor: &mut usize) -> Result<u32, AudioError> {
    let end = cursor
        .checked_add(4)
        .filter(|&end| end <= block.len())
        .ok_or(AudioError::InvalidData("truncated FLAC picture block"))?;
    let value = u32::from_be_bytes(block[*cursor..end].try_into().unwrap());
    *cursor = end;
    Ok(value)
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(bytes: &[u8]) -> Result<String, AudioError> {
    let capacity = bytes
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_div(3))
        .and_then(|length| length.checked_mul(4))
        .ok_or(AudioError::InvalidData("picture base64 length overflow"))?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(capacity)
        .map_err(|_| AudioError::InvalidData("picture base64 allocation failed"))?;
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        encoded.push(BASE64[usize::from(a >> 2)] as char);
        encoded.push(BASE64[usize::from(((a & 3) << 4) | (b >> 4))] as char);
        encoded.push(if chunk.len() > 1 {
            BASE64[usize::from(((b & 15) << 2) | (c >> 6))] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            BASE64[usize::from(c & 63)] as char
        } else {
            '='
        });
    }
    Ok(encoded)
}

fn decode_base64(value: &str) -> Result<Vec<u8>, AudioError> {
    let bytes = value.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(AudioError::InvalidData("invalid picture base64 length"));
    }
    let padding = if bytes.ends_with(b"==") {
        2
    } else {
        usize::from(bytes.ends_with(b"="))
    };
    let output_len = (bytes.len() / 4)
        .checked_mul(3)
        .and_then(|length| length.checked_sub(padding))
        .ok_or(AudioError::InvalidData("picture base64 length overflow"))?;
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(output_len)
        .map_err(|_| AudioError::InvalidData("picture base64 allocation failed"))?;
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let a = base64_value(chunk[0]).ok_or(AudioError::InvalidData("invalid picture base64"))?;
        let b = base64_value(chunk[1]).ok_or(AudioError::InvalidData("invalid picture base64"))?;
        let c = if chunk[2] == b'=' && last {
            if chunk[3] != b'=' || b & 15 != 0 {
                return Err(AudioError::InvalidData("invalid picture base64 padding"));
            }
            None
        } else {
            Some(base64_value(chunk[2]).ok_or(AudioError::InvalidData("invalid picture base64"))?)
        };
        let d = if chunk[3] == b'=' && last {
            if c.is_some_and(|value| value & 3 != 0) {
                return Err(AudioError::InvalidData("invalid picture base64 padding"));
            }
            None
        } else {
            if c.is_none() {
                return Err(AudioError::InvalidData("invalid picture base64 padding"));
            }
            Some(base64_value(chunk[3]).ok_or(AudioError::InvalidData("invalid picture base64"))?)
        };
        decoded.push((a << 2) | (b >> 4));
        if let Some(c) = c {
            decoded.push((b << 4) | (c >> 2));
            if let Some(d) = d {
                decoded.push((c << 6) | d);
            }
        }
    }
    if decoded.len() != output_len {
        return Err(AudioError::InvalidData("invalid picture base64 padding"));
    }
    Ok(decoded)
}

fn base64_value(byte: u8) -> Option<u8> {
    BASE64
        .iter()
        .position(|&value| value == byte)
        .map(|value| value as u8)
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

fn read_u32(packet: &[u8], cursor: &mut usize) -> Result<u32, CommentHeaderParseError> {
    let end = cursor
        .checked_add(4)
        .filter(|&end| end <= packet.len())
        .ok_or(CommentHeaderParseError::Truncated)?;
    let value = u32::from_le_bytes(packet[*cursor..end].try_into().unwrap());
    *cursor = end;
    Ok(value)
}

fn read_utf8<'a>(packet: &'a [u8], cursor: &mut usize) -> Result<&'a str, CommentHeaderParseError> {
    let length = usize::try_from(read_u32(packet, cursor)?)
        .map_err(|_| AudioError::InvalidFormat("Vorbis comment length overflow"))?;
    let end = cursor
        .checked_add(length)
        .filter(|&end| end <= packet.len())
        .ok_or(CommentHeaderParseError::Truncated)?;
    let value = std::str::from_utf8(&packet[*cursor..end]).map_err(|_| {
        CommentHeaderParseError::Audio(AudioError::InvalidFormat(
            "Vorbis comment field is not UTF-8",
        ))
    })?;
    *cursor = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use zemer_core::Metadata;
    use zemer_core::Picture;

    use super::{VorbisCommentHeader, VorbisIdentificationHeader, encode_comment_header};
    use super::{decode_base64, encode_base64};

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
    fn stream_parse_ignores_truncated_comments_but_rejects_invalid_framing() {
        let complete = comment(&["TITLE=Song"]);
        for length in 0..complete.len() {
            let truncated = &complete[..length];
            let comments = VorbisCommentHeader::parse_for_stream(truncated)
                .unwrap_or_else(|_| panic!("truncated comment prefix of length {length} failed"));
            assert!(comments.metadata().iter().next().is_none());
            assert!(VorbisCommentHeader::parse(truncated).is_err());
        }

        let mut unframed = comment(&[]);
        *unframed.last_mut().unwrap() = 0;
        assert!(VorbisCommentHeader::parse_for_stream(&unframed).is_err());
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
    fn round_trips_embedded_pictures_as_flac_picture_comments() {
        let picture = Picture::new(3, "image/jpeg", "front", 640, 480, 24, 0, [1, 2, 3, 4]);
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "Artwork test");
        metadata.add_picture(picture.clone());

        let encoded = encode_comment_header("Zemer", &metadata).unwrap();
        assert!(
            std::str::from_utf8(&encoded)
                .unwrap()
                .contains("METADATA_BLOCK_PICTURE=")
        );
        let parsed = VorbisCommentHeader::parse(&encoded).unwrap();
        assert_eq!(parsed.metadata().values("TITLE"), ["Artwork test"]);
        assert_eq!(parsed.metadata().pictures(), [picture]);
    }

    #[test]
    fn rejects_malformed_embedded_picture_comments() {
        for value in ["!", "AAAA", "===="] {
            let packet = comment(&[&format!("METADATA_BLOCK_PICTURE={value}")]);
            assert!(VorbisCommentHeader::parse(&packet).is_err(), "{value}");
        }

        let mut metadata = Metadata::default();
        metadata.insert("METADATA_BLOCK_PICTURE", "caller-supplied opaque value");
        assert!(encode_comment_header("Zemer", &metadata).is_err());
    }

    #[test]
    fn base64_round_trips_all_padding_shapes() {
        for length in 0..10 {
            let bytes: Vec<u8> = (0..length)
                .map(|value| (value as u8).wrapping_mul(37))
                .collect();
            assert_eq!(
                decode_base64(&encode_base64(&bytes).unwrap()).unwrap(),
                bytes
            );
        }
        for value in ["AB==", "AAB=", "AA=A", "A===", "AA"] {
            assert!(decode_base64(value).is_err(), "{value}");
        }
    }

    #[test]
    fn rejects_unrepresentable_comment_names() {
        let mut metadata = Metadata::default();
        metadata.insert("BAD=KEY", "value");
        assert!(encode_comment_header("Zemer", &metadata).is_err());
    }
}
