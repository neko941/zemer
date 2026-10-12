use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};
use zemer_metadata::apev2::ApeTag;

pub(crate) const BLOCK_FRAMES: usize = 131_072;
const FLOAT_BLOCK_FRAMES: usize = 32_768;
pub(crate) const FILE_VERSION: u16 = 0x410;
pub(crate) const MONO_DATA: u32 = 1 << 2;
const FLOAT_DATA: u32 = 1 << 7;
pub(crate) const INT32_DATA: u32 = 1 << 8;
pub(crate) const INITIAL_BLOCK: u32 = 1 << 11;
pub(crate) const FINAL_BLOCK: u32 = 1 << 12;
pub(crate) const HAS_CHECKSUM: u32 = 1 << 28;
pub(crate) const SAMPLE_RATES: [u32; 15] = [
    6_000, 8_000, 9_600, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000,
    88_200, 96_000, 192_000,
];
pub(crate) const MEDIAN_DIVISORS: [u64; 3] = [128, 64, 32];

/// Writes lossless integer PCM and IEEE float32 as WavPack v5 blocks.
pub struct WavPackEncoder<W> {
    writer: W,
    spec: AudioSpec,
    metadata: Metadata,
    samples: Vec<i32>,
    float_samples: Vec<f32>,
    frames: u64,
    encoded_frames: u64,
    blocks: Vec<BlockPatch>,
    poisoned: bool,
    finished: bool,
}

pub(crate) struct BlockPatch {
    pub(crate) offset: u64,
    pub(crate) payload_words: usize,
    pub(crate) checksum: u32,
    pub(crate) header: [u8; 32],
}

