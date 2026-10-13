use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioError, DsdBuffer, DsdEncoder, DsdSpec, Metadata};

use crate::{DsdiffComment, DsdiffMarker, marker::validate_markers};

const MAX_DST_CHANNELS: usize = 64;
pub(super) const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COMMENTS: usize = u16::MAX as usize;
pub(super) const MAX_UNTIMED_COMMENTS: usize = 2_000;
const MAX_MARKERS: usize = 100_000;

/// Writes raw DSDIFF audio or compressed DST frames with bypass fallback.
pub struct DsdiffEncoder<W> {
    writer: W,
    spec: DsdSpec,
    channel_ids: Vec<[u8; 4]>,
    channel_ids_offset: u64,
    metadata: Metadata,
    comments: Vec<DsdiffComment>,
    markers: Vec<DsdiffMarker>,
    data_size_offset: u64,
    data_start: u64,
    dst_frame_bytes: Option<usize>,
    compress_dst: bool,
    dst_frame_count_offset: Option<u64>,
    pending_dst: Vec<u8>,
    dst_frame_count: u32,
    sample_count: u64,
    finished: bool,
    poisoned: bool,
}

impl<W: Write + Seek> DsdiffEncoder<W> {
    pub fn new(writer: W, spec: DsdSpec) -> Result<Self, AudioError> {
        Self::new_with_dst(writer, spec, false, false)
    }

    /// Writes compressed DST frames when the first-order model is effective,
    /// falling back to valid processing-mode-0 bypass frames otherwise.
    pub fn new_dst(writer: W, spec: DsdSpec) -> Result<Self, AudioError> {
        Self::new_with_dst(writer, spec, true, true)
    }

    /// Writes DST processing-mode-0 bypass frames without compression.
    pub fn new_dst_bypass(writer: W, spec: DsdSpec) -> Result<Self, AudioError> {
        Self::new_with_dst(writer, spec, true, false)
    }

