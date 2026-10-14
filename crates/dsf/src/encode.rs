use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioError, DsdBuffer, DsdEncoder, DsdSpec, Metadata};

use crate::id3::{encode_metadata, parse_id3_tag};

const BLOCK_BYTES: usize = 4096;
const HEADER_BYTES: u64 = 92;

/// Writes DSF 1.01 raw DSD in channel-block order.
pub struct DsfEncoder<W> {
    writer: W,
    spec: DsdSpec,
    bits_per_sample: u32,
    metadata: Metadata,
    id3_tag: Option<Vec<u8>>,
    channel_blocks: Vec<Vec<u8>>,
    last_octets: Vec<u8>,
    block_frames: usize,
    frames: u64,
    sample_count: Option<u64>,
    finished: bool,
    poisoned: bool,
}

impl<W: Write + Seek> DsfEncoder<W> {
    pub fn new(writer: W, spec: DsdSpec) -> Result<Self, AudioError> {
        Self::new_with_bits_per_sample(writer, spec, 1)
    }

    /// Writes DSF DSD octets in the selected one-bit or eight-bit storage order.
    pub fn new_with_bits_per_sample(
        mut writer: W,
        spec: DsdSpec,
        bits_per_sample: u32,
    ) -> Result<Self, AudioError> {
        let channel_type = dsf_channel_type(spec)?;
        if !matches!(bits_per_sample, 1 | 8) {
            return Err(AudioError::Unsupported(
                "DSF 1.01 supports one- or eight-bit DSD storage",
            ));
        }
        if !matches!(spec.bit_rate(), 2_822_400 | 5_644_800) {
            return Err(AudioError::Unsupported(
                "DSF 1.01 supports 2.8224 and 5.6448 MHz DSD",
            ));
        }
        if writer.stream_position()? != 0 || writer.seek(SeekFrom::End(0))? != 0 {
            return Err(AudioError::InvalidData(
                "DSF encoder requires an empty writer at position zero",
            ));
        }
        writer.seek(SeekFrom::Start(0))?;
        let mut header = Vec::with_capacity(HEADER_BYTES as usize);
        header.extend_from_slice(b"DSD ");
        header.extend_from_slice(&28_u64.to_le_bytes());
        header.extend_from_slice(&0_u64.to_le_bytes());
        header.extend_from_slice(&0_u64.to_le_bytes());
        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&52_u64.to_le_bytes());
        header.extend_from_slice(&1_u32.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&channel_type.to_le_bytes());
        header.extend_from_slice(&u32::from(spec.channels()).to_le_bytes());
        header.extend_from_slice(&spec.bit_rate().to_le_bytes());
        header.extend_from_slice(&bits_per_sample.to_le_bytes());
        header.extend_from_slice(&0_u64.to_le_bytes());
        header.extend_from_slice(&(BLOCK_BYTES as u32).to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&12_u64.to_le_bytes());
        debug_assert_eq!(header.len() as u64, HEADER_BYTES);
        writer.write_all(&header)?;

