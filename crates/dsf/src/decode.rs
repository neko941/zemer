use std::io::{Read, Seek, SeekFrom};

use zemer_core::{AudioError, DsdBuffer, DsdDecoder, DsdSpec, Metadata};

const DSD_HEADER_SIZE: u64 = 28;
const FMT_CHUNK_SIZE: u64 = 52;
const BLOCK_BYTES: usize = 4096;
const MAX_CHANNELS: u32 = 6;

/// Streams raw one-bit DSD samples from a DSF 1.01 file.
pub struct DsfDecoder<R> {
    reader: R,
    spec: DsdSpec,
    metadata: Metadata,
    sample_count: u64,
    total_octets: u64,
    emitted_octets: u64,
    block_octets: usize,
    block_index: u64,
    channels: usize,
    channel_blocks: Vec<Vec<u8>>,
}

impl<R: Read + Seek> DsfDecoder<R> {
    pub fn new(mut reader: R) -> Result<Self, AudioError> {
        let mut dsd = [0; 28];
        reader.read_exact(&mut dsd)?;
        if &dsd[..4] != b"DSD "
            || u64::from_le_bytes(dsd[4..12].try_into().unwrap()) != DSD_HEADER_SIZE
        {
            return Err(AudioError::InvalidFormat("invalid DSF DSD chunk"));
        }
        let file_size = u64::from_le_bytes(dsd[12..20].try_into().unwrap());
        let metadata_offset = u64::from_le_bytes(dsd[20..28].try_into().unwrap());
        let actual_size = reader.seek(SeekFrom::End(0))?;
        if file_size != actual_size || file_size < DSD_HEADER_SIZE + FMT_CHUNK_SIZE + 12 {
            return Err(AudioError::InvalidFormat("invalid DSF file size"));
        }

        reader.seek(SeekFrom::Start(DSD_HEADER_SIZE))?;
        let mut fmt = [0; 52];
        reader.read_exact(&mut fmt)?;
        let fmt_size = u64::from_le_bytes(fmt[4..12].try_into().unwrap());
        let version = u32::from_le_bytes(fmt[12..16].try_into().unwrap());
        let format_id = u32::from_le_bytes(fmt[16..20].try_into().unwrap());
        let channel_type = u32::from_le_bytes(fmt[20..24].try_into().unwrap());
        let channels = u32::from_le_bytes(fmt[24..28].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(fmt[28..32].try_into().unwrap());
        let bits_per_sample = u32::from_le_bytes(fmt[32..36].try_into().unwrap());
        let sample_count = u64::from_le_bytes(fmt[36..44].try_into().unwrap());
        let block_size = u32::from_le_bytes(fmt[44..48].try_into().unwrap());
        let reserved = u32::from_le_bytes(fmt[48..52].try_into().unwrap());
        if &fmt[..4] != b"fmt "
            || fmt_size != FMT_CHUNK_SIZE
            || version != 1
            || format_id != 0
            || !(1..=MAX_CHANNELS).contains(&channels)
            || !valid_channel_type(channel_type, channels)
            || !matches!(sample_rate, 2_822_400 | 5_644_800)
            || bits_per_sample != 1
            || sample_count == 0
            || block_size != BLOCK_BYTES as u32
            || reserved != 0
        {
            return Err(AudioError::Unsupported(
                "unsupported DSF 1.01 format profile",
            ));
        }

        let mut data = [0; 12];
        reader.read_exact(&mut data)?;
        let data_size = u64::from_le_bytes(data[4..12].try_into().unwrap());
        let blocks_per_channel = sample_count
            .checked_add((BLOCK_BYTES as u64) * 8 - 1)
            .ok_or(AudioError::InvalidData("DSF sample count overflow"))?
            / ((BLOCK_BYTES as u64) * 8);
        let expected_data_size = blocks_per_channel
            .checked_mul(BLOCK_BYTES as u64)
            .and_then(|size| size.checked_mul(u64::from(channels)))
            .and_then(|size| size.checked_add(12))
            .ok_or(AudioError::InvalidData("DSF data size overflow"))?;
        let data_end = DSD_HEADER_SIZE
            .checked_add(FMT_CHUNK_SIZE)
            .and_then(|size| size.checked_add(data_size))
            .ok_or(AudioError::InvalidData("DSF data boundary overflow"))?;
        if &data[..4] != b"data"
            || data_size != expected_data_size
            || data_end > file_size
            || (metadata_offset != 0 && metadata_offset < data_end)
            || (metadata_offset != 0 && metadata_offset >= file_size)
        {
            return Err(AudioError::InvalidFormat("invalid DSF data chunk bounds"));
        }

        reader.seek(SeekFrom::Start(DSD_HEADER_SIZE + FMT_CHUNK_SIZE + 12))?;
        let channels = channels as usize;
        let mut channel_blocks = Vec::new();
        channel_blocks
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::InvalidData("DSF channel count is too large"))?;
        for _ in 0..channels {
            channel_blocks.push(vec![0; BLOCK_BYTES]);
        }
        Ok(Self {
            reader,
            spec: DsdSpec::new_with_speaker_mask(
                sample_rate,
                channels as u16,
                channel_mask(channel_type),
            )
            .map_err(|error| AudioError::InvalidData(error.0))?,
            metadata: Metadata::default(),
            sample_count,
            total_octets: sample_count.div_ceil(8),
            emitted_octets: 0,
            block_octets: 0,
            block_index: 0,
            channels,
            channel_blocks,
        })
    }