    fn new_with_dst(
        mut writer: W,
        spec: DsdSpec,
        use_dst: bool,
        compress_dst: bool,
    ) -> Result<Self, AudioError> {
        if use_dst && usize::from(spec.channels()) > MAX_DST_CHANNELS {
            return Err(AudioError::Unsupported(
                "DST encoding supports at most 64 channels",
            ));
        }
        let dst_frame_bytes = use_dst
            .then(|| dst_frame_bytes(spec.bit_rate()))
            .transpose()?;
        let mut pending_dst = Vec::new();
        if let Some(frame_bytes) = dst_frame_bytes {
            let frame_size = frame_bytes
                .checked_mul(usize::from(spec.channels()))
                .ok_or(AudioError::InvalidData("DST frame size overflow"))?;
            pending_dst
                .try_reserve_exact(frame_size)
                .map_err(|_| AudioError::InvalidData("DST frame is too large"))?;
        }
        if writer.stream_position()? != 0 || writer.seek(SeekFrom::End(0))? != 0 {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder requires an empty writer at position zero",
            ));
        }
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(b"FRM8\0\0\0\0\0\0\0\0DSD ")?;
        write_chunk(&mut writer, *b"FVER", &[1, 5, 0, 0])?;
        let channel_ids = default_channel_ids(spec)?;
        let mut properties = Vec::new();
        properties.extend_from_slice(b"SND ");
        let mut rate = Vec::with_capacity(4);
        rate.extend_from_slice(&spec.bit_rate().to_be_bytes());
        append_chunk(&mut properties, *b"FS  ", &rate)?;
        let mut channel_data = Vec::with_capacity(2 + channel_ids.len() * 4);
        channel_data.extend_from_slice(&spec.channels().to_be_bytes());
        for id in &channel_ids {
            channel_data.extend_from_slice(id);
        }
        append_chunk(&mut properties, *b"CHNL", &channel_data)?;
        let mut compression = Vec::from(if use_dst { &b"DST "[..] } else { &b"DSD "[..] });
        let compression_name: &[u8] = if use_dst { b"DST" } else { b"not compressed" };
        compression.push(compression_name.len() as u8);
        compression.extend_from_slice(compression_name);
        append_chunk(&mut properties, *b"CMPR", &compression)?;
        let properties_start = writer.stream_position()?;
        write_chunk(&mut writer, *b"PROP", &properties)?;
        let channel_ids_offset = properties_start + 12 + 4 + 16 + 12 + 2;
        writer.write_all(if use_dst { b"DST " } else { b"DSD " })?;
        let data_size_offset = writer.stream_position()?;
        writer.write_all(&[0; 8])?;
        let data_start = writer.stream_position()?;
        let dst_frame_count_offset = if use_dst {
            write_chunk_header(&mut writer, *b"FRTE", 6)?;
            let offset = writer.stream_position()?;
            writer.write_all(&0_u32.to_be_bytes())?;
            writer.write_all(&75_u16.to_be_bytes())?;
            Some(offset)
        } else {
            None
        };
        Ok(Self {
            writer,
            spec,
            channel_ids,
            channel_ids_offset,
            metadata: Metadata::default(),
            comments: Vec::new(),
            markers: Vec::new(),
            data_size_offset,
            data_start,
            dst_frame_bytes,
            compress_dst,
            dst_frame_count_offset,
            pending_dst,
            dst_frame_count: 0,
            sample_count: 0,
            finished: false,
            poisoned: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Replaces the format-specific DSDIFF comment records.
    pub fn set_comments(&mut self, comments: &[DsdiffComment]) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder is finished or unusable",
            ));
        }
        validate_comments(comments, usize::from(self.spec.channels()))?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(comments.len())
            .map_err(|_| AudioError::InvalidData("DSDIFF comments are too large"))?;
        copied.extend_from_slice(comments);
        self.comments = copied;
        Ok(())
    }

    /// Replaces the DSDIFF `DIIN/MARK` records.
    pub fn set_markers(&mut self, markers: &[DsdiffMarker]) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder is finished or unusable",
            ));
        }
        validate_markers(markers, self.spec.bit_rate(), &self.channel_ids)?;
        if markers.len() > MAX_MARKERS {
            return Err(AudioError::InvalidData("too many DSDIFF markers"));
        }
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(markers.len())
            .map_err(|_| AudioError::InvalidData("DSDIFF markers are too large"))?;
        copied.extend_from_slice(markers);
        self.markers = copied;
        Ok(())
    }

    fn finish_inner(&mut self) -> Result<(), AudioError> {
        if self.sample_count == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty DSDIFF stream",
            ));
        }
        if self.dst_frame_bytes.is_some() && !self.pending_dst.is_empty() {
            return Err(AudioError::InvalidData(
                "DST bypass encoding requires complete 1/75-second frames",
            ));
        }
        let data_end = self.writer.stream_position()?;
        let data_bytes = data_end
            .checked_sub(self.data_start)
            .ok_or(AudioError::InvalidData("DSDIFF data size underflow"))?;
        if self.dst_frame_bytes.is_none() && data_bytes & 1 != 0 {
            self.writer.write_all(&[0])?;
        }
        if !self.comments.is_empty() {
            let mut payload = Vec::new();
            payload.extend_from_slice(&(self.comments.len() as u16).to_be_bytes());
            for comment in &self.comments {
                payload.extend_from_slice(&comment.year.to_be_bytes());
                payload.extend_from_slice(&[
                    comment.month,
                    comment.day,
                    comment.hour,
                    comment.minute,
                ]);
                payload.extend_from_slice(&comment.kind.to_be_bytes());
                payload.extend_from_slice(&comment.reference.to_be_bytes());
                payload.extend_from_slice(&(comment.text.len() as u32).to_be_bytes());
                payload.extend_from_slice(comment.text.as_bytes());
                if comment.text.len() & 1 != 0 {
                    payload.push(0);
                }
            }
            write_chunk(&mut self.writer, *b"COMT", &payload)?;
        }
        let mut edited_master = Vec::new();
        for (key, id) in [("ARTIST", *b"DIAR"), ("TITLE", *b"DITI")] {
            let values = self.metadata.values(key);
            if let Some(value) = values.first().filter(|value| !value.is_empty()) {
                let mut payload = Vec::with_capacity(4 + value.len());
                payload.extend_from_slice(
                    &u32::try_from(value.len())
                        .map_err(|_| AudioError::InvalidData("DSDIFF text is too large"))?
                        .to_be_bytes(),
                );
                payload.extend_from_slice(value.as_bytes());
                edited_master.extend_from_slice(&chunk_bytes(id, &payload)?);
            }
        }
        for marker in &self.markers {
            let mut payload = Vec::with_capacity(22 + marker.text.len());
            payload.extend_from_slice(&marker.hours.to_be_bytes());
            payload.extend_from_slice(&[marker.minutes, marker.seconds]);
            payload.extend_from_slice(&marker.samples.to_be_bytes());
            payload.extend_from_slice(&marker.offset.to_be_bytes());
            payload.extend_from_slice(&marker.kind.to_be_bytes());
            payload.extend_from_slice(&marker.channel.to_be_bytes());
            payload.extend_from_slice(&marker.track_flags.to_be_bytes());
            payload.extend_from_slice(&(marker.text.len() as u32).to_be_bytes());
            payload.extend_from_slice(marker.text.as_bytes());
            edited_master.extend_from_slice(&chunk_bytes(*b"MARK", &payload)?);
        }
        if !edited_master.is_empty() {
            write_chunk(&mut self.writer, *b"DIIN", &edited_master)?;
        }
        let file_end = self.writer.stream_position()?;
        let form_size = file_end
            .checked_sub(12)
            .ok_or(AudioError::InvalidData("DSDIFF FORM size underflow"))?;
        self.writer.seek(SeekFrom::Start(self.data_size_offset))?;
        self.writer.write_all(&data_bytes.to_be_bytes())?;
        if let Some(offset) = self.dst_frame_count_offset {
            self.writer.seek(SeekFrom::Start(offset))?;
            self.writer.write_all(&self.dst_frame_count.to_be_bytes())?;
        }
        self.writer.seek(SeekFrom::Start(4))?;
        self.writer.write_all(&form_size.to_be_bytes())?;
        self.writer.seek(SeekFrom::End(0))?;
        self.writer.flush()?;
        Ok(())
    }
}