impl<W: Write + Seek> WavPackEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        if spec.channels() > 32 {
            return Err(AudioError::Unsupported(
                "WavPack encoding currently supports up to 32 channels",
            ));
        }
        if !matches!(
            spec.sample_format(),
            SampleFormat::PcmU8
                | SampleFormat::PcmI16
                | SampleFormat::PcmI24
                | SampleFormat::PcmI32
                | SampleFormat::Float32
        ) {
            return Err(AudioError::Unsupported(
                "WavPack encoding supports integer PCM and float32",
            ));
        }
        let sample_capacity = BLOCK_FRAMES * usize::from(spec.channels());
        let mut samples = Vec::new();
        let mut float_samples = Vec::new();
        if spec.sample_format() == SampleFormat::Float32 {
            float_samples
                .try_reserve_exact(FLOAT_BLOCK_FRAMES * usize::from(spec.channels()))
                .map_err(|_| AudioError::InvalidData("WavPack encoder buffer is too large"))?;
        } else {
            samples
                .try_reserve_exact(sample_capacity)
                .map_err(|_| AudioError::InvalidData("WavPack encoder buffer is too large"))?;
        }
        Ok(Self {
            writer,
            spec,
            metadata: Metadata::default(),
            samples,
            float_samples,
            frames: 0,
            encoded_frames: 0,
            blocks: Vec::new(),
            poisoned: false,
            finished: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn flush_block(&mut self) -> Result<(), AudioError> {
        if self.spec.sample_format() == SampleFormat::Float32 {
            return self.flush_float_block();
        }
        if self.samples.is_empty() {
            return Ok(());
        }
        let block_index = self.encoded_frames;
        let channels = usize::from(self.spec.channels());
        let frames = u32::try_from(self.samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack block is too large"))?;
        let samples = std::mem::take(&mut self.samples);
        if channels <= 2 {
            self.write_sample_block(&samples, channels, block_index, true, true, None)?;
        } else {
            let mask = self.spec.speaker_mask().unwrap_or_default();
            let mut channel_info = vec![self.spec.channels() as u8];
            channel_info.extend_from_slice(&mask.to_le_bytes());
            for channel in 0..channels {
                let channel_samples: Vec<i32> = samples
                    .iter()
                    .skip(channel)
                    .step_by(channels)
                    .copied()
                    .collect();
                self.write_sample_block(
                    &channel_samples,
                    1,
                    block_index,
                    channel == 0,
                    channel + 1 == channels,
                    (channel == 0).then_some(&channel_info),
                )?;
            }
        }
        self.encoded_frames += u64::from(frames);
        self.samples = samples;
        self.samples.clear();
        Ok(())
    }

    fn flush_float_block(&mut self) -> Result<(), AudioError> {
        if self.float_samples.is_empty() {
            return Ok(());
        }
        let block_index = self.encoded_frames;
        let channels = usize::from(self.spec.channels());
        let frames = u32::try_from(self.float_samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack block is too large"))?;
        let samples = std::mem::take(&mut self.float_samples);
        if channels <= 2 {
            self.write_float_sample_block(&samples, channels, block_index, true, true, None)?;
        } else {
            let mask = self.spec.speaker_mask().unwrap_or_default();
            let mut channel_info = vec![self.spec.channels() as u8];
            channel_info.extend_from_slice(&mask.to_le_bytes());
            let samples_ref = samples.as_slice();
            for first_channel in (0..channels).step_by(2) {
                let block_channels = (channels - first_channel).min(2);
                let channel_samples = (0..frames as usize)
                    .flat_map(|frame| {
                        (first_channel..first_channel + block_channels)
                            .map(move |channel| samples_ref[frame * channels + channel])
                    })
                    .collect::<Vec<_>>();
                self.write_float_sample_block(
                    &channel_samples,
                    block_channels,
                    block_index,
                    first_channel == 0,
                    first_channel + block_channels == channels,
                    (first_channel == 0).then_some(&channel_info),
                )?;
            }
        }
        self.encoded_frames += u64::from(frames);
        self.float_samples = samples;
        self.float_samples.clear();
        Ok(())
    }

    fn write_float_sample_block(
        &mut self,
        samples: &[f32],
        channels: usize,
        block_index: u64,
        initial: bool,
        final_block: bool,
        channel_info: Option<&[u8]>,
    ) -> Result<(), AudioError> {
        let mut flags = 3 | (23 << 18) | FLOAT_DATA | HAS_CHECKSUM;
        if channels == 1 {
            flags |= MONO_DATA;
        }
        if initial {
            flags |= INITIAL_BLOCK;
        }
        if final_block {
            flags |= FINAL_BLOCK;
        }
        let rate_index = SAMPLE_RATES
            .iter()
            .position(|&rate| rate == self.spec.sample_rate());
        if let Some(index) = rate_index {
            flags |= (index as u32) << 23;
        } else {
            flags |= 15 << 23;
        }
        let mut payload = Vec::new();
        write_sub_block(&mut payload, 0x02, &[22])?;
        let mut entropy = Vec::with_capacity(channels * 6);
        for _ in 0..channels * 3 {
            entropy.extend_from_slice(&4096_u16.to_le_bytes());
        }
        write_sub_block(&mut payload, 0x05, &entropy)?;
        if let Some(channel_info) = channel_info {
            write_sub_block(&mut payload, 0x0d, channel_info)?;
        }
        if rate_index.is_none() {
            write_sub_block(&mut payload, 0x27, &self.spec.sample_rate().to_le_bytes())?;
        }
        write_sub_block(&mut payload, 0x08, &[0x08, 0, 127, 127])?;
        let sample_words = vec![0; samples.len()];
        let mut bitstream = encode_words(&sample_words, channels)?;
        if bitstream.len() % 2 != 0 {
            bitstream.push(0);
        }
        write_sub_block(&mut payload, 0x0a, &bitstream)?;
        let mut wvx = Vec::with_capacity(4 + (samples.len() * 33).div_ceil(8));
        wvx.extend_from_slice(&wvx_float_checksum(samples).to_le_bytes());
        // shortcut: stores float bits uncompressed in WVX until blockwise normalization is implemented.
        let mut float_bits = BitWriter::default();
        for sample in samples {
            let bits = sample.to_bits();
            float_bits.write_bit(true);
            for bit in 0..23 {
                float_bits.write_bit(bits & (1 << bit) != 0);
            }
            for bit in 0..8 {
                float_bits.write_bit(bits & (1 << (bit + 23)) != 0);
            }
            float_bits.write_bit(bits & (1 << 31) != 0);
        }
        wvx.extend_from_slice(&float_bits.finish());
        if wvx.len() % 2 != 0 {
            wvx.push(0);
        }
        write_sub_block(&mut payload, 0x0c, &wvx)?;
        let frame_count = u32::try_from(samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack block is too large"))?;
        let crc = sample_checksum(&sample_words, channels);
        let block_size = 32_usize
            .checked_add(payload.len())
            .and_then(|size| size.checked_add(6))
            .ok_or(AudioError::InvalidData("WavPack block size overflow"))?;
        let header = make_header(block_size, block_index, frame_count, flags, crc)?;
        let check = block_checksum(&header, &payload);
        let offset = self.writer.stream_position()?;
        self.blocks.push(BlockPatch {
            offset,
            payload_words: payload.len() / 2,
            checksum: check,
            header,
        });
        payload.extend_from_slice(&[0x2f, 2]);
        payload.extend_from_slice(&check.to_le_bytes());
        self.writer.write_all(&header)?;
        self.writer.write_all(&payload)?;
        Ok(())
    }

    fn write_sample_block(
        &mut self,
        samples: &[i32],
        channels: usize,
        block_index: u64,
        initial: bool,
        final_block: bool,
        channel_info: Option<&[u8]>,
    ) -> Result<(), AudioError> {
        let bytes_per_sample: u32 = match self.spec.sample_format() {
            SampleFormat::PcmU8 => 1,
            SampleFormat::PcmI16 => 2,
            SampleFormat::PcmI24 => 3,
            SampleFormat::PcmI32 => 4,
            _ => unreachable!(),
        };
        let is_extended = self.spec.sample_format() == SampleFormat::PcmI32;
        let encoded_bits = if is_extended {
            24
        } else {
            sample_bits(self.spec.sample_format())
        };
        let mut flags = (bytes_per_sample - 1) | (u32::from(encoded_bits - 1) << 18) | HAS_CHECKSUM;
        if channels == 1 {
            flags |= MONO_DATA;
        }
        if initial {
            flags |= INITIAL_BLOCK;
        }
        if final_block {
            flags |= FINAL_BLOCK;
        }
        if is_extended {
            flags |= INT32_DATA;
        }
        let rate_index = SAMPLE_RATES
            .iter()
            .position(|&rate| rate == self.spec.sample_rate());
        if let Some(index) = rate_index {
            flags |= (index as u32) << 23;
        } else {
            flags |= 15 << 23;
        }
        let mut payload = Vec::new();
        write_sub_block(&mut payload, 0x02, &[22])?;
        let mut entropy = Vec::with_capacity(channels * 6);
        for _ in 0..channels * 3 {
            entropy.extend_from_slice(&4096_u16.to_le_bytes());
        }
        write_sub_block(&mut payload, 0x05, &entropy)?;
        if let Some(channel_info) = channel_info {
            write_sub_block(&mut payload, 0x0d, channel_info)?;
        }
        if rate_index.is_none() {
            write_sub_block(&mut payload, 0x27, &self.spec.sample_rate().to_le_bytes())?;
        }
        let coded_samples: Vec<i32> = if is_extended {
            samples.iter().map(|sample| sample >> 8).collect()
        } else {
            samples.to_vec()
        };
        if is_extended {
            write_sub_block(&mut payload, 0x09, &[8, 0, 0, 0])?;
        }
        let mut bitstream = encode_words(&coded_samples, channels)?;
        if bitstream.len() % 2 != 0 {
            bitstream.push(0);
        }
        write_sub_block(&mut payload, 0x0a, &bitstream)?;
        if is_extended {
            let mut wvx = Vec::with_capacity(4 + coded_samples.len());
            wvx.extend_from_slice(&wvx_integer_checksum(samples).to_le_bytes());
            let mut low_bits = BitWriter::default();
            for sample in samples {
                for bit in 0..8 {
                    low_bits.write_bit(*sample as u32 & (1 << bit) != 0);
                }
            }
            wvx.extend_from_slice(&low_bits.finish());
            if wvx.len() % 2 != 0 {
                wvx.push(0);
            }
            write_sub_block(&mut payload, 0x0c, &wvx)?;
        }
        let frame_count = u32::try_from(samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack block is too large"))?;
        let crc = sample_checksum(&coded_samples, channels);
        let block_size = 32_usize
            .checked_add(payload.len())
            .and_then(|size| size.checked_add(6))
            .ok_or(AudioError::InvalidData("WavPack block size overflow"))?;
        let header = make_header(block_size, block_index, frame_count, flags, crc)?;
        let check = block_checksum(&header, &payload);
        let offset = self.writer.stream_position()?;
        self.blocks.push(BlockPatch {
            offset,
            payload_words: payload.len() / 2,
            checksum: check,
            header,
        });
        payload.extend_from_slice(&[0x2f, 2]);
        payload.extend_from_slice(&check.to_le_bytes());
        self.writer.write_all(&header)?;
        self.writer.write_all(&payload)?;
        Ok(())
    }

    fn patch_total_samples(&mut self) -> Result<(), AudioError> {
        patch_total_samples(&mut self.writer, self.frames, &self.blocks)
    }
}

impl<W: Write + Seek> AudioEncoder for WavPackEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData(
                "WavPack encoder is already finished",
            ));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WavPack encoder is unusable after an earlier write failure",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "WavPack input PCM specification changed",
            ));
        }
        let channels = usize::from(self.spec.channels());
        let frames = u64::try_from(block.frames())
            .map_err(|_| AudioError::InvalidData("WavPack frame count overflow"))?;
        self.frames = self
            .frames
            .checked_add(frames)
            .filter(|&count| count < (1_u64 << 40))
            .ok_or(AudioError::InvalidData(
                "WavPack sample count exceeds 40 bits",
            ))?;
        let result = match block.samples() {
            Samples::F32(values) => append_float_samples(self, values.iter().copied(), channels),
            Samples::U8(values) => append_samples(
                self,
                values.iter().map(|&value| i32::from(value) - 128),
                channels,
            ),
            Samples::I16(values) => {
                append_samples(self, values.iter().map(|&value| i32::from(value)), channels)
            }
            Samples::I24(values) | Samples::I32(values) => {
                append_samples(self, values.iter().copied(), channels)
            }
            _ => return Err(AudioError::Unsupported("WavPack integer PCM required")),
        };
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WavPack encoder is unusable after an earlier write failure",
            ));
        }
        if self.frames == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty WavPack stream",
            ));
        }
        let result = (|| {
            self.flush_block()?;
            self.patch_total_samples()?;
            self.writer.seek(SeekFrom::End(0))?;
            if self.metadata.iter().next().is_some() {
                let mut tag = ApeTag::new();
                tag.replace_text_metadata(&self.metadata)
                    .map_err(|_| AudioError::InvalidData("invalid WavPack APEv2 metadata"))?;
                self.writer.write_all(&tag.encode().map_err(|_| {
                    AudioError::InvalidData("WavPack APEv2 metadata exceeds the supported size")
                })?)?;
            }
            self.writer.flush()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.finished = true;
        }
        result
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData(
                "WavPack encoder is already finished",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }
}