    /// Exact per-channel DSD bit count from the DSF `fmt ` chunk.
    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    fn load_block(&mut self) -> Result<(), AudioError> {
        let first_octet = self
            .block_index
            .checked_mul((BLOCK_BYTES as u64) * 8)
            .ok_or(AudioError::InvalidData("DSF block position overflow"))?;
        let remaining_samples = self.sample_count - first_octet;
        self.block_octets = usize::try_from(remaining_samples.div_ceil(8))
            .map_err(|_| AudioError::InvalidData("DSF block size overflow"))?
            .min(BLOCK_BYTES);
        for channel in 0..self.channels {
            self.reader.read_exact(&mut self.channel_blocks[channel])?;
            if self.channel_blocks[channel][self.block_octets..]
                .iter()
                .any(|&byte| byte != 0)
            {
                return Err(AudioError::InvalidData("nonzero DSF block padding"));
            }
            self.channel_blocks[channel][..self.block_octets]
                .iter_mut()
                .for_each(|byte| *byte = byte.reverse_bits());
            let final_samples = (self.sample_count % 8) as u8;
            if remaining_samples <= 8
                && final_samples != 0
                && self.channel_blocks[channel][self.block_octets - 1]
                    & ((1 << (8 - final_samples)) - 1)
                    != 0
            {
                return Err(AudioError::InvalidData("nonzero DSF partial-octet padding"));
            }
        }
        self.block_index += 1;
        Ok(())
    }
}

impl<R: Read + Seek> DsdDecoder for DsfDecoder<R> {
    fn dsd_spec(&self) -> DsdSpec {
        self.spec
    }

    fn metadata(&self) -> &Metadata {
        &self.metadata
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
        if self.emitted_octets == self.total_octets {
            return Ok(None);
        }
        let next_block_start = self
            .block_index
            .checked_mul(BLOCK_BYTES as u64)
            .ok_or(AudioError::InvalidData("DSF block position overflow"))?;
        if self.emitted_octets == next_block_start {
            self.load_block()?;
        }
        let block_start = self
            .block_index
            .checked_sub(1)
            .and_then(|index| index.checked_mul(BLOCK_BYTES as u64))
            .ok_or(AudioError::InvalidData("DSF block position overflow"))?;
        let within_block = usize::try_from(self.emitted_octets - block_start)
            .map_err(|_| AudioError::InvalidData("DSF output position overflow"))?;
        let frames = max_octet_frames
            .min(self.block_octets - within_block)
            .min(usize::try_from(self.total_octets - self.emitted_octets).unwrap_or(usize::MAX));
        let output_len = frames
            .checked_mul(self.channels)
            .ok_or(AudioError::InvalidData("DSF output size overflow"))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(output_len)
            .map_err(|_| AudioError::InvalidData("DSF output block is too large"))?;
        for frame in within_block..within_block + frames {
            for channel in 0..self.channels {
                output.push(self.channel_blocks[channel][frame]);
            }
        }
        self.emitted_octets += frames as u64;
        Ok(Some(
            DsdBuffer::new(self.spec, output).map_err(|error| AudioError::InvalidData(error.0))?,
        ))
    }
}

fn valid_channel_type(channel_type: u32, channels: u32) -> bool {
    matches!(
        (channel_type, channels),
        (1, 1) | (2, 2) | (3, 3) | (4, 4) | (5, 4) | (6, 5) | (7, 6)
    )
}

fn channel_mask(channel_type: u32) -> Option<u32> {
    match channel_type {
        2 => Some(0x3),
        3 => Some(0x7),
        4 => Some(0x33),
        5 => Some(0x0f),
        6 => Some(0x37),
        7 => Some(0x3f),
        _ => None,
    }
}
