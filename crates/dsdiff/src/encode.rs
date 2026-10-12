use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioError, DsdBuffer, DsdEncoder, DsdSpec, Metadata};

const MAX_CHANNELS: usize = 32;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// Writes uncompressed, raw DSDIFF audio.
pub struct DsdiffEncoder<W> {
    writer: W,
    spec: DsdSpec,
    channel_ids: Vec<[u8; 4]>,
    channel_ids_offset: u64,
    metadata: Metadata,
    data_size_offset: u64,
    data_start: u64,
    sample_count: u64,
    finished: bool,
    poisoned: bool,
}

impl<W: Write + Seek> DsdiffEncoder<W> {
    pub fn new(mut writer: W, spec: DsdSpec) -> Result<Self, AudioError> {
        if spec.channels() as usize > MAX_CHANNELS {
            return Err(AudioError::Unsupported(
                "DSDIFF supports at most 32 channels in this profile",
            ));
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
        let mut compression = Vec::from(&b"DSD "[..]);
        compression.push(14);
        compression.extend_from_slice(b"not compressed");
        append_chunk(&mut properties, *b"CMPR", &compression)?;
        let properties_start = writer.stream_position()?;
        write_chunk(&mut writer, *b"PROP", &properties)?;
        let channel_ids_offset = properties_start + 12 + 4 + 16 + 12 + 2;
        writer.write_all(b"DSD ")?;
        let data_size_offset = writer.stream_position()?;
        writer.write_all(&[0; 8])?;
        let data_start = writer.stream_position()?;
        Ok(Self {
            writer,
            spec,
            channel_ids,
            channel_ids_offset,
            metadata: Metadata::default(),
            data_size_offset,
            data_start,
            sample_count: 0,
            finished: false,
            poisoned: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn finish_inner(&mut self) -> Result<(), AudioError> {
        if self.sample_count == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty DSDIFF stream",
            ));
        }
        let data_end = self.writer.stream_position()?;
        let data_bytes = data_end
            .checked_sub(self.data_start)
            .ok_or(AudioError::InvalidData("DSDIFF data size underflow"))?;
        if data_bytes & 1 != 0 {
            self.writer.write_all(&[0])?;
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
        if !edited_master.is_empty() {
            write_chunk(&mut self.writer, *b"DIIN", &edited_master)?;
        }
        let file_end = self.writer.stream_position()?;
        let form_size = file_end
            .checked_sub(12)
            .ok_or(AudioError::InvalidData("DSDIFF FORM size underflow"))?;
        self.writer.seek(SeekFrom::Start(self.data_size_offset))?;
        self.writer.write_all(&data_bytes.to_be_bytes())?;
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
        if let Err(error) = self.writer.write_all(block.bytes()) {
            self.poisoned = true;
            return Err(error.into());
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
            if !matches!(key, "TITLE" | "ARTIST" | "DSDIFF_CHANNEL_ID") {
                return Err(AudioError::Unsupported(
                    "DSDIFF writer supports title, artist, and channel IDs only",
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
        Ok(())
    }
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
    if count > 1000 {
        return Err(AudioError::Unsupported(
            "DSDIFF context channel IDs support at most 1000 channels",
        ));
    }
    Ok((0..count)
        .map(|index| {
            let id: [u8; 4] = format!("C{index:03}").as_bytes().try_into().unwrap();
            id
        })
        .collect())
}

fn valid_channel_id(id: &[u8; 4]) -> bool {
    id[0] != b' ' && id.iter().all(|byte| (0x20..=0x7e).contains(byte))
}

fn write_chunk<W: Write>(writer: &mut W, id: [u8; 4], payload: &[u8]) -> Result<(), AudioError> {
    writer.write_all(&chunk_bytes(id, payload)?)?;
    Ok(())
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