fn append_samples<W: Write + Seek>(
    encoder: &mut WavPackEncoder<W>,
    values: impl Iterator<Item = i32>,
    channels: usize,
) -> Result<(), AudioError> {
    for sample in values {
        encoder.samples.push(sample);
        if encoder.samples.len() == BLOCK_FRAMES * channels {
            encoder.flush_block()?;
        }
    }
    Ok(())
}

fn append_float_samples<W: Write + Seek>(
    encoder: &mut WavPackEncoder<W>,
    values: impl Iterator<Item = f32>,
    channels: usize,
) -> Result<(), AudioError> {
    for sample in values {
        encoder.float_samples.push(sample);
        if encoder.float_samples.len() == FLOAT_BLOCK_FRAMES * channels {
            encoder.flush_block()?;
        }
    }
    Ok(())
}

fn wvx_float_checksum(samples: &[f32]) -> u32 {
    samples.iter().fold(u32::MAX, |checksum, sample| {
        let bits = sample.to_bits();
        checksum
            .wrapping_mul(27)
            .wrapping_add((bits & 0x7f_ffff).wrapping_mul(9))
            .wrapping_add(((bits >> 23) & 0xff).wrapping_mul(3))
            .wrapping_add(bits >> 31)
    })
}

pub(crate) fn encode_words(samples: &[i32], channels: usize) -> Result<Vec<u8>, AudioError> {
    let mut writer = BitWriter::default();
    let mut medians = [[32_768_u32; 3]; 2];
    let mut holding_zero = false;
    let mut holding_one = false;
    for (word_index, &sample) in samples.iter().enumerate() {
        let channel = word_index % channels;
        if medians[0][0] & !1 == 0
            && (channels == 1 || medians[1][0] & !1 == 0)
            && !holding_zero
            && !holding_one
        {
            writer.write_zero_run(0);
        }
        let magnitude = if sample < 0 {
            !(sample as u32)
        } else {
            sample as u32
        };
        let median0 = get_median(medians[channel][0]);
        let (category, low, width) = if magnitude < median0 {
            update_median(&mut medians[channel][0], 0, false)?;
            (0, 0, median0)
        } else {
            let median1 = get_median(medians[channel][1]);
            if magnitude - median0 < median1 {
                update_median(&mut medians[channel][0], 0, true)?;
                update_median(&mut medians[channel][1], 1, false)?;
                (1, median0, median1)
            } else {
                let median2 = get_median(medians[channel][2]);
                let count = 2 + (magnitude - median0 - median1) / median2;
                let low = u64::from(median0)
                    + u64::from(median1)
                    + u64::from(count - 2) * u64::from(median2);
                update_median(&mut medians[channel][0], 0, true)?;
                update_median(&mut medians[channel][1], 1, true)?;
                update_median(&mut medians[channel][2], 2, count > 2)?;
                (
                    count,
                    u32::try_from(low)
                        .map_err(|_| AudioError::InvalidData("WavPack word range overflow"))?,
                    median2,
                )
            }
        };
        let old_holding_one = holding_one;
        let use_prefix = !holding_zero;
        let next_channel = (channel + 1) % channels;
        let next_value = samples.get(word_index + 1).copied().unwrap_or(0);
        let next_magnitude = if next_value < 0 {
            !(next_value as u32)
        } else {
            next_value as u32
        };
        let next_hold_one = category_for(next_magnitude, medians[next_channel]) != 0;
        if use_prefix {
            let raw = if old_holding_one {
                (category - 1) * 2 + u32::from(next_hold_one)
            } else {
                category * 2 + u32::from(next_hold_one)
            };
            writer.write_magnitude_prefix(raw);
            holding_one = next_hold_one;
            holding_zero = !holding_one;
        } else {
            if category != 0 {
                return Err(AudioError::InvalidData("invalid WavPack entropy state"));
            }
            holding_one = false;
            holding_zero = false;
        }
        let range_low = low & 0x7fff_ffff;
        let range_high = range_low.wrapping_add(width.saturating_sub(1)) & 0x7fff_ffff;
        let high = range_high.max(range_low);
        let offset = magnitude
            .checked_sub(range_low)
            .filter(|&value| value <= high - range_low)
            .ok_or(AudioError::InvalidData(
                "WavPack magnitude is outside its code range",
            ))?;
        writer.write_code(high - range_low, offset)?;
        writer.write_bit(sample < 0);
    }
    Ok(writer.finish())
}

