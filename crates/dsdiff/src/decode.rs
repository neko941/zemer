use std::io::{Read, Seek, SeekFrom};

use zemer_core::{AudioError, DsdBuffer, DsdDecoder, DsdSpec, Metadata};

const MAX_CHANNELS: u16 = 32;
const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;

/// Streams uncompressed DSDIFF audio without converting it to PCM.
pub struct DsdiffDecoder<R> {
    reader: R,
    spec: DsdSpec,
    metadata: Metadata,
    data_start: u64,
    data_bytes: u64,
    read_bytes: u64,
}

impl<R: Read + Seek> DsdiffDecoder<R> {
    pub fn new(mut reader: R) -> Result<Self, AudioError> {
        let start = reader.stream_position()?;
        let file_end = reader.seek(SeekFrom::End(0))?;
        if start
            .checked_add(16)
            .is_none_or(|minimum| file_end < minimum)
        {
            return Err(AudioError::InvalidFormat("truncated DSDIFF file"));
        }
        reader.seek(SeekFrom::Start(start))?;
        let mut header = [0; 12];
        reader.read_exact(&mut header)?;
        let form_size = u64::from_be_bytes(header[4..12].try_into().unwrap());
        let form_end = start
            .checked_add(12)
            .and_then(|value| value.checked_add(form_size))
            .filter(|&value| value == file_end)
            .ok_or(AudioError::InvalidFormat("invalid DSDIFF FORM size"))?;
        if &header[..4] != b"FRM8" || form_size < 4 || form_size & 1 != 0 {
            return Err(AudioError::InvalidFormat("invalid DSDIFF FORM header"));
        }
        let mut form_type = [0; 4];
        reader.read_exact(&mut form_type)?;
        if &form_type != b"DSD " {
            return Err(AudioError::Unsupported("unsupported DSDIFF FORM type"));
        }

        let mut first_chunk = true;
        let mut saw_version = false;
        let mut properties = None;
        let mut data = None;
        let mut saw_di_in = false;
        let mut metadata = Metadata::default();
        while reader.stream_position()? < form_end {
            let chunk = read_chunk_header(&mut reader, form_end)?;
            match &chunk.id {
                b"FVER" => {
                    if !first_chunk || saw_version || chunk.size != 4 {
                        return Err(AudioError::InvalidFormat("invalid DSDIFF FVER chunk"));
                    }
                    let mut version = [0; 4];
                    reader.read_exact(&mut version)?;
                    if version[0] != 1 || version[2..] != [0, 0] {
                        return Err(AudioError::Unsupported(
                            "unsupported DSDIFF major format version",
                        ));
                    }
                    saw_version = true;
                }
                b"PROP" => {
                    if !saw_version || properties.is_some() || data.is_some() {
                        return Err(AudioError::InvalidFormat("misplaced DSDIFF PROP chunk"));
                    }
                    properties = Some(read_properties(&mut reader, chunk)?);
                }
                b"DSD " => {
                    if data.is_some() || properties.is_none() {
                        return Err(AudioError::InvalidFormat("misplaced DSDIFF DSD chunk"));
                    }
                    if properties.as_ref().unwrap().compression != *b"DSD " {
                        return Err(AudioError::InvalidFormat(
                            "DSDIFF sound chunk disagrees with its compression type",
                        ));
                    }
                    let channels = properties.as_ref().unwrap().channels.len() as u64;
                    if chunk.size == 0 || chunk.size % channels != 0 {
                        return Err(AudioError::InvalidFormat(
                            "DSDIFF DSD data is empty or has a partial channel frame",
                        ));
                    }
                    if chunk.size & 1 != 0 {
                        reader.seek(SeekFrom::Start(chunk.end))?;
                        let mut padding = [0];
                        reader.read_exact(&mut padding)?;
                        if padding[0] != 0 {
                            return Err(AudioError::InvalidFormat(
                                "invalid DSDIFF sound-data padding byte",
                            ));
                        }
                    }
                    data = Some((chunk.payload_start, chunk.size));
                }
                b"DST " => {
                    return Err(AudioError::Unsupported(
                        "DST-compressed DSDIFF is not supported",
                    ));
                }
                b"DIIN" => {
                    if saw_di_in {
                        return Err(AudioError::InvalidFormat("duplicate DSDIFF DIIN chunk"));
                    }
                    read_di_in(&mut reader, chunk, &mut metadata)?;
                    saw_di_in = true;
                }
                _ => {}
            }
            first_chunk = false;
            reader.seek(SeekFrom::Start(chunk.padded_end))?;
        }
        if !saw_version || reader.stream_position()? != form_end {
            return Err(AudioError::InvalidFormat("truncated DSDIFF chunk sequence"));
        }
        let properties = properties.ok_or(AudioError::InvalidFormat(
            "DSDIFF is missing its sound properties",
        ))?;
        let (data_start, data_bytes) = data.ok_or(AudioError::InvalidFormat(
            "DSDIFF is missing its DSD sound data",
        ))?;
        for channel in &properties.channels {
            metadata.insert(
                "DSDIFF_CHANNEL_ID",
                String::from_utf8_lossy(channel).into_owned(),
            );
        }
        let spec = DsdSpec::new_with_speaker_mask(
            properties.sample_rate,
            properties.channels.len() as u16,
            speaker_mask(&properties.channels),
        )
        .map_err(|error| AudioError::InvalidData(error.0))?;
        reader.seek(SeekFrom::Start(data_start))?;
        Ok(Self {
            reader,
            spec,
            metadata,
            data_start,
            data_bytes,
            read_bytes: 0,
        })
    }
}