impl<W: Write + Seek> DsdEncoder for DsdiffEncoder<W> {
    fn dsd_spec(&self) -> DsdSpec {
        self.spec
    }

    fn write_dsd_octets(&mut self, block: &DsdBuffer) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder is finished or unusable after an earlier write error",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "DSDIFF specification changed while encoding",
            ));
        }
        let samples = u64::try_from(block.bytes().len())
            .ok()
            .and_then(|bytes| bytes.checked_div(u64::from(self.spec.channels())))
            .and_then(|frames| frames.checked_mul(8))
            .and_then(|samples| self.sample_count.checked_add(samples))
            .ok_or(AudioError::InvalidData("DSDIFF sample count overflow"))?;
        let result = if let Some(frame_bytes) = self.dst_frame_bytes {
            self.write_dst(block.bytes(), frame_bytes)
        } else {
            self.writer
                .write_all(block.bytes())
                .map_err(AudioError::from)
        };
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error);
        }
        self.sample_count = samples;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder is already finished",
            ));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder is unusable after an earlier write error",
            ));
        }
        if let Err(error) = self.finish_inner() {
            self.poisoned = true;
            return Err(error);
        }
        self.finished = true;
        Ok(())
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "DSDIFF encoder is finished or unusable",
            ));
        }
        for (key, values) in metadata.iter() {
            if !matches!(key, "TITLE" | "ARTIST" | "COMMENT" | "DSDIFF_CHANNEL_ID") {
                return Err(AudioError::Unsupported(
                    "DSDIFF writer supports title, artist, comments, and channel IDs only",
                ));
            }
            if values.iter().any(|value| {
                value.len() > MAX_TEXT_BYTES
                    || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            }) {
                return Err(AudioError::InvalidData(
                    "DSDIFF text metadata must be printable ASCII",
                ));
            }
        }
        if metadata.values("TITLE").len() > 1 || metadata.values("ARTIST").len() > 1 {
            return Err(AudioError::Unsupported(
                "DSDIFF DIIN stores at most one title and artist",
            ));
        }
        let comment_values = metadata.values("COMMENT");
        if comment_values.len() > MAX_UNTIMED_COMMENTS {
            return Err(AudioError::InvalidData(
                "too many DSDIFF comments without timestamps",
            ));
        }
        let mut comments = Vec::new();
        comments
            .try_reserve_exact(comment_values.len())
            .map_err(|_| AudioError::InvalidData("DSDIFF comments are too large"))?;
        comments.extend(
            comment_values
                .iter()
                .enumerate()
                .map(|(index, text)| DsdiffComment {
                    year: index as u16,
                    month: 0,
                    day: 0,
                    hour: 0,
                    minute: 0,
                    kind: 0,
                    reference: 0,
                    text: text.clone(),
                }),
        );
        comments.extend(
            self.comments
                .iter()
                .filter(|comment| comment.kind != 0)
                .cloned(),
        );
        validate_comments(&comments, usize::from(self.spec.channels()))?;
        let channel_ids = metadata.values("DSDIFF_CHANNEL_ID");
        let ids = if !channel_ids.is_empty() {
            if channel_ids.len() != self.channel_ids.len()
                || channel_ids.iter().any(|value| value.len() != 4)
            {
                return Err(AudioError::InvalidData(
                    "DSDIFF channel ID metadata does not match the channel count",
                ));
            }
            let mut ids = Vec::new();
            ids.try_reserve_exact(channel_ids.len())
                .map_err(|_| AudioError::InvalidData("DSDIFF channel list is too large"))?;
            for value in channel_ids {
                let id: [u8; 4] = value.as_bytes().try_into().unwrap();
                if !valid_channel_id(&id) {
                    return Err(AudioError::InvalidData("invalid DSDIFF channel ID"));
                }
                ids.push(id);
            }
            ids
        } else {
            default_channel_ids(self.spec)?
        };
        validate_markers(&self.markers, self.spec.bit_rate(), &ids)?;
        let position = self.writer.stream_position()?;
        self.writer.seek(SeekFrom::Start(self.channel_ids_offset))?;
        for id in &ids {
            if let Err(error) = self.writer.write_all(id) {
                self.poisoned = true;
                return Err(error.into());
            }
        }
        if let Err(error) = self.writer.seek(SeekFrom::Start(position)) {
            self.poisoned = true;
            return Err(error.into());
        }
        self.channel_ids = ids;
        self.metadata = metadata.clone();
        self.comments = comments;
        Ok(())
    }
}