fn category_for(magnitude: u32, medians: [u32; 3]) -> u32 {
    let m0 = get_median(medians[0]);
    if magnitude < m0 {
        return 0;
    }
    let m1 = get_median(medians[1]);
    if magnitude - m0 < m1 {
        return 1;
    }
    let m2 = get_median(medians[2]);
    2 + (magnitude - m0 - m1) / m2
}

fn sample_bits(format: SampleFormat) -> u8 {
    match format {
        SampleFormat::PcmU8 => 8,
        SampleFormat::PcmI16 => 16,
        SampleFormat::PcmI24 => 24,
        SampleFormat::PcmI32 => 32,
        _ => unreachable!(),
    }
}

pub(crate) fn get_median(value: u32) -> u32 {
    (value >> 4) + 1
}

pub(crate) fn update_median(
    value: &mut u32,
    index: usize,
    increment: bool,
) -> Result<(), AudioError> {
    let divisor = MEDIAN_DIVISORS[index];
    let value64 = u64::from(*value);
    let delta = if increment {
        (value64 + divisor) / divisor * 5
    } else {
        (value64 + divisor - 2) / divisor * 2
    };
    let next = if increment {
        value64 + delta
    } else {
        value64.saturating_sub(delta)
    };
    *value = u32::try_from(next).map_err(|_| AudioError::InvalidData("WavPack median overflow"))?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    offset: u8,
}

