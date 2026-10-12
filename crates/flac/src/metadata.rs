use std::io::Read;

use zemer_core::{AudioError, Metadata};

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
) -> Result<(FlacStreamInfo, Metadata), AudioError> {
    let mut signature = [0_u8; 4];
    reader.read_exact(&mut signature)?;
    if &signature != b"fLaC" {
        return Err(AudioError::InvalidFormat("missing native FLAC signature"));
    }

    let mut stream_info = None;
    let mut metadata = Metadata::default();
    let mut first = true;
    let mut has_comments = false;
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
            _ => {}
        }
        if last {
            break;
        }
    }
    stream_info
        .map(|info| (info, metadata))
        .ok_or(AudioError::InvalidFormat("missing FLAC STREAMINFO block"))
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
                .any(|byte| !(0x20..=0x7d).contains(&byte) || byte == b'=')
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