impl<R: Read + Seek> DsdDecoder for DsdiffDecoder<R> {
    fn dsd_spec(&self) -> DsdSpec {
        self.spec
    }

    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn sample_count(&self) -> Option<u64> {
        self.data_bytes
            .checked_div(u64::from(self.spec.channels()))?
            .checked_mul(8)
    }

    fn read_dsd_octets(
        &mut self,
        max_octet_frames: usize,
    ) -> Result<Option<DsdBuffer>, AudioError> {
        if max_octet_frames == 0 {
            return Err(AudioError::InvalidData(
                "max_octet_frames must be greater than zero",
            ));
        }
        if self.read_bytes == self.data_bytes {
            return Ok(None);
        }
        let channels = usize::from(self.spec.channels());
        let frames = max_octet_frames.min(
            usize::try_from((self.data_bytes - self.read_bytes) / channels as u64)
                .unwrap_or(usize::MAX),
        );
        let byte_count = frames
            .checked_mul(channels)
            .ok_or(AudioError::InvalidData("DSDIFF output block size overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| AudioError::InvalidData("DSDIFF output block is too large"))?;
        bytes.resize(byte_count, 0);
        self.reader.seek(SeekFrom::Start(
            self.data_start
                .checked_add(self.read_bytes)
                .ok_or(AudioError::InvalidData("DSDIFF audio position overflow"))?,
        ))?;
        self.reader.read_exact(&mut bytes)?;
        self.read_bytes += byte_count as u64;
        Ok(Some(
            DsdBuffer::new(self.spec, bytes).map_err(|error| AudioError::InvalidData(error.0))?,
        ))
    }
}

struct Properties {
    sample_rate: u32,
    channels: Vec<[u8; 4]>,
    compression: [u8; 4],
}

#[derive(Clone, Copy)]
struct Chunk {
    id: [u8; 4],
    size: u64,
    payload_start: u64,
    end: u64,
    padded_end: u64,
}

fn read_chunk_header<R: Read + Seek>(reader: &mut R, parent_end: u64) -> Result<Chunk, AudioError> {
    let position = reader.stream_position()?;
    let mut header = [0; 12];
    if parent_end.saturating_sub(position) < header.len() as u64 {
        return Err(AudioError::InvalidFormat("truncated DSDIFF chunk header"));
    }
    reader.read_exact(&mut header)?;
    let size = u64::from_be_bytes(header[4..12].try_into().unwrap());
    let payload_start = position + 12;
    let end = payload_start
        .checked_add(size)
        .filter(|&value| value <= parent_end)
        .ok_or(AudioError::InvalidFormat("DSDIFF chunk exceeds its parent"))?;
    let padded_end = end
        .checked_add(size & 1)
        .filter(|&value| value <= parent_end)
        .ok_or(AudioError::InvalidFormat(
            "DSDIFF chunk padding exceeds its parent",
        ))?;
    Ok(Chunk {
        id: header[..4].try_into().unwrap(),
        size,
        payload_start,
        end,
        padded_end,
    })
}

fn read_properties<R: Read + Seek>(reader: &mut R, chunk: Chunk) -> Result<Properties, AudioError> {
    if chunk.size < 4 {
        return Err(AudioError::InvalidFormat("truncated DSDIFF PROP type"));
    }
    let mut prop_type = [0; 4];
    reader.read_exact(&mut prop_type)?;
    if &prop_type != b"SND " {
        return Err(AudioError::Unsupported("unsupported DSDIFF property type"));
    }
    let mut sample_rate = None;
    let mut channels = None;
    let mut compression = None;
    while reader.stream_position()? < chunk.end {
        let local = read_chunk_header(reader, chunk.end)?;
        match &local.id {
            b"FS  " => {
                if sample_rate.is_some() || local.size != 4 {
                    return Err(AudioError::InvalidFormat(
                        "invalid DSDIFF sample-rate chunk",
                    ));
                }
                let mut value = [0; 4];
                reader.read_exact(&mut value)?;
                let rate = u32::from_be_bytes(value);
                if rate == 0 {
                    return Err(AudioError::InvalidFormat("zero DSDIFF sample rate"));
                }
                sample_rate = Some(rate);
            }
            b"CHNL" => {
                if channels.is_some() || local.size < 2 {
                    return Err(AudioError::InvalidFormat("invalid DSDIFF channel chunk"));
                }
                let mut count = [0; 2];
                reader.read_exact(&mut count)?;
                let count = u16::from_be_bytes(count);
                if count == 0 || count > MAX_CHANNELS || local.size != 2 + u64::from(count) * 4 {
                    return Err(AudioError::Unsupported(
                        "unsupported DSDIFF channel count or channel list",
                    ));
                }
                let mut ids = Vec::new();
                ids.try_reserve_exact(usize::from(count))
                    .map_err(|_| AudioError::InvalidData("DSDIFF channel list is too large"))?;
                for _ in 0..count {
                    let mut id = [0; 4];
                    reader.read_exact(&mut id)?;
                    if id[0] == b' ' || !id.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
                        return Err(AudioError::InvalidFormat("invalid DSDIFF channel ID"));
                    }
                    ids.push(id);
                }
                channels = Some(ids);
            }
            b"CMPR" => {
                if compression.is_some() || !(5..=260).contains(&local.size) {
                    return Err(AudioError::InvalidFormat(
                        "invalid DSDIFF compression chunk",
                    ));
                }
                let mut compression_id = [0; 4];
                reader.read_exact(&mut compression_id)?;
                let mut name_len = [0; 1];
                reader.read_exact(&mut name_len)?;
                if local.size != 5 + u64::from(name_len[0]) {
                    return Err(AudioError::InvalidFormat("invalid DSDIFF compression name"));
                }
                let mut name = vec![0; usize::from(name_len[0])];
                reader.read_exact(&mut name)?;
                if !name.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
                    return Err(AudioError::InvalidFormat("invalid DSDIFF compression name"));
                }
                if &compression_id != b"DSD " {
                    return Err(AudioError::Unsupported(
                        "compressed DSDIFF audio is not supported",
                    ));
                }
                compression = Some(compression_id);
            }
            _ => {}
        }
        reader.seek(SeekFrom::Start(local.padded_end))?;
    }
    if reader.stream_position()? != chunk.end {
        return Err(AudioError::InvalidFormat("truncated DSDIFF property chunk"));
    }
    Ok(Properties {
        sample_rate: sample_rate.ok_or(AudioError::InvalidFormat(
            "DSDIFF is missing its sample-rate property",
        ))?,
        channels: channels.ok_or(AudioError::InvalidFormat(
            "DSDIFF is missing its channel property",
        ))?,
        compression: compression.ok_or(AudioError::InvalidFormat(
            "DSDIFF is missing its compression property",
        ))?,
    })
}