impl BitWriter {
    pub(crate) fn write_bit(&mut self, bit: bool) {
        if self.offset == 0 {
            self.bytes.push(0);
        }
        if bit {
            *self.bytes.last_mut().unwrap() |= 1 << self.offset;
        }
        self.offset = (self.offset + 1) & 7;
    }

    pub(crate) fn write_zero_run(&mut self, run: u32) {
        if run < 2 {
            self.write_bit(run == 1);
            if run == 1 {
                self.write_bit(false);
            }
            return;
        }
        let width = u32::BITS - run.leading_zeros();
        for _ in 0..width {
            self.write_bit(true);
        }
        self.write_bit(false);
        for bit in 0..width - 1 {
            self.write_bit(run & (1 << bit) != 0);
        }
    }

    pub(crate) fn write_magnitude_prefix(&mut self, value: u32) {
        let ones = value.min(16);
        for _ in 0..ones {
            self.write_bit(true);
        }
        self.write_bit(false);
        if value >= 16 {
            self.write_zero_run(value - 16);
        }
    }

    pub(crate) fn write_code(&mut self, max_code: u32, value: u32) -> Result<(), AudioError> {
        if value > max_code {
            return Err(AudioError::InvalidData(
                "invalid WavPack adjusted-binary code",
            ));
        }
        if max_code < 2 {
            if max_code == 1 {
                self.write_bit(value != 0);
            }
            return Ok(());
        }
        let bit_count = u32::BITS - max_code.leading_zeros();
        let extras = (1_u64 << bit_count) - u64::from(max_code) - 1;
        if u64::from(value) < extras {
            for bit in 0..bit_count - 1 {
                self.write_bit(value & (1 << bit) != 0);
            }
        } else {
            let adjusted = u64::from(value) + extras;
            let prefix = (adjusted >> 1) as u32;
            for bit in 0..bit_count - 1 {
                self.write_bit(prefix & (1 << bit) != 0);
            }
            self.write_bit(adjusted & 1 != 0);
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) fn write_sub_block(
    payload: &mut Vec<u8>,
    id: u8,
    data: &[u8],
) -> Result<(), AudioError> {
    let words = data.len().div_ceil(2);
    if words > 0x00ff_ffff {
        return Err(AudioError::InvalidData(
            "WavPack metadata sub-block exceeds the 24-bit word limit",
        ));
    }
    let odd = if data.len() % 2 != 0 { 0x40 } else { 0 };
    if words > u8::MAX as usize || matches!(id, 0x0a | 0x0c | 0x2c) {
        payload.push(id | odd | 0x80);
        let words = words as u32;
        payload.extend_from_slice(&words.to_le_bytes()[..3]);
    } else {
        payload.push(id | odd);
        payload.push(words as u8);
    }
    payload.extend_from_slice(data);
    if data.len() % 2 != 0 {
        payload.push(0);
    }
    Ok(())
}

pub(crate) fn make_header(
    block_size: usize,
    block_index: u64,
    block_samples: u32,
    flags: u32,
    crc: u32,
) -> Result<[u8; 32], AudioError> {
    let ck_size = u32::try_from(block_size - 8)
        .map_err(|_| AudioError::InvalidData("WavPack block exceeds 32-bit size"))?;
    let mut header = [0; 32];
    header[..4].copy_from_slice(b"wvpk");
    header[4..8].copy_from_slice(&ck_size.to_le_bytes());
    header[8..10].copy_from_slice(&FILE_VERSION.to_le_bytes());
    header[10] = (block_index >> 32) as u8;
    header[11] = 0;
    header[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    header[16..20].copy_from_slice(&(block_index as u32).to_le_bytes());
    header[20..24].copy_from_slice(&block_samples.to_le_bytes());
    header[24..28].copy_from_slice(&flags.to_le_bytes());
    header[28..32].copy_from_slice(&crc.to_le_bytes());
    Ok(header)
}

pub(crate) fn sample_checksum(samples: &[i32], channels: usize) -> u32 {
    let mut checksum = u32::MAX;
    if channels == 1 {
        for &sample in samples {
            checksum = checksum.wrapping_mul(3).wrapping_add(sample as u32);
        }
    } else {
        for pair in samples.chunks_exact(2) {
            checksum = checksum
                .wrapping_add(checksum.wrapping_shl(3))
                .wrapping_add((pair[0] as u32).wrapping_shl(1))
                .wrapping_add(pair[0] as u32)
                .wrapping_add(pair[1] as u32);
        }
    }
    checksum
}

pub(crate) fn wvx_integer_checksum(samples: &[i32]) -> u32 {
    samples.iter().fold(u32::MAX, |checksum, sample| {
        let sample = *sample as u32;
        checksum
            .wrapping_mul(9)
            .wrapping_add((sample & 0xffff).wrapping_mul(3))
            .wrapping_add(sample >> 16)
    })
}

pub(crate) fn block_checksum(header: &[u8; 32], payload: &[u8]) -> u32 {
    let mut checksum = u32::MAX;
    for word in header.chunks_exact(2).chain(payload.chunks_exact(2)) {
        checksum = checksum
            .wrapping_mul(3)
            .wrapping_add(u32::from(u16::from_le_bytes([word[0], word[1]])));
    }
    checksum
}

pub(crate) fn patch_total_samples<W: Write + Seek>(
    writer: &mut W,
    total: u64,
    blocks: &[BlockPatch],
) -> Result<(), AudioError> {
    for block in blocks {
        let mut fields = [block.header[10], (total >> 32) as u8, 0, 0, 0, 0];
        fields[2..].copy_from_slice(&(total as u32).to_le_bytes());
        let mut checksum = block.checksum;
        let total_words = 16 + block.payload_words;
        for (index, offset) in [10_usize, 12, 14].into_iter().enumerate() {
            let old = u16::from_le_bytes([block.header[offset], block.header[offset + 1]]);
            let new = u16::from_le_bytes([fields[index * 2], fields[index * 2 + 1]]);
            let exponent = total_words - 1 - (5 + index);
            checksum = checksum.wrapping_add(
                u32::from(new)
                    .wrapping_sub(u32::from(old))
                    .wrapping_mul(pow3(exponent)),
            );
        }
        writer.seek(SeekFrom::Start(block.offset + 11))?;
        writer.write_all(&fields[1..])?;
        let crc_offset = block.offset + 32 + (block.payload_words as u64 * 2) + 2;
        writer.seek(SeekFrom::Start(crc_offset))?;
        writer.write_all(&checksum.to_le_bytes())?;
    }
    Ok(())
}

fn pow3(mut exponent: usize) -> u32 {
    let mut base = 3_u32;
    let mut result = 1_u32;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = result.wrapping_mul(base);
        }
        base = base.wrapping_mul(base);
        exponent >>= 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{
        AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
    };

    use super::WavPackEncoder;
    use crate::WavPackDecoder;

    fn round_trip(spec: AudioSpec, samples: Samples) -> Samples {
        let source = PcmBuffer::new(spec, samples.clone()).unwrap();
        let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder
            .write_block(&source)
            .unwrap_or_else(|error| panic!("encode {:?}: {error}", spec.sample_format()));
        encoder
            .finish()
            .unwrap_or_else(|error| panic!("finish {:?}: {error}", spec.sample_format()));
        let bytes = encoder.into_inner().into_inner();
        let mut decoder = WavPackDecoder::new(Cursor::new(bytes)).unwrap();
        let mut decoded = None;
        while let Some(block) = decoder.read_frames(4096).unwrap() {
            if let Some(output) = &mut decoded {
                append(output, block.samples());
            } else {
                decoded = Some(block.samples().clone());
            }
        }
        decoded.unwrap()
    }

    fn append(output: &mut Samples, block: &Samples) {
        match (output, block) {
            (Samples::U8(a), Samples::U8(b)) => a.extend_from_slice(b),
            (Samples::I16(a), Samples::I16(b)) => a.extend_from_slice(b),
            (Samples::I24(a), Samples::I24(b)) => a.extend_from_slice(b),
            (Samples::I32(a), Samples::I32(b)) => a.extend_from_slice(b),
            _ => panic!("decoded sample format changed"),
        }
    }

    #[test]
    fn lossless_pcm_round_trips_all_integer_widths_and_stereo() {
        for (format, samples) in [
            (
                SampleFormat::PcmU8,
                Samples::U8(vec![0, 255, 128, 1, 254, 127]),
            ),
            (
                SampleFormat::PcmI16,
                Samples::I16(vec![i16::MIN, i16::MAX, -1, 0, 1234, -9876]),
            ),
            (
                SampleFormat::PcmI24,
                Samples::I24(vec![-8_388_608, 8_388_607, -1, 0, 1234, -9876]),
            ),
            (
                SampleFormat::PcmI32,
                Samples::I32(vec![i32::MIN, i32::MAX, -1, 0, 1234, -9876]),
            ),
        ] {
            let spec = AudioSpec::new(44_100, 2, format).unwrap();
            assert_eq!(
                round_trip(spec, samples.clone()),
                samples,
                "failed sample format {format:?}"
            );
        }
    }

    #[test]
    fn lossless_pcm_round_trips_mono_and_custom_rates_with_metadata() {
        let spec = AudioSpec::new(123_456, 1, SampleFormat::PcmI16).unwrap();
        let source = PcmBuffer::new(spec, Samples::I16(vec![0, 1, -1, 1234, -2345])).unwrap();
        let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("artist", "Zemer");
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_block(&source).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        let mut decoder = WavPackDecoder::new_seekable(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.metadata().values("ARTIST"), ["Zemer"]);
        assert_eq!(decoder.spec().sample_rate(), 123_456);
        assert_eq!(decoder.spec().channels(), 1);
        assert_eq!(decoder.read_frames(16).unwrap().unwrap(), source);
        assert!(decoder.read_frames(16).unwrap().is_none());
    }

    #[test]
    fn multi_block_stream_stops_at_the_tag_and_decodes_every_frame() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap();
        let values: Vec<i16> = (0..(131_073 * 2))
            .map(|index| ((index * 31) % 65_536) as i16)
            .collect();
        let source = PcmBuffer::new(spec, Samples::I16(values.clone())).unwrap();
        let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("title", "two blocks");
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_block(&source).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        let mut decoder = WavPackDecoder::new_seekable(Cursor::new(bytes)).unwrap();
        let mut output = Vec::new();
        while let Some(block) = decoder.read_frames(8192).unwrap() {
            let Samples::I16(block) = block.samples() else {
                panic!("decoded sample format changed")
            };
            output.extend_from_slice(block);
        }
        assert_eq!(decoder.metadata().values("TITLE"), ["two blocks"]);
        assert_eq!(output, values);
    }

    #[test]
    fn rejects_empty_files_and_non_integer_formats() {
        let empty_spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
        let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), empty_spec).unwrap();
        assert!(encoder.finish().is_err());
        let double_spec = AudioSpec::new(44_100, 1, SampleFormat::Float64).unwrap();
        assert!(WavPackEncoder::new(Cursor::new(Vec::new()), double_spec).is_err());
    }
}
