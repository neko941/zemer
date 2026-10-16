use std::io::Read;

use zemer_core::{AudioError, Metadata, Picture};

pub(crate) const MAX_METADATA_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlacMetadataBlock {
    kind: u8,
    data: Vec<u8>,
}

impl FlacMetadataBlock {
    pub fn kind(&self) -> u8 {
        self.kind
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn picture(&self) -> Result<Option<FlacPicture<'_>>, AudioError> {
        (self.kind == 6)
            .then(|| parse_picture(&self.data))
            .transpose()
    }

    pub fn cuesheet(&self) -> Result<Option<FlacCueSheet>, AudioError> {
        (self.kind == 5)
            .then(|| parse_cuesheet(&self.data))
            .transpose()
    }

    pub fn seektable(&self) -> Result<Option<FlacSeekTable<'_>>, AudioError> {
        (self.kind == 3)
            .then(|| parse_seektable(&self.data))
            .transpose()
    }

    pub fn application(&self) -> Result<Option<FlacApplication<'_>>, AudioError> {
        (self.kind == 2)
            .then(|| parse_application(&self.data))
            .transpose()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlacPicture<'a> {
    pub picture_type: u32,
    pub media_type: &'a str,
    pub description: &'a str,
    pub width: u32,
    pub height: u32,
    pub color_depth: u32,
    pub colors: u32,
    pub data: &'a [u8],
}

