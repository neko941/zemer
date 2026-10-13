use std::io::{Read, Seek, SeekFrom};

use zemer_core::{AudioError, DsdBuffer, DsdDecoder, DsdSpec, Metadata};

use crate::{DsdiffComment, DsdiffMarker, marker::validate_markers};

const MAX_DST_CHANNELS: usize = 64;
const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MARKERS: usize = 100_000;
const MAX_DST_FRAMES: usize = 4_000_000;

/// Streams raw or DST-compressed DSDIFF audio without converting it to PCM.
pub struct DsdiffDecoder<R> {
    reader: R,
    spec: DsdSpec,
    metadata: Metadata,
    comments: Vec<DsdiffComment>,
    markers: Vec<DsdiffMarker>,
    audio: AudioData,
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
        let mut audio = None;
        let mut saw_di_in = false;
        let mut saw_comments = false;
        let mut metadata = Metadata::default();
        let mut comments = Vec::new();
        let mut markers = Vec::new();
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
                    if !saw_version || properties.is_some() || audio.is_some() {
                        return Err(AudioError::InvalidFormat("misplaced DSDIFF PROP chunk"));
                    }
                    properties = Some(read_properties(&mut reader, chunk)?);
                }
                b"DSD " => {
                    if audio.is_some() || properties.is_none() {
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
                    audio = Some(AudioData::Raw {
                        data_start: chunk.payload_start,
                        data_bytes: chunk.size,
                        read_bytes: 0,
                    });
                }
                b"DST " => {
                    if audio.is_some() || properties.is_none() {
                        return Err(AudioError::InvalidFormat("misplaced DSDIFF DST chunk"));
                    }
                    let properties = properties.as_ref().unwrap();
                    if properties.compression != *b"DST " {
                        return Err(AudioError::InvalidFormat(
                            "DSDIFF DST chunk disagrees with its compression type",
                        ));
                    }
                    if properties.channels.len() > MAX_DST_CHANNELS {
                        return Err(AudioError::Unsupported(
                            "DST decoding supports at most 64 channels",
                        ));
                    }
                    let frame_bytes = dst_frame_bytes(properties.sample_rate)?;
                    audio = Some(read_dst(
                        &mut reader,
                        chunk,
                        properties.channels.len(),
                        frame_bytes,
                    )?);
                }
                b"DIIN" => {
                    if saw_di_in {
                        return Err(AudioError::InvalidFormat("duplicate DSDIFF DIIN chunk"));
                    }
                    read_di_in(&mut reader, chunk, &mut metadata, &mut markers)?;
                    saw_di_in = true;
                }
                b"COMT" => {
                    if saw_comments {
                        return Err(AudioError::InvalidFormat("duplicate DSDIFF COMT chunk"));
                    }
                    comments = read_comments(&mut reader, chunk)?;
                    saw_comments = true;
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
        validate_markers(&markers, properties.sample_rate, &properties.channels)?;
        for comment in &comments {
            if (comment.kind == 0 && comment.reference != 0)
                || (comment.kind == 1 && usize::from(comment.reference) > properties.channels.len())
            {
                return Err(AudioError::InvalidFormat(
                    "invalid DSDIFF comment reference",
                ));
            }
            if comment.kind == 0 {
                metadata.insert("COMMENT", comment.text.clone());
            }
        }
        let audio = audio.ok_or(AudioError::InvalidFormat(
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
        if let AudioData::Raw { data_start, .. } = &audio {
            reader.seek(SeekFrom::Start(*data_start))?;
        }
        Ok(Self {
            reader,
            spec,
            metadata,
            comments,
            markers,
            audio,
        })
    }

    /// Returns all typed DSDIFF comment records, including non-general comments.
    pub fn comments(&self) -> &[DsdiffComment] {
        &self.comments
    }

    /// Returns all typed DSDIFF marker records in file order.
    pub fn markers(&self) -> &[DsdiffMarker] {
        &self.markers
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
        self.audio
            .octet_frames(self.spec.channels())?
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
        let channels = usize::from(self.spec.channels());
        let bytes = match &mut self.audio {
            AudioData::Raw {
                data_start,
                data_bytes,
                read_bytes,
            } => {
                if *read_bytes == *data_bytes {
                    return Ok(None);
                }
                let frames = max_octet_frames.min(
                    usize::try_from((*data_bytes - *read_bytes) / channels as u64)
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
                    data_start
                        .checked_add(*read_bytes)
                        .ok_or(AudioError::InvalidData("DSDIFF audio position overflow"))?,
                ))?;
                self.reader.read_exact(&mut bytes)?;
                *read_bytes += byte_count as u64;
                bytes
            }
            AudioData::Dst {
                frames,
                frame_bytes,
                next_frame,
                decoded_frame,
                decoded_position,
                consumed_octet_frames,
                has_crc,
            } => {
                let total_frames = u64::try_from(frames.len())
                    .ok()
                    .and_then(|count| count.checked_mul(*frame_bytes as u64))
                    .ok_or(AudioError::InvalidData("DSDIFF DST sample count overflow"))?;
                if *consumed_octet_frames == total_frames {
                    return Ok(None);
                }
                let available =
                    usize::try_from(total_frames - *consumed_octet_frames).unwrap_or(usize::MAX);
                let output_frames = max_octet_frames.min(available);
                let byte_count = output_frames
                    .checked_mul(channels)
                    .ok_or(AudioError::InvalidData("DSDIFF output block size overflow"))?;
                let mut output = Vec::new();
                output
                    .try_reserve_exact(byte_count)
                    .map_err(|_| AudioError::InvalidData("DSDIFF output block is too large"))?;
                while output.len() < byte_count {
                    if *decoded_position == decoded_frame.len() {
                        let info = frames
                            .get(*next_frame)
                            .ok_or(AudioError::InvalidFormat("truncated DSDIFF DST frames"))?;
                        let frame_size = usize::try_from(info.size).map_err(|_| {
                            AudioError::InvalidData("DSDIFF DST frame is too large")
                        })?;
                        let mut frame = Vec::new();
                        frame.try_reserve_exact(frame_size).map_err(|_| {
                            AudioError::InvalidData("DSDIFF DST frame is too large")
                        })?;
                        frame.resize(frame_size, 0);
                        self.reader.seek(SeekFrom::Start(info.offset))?;
                        self.reader.read_exact(&mut frame)?;
                        let channel_data = dst::decode_frame(&frame, channels, *frame_bytes)?;
                        decoded_frame.clear();
                        decoded_frame
                            .try_reserve_exact(frame_bytes.saturating_mul(channels))
                            .map_err(|_| {
                                AudioError::InvalidData("DSDIFF DST frame is too large")
                            })?;
                        for byte_index in 0..*frame_bytes {
                            for channel in 0..channels {
                                decoded_frame
                                    .push(channel_data[channel * *frame_bytes + byte_index]);
                            }
                        }
                        if *has_crc && info.crc != Some(dsdiff_crc32(decoded_frame)) {
                            return Err(AudioError::InvalidFormat("DSDIFF DST frame CRC mismatch"));
                        }
                        *decoded_position = 0;
                        *next_frame += 1;
                    }
                    let count =
                        (byte_count - output.len()).min(decoded_frame.len() - *decoded_position);
                    output.extend_from_slice(
                        &decoded_frame[*decoded_position..*decoded_position + count],
                    );
                    *decoded_position += count;
                }
                *consumed_octet_frames += output_frames as u64;
                output
            }
        };
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

struct DstFrameInfo {
    offset: u64,
    size: u64,
    crc: Option<u32>,
}

enum AudioData {
    Raw {
        data_start: u64,
        data_bytes: u64,
        read_bytes: u64,
    },
    Dst {
        frames: Vec<DstFrameInfo>,
        frame_bytes: usize,
        next_frame: usize,
        decoded_frame: Vec<u8>,
        decoded_position: usize,
        consumed_octet_frames: u64,
        has_crc: bool,
    },
}

impl AudioData {
    fn octet_frames(&self, channels: u16) -> Option<u64> {
        match self {
            Self::Raw { data_bytes, .. } => data_bytes.checked_div(u64::from(channels)),
            Self::Dst {
                frames,
                frame_bytes,
                ..
            } => u64::try_from(frames.len())
                .ok()?
                .checked_mul(*frame_bytes as u64),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Chunk {
    pub(super) id: [u8; 4],
    pub(super) size: u64,
    pub(super) payload_start: u64,
    pub(super) end: u64,
    pub(super) padded_end: u64,
}

pub(super) fn read_chunk_header<R: Read + Seek>(
    reader: &mut R,
    parent_end: u64,
) -> Result<Chunk, AudioError> {
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
                if count == 0 || local.size != 2 + u64::from(count) * 4 {
                    return Err(AudioError::InvalidFormat(
                        "invalid DSDIFF channel count or channel list",
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
                if !matches!(&compression_id, b"DSD " | b"DST ") {
                    return Err(AudioError::Unsupported(
                        "unsupported DSDIFF compression type",
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

fn dst_frame_bytes(sample_rate: u32) -> Result<usize, AudioError> {
    let bytes = match sample_rate {
        2_822_400 => 4_704,
        5_644_800 => 9_408,
        11_289_600 => 18_816,
        _ => {
            return Err(AudioError::Unsupported(
                "DST supports DSD64, DSD128, and DSD256 sample rates",
            ));
        }
    };
    Ok(bytes)
}

fn read_dst<R: Read + Seek>(
    reader: &mut R,
    chunk: Chunk,
    channels: usize,
    frame_bytes: usize,
) -> Result<AudioData, AudioError> {
    let frame_info = read_chunk_header(reader, chunk.end)?;
    if &frame_info.id != b"FRTE" || frame_info.size != 6 || frame_info.padded_end > chunk.end {
        return Err(AudioError::InvalidFormat(
            "DST sound data must begin with a six-byte FRTE chunk",
        ));
    }
    let mut frame_rate = [0; 6];
    reader.read_exact(&mut frame_rate)?;
    let frame_count = u32::from_be_bytes(frame_rate[..4].try_into().unwrap()) as usize;
    if u16::from_be_bytes(frame_rate[4..].try_into().unwrap()) != 75 || frame_count == 0 {
        return Err(AudioError::InvalidFormat(
            "invalid DSDIFF DST frame rate or count",
        ));
    }
    if frame_count > MAX_DST_FRAMES {
        return Err(AudioError::InvalidData("too many DSDIFF DST frames"));
    }
    let mut frames: Vec<DstFrameInfo> = Vec::new();
    frames
        .try_reserve_exact(frame_count)
        .map_err(|_| AudioError::InvalidData("DSDIFF DST frame index is too large"))?;
    let mut position = frame_info.padded_end;
    let mut crc_count = 0;
    let mut saw_index = false;
    let mut last_was_frame = false;
    let mut crc_mode = None;
    while position < chunk.end {
        reader.seek(SeekFrom::Start(position))?;
        let local = read_chunk_header(reader, chunk.end)?;
        match &local.id {
            b"DSTF" => {
                if saw_index
                    || frames.len() == frame_count
                    || local.size == 0
                    || (crc_mode == Some(true)
                        && frames.last().is_some_and(|frame| frame.crc.is_none()))
                {
                    return Err(AudioError::InvalidFormat(
                        "invalid DSDIFF DST frame sequence",
                    ));
                }
                let maximum = channels
                    .checked_mul(frame_bytes)
                    .and_then(|size| size.checked_add(1))
                    .ok_or(AudioError::InvalidData("DSDIFF DST frame size overflow"))?
                    as u64;
                if local.size > maximum {
                    return Err(AudioError::InvalidFormat("DSDIFF DST frame is too large"));
                }
                frames.push(DstFrameInfo {
                    offset: local.payload_start,
                    size: local.size,
                    crc: None,
                });
                last_was_frame = true;
            }
            b"DSTC" => {
                if local.size != 4 || !last_was_frame {
                    return Err(AudioError::InvalidFormat("invalid DSDIFF DST CRC chunk"));
                }
                let frame = frames
                    .last_mut()
                    .ok_or(AudioError::InvalidFormat("DST CRC has no preceding frame"))?;
                if frame.crc.is_some() {
                    return Err(AudioError::InvalidFormat("duplicate DSDIFF DST CRC chunk"));
                }
                let mut crc = [0; 4];
                reader.read_exact(&mut crc)?;
                frame.crc = Some(u32::from_be_bytes(crc));
                crc_count += 1;
                crc_mode = Some(true);
                last_was_frame = false;
            }
            b"DSTI" => {
                if saw_index || local.size % 12 != 0 || frames.len() != frame_count {
                    return Err(AudioError::InvalidFormat("invalid DSDIFF DST index chunk"));
                }
                if last_was_frame {
                    crc_mode.get_or_insert(false);
                    last_was_frame = false;
                }
                saw_index = true;
            }
            _ => {
                if last_was_frame {
                    crc_mode.get_or_insert(false);
                    last_was_frame = false;
                }
            }
        }
        if &local.id == b"DSTF" && crc_mode.is_none() {
            crc_mode = Some(false);
        }
        position = local.padded_end;
    }
    if position != chunk.end || frames.len() != frame_count {
        return Err(AudioError::InvalidFormat("DSDIFF DST frame count mismatch"));
    }
    if crc_count != 0 && crc_count != frame_count {
        return Err(AudioError::InvalidFormat(
            "incomplete DSDIFF DST CRC sequence",
        ));
    }
    if crc_count == 0 && frames.iter().any(|frame| frame.crc.is_some()) {
        return Err(AudioError::InvalidFormat("invalid DSDIFF DST CRC sequence"));
    }
    Ok(AudioData::Dst {
        frames,
        frame_bytes,
        next_frame: 0,
        decoded_frame: Vec::new(),
        decoded_position: 0,
        consumed_octet_frames: 0,
        has_crc: crc_count != 0,
    })
}

fn dsdiff_crc32(data: &[u8]) -> u32 {
    const POLYNOMIAL: u32 = 0x8000_0011;
    let mut crc = 0_u32;
    for &byte in data {
        for shift in (0..8).rev() {
            let feedback = (crc >> 31) ^ u32::from((byte >> shift) & 1);
            crc <<= 1;
            if feedback != 0 {
                crc ^= POLYNOMIAL;
            }
        }
    }
    crc
}

fn read_di_in<R: Read + Seek>(
    reader: &mut R,
    chunk: Chunk,
    metadata: &mut Metadata,
    markers: &mut Vec<DsdiffMarker>,
) -> Result<(), AudioError> {
    let mut saw_title = false;
    let mut saw_artist = false;
    let mut total_marker_text = 0_u64;
    while reader.stream_position()? < chunk.end {
        let text = read_chunk_header(reader, chunk.end)?;
        if &text.id == b"MARK" {
            if text.size < 22 || text.size - 22 > MAX_TEXT_BYTES {
                return Err(AudioError::InvalidFormat("invalid DSDIFF marker size"));
            }
            let mut header = [0; 22];
            reader.read_exact(&mut header)?;
            let length = u32::from_be_bytes(header[18..22].try_into().unwrap()) as usize;
            if length as u64 != text.size - 22 {
                return Err(AudioError::InvalidFormat(
                    "invalid DSDIFF marker text length",
                ));
            }
            add_marker_text_length(&mut total_marker_text, length as u64)?;
            if markers.len() == MAX_MARKERS {
                return Err(AudioError::InvalidData("too many DSDIFF markers"));
            }
            markers
                .try_reserve(1)
                .map_err(|_| AudioError::InvalidData("DSDIFF markers are too large"))?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(length)
                .map_err(|_| AudioError::InvalidData("DSDIFF marker text is too large"))?;
            bytes.resize(length, 0);
            reader.read_exact(&mut bytes)?;
            if !bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
                return Err(AudioError::InvalidFormat("invalid DSDIFF marker text"));
            }
            markers.push(DsdiffMarker {
                hours: u16::from_be_bytes(header[0..2].try_into().unwrap()),
                minutes: header[2],
                seconds: header[3],
                samples: u32::from_be_bytes(header[4..8].try_into().unwrap()),
                offset: i32::from_be_bytes(header[8..12].try_into().unwrap()),
                kind: u16::from_be_bytes(header[12..14].try_into().unwrap()),
                channel: u16::from_be_bytes(header[14..16].try_into().unwrap()),
                track_flags: u16::from_be_bytes(header[16..18].try_into().unwrap()),
                text: String::from_utf8(bytes).unwrap(),
            });
        } else if matches!(&text.id, b"DITI" | b"DIAR") {
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

fn add_marker_text_length(total: &mut u64, length: u64) -> Result<(), AudioError> {
    *total = total
        .checked_add(length)
        .filter(|&value| value <= MAX_TEXT_BYTES)
        .ok_or(AudioError::InvalidData(
            "DSDIFF marker text exceeds its aggregate limit",
        ))?;
    Ok(())
}

fn read_comments<R: Read + Seek>(
    reader: &mut R,
    chunk: Chunk,
) -> Result<Vec<DsdiffComment>, AudioError> {
    if chunk.size < 2 {
        return Err(AudioError::InvalidFormat("truncated DSDIFF COMT count"));
    }
    let mut count_bytes = [0; 2];
    reader.read_exact(&mut count_bytes)?;
    let count = usize::from(u16::from_be_bytes(count_bytes));
    let mut comments = Vec::new();
    comments
        .try_reserve_exact(count)
        .map_err(|_| AudioError::InvalidData("DSDIFF comments are too large"))?;
    let mut total_text = 0_u64;
    for _ in 0..count {
        let position = reader.stream_position()?;
        if chunk.end.saturating_sub(position) < 14 {
            return Err(AudioError::InvalidFormat("truncated DSDIFF comment record"));
        }
        let mut header = [0; 14];
        reader.read_exact(&mut header)?;
        let length = u64::from(u32::from_be_bytes(header[10..14].try_into().unwrap()));
        total_text = total_text
            .checked_add(length)
            .filter(|&total| total <= MAX_TEXT_BYTES)
            .ok_or(AudioError::InvalidData(
                "DSDIFF comment text exceeds its limit",
            ))?;
        let end = reader
            .stream_position()?
            .checked_add(length)
            .filter(|&end| end <= chunk.end)
            .ok_or(AudioError::InvalidFormat("DSDIFF comment exceeds COMT"))?;
        let padded_end = end
            .checked_add(length & 1)
            .filter(|&end| end <= chunk.end)
            .ok_or(AudioError::InvalidFormat(
                "DSDIFF comment padding exceeds COMT",
            ))?;
        let length = usize::try_from(length)
            .map_err(|_| AudioError::InvalidData("DSDIFF comment is too large"))?;
        let mut text = Vec::new();
        text.try_reserve_exact(length)
            .map_err(|_| AudioError::InvalidData("DSDIFF comment is too large"))?;
        text.resize(length, 0);
        reader.read_exact(&mut text)?;
        if !text.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
            return Err(AudioError::InvalidFormat("non-ASCII DSDIFF comment"));
        }
        if end != padded_end {
            let mut padding = [0];
            reader.read_exact(&mut padding)?;
            if padding[0] != 0 {
                return Err(AudioError::InvalidFormat("invalid DSDIFF comment padding"));
            }
        }
        let kind = u16::from_be_bytes(header[6..8].try_into().unwrap());
        let reference = u16::from_be_bytes(header[8..10].try_into().unwrap());
        if header[2] > 12 || header[3] > 31 || header[4] > 23 || header[5] > 59 {
            return Err(AudioError::InvalidFormat(
                "invalid DSDIFF comment timestamp",
            ));
        }
        comments.push(DsdiffComment {
            year: u16::from_be_bytes(header[..2].try_into().unwrap()),
            month: header[2],
            day: header[3],
            hour: header[4],
            minute: header[5],
            kind,
            reference,
            text: String::from_utf8(text).unwrap(),
        });
    }
    if reader.stream_position()? != chunk.end {
        return Err(AudioError::InvalidFormat(
            "invalid DSDIFF COMT record count",
        ));
    }
    Ok(comments)
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

#[cfg(test)]
mod tests {
    use super::{MAX_TEXT_BYTES, add_marker_text_length};

    #[test]
    fn marker_text_has_an_aggregate_allocation_limit() {
        let mut total = 0;
        add_marker_text_length(&mut total, MAX_TEXT_BYTES - 1).unwrap();
        assert!(add_marker_text_length(&mut total, 2).is_err());
        assert_eq!(total, MAX_TEXT_BYTES - 1);
    }
}