impl<W: Write + Seek> DsdiffEncoder<W> {
    fn write_dst(&mut self, mut input: &[u8], frame_bytes: usize) -> Result<(), AudioError> {
        let frame_size = frame_bytes
            .checked_mul(usize::from(self.spec.channels()))
            .ok_or(AudioError::InvalidData("DST frame size overflow"))?;
        while !input.is_empty() {
            let count = (frame_size - self.pending_dst.len()).min(input.len());
            self.pending_dst.extend_from_slice(&input[..count]);
            input = &input[count..];
            if self.pending_dst.len() == frame_size {
                if self.dst_frame_count == u32::MAX {
                    return Err(AudioError::InvalidData("too many DSDIFF DST frames"));
                }
                let encoded = if self.compress_dst {
                    let mut channel_data = Vec::new();
                    channel_data
                        .try_reserve_exact(frame_size)
                        .map_err(|_| AudioError::InvalidData("DST frame is too large"))?;
                    channel_data.resize(frame_size, 0);
                    let channels = usize::from(self.spec.channels());
                    for byte_index in 0..frame_bytes {
                        for channel in 0..channels {
                            channel_data[channel * frame_bytes + byte_index] =
                                self.pending_dst[byte_index * channels + channel];
                        }
                    }
                    match dst::encode_frame(&channel_data, channels, frame_bytes) {
                        Ok(frame) => Some(frame),
                        Err(AudioError::Unsupported(_)) => None,
                        Err(error) => return Err(error),
                    }
                } else {
                    None
                };
                if let Some(frame) = encoded {
                    let frame_len = u64::try_from(frame.len())
                        .map_err(|_| AudioError::InvalidData("DST frame is too large"))?;
                    write_chunk_header(&mut self.writer, *b"DSTF", frame_len)?;
                    self.writer.write_all(&frame)?;
                    if frame.len() & 1 != 0 {
                        self.writer.write_all(&[0])?;
                    }
                } else {
                    write_chunk_header(&mut self.writer, *b"DSTF", (frame_size + 1) as u64)?;
                    self.writer.write_all(&[0])?;
                    self.writer.write_all(&self.pending_dst)?;
                    if frame_size & 1 == 0 {
                        self.writer.write_all(&[0])?;
                    }
                }
                self.pending_dst.clear();
                self.dst_frame_count += 1;
            }
        }
        Ok(())
    }
}