impl FlacPicture<'_> {
    pub fn is_uri(&self) -> bool {
        self.media_type == "-->"
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlacCueSheet {
    pub media_catalog_number: [u8; 128],
    pub lead_in_samples: u64,
    pub is_cd_audio: bool,
    pub tracks: Vec<FlacCueTrack>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlacCueTrack {
    pub offset_samples: u64,
    pub number: u8,
    pub isrc: [u8; 12],
    pub is_audio: bool,
    pub pre_emphasis: bool,
    pub indices: Vec<FlacCueIndex>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlacCueIndex {
    pub offset_samples: u64,
    pub number: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlacApplication<'a> {
    pub id: [u8; 4],
    pub data: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlacSeekTable<'a> {
    data: &'a [u8],
}

impl FlacSeekTable<'_> {
    pub fn points(&self) -> impl Iterator<Item = FlacSeekPoint> + '_ {
        self.data.chunks_exact(18).map(|point| FlacSeekPoint {
            sample_number: u64::from_be_bytes(point[..8].try_into().unwrap()),
            stream_offset: u64::from_be_bytes(point[8..16].try_into().unwrap()),
            frame_samples: u16::from_be_bytes(point[16..18].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlacSeekPoint {
    pub sample_number: u64,
    pub stream_offset: u64,
    pub frame_samples: u16,
}

impl FlacSeekPoint {
    pub fn is_placeholder(&self) -> bool {
        self.sample_number == u64::MAX
    }
}

fn parse_application(data: &[u8]) -> Result<FlacApplication<'_>, AudioError> {
    let mut cursor = 0;
    let id = take(data, &mut cursor, 4)?.try_into().unwrap();
    Ok(FlacApplication {
        id,
        data: &data[cursor..],
    })
}

fn parse_seektable(data: &[u8]) -> Result<FlacSeekTable<'_>, AudioError> {
    if !data.chunks_exact(18).remainder().is_empty() {
        return Err(AudioError::InvalidData("invalid FLAC seektable length"));
    }
    let mut previous_sample = 0;
    let mut has_previous = false;
    let mut has_placeholder = false;
    for point in data.chunks_exact(18) {
        let sample_number = u64::from_be_bytes(point[..8].try_into().unwrap());
        if sample_number == u64::MAX {
            has_placeholder = true;
        } else if has_placeholder || (has_previous && sample_number <= previous_sample) {
            return Err(AudioError::InvalidData(
                "FLAC seekpoints must be sorted and unique before placeholders",
            ));
        } else {
            previous_sample = sample_number;
            has_previous = true;
        }
    }
    Ok(FlacSeekTable { data })
}

fn parse_cuesheet(data: &[u8]) -> Result<FlacCueSheet, AudioError> {
    let mut cursor = 0;
    let media_catalog_number: [u8; 128] = take(data, &mut cursor, 128)?.try_into().unwrap();
    let mut zero_padding_started = false;
    if media_catalog_number.iter().any(|&byte| {
        if byte == 0 {
            zero_padding_started = true;
            false
        } else {
            zero_padding_started || !(0x20..=0x7e).contains(&byte)
        }
    }) {
        return Err(AudioError::InvalidData(
            "invalid FLAC cuesheet media catalog number",
        ));
    }
    let lead_in_samples = read_be_u64(data, &mut cursor)?;
    let is_cd_audio = match take(data, &mut cursor, 1)?[0] {
        flags if flags & 0x7f == 0 => flags & 0x80 != 0,
        _ => {
            return Err(AudioError::InvalidData(
                "nonzero FLAC cuesheet reserved bits",
            ));
        }
    };
    if take(data, &mut cursor, 258)?.iter().any(|&byte| byte != 0) {
        return Err(AudioError::InvalidData(
            "nonzero FLAC cuesheet reserved bits",
        ));
    }
    let track_count = usize::from(take(data, &mut cursor, 1)?[0]);
    if track_count == 0 || (is_cd_audio && track_count > 100) {
        return Err(AudioError::InvalidData("invalid FLAC cuesheet track count"));
    }

    let mut tracks = Vec::with_capacity(track_count);
    let mut track_numbers = [false; 256];
    for track_index in 0..track_count {
        let offset_samples = read_be_u64(data, &mut cursor)?;
        let number = take(data, &mut cursor, 1)?[0];
        let isrc: [u8; 12] = take(data, &mut cursor, 12)?.try_into().unwrap();
        if !isrc.iter().all(u8::is_ascii_alphanumeric) && isrc != [0; 12] {
            return Err(AudioError::InvalidData("invalid FLAC cuesheet track ISRC"));
        }
        let flags = take(data, &mut cursor, 1)?[0];
        if flags & 0x3f != 0 || take(data, &mut cursor, 13)?.iter().any(|&byte| byte != 0) {
            return Err(AudioError::InvalidData(
                "nonzero FLAC cuesheet reserved bits",
            ));
        }
        let index_count = usize::from(take(data, &mut cursor, 1)?[0]);
        let is_lead_out = track_index + 1 == track_count;
        let lead_out_number = if is_cd_audio { 170 } else { 255 };
        if number == 0
            || track_numbers[usize::from(number)]
            || (is_lead_out != (number == lead_out_number))
            || (!is_lead_out && index_count == 0)
            || (is_lead_out && index_count != 0)
            || (is_cd_audio && index_count > 100)
            || (is_cd_audio && offset_samples % 588 != 0)
        {
            return Err(AudioError::InvalidData("invalid FLAC cuesheet track"));
        }
        if is_cd_audio && !is_lead_out && !(1..=99).contains(&number) {
            return Err(AudioError::InvalidData("invalid FLAC CD-DA track number"));
        }
        track_numbers[usize::from(number)] = true;

        let mut indices: Vec<FlacCueIndex> = Vec::with_capacity(index_count);
        for index_position in 0..index_count {
            let offset_samples = read_be_u64(data, &mut cursor)?;
            let number = take(data, &mut cursor, 1)?[0];
            if take(data, &mut cursor, 3)?.iter().any(|&byte| byte != 0) {
                return Err(AudioError::InvalidData(
                    "nonzero FLAC cuesheet reserved bits",
                ));
            }
            if is_cd_audio
                && (offset_samples % 588 != 0
                    || (index_position == 0 && number > 1)
                    || (index_position != 0
                        && indices[index_position - 1].number.checked_add(1) != Some(number)))
            {
                return Err(AudioError::InvalidData("invalid FLAC CD-DA track index"));
            }
            indices.push(FlacCueIndex {
                offset_samples,
                number,
            });
        }
        tracks.push(FlacCueTrack {
            offset_samples,
            number,
            isrc,
            is_audio: flags & 0x80 == 0,
            pre_emphasis: flags & 0x40 != 0,
            indices,
        });
    }
    if cursor != data.len() {
        return Err(AudioError::InvalidData(
            "trailing bytes in FLAC cuesheet block",
        ));
    }

    Ok(FlacCueSheet {
        media_catalog_number,
        lead_in_samples,
        is_cd_audio,
        tracks,
    })
}

fn parse_picture(data: &[u8]) -> Result<FlacPicture<'_>, AudioError> {
    let mut cursor = 0;
    let picture_type = read_be_u32(data, &mut cursor)?;
    let media_type_length = read_be_u32(data, &mut cursor)?;
    let media_type_bytes = take(data, &mut cursor, media_type_length as usize)?;
    if media_type_bytes
        .iter()
        .any(|&byte| !(0x20..=0x7e).contains(&byte))
    {
        return Err(AudioError::InvalidData(
            "FLAC picture media type is not printable ASCII",
        ));
    }
    let media_type = std::str::from_utf8(media_type_bytes)
        .map_err(|_| AudioError::InvalidData("FLAC picture media type is not UTF-8"))?;
    let description_length = read_be_u32(data, &mut cursor)?;
    let description = std::str::from_utf8(take(data, &mut cursor, description_length as usize)?)
        .map_err(|_| AudioError::InvalidData("FLAC picture description is not UTF-8"))?;
    let width = read_be_u32(data, &mut cursor)?;
    let height = read_be_u32(data, &mut cursor)?;
    let color_depth = read_be_u32(data, &mut cursor)?;
    let colors = read_be_u32(data, &mut cursor)?;
    let picture_length = read_be_u32(data, &mut cursor)?;
    let picture_data = take(data, &mut cursor, picture_length as usize)?;
    if cursor != data.len() {
        return Err(AudioError::InvalidData(
            "trailing bytes in FLAC picture block",
        ));
    }

    Ok(FlacPicture {
        picture_type,
        media_type,
        description,
        width,
        height,
        color_depth,
        colors,
        data: picture_data,
    })
}

fn read_be_u32(data: &[u8], cursor: &mut usize) -> Result<u32, AudioError> {
    Ok(u32::from_be_bytes(
        take(data, cursor, 4)?.try_into().unwrap(),
    ))
}

fn read_be_u64(data: &[u8], cursor: &mut usize) -> Result<u64, AudioError> {
    Ok(u64::from_be_bytes(
        take(data, cursor, 8)?.try_into().unwrap(),
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlacStreamInfo {
    min_block_size: u16,
    max_block_size: u16,
    min_frame_size: u32,
    max_frame_size: u32,
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    total_samples: u64,
    md5: [u8; 16],
}

impl FlacStreamInfo {
    pub fn min_block_size(&self) -> u16 {
        self.min_block_size
    }

    pub fn max_block_size(&self) -> u16 {
        self.max_block_size
    }

    pub fn min_frame_size(&self) -> Option<u32> {
        (self.min_frame_size != 0).then_some(self.min_frame_size)
    }

    pub fn max_frame_size(&self) -> Option<u32> {
        (self.max_frame_size != 0).then_some(self.max_frame_size)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    pub fn bits_per_sample(&self) -> u8 {
        self.bits_per_sample
    }

    pub fn total_samples(&self) -> Option<u64> {
        (self.total_samples != 0).then_some(self.total_samples)
    }

    pub fn md5(&self) -> [u8; 16] {
        self.md5
    }
}

pub(crate) fn read_metadata<R: Read>(
    reader: &mut R,
) -> Result<(FlacStreamInfo, Metadata, Vec<FlacMetadataBlock>), AudioError> {
    let mut signature = [0_u8; 4];
    reader.read_exact(&mut signature)?;
    if &signature != b"fLaC" {
        return Err(AudioError::InvalidFormat("missing native FLAC signature"));
    }

    let mut stream_info = None;
    let mut metadata = Metadata::default();
    let mut blocks = Vec::new();
    let mut metadata_bytes = 0_usize;
    let mut first = true;
    let mut has_comments = false;
    let mut has_seektable = false;
    loop {
        let mut header = [0_u8; 4];
        reader.read_exact(&mut header)?;
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7f;
        let length =
            (usize::from(header[1]) << 16) | (usize::from(header[2]) << 8) | usize::from(header[3]);
        if kind == 127 {
            return Err(AudioError::InvalidFormat("reserved FLAC metadata type"));
        }
        if first && (kind != 0 || length != 34) {
            return Err(AudioError::InvalidFormat(
                "first FLAC metadata block must be 34-byte STREAMINFO",
            ));
        }
        first = false;

        metadata_bytes = checked_metadata_size(metadata_bytes, length)?;

        let mut data = vec![0_u8; length];
        reader.read_exact(&mut data)?;
        match kind {
            0 => {
                if stream_info.is_some() {
                    return Err(AudioError::InvalidFormat("duplicate FLAC STREAMINFO block"));
                }
                stream_info = Some(parse_stream_info(&data)?);
            }
            4 => {
                if has_comments {
                    return Err(AudioError::InvalidFormat(
                        "duplicate FLAC Vorbis comment block",
                    ));
                }
                has_comments = true;
                parse_vorbis_comments(&data, &mut metadata)?;
            }
            3 => {
                if has_seektable {
                    return Err(AudioError::InvalidFormat("duplicate FLAC seektable block"));
                }
                has_seektable = true;
            }
            _ => {}
        }
        blocks.push(FlacMetadataBlock { kind, data });
        if last {
            break;
        }
    }
    stream_info
        .map(|info| (info, metadata, blocks))
        .ok_or(AudioError::InvalidFormat("missing FLAC STREAMINFO block"))
}

pub(crate) fn encode_picture(picture: &Picture) -> Result<Vec<u8>, AudioError> {
    let size = validate_picture(picture)?;
    let media_type_length = u32::try_from(picture.media_type().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture media type is too large"))?;
    let description_length = u32::try_from(picture.description().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture description is too large"))?;
    let data_length = u32::try_from(picture.data().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture data is too large"))?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(size)
        .map_err(|_| AudioError::InvalidData("FLAC picture allocation failed"))?;
    encoded.extend_from_slice(&picture.picture_type().to_be_bytes());
    encoded.extend_from_slice(&media_type_length.to_be_bytes());
    encoded.extend_from_slice(picture.media_type().as_bytes());
    encoded.extend_from_slice(&description_length.to_be_bytes());
    encoded.extend_from_slice(picture.description().as_bytes());
    for value in [
        picture.dimensions().0,
        picture.dimensions().1,
        picture.color_depth(),
        picture.colors(),
        data_length,
    ] {
        encoded.extend_from_slice(&value.to_be_bytes());
    }
    encoded.extend_from_slice(picture.data());
    Ok(encoded)
}

pub(crate) fn validate_picture(picture: &Picture) -> Result<usize, AudioError> {
    if !picture.media_type().is_ascii() {
        return Err(AudioError::InvalidData(
            "FLAC picture media type must be ASCII",
        ));
    }
    u32::try_from(picture.media_type().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture media type is too large"))?;
    u32::try_from(picture.description().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture description is too large"))?;
    u32::try_from(picture.data().len())
        .map_err(|_| AudioError::InvalidData("FLAC picture data is too large"))?;
    32_usize
        .checked_add(picture.media_type().len())
        .and_then(|size| size.checked_add(picture.description().len()))
        .and_then(|size| size.checked_add(picture.data().len()))
        .filter(|&size| size <= 0x00ff_ffff)
        .ok_or(AudioError::InvalidData("FLAC picture block is too large"))
}

pub(crate) fn checked_metadata_size(current: usize, next: usize) -> Result<usize, AudioError> {
    let retained_size = std::mem::size_of::<FlacMetadataBlock>()
        .checked_add(next)
        .ok_or(AudioError::InvalidData(
            "FLAC metadata exceeds the 128 MiB limit",
        ))?;
    current
        .checked_add(retained_size)
        .filter(|&total| total <= MAX_METADATA_BYTES)
        .ok_or(AudioError::InvalidData(
            "FLAC metadata exceeds the 128 MiB limit",
        ))
}

fn parse_stream_info(data: &[u8]) -> Result<FlacStreamInfo, AudioError> {
    if data.len() != 34 {
        return Err(AudioError::InvalidFormat("invalid FLAC STREAMINFO length"));
    }
    let min_block_size = u16::from_be_bytes([data[0], data[1]]);
    let max_block_size = u16::from_be_bytes([data[2], data[3]]);
    let min_frame_size =
        (u32::from(data[4]) << 16) | (u32::from(data[5]) << 8) | u32::from(data[6]);
    let max_frame_size =
        (u32::from(data[7]) << 16) | (u32::from(data[8]) << 8) | u32::from(data[9]);
    let packed = u64::from_be_bytes(data[10..18].try_into().unwrap());
    let sample_rate = (packed >> 44) as u32;
    let channels = ((packed >> 41) & 0x7) as u8 + 1;
    let bits_per_sample = ((packed >> 36) & 0x1f) as u8 + 1;
    let total_samples = packed & 0x0f_ffff_ffff;
    let mut md5 = [0_u8; 16];
    md5.copy_from_slice(&data[18..34]);

    if !(16..=65_535).contains(&min_block_size)
        || max_block_size < min_block_size
        || sample_rate == 0
        || !(4..=32).contains(&bits_per_sample)
        || (min_frame_size != 0 && max_frame_size != 0 && min_frame_size > max_frame_size)
    {
        return Err(AudioError::InvalidData("invalid FLAC STREAMINFO values"));
    }
    Ok(FlacStreamInfo {
        min_block_size,
        max_block_size,
        min_frame_size,
        max_frame_size,
        sample_rate,
        channels,
        bits_per_sample,
        total_samples,
        md5,
    })
}

fn parse_vorbis_comments(data: &[u8], metadata: &mut Metadata) -> Result<(), AudioError> {
    let mut cursor = 0_usize;
    let vendor_len = read_le_len(data, &mut cursor)?;
    let vendor = take(data, &mut cursor, vendor_len)?;
    std::str::from_utf8(vendor)
        .map_err(|_| AudioError::InvalidData("FLAC comment vendor is not UTF-8"))?;
    let count = read_le_len(data, &mut cursor)?;
    if count > data.len().saturating_sub(cursor) / 4 {
        return Err(AudioError::InvalidData(
            "FLAC comment count exceeds block length",
        ));
    }
    for _ in 0..count {
        let length = read_le_len(data, &mut cursor)?;
        let field = std::str::from_utf8(take(data, &mut cursor, length)?)
            .map_err(|_| AudioError::InvalidData("FLAC comment field is not UTF-8"))?;
        let (key, value) = field.split_once('=').ok_or(AudioError::InvalidData(
            "FLAC comment field has no value separator",
        ))?;
        if key.is_empty()
            || key
                .bytes()
                .any(|byte| !(0x20..=0x7e).contains(&byte) || byte == b'=')
        {
            return Err(AudioError::InvalidData("invalid FLAC comment field name"));
        }
        metadata.insert(key, value.to_owned());
    }
    if cursor != data.len() {
        return Err(AudioError::InvalidData(
            "trailing bytes in FLAC comment block",
        ));
    }
    Ok(())
}

fn read_le_len(data: &[u8], cursor: &mut usize) -> Result<usize, AudioError> {
    let bytes = take(data, cursor, 4)?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
}

fn take<'a>(data: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], AudioError> {
    let end = cursor
        .checked_add(length)
        .filter(|&end| end <= data.len())
        .ok_or(AudioError::InvalidData(
            "FLAC metadata field exceeds block length",
        ))?;
    let result = &data[*cursor..end];
    *cursor = end;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{AudioError, Metadata};

    use super::{
        FlacMetadataBlock, MAX_METADATA_BYTES, checked_metadata_size, parse_application,
        parse_cuesheet, parse_picture, parse_seektable, parse_vorbis_comments,
    };
    use crate::FlacDecoder;

    fn stream_info() -> [u8; 34] {
        let mut data = [0_u8; 34];
        data[..2].copy_from_slice(&4096_u16.to_be_bytes());
        data[2..4].copy_from_slice(&4096_u16.to_be_bytes());
        let packed = (44_100_u64 << 44) | (1 << 41) | (15 << 36) | 1;
        data[10..18].copy_from_slice(&packed.to_be_bytes());
        data
    }

    fn block(kind: u8, last: bool, payload: &[u8]) -> Vec<u8> {
        let length = payload.len();
        let mut data = vec![
            kind | if last { 0x80 } else { 0 },
            (length >> 16) as u8,
            (length >> 8) as u8,
            length as u8,
        ];
        data.extend_from_slice(payload);
        data
    }

    #[test]
    fn retains_raw_application_and_unknown_metadata_blocks() {
        let app = [0x5a, 0xa5, 1, 2];
        let unknown = [7, 0, 0xff];
        let mut file = b"fLaC".to_vec();
        file.extend(block(0, false, &stream_info()));
        file.extend(block(2, false, &app));
        file.extend(block(11, true, &unknown));

        let decoder = FlacDecoder::new(Cursor::new(file)).unwrap();
        let blocks = decoder.metadata_blocks();
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[1],
            FlacMetadataBlock {
                kind: 2,
                data: app.to_vec()
            }
        );
        assert_eq!(
            blocks[2],
            FlacMetadataBlock {
                kind: 11,
                data: unknown.to_vec()
            }
        );
    }

    #[test]
    fn rejects_duplicate_seektable_metadata_blocks() {
        let mut file = b"fLaC".to_vec();
        file.extend(block(0, false, &stream_info()));
        file.extend(block(3, false, &[]));
        file.extend(block(3, true, &[]));
        assert!(matches!(
            FlacDecoder::new(Cursor::new(file)),
            Err(AudioError::InvalidFormat("duplicate FLAC seektable block"))
        ));
    }

    #[test]
    fn metadata_size_limit_is_checked_without_overflow() {
        let block_size = std::mem::size_of::<FlacMetadataBlock>();
        assert_eq!(checked_metadata_size(10, 20).unwrap(), 30 + block_size);
        assert_eq!(
            checked_metadata_size(MAX_METADATA_BYTES - block_size, 0).unwrap(),
            MAX_METADATA_BYTES
        );
        assert!(checked_metadata_size(MAX_METADATA_BYTES, 0).is_err());
        assert!(checked_metadata_size(usize::MAX, 1).is_err());
    }

    fn picture_block() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&3_u32.to_be_bytes());
        data.extend_from_slice(&9_u32.to_be_bytes());
        data.extend_from_slice(b"image/png");
        let description = "כריכה קדמית".as_bytes();
        data.extend_from_slice(&(description.len() as u32).to_be_bytes());
        data.extend_from_slice(description);
        for field in [600_u32, 600, 24, 0] {
            data.extend_from_slice(&field.to_be_bytes());
        }
        data.extend_from_slice(&4_u32.to_be_bytes());
        data.extend_from_slice(b"PNG!");
        data
    }

    fn cuesheet_block(is_cd_audio: bool) -> Vec<u8> {
        let mut data = vec![0; 128];
        data.extend_from_slice(&0_u64.to_be_bytes());
        data.push(u8::from(is_cd_audio) << 7);
        data.extend_from_slice(&[0; 258]);
        data.push(2);
        for (offset, number, index_count) in [
            (0_u64, 1_u8, 1_u8),
            (11_760, if is_cd_audio { 170 } else { 255 }, 0),
        ] {
            data.extend_from_slice(&offset.to_be_bytes());
            data.push(number);
            data.extend_from_slice(&[0; 12]);
            data.push(0);
            data.extend_from_slice(&[0; 13]);
            data.push(index_count);
            if index_count != 0 {
                data.extend_from_slice(&0_u64.to_be_bytes());
                data.push(1);
                data.extend_from_slice(&[0; 3]);
            }
        }
        data
    }

    #[test]
    fn parses_cuesheet_tracks_and_indices() {
        let bytes = cuesheet_block(false);
        let block = FlacMetadataBlock {
            kind: 5,
            data: bytes,
        };
        let sheet = block.cuesheet().unwrap().unwrap();
        assert!(!sheet.is_cd_audio);
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].number, 1);
        assert!(sheet.tracks[0].is_audio);
        assert_eq!(
            sheet.tracks[0].indices,
            [super::FlacCueIndex {
                offset_samples: 0,
                number: 1
            }]
        );
        assert_eq!(sheet.tracks[1].number, 255);
        assert_eq!(sheet.tracks[1].indices, []);
        assert_eq!(
            FlacMetadataBlock {
                kind: 6,
                data: vec![]
            }
            .cuesheet()
            .unwrap(),
            None
        );
    }

    #[test]
    fn parses_cd_da_cuesheet_with_sector_aligned_offsets() {
        let mut bytes = cuesheet_block(true);
        bytes[0..13].copy_from_slice(b"1234567890123");
        bytes[128..136].copy_from_slice(&88_200_u64.to_be_bytes());
        let sheet = parse_cuesheet(&bytes).unwrap();
        assert!(sheet.is_cd_audio);
        assert_eq!(&sheet.media_catalog_number[..13], b"1234567890123");
        assert_eq!(sheet.lead_in_samples, 88_200);
        assert_eq!(sheet.tracks.last().unwrap().number, 170);
    }

    #[test]
    fn rejects_truncated_and_invalid_cuesheets() {
        let valid = cuesheet_block(false);
        for length in 0..valid.len() {
            assert!(
                parse_cuesheet(&valid[..length]).is_err(),
                "accepted {length} bytes"
            );
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(parse_cuesheet(&trailing).is_err());

        let mut bad_reserved = valid.clone();
        bad_reserved[137] = 1;
        assert!(parse_cuesheet(&bad_reserved).is_err());

        let mut duplicate_track = valid.clone();
        duplicate_track[396 + 36 + 12 + 8] = 1;
        assert!(parse_cuesheet(&duplicate_track).is_err());

        let mut bad_lead_out = valid.clone();
        bad_lead_out[396 + 36 + 12 + 8] = 254;
        assert!(parse_cuesheet(&bad_lead_out).is_err());
    }

    #[test]
    fn rejects_misaligned_cd_da_offsets_and_bad_isrc() {
        let mut misaligned = cuesheet_block(true);
        misaligned[396 + 36] = 2;
        assert!(parse_cuesheet(&misaligned).is_err());

        let mut bad_isrc = cuesheet_block(false);
        bad_isrc[396 + 9] = b'!';
        assert!(parse_cuesheet(&bad_isrc).is_err());
    }

    fn seekpoint(sample_number: u64, stream_offset: u64, frame_samples: u16) -> [u8; 18] {
        let mut point = [0; 18];
        point[..8].copy_from_slice(&sample_number.to_be_bytes());
        point[8..16].copy_from_slice(&stream_offset.to_be_bytes());
        point[16..].copy_from_slice(&frame_samples.to_be_bytes());
        point
    }

    #[test]
    fn parses_seektable_points_without_copying_and_keeps_placeholders() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&seekpoint(0, 0, 4096));
        bytes.extend_from_slice(&seekpoint(8192, 735, 2048));
        bytes.extend_from_slice(&seekpoint(u64::MAX, 0x1234, 77));
        bytes.extend_from_slice(&seekpoint(u64::MAX, 0xabcd, 88));
        let block = FlacMetadataBlock {
            kind: 3,
            data: bytes,
        };
        let table = block.seektable().unwrap().unwrap();
        let points: Vec<_> = table.points().collect();
        assert_eq!(
            points,
            [
                super::FlacSeekPoint {
                    sample_number: 0,
                    stream_offset: 0,
                    frame_samples: 4096,
                },
                super::FlacSeekPoint {
                    sample_number: 8192,
                    stream_offset: 735,
                    frame_samples: 2048,
                },
                super::FlacSeekPoint {
                    sample_number: u64::MAX,
                    stream_offset: 0x1234,
                    frame_samples: 77,
                },
                super::FlacSeekPoint {
                    sample_number: u64::MAX,
                    stream_offset: 0xabcd,
                    frame_samples: 88,
                },
            ]
        );
        assert!(points[2].is_placeholder());
        assert_eq!(table.data.as_ptr(), block.data().as_ptr());
        assert_eq!(
            FlacMetadataBlock {
                kind: 6,
                data: vec![]
            }
            .seektable()
            .unwrap(),
            None
        );
        assert_eq!(parse_seektable(&[]).unwrap().points().count(), 0);
    }

    #[test]
    fn rejects_unsorted_duplicate_or_misplaced_flac_seekpoints() {
        let unsorted = [seekpoint(4, 0, 1), seekpoint(3, 1, 1)].concat();
        assert!(parse_seektable(&unsorted).is_err());
        let duplicate = [seekpoint(4, 0, 1), seekpoint(4, 1, 1)].concat();
        assert!(parse_seektable(&duplicate).is_err());
        let placeholder_then_point = [seekpoint(u64::MAX, 0, 0), seekpoint(4, 1, 1)].concat();
        assert!(parse_seektable(&placeholder_then_point).is_err());
        assert!(parse_seektable(&[0; 17]).is_err());
    }

    #[test]
    fn parses_application_id_and_borrows_payload() {
        let block = FlacMetadataBlock {
            kind: 2,
            data: b"TESTpayload".to_vec(),
        };
        let application = block.application().unwrap().unwrap();
        assert_eq!(&application.id, b"TEST");
        assert_eq!(application.data, b"payload");
        assert_eq!(application.data.as_ptr(), block.data()[4..].as_ptr());
        assert!(parse_application(&[1, 2, 3]).is_err());
        assert_eq!(
            FlacMetadataBlock {
                kind: 6,
                data: vec![]
            }
            .application()
            .unwrap(),
            None
        );
    }

    #[test]
    fn parses_picture_fields_and_borrows_embedded_data() {
        let bytes = picture_block();
        let block = FlacMetadataBlock {
            kind: 6,
            data: bytes,
        };
        let picture = block.picture().unwrap().unwrap();
        assert_eq!(picture.picture_type, 3);
        assert_eq!(picture.media_type, "image/png");
        assert_eq!(picture.description, "כריכה קדמית");
        assert_eq!(
            (picture.width, picture.height, picture.color_depth),
            (600, 600, 24)
        );
        assert_eq!(picture.colors, 0);
        assert_eq!(picture.data, b"PNG!");
        assert!(!picture.is_uri());
        assert_eq!(
            FlacMetadataBlock {
                kind: 2,
                data: Vec::new()
            }
            .picture()
            .unwrap(),
            None
        );
    }

    #[test]
    fn picture_uri_is_reported_without_fetching_it() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&99_u32.to_be_bytes());
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(b"-->");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&9_u32.to_be_bytes());
        bytes.extend_from_slice(b"cover.jpg");

        let picture = parse_picture(&bytes).unwrap();
        assert_eq!(picture.picture_type, 99);
        assert!(picture.is_uri());
        assert_eq!(picture.data, b"cover.jpg");
    }

    #[test]
    fn rejects_truncated_and_malformed_picture_payloads() {
        let valid = picture_block();
        for length in 0..valid.len() {
            assert!(
                parse_picture(&valid[..length]).is_err(),
                "accepted {length} bytes"
            );
        }

        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(matches!(
            parse_picture(&trailing),
            Err(AudioError::InvalidData(
                "trailing bytes in FLAC picture block"
            ))
        ));

        let mut bad_mime = valid.clone();
        bad_mime[11] = 0x80;
        assert!(matches!(
            parse_picture(&bad_mime),
            Err(AudioError::InvalidData(
                "FLAC picture media type is not printable ASCII"
            ))
        ));
    }

    #[test]
    fn accepts_the_full_printable_ascii_range_in_comment_names() {
        let field = b"~=value";
        let mut block = Vec::from(0_u32.to_le_bytes());
        block.extend_from_slice(&1_u32.to_le_bytes());
        block.extend_from_slice(&(field.len() as u32).to_le_bytes());
        block.extend_from_slice(field);

        let mut metadata = Metadata::default();
        parse_vorbis_comments(&block, &mut metadata).unwrap();
        assert_eq!(metadata.values("~"), ["value"]);
    }

    #[test]
    fn rejects_nonprintable_ascii_in_comment_names() {
        let field = b"A\x1f=value";
        let mut block = Vec::from(0_u32.to_le_bytes());
        block.extend_from_slice(&1_u32.to_le_bytes());
        block.extend_from_slice(&(field.len() as u32).to_le_bytes());
        block.extend_from_slice(field);

        assert!(matches!(
            parse_vorbis_comments(&block, &mut Metadata::default()),
            Err(AudioError::InvalidData("invalid FLAC comment field name"))
        ));
    }
}