fn read_di_in<R: Read + Seek>(
    reader: &mut R,
    chunk: Chunk,
    metadata: &mut Metadata,
) -> Result<(), AudioError> {
    let mut saw_title = false;
    let mut saw_artist = false;
    while reader.stream_position()? < chunk.end {
        let text = read_chunk_header(reader, chunk.end)?;
        if matches!(&text.id, b"DITI" | b"DIAR") {
            if text.size < 4 {
                return Err(AudioError::InvalidFormat("truncated DSDIFF text length"));
            }
            if text.size - 4 > MAX_TEXT_BYTES {
                return Err(AudioError::InvalidData(
                    "DSDIFF text metadata exceeds its limit",
                ));
            }
            let mut size = [0; 4];
            reader.read_exact(&mut size)?;
            let size = u64::from(u32::from_be_bytes(size));
            if size != text.size - 4 {
                return Err(AudioError::InvalidFormat("invalid DSDIFF text length"));
            }
            let length = usize::try_from(size)
                .map_err(|_| AudioError::InvalidData("DSDIFF text metadata is too large"))?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(length)
                .map_err(|_| AudioError::InvalidData("DSDIFF text metadata is too large"))?;
            bytes.resize(length, 0);
            reader.read_exact(&mut bytes)?;
            if !bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
                return Err(AudioError::InvalidFormat(
                    "non-ASCII DSDIFF title or artist",
                ));
            }
            if &text.id == b"DITI" {
                if saw_title {
                    return Err(AudioError::InvalidFormat("duplicate DSDIFF title chunk"));
                }
                saw_title = true;
                if !bytes.is_empty() {
                    metadata.insert("TITLE", String::from_utf8(bytes).unwrap());
                }
            } else {
                if saw_artist {
                    return Err(AudioError::InvalidFormat("duplicate DSDIFF artist chunk"));
                }
                saw_artist = true;
                if !bytes.is_empty() {
                    metadata.insert("ARTIST", String::from_utf8(bytes).unwrap());
                }
            }
        }
        reader.seek(SeekFrom::Start(text.padded_end))?;
    }
    if reader.stream_position()? != chunk.end {
        return Err(AudioError::InvalidFormat("truncated DSDIFF DIIN chunk"));
    }
    Ok(())
}

fn speaker_mask(channels: &[[u8; 4]]) -> Option<u32> {
    match channels {
        [[b'C', b' ', b' ', b' ']] => Some(0x4),
        [left, right] if left == b"SLFT" && right == b"SRGT" => Some(0x3),
        [left, right, center, lfe, surround_left, surround_right]
            if left == b"MLFT"
                && right == b"MRGT"
                && center == b"C   "
                && lfe == b"LFE "
                && surround_left == b"LS  "
                && surround_right == b"RS  " =>
        {
            Some(0x3f)
        }
        _ => None,
    }
}