pub(super) fn validate_comments(
    comments: &[DsdiffComment],
    channels: usize,
) -> Result<(), AudioError> {
    if comments.len() > MAX_COMMENTS {
        return Err(AudioError::InvalidData("too many DSDIFF comments"));
    }
    let mut total_text = 0_usize;
    for comment in comments {
        if comment.text.len() > u32::MAX as usize
            || !comment
                .text
                .bytes()
                .all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return Err(AudioError::InvalidData(
                "DSDIFF comments must be printable ASCII",
            ));
        }
        if comment.month > 12 || comment.day > 31 || comment.hour > 23 || comment.minute > 59 {
            return Err(AudioError::InvalidData("invalid DSDIFF comment timestamp"));
        }
        if (comment.kind == 0 && comment.reference != 0)
            || (comment.kind == 1 && usize::from(comment.reference) > channels)
        {
            return Err(AudioError::InvalidData("invalid DSDIFF comment reference"));
        }
        total_text = total_text
            .checked_add(comment.text.len())
            .filter(|&total| total <= MAX_TEXT_BYTES)
            .ok_or(AudioError::InvalidData(
                "DSDIFF comment text exceeds its limit",
            ))?;
    }
    Ok(())
}

fn default_channel_ids(spec: DsdSpec) -> Result<Vec<[u8; 4]>, AudioError> {
    let values: &[[u8; 4]] = match (spec.channels(), spec.speaker_mask()) {
        (1, _) => &[*b"C   "],
        (2, _) => &[*b"SLFT", *b"SRGT"],
        (5, Some(0x37)) => &[*b"MLFT", *b"MRGT", *b"C   ", *b"LS  ", *b"RS  "],
        (6, Some(0x3f)) => &[*b"MLFT", *b"MRGT", *b"C   ", *b"LFE ", *b"LS  ", *b"RS  "],
        _ => &[],
    };
    if !values.is_empty() {
        return Ok(values.to_vec());
    }
    let count = usize::from(spec.channels());
    Ok((0..count).map(default_channel_id).collect())
}

fn default_channel_id(index: usize) -> [u8; 4] {
    const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    if index < 1000 {
        return format!("C{index:03}").as_bytes().try_into().unwrap();
    }
    let mut value = index - 1000;
    let mut id = [b'~'; 4];
    for digit in id[1..].iter_mut().rev() {
        *digit = DIGITS[value % DIGITS.len()];
        value /= DIGITS.len();
    }
    id
}

fn valid_channel_id(id: &[u8; 4]) -> bool {
    id[0] != b' ' && id.iter().all(|byte| (0x20..=0x7e).contains(byte))
}

pub(super) fn write_chunk<W: Write>(
    writer: &mut W,
    id: [u8; 4],
    payload: &[u8],
) -> Result<(), AudioError> {
    write_chunk_header(writer, id, payload.len() as u64)?;
    writer.write_all(payload)?;
    if payload.len() & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn write_chunk_header<W: Write>(writer: &mut W, id: [u8; 4], size: u64) -> Result<(), AudioError> {
    writer.write_all(&id)?;
    writer.write_all(&size.to_be_bytes())?;
    Ok(())
}

fn dst_frame_bytes(bit_rate: u32) -> Result<usize, AudioError> {
    match bit_rate {
        2_822_400 => Ok(4_704),
        5_644_800 => Ok(9_408),
        11_289_600 => Ok(18_816),
        _ => Err(AudioError::Unsupported(
            "DST supports DSD64, DSD128, and DSD256 sample rates",
        )),
    }
}

fn append_chunk(output: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) -> Result<(), AudioError> {
    output.extend_from_slice(&chunk_bytes(id, payload)?);
    Ok(())
}

fn chunk_bytes(id: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, AudioError> {
    let size = u64::try_from(payload.len())
        .map_err(|_| AudioError::InvalidData("DSDIFF chunk size overflow"))?;
    let output_size = 12_usize
        .checked_add(payload.len())
        .and_then(|size| size.checked_add(payload.len() & 1))
        .ok_or(AudioError::InvalidData("DSDIFF chunk size overflow"))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_size)
        .map_err(|_| AudioError::InvalidData("DSDIFF chunk is too large"))?;
    output.extend_from_slice(&id);
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(payload);
    if payload.len() & 1 != 0 {
        output.push(0);
    }
    Ok(output)
}