        let channels = usize::from(spec.channels());
        let mut channel_blocks = Vec::new();
        channel_blocks
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::InvalidData("DSF channel buffer is too large"))?;
        for _ in 0..channels {
            channel_blocks.push(vec![0; BLOCK_BYTES]);
        }
        let mut last_octets = Vec::new();
        last_octets
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::InvalidData("DSF channel buffer is too large"))?;
        last_octets.resize(channels, 0);
        Ok(Self {
            writer,
            spec,
            bits_per_sample,
            metadata: Metadata::default(),
            id3_tag: None,
            channel_blocks,
            last_octets,
            block_frames: 0,
            frames: 0,
            sample_count: None,
            finished: false,
            poisoned: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Copies a complete bounded ID3 tag without altering opaque frames.
    pub fn set_id3_tag(&mut self, id3_tag: &[u8]) -> Result<(), AudioError> {
        if self.finished || self.frames != 0 {
            return Err(AudioError::InvalidData(
                "DSF ID3 tag must be set before writing audio",
            ));
        }
        self.metadata = parse_id3_tag(id3_tag)?;
        self.id3_tag = Some(id3_tag.to_vec());
        Ok(())
    }

    /// Sets the exact final per-channel DSD bit count after all octets are written.
    /// The supplied count must fit the written octets; unused bits in the final
    /// octet are serialized as zero.
    pub fn set_sample_count(&mut self, sample_count: u64) -> Result<(), AudioError> {
        if self.finished || self.poisoned || self.frames == 0 || self.sample_count.is_some() {
            return Err(AudioError::InvalidData(
                "DSF sample count must be set once after writing audio",
            ));
        }
        let octets = sample_count
            .checked_sub(1)
            .map(|count| count / 8 + 1)
            .ok_or(AudioError::InvalidData("DSF sample count must be non-zero"))?;
        if octets != self.frames {
            return Err(AudioError::InvalidData(
                "DSF sample count does not match the written octets",
            ));
        }
        let final_bits = (sample_count % 8) as u8;
        if final_bits != 0 {
            let unused_mask = (1 << (8 - final_bits)) - 1;
            if self
                .last_octets
                .iter()
                .any(|octet| octet & unused_mask != 0)
            {
                return Err(AudioError::InvalidData(
                    "unused bits in the final DSF octet must be zero",
                ));
            }
        }
        self.sample_count = Some(sample_count);
        Ok(())
    }

    fn flush_block(&mut self) -> Result<(), AudioError> {
        if self.block_frames == 0 {
            return Ok(());
        }
        for channel in &self.channel_blocks {
            self.writer.write_all(channel)?;
        }
        self.block_frames = 0;
        for channel in &mut self.channel_blocks {
            channel.fill(0);
        }
        Ok(())
    }

    fn finish_inner(&mut self) -> Result<(), AudioError> {
        if self.frames == 0 {
            return Err(AudioError::InvalidData("cannot encode an empty DSF stream"));
        }
        self.flush_block()?;
        let data_end = self.writer.stream_position()?;
        let encoded_tag = if self.id3_tag.is_none() && self.metadata.iter().next().is_some() {
            Some(encode_metadata(&self.metadata)?)
        } else {
            None
        };
        let tag = self
            .id3_tag
            .as_deref()
            .or(encoded_tag.as_deref())
            .unwrap_or_default();
        self.writer.write_all(tag)?;
        let file_size = self.writer.stream_position()?;
        let metadata_offset = if tag.is_empty() { 0 } else { data_end };
        let sample_count = self.sample_count.unwrap_or(self.frames * 8);
        let data_size = data_end
            .checked_sub(HEADER_BYTES - 12)
            .ok_or(AudioError::InvalidData("DSF data size underflow"))?;
        self.writer.seek(SeekFrom::Start(12))?;
        self.writer.write_all(&file_size.to_le_bytes())?;
        self.writer.write_all(&metadata_offset.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(64))?;
        self.writer.write_all(&sample_count.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(84))?;
        self.writer.write_all(&data_size.to_le_bytes())?;
        self.writer.seek(SeekFrom::End(0))?;
        self.writer.flush()?;
        Ok(())
    }
}

impl<W: Write + Seek> DsdEncoder for DsfEncoder<W> {
    fn dsd_spec(&self) -> DsdSpec {
        self.spec
    }

    fn write_dsd_octets(&mut self, block: &DsdBuffer) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("DSF encoder is already finished"));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "DSF encoder is unusable after an earlier write failure",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "DSF specification changed while encoding",
            ));
        }
        if self.sample_count.is_some() && !block.is_empty() {
            return Err(AudioError::InvalidData(
                "cannot append DSD octets after setting the DSF sample count",
            ));
        }
        let frames = u64::try_from(block.frames())
            .map_err(|_| AudioError::InvalidData("DSF frame count overflow"))?;
        let next_frames = self
            .frames
            .checked_add(frames)
            .filter(|&count| count <= u64::MAX / 8)
            .ok_or(AudioError::InvalidData("DSF sample count overflow"))?;
        let channels = usize::from(self.spec.channels());
        let result = (|| {
            for frame in block.bytes().chunks_exact(channels) {
                for (channel, &octet) in self.channel_blocks.iter_mut().zip(frame) {
                    channel[self.block_frames] = if self.bits_per_sample == 1 {
                        octet.reverse_bits()
                    } else {
                        octet
                    };
                }
                for (last_octet, &octet) in self.last_octets.iter_mut().zip(frame) {
                    *last_octet = octet;
                }
                self.block_frames += 1;
                if self.block_frames == BLOCK_BYTES {
                    self.flush_block()?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.frames = next_frames;
        }
        result
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "DSF encoder is unusable after an earlier write failure",
            ));
        }
        let result = self.finish_inner();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.finished = true;
        }
        result
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("DSF encoder is already finished"));
        }
        if self.id3_tag.is_some() && self.metadata != *metadata {
            return Err(AudioError::Unsupported(
                "cannot change parsed text while copying an opaque DSF ID3 tag",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }

    fn set_sample_count(&mut self, sample_count: u64) -> Result<(), AudioError> {
        DsfEncoder::set_sample_count(self, sample_count)
    }
}

fn dsf_channel_type(spec: DsdSpec) -> Result<u32, AudioError> {
    let channels = spec.channels();
    let mask = spec.speaker_mask();
    let channel_type = match channels {
        1 if mask.is_none() => 1,
        2 if mask.is_none() || mask == Some(0x3) => 2,
        3 if mask.is_none() || mask == Some(0x7) => 3,
        4 if mask.is_none() || mask == Some(0x33) => 4,
        4 if mask == Some(0x0f) => 5,
        5 if mask.is_none() || mask == Some(0x37) => 6,
        6 if mask.is_none() || mask == Some(0x3f) => 7,
        _ => {
            return Err(AudioError::Unsupported(
                "DSF encoding requires a supported channel layout",
            ));
        }
    };
    Ok(channel_type)
}
