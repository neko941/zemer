use std::io::{Cursor, Read};

use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{
    WavPackBlockHeader, bitstream::BitReader, entropy::EntropyDecoder, float::FloatInfo,
    hybrid::HybridState, metadata::visit_metadata_sub_blocks,
};

const MAX_BLOCK_SIZE: u64 = 64 * 1024 * 1024;
const MONO_DATA: u32 = 1 << 2;
const HYBRID_FLAG: u32 = 1 << 3;
const JOINT_STEREO: u32 = 1 << 4;
const HYBRID_SHAPE: u32 = 1 << 6;
const FLOAT_DATA: u32 = 1 << 7;
const INT32_DATA: u32 = 1 << 8;
const INITIAL_BLOCK: u32 = 1 << 11;
const FINAL_BLOCK: u32 = 1 << 12;
const DSD_DATA: u32 = 1 << 31;
const FALSE_STEREO: u32 = 1 << 30;
const BYTES_STORED: u32 = 0x3;
const SHIFT_MASK: u32 = 0x3e000;
const MAX_CHANNELS: usize = 256;
const MAX_DECODED_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const SAMPLE_RATES: [u32; 15] = [
    6_000, 8_000, 9_600, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000,
    88_200, 96_000, 192_000,
];

pub struct WavPackDecoder<R> {
    reader: R,
    spec: AudioSpec,
    metadata: Metadata,
    pending: Option<Samples>,
    pending_offset: usize,
    has_audio_block: bool,
    finished: bool,
}

impl<R: Read> WavPackDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let mut decoder = Self {
            reader,
            spec: AudioSpec::new(44_100, 1, SampleFormat::PcmI16)?,
            metadata: Metadata::default(),
            pending: None,
            pending_offset: 0,
            has_audio_block: false,
            finished: false,
        };
        while decoder.pending.is_none() && decoder.read_next_block()? {}
        if decoder.pending.is_none() {
            return Err(AudioError::InvalidFormat(
                "WavPack stream has no audio blocks",
            ));
        }
        Ok(decoder)
    }

    fn read_next_block(&mut self) -> Result<bool, AudioError> {
        let Some(header) = WavPackBlockHeader::read_from(&mut self.reader)? else {
            self.finished = true;
            return Ok(false);
        };
        validate_header(&header)?;
        if header.block_samples == 0 {
            let payload = read_payload(&mut self.reader, &header)?;
            let mut input = Cursor::new(payload.as_slice());
            visit_metadata_sub_blocks(&mut input, payload.len() as u64, |_, _| Ok(()))?;
            return Ok(true);
        }

        if header.flags & INITIAL_BLOCK == 0 {
            return Err(AudioError::InvalidData(
                "WavPack audio block group does not start with INITIAL_BLOCK",
            ));
        }

        let mut blocks: Vec<(AudioSpec, Samples)> = Vec::new();
        let mut channel_info = None;
        let mut current = header;
        let mut first_block = true;
        loop {
            validate_header(&current)?;
            if current.block_index() != header.block_index()
                || current.block_samples != header.block_samples
            {
                return Err(AudioError::InvalidData(
                    "WavPack channel blocks disagree on sample position or count",
                ));
            }
            if !first_block && current.flags & INITIAL_BLOCK != 0 {
                return Err(AudioError::InvalidData(
                    "WavPack channel block group has an unexpected INITIAL_BLOCK",
                ));
            }
            let payload = read_payload(&mut self.reader, &current)?;
            let (block_spec, samples, info) = decode_lossless_pcm(&current, &payload)?;
            if self.has_audio_block
                && (block_spec.sample_rate() != self.spec.sample_rate()
                    || block_spec.sample_format() != self.spec.sample_format())
            {
                return Err(AudioError::InvalidData(
                    "WavPack stream format changed between blocks",
                ));
            }
            if let Some((first_spec, _)) = blocks.first()
                && (block_spec.sample_rate() != first_spec.sample_rate()
                    || block_spec.sample_format() != first_spec.sample_format())
            {
                return Err(AudioError::InvalidData(
                    "WavPack channel blocks use different sample formats",
                ));
            }
            if let Some(info) = info {
                if channel_info.is_some() {
                    return Err(AudioError::InvalidData(
                        "duplicate WavPack channel information in block group",
                    ));
                }
                channel_info = Some(info);
            }
            blocks.push((block_spec, samples));
            first_block = false;
            if current.flags & FINAL_BLOCK != 0 {
                break;
            }
            current = WavPackBlockHeader::read_from(&mut self.reader)?.ok_or(
                AudioError::InvalidData("truncated WavPack multichannel block group"),
            )?;
            if current.block_samples == 0 {
                return Err(AudioError::InvalidData(
                    "WavPack multichannel block group contains a non-audio block",
                ));
            }
        }

        let channels = blocks
            .iter()
            .map(|(spec, _)| usize::from(spec.channels()))
            .sum::<usize>();
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(AudioError::Unsupported(
                "WavPack channel count exceeds the supported limit",
            ));
        }
        let speaker_mask = match channel_info {
            Some(info) if usize::from(info.channels) == channels => info.speaker_mask,
            Some(_) => {
                return Err(AudioError::InvalidData(
                    "WavPack channel information disagrees with its block group",
                ));
            }
            None if channels > 2 => {
                return Err(AudioError::InvalidData(
                    "multichannel WavPack stream has no CHANNEL_INFO metadata",
                ));
            }
            None => None,
        };
        let first_spec = blocks[0].0;
        let frames = usize::try_from(header.block_samples).map_err(|_| {
            AudioError::InvalidData("WavPack block sample count does not fit memory")
        })?;
        let bytes_needed = frames
            .checked_mul(channels)
            .and_then(|samples| samples.checked_mul(4))
            .ok_or(AudioError::InvalidData(
                "decoded WavPack block size overflow",
            ))?;
        if bytes_needed > MAX_DECODED_BLOCK_BYTES {
            return Err(AudioError::InvalidData(
                "decoded WavPack channel block group exceeds the configured size limit",
            ));
        }
        let samples = interleave_blocks(&blocks, frames, channels, first_spec.sample_format())?;
        let spec = AudioSpec::new_with_speaker_mask(
            first_spec.sample_rate(),
            channels as u16,
            first_spec.sample_format(),
            speaker_mask,
        )?;
        if self.has_audio_block && spec != self.spec {
            return Err(AudioError::InvalidData(
                "WavPack stream format changed between blocks",
            ));
        }
        self.has_audio_block = true;
        self.spec = spec;
        self.pending = Some(samples);
        self.pending_offset = 0;
        Ok(true)
    }
}

fn validate_header(header: &WavPackBlockHeader) -> Result<(), AudioError> {
    if header.version < 0x402 || header.version > 0x410 {
        return Err(AudioError::Unsupported(
            "unsupported WavPack stream version",
        ));
    }
    if header.block_size > MAX_BLOCK_SIZE {
        return Err(AudioError::InvalidData(
            "WavPack block exceeds the configured size limit",
        ));
    }
    Ok(())
}

fn read_payload<R: Read>(
    reader: &mut R,
    header: &WavPackBlockHeader,
) -> Result<Vec<u8>, AudioError> {
    let payload_size = usize::try_from(header.payload_size())
        .map_err(|_| AudioError::InvalidData("WavPack block size does not fit memory"))?;
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(payload_size)
        .map_err(|_| AudioError::InvalidData("WavPack block is too large to allocate"))?;
    payload.resize(payload_size, 0);
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn interleave_blocks(
    blocks: &[(AudioSpec, Samples)],
    frames: usize,
    channels: usize,
    format: SampleFormat,
) -> Result<Samples, AudioError> {
    fn interleave_values<T: Copy>(
        blocks: &[(AudioSpec, Samples)],
        frames: usize,
        channels: usize,
        values: impl for<'a> Fn(&'a Samples) -> Option<&'a [T]>,
    ) -> Result<Vec<T>, AudioError> {
        let mut output = Vec::new();
        output
            .try_reserve_exact(frames * channels)
            .map_err(|_| AudioError::InvalidData("decoded WavPack block is too large"))?;
        for frame in 0..frames {
            for (spec, samples) in blocks {
                let block_channels = usize::from(spec.channels());
                let samples = values(samples).ok_or(AudioError::InvalidData(
                    "WavPack channel blocks use different sample formats",
                ))?;
                let expected = frames
                    .checked_mul(block_channels)
                    .ok_or(AudioError::InvalidData("WavPack sample count overflow"))?;
                if samples.len() != expected {
                    return Err(AudioError::InvalidData(
                        "WavPack channel block has an invalid sample count",
                    ));
                }
                let start = frame * block_channels;
                output.extend_from_slice(&samples[start..start + block_channels]);
            }
        }
        if output.len() != frames * channels {
            return Err(AudioError::InvalidData(
                "WavPack channel group has an invalid channel count",
            ));
        }
        Ok(output)
    }

    match format {
        SampleFormat::PcmU8 => Ok(Samples::U8(interleave_values(
            blocks,
            frames,
            channels,
            |samples| match samples {
                Samples::U8(values) => Some(values),
                _ => None,
            },
        )?)),
        SampleFormat::PcmI16 => Ok(Samples::I16(interleave_values(
            blocks,
            frames,
            channels,
            |samples| match samples {
                Samples::I16(values) => Some(values),
                _ => None,
            },
        )?)),
        SampleFormat::PcmI24 => Ok(Samples::I24(interleave_values(
            blocks,
            frames,
            channels,
            |samples| match samples {
                Samples::I24(values) => Some(values),
                _ => None,
            },
        )?)),
        SampleFormat::PcmI32 => Ok(Samples::I32(interleave_values(
            blocks,
            frames,
            channels,
            |samples| match samples {
                Samples::I32(values) => Some(values),
                _ => None,
            },
        )?)),
        SampleFormat::Float32 => Ok(Samples::F32(interleave_values(
            blocks,
            frames,
            channels,
            |samples| match samples {
                Samples::F32(values) => Some(values),
                _ => None,
            },
        )?)),
        SampleFormat::Float64 => Err(AudioError::Unsupported(
            "64-bit floating-point WavPack decoding is not supported yet",
        )),
    }
}

impl<R: Read> AudioDecoder for WavPackDecoder<R> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        if max_frames == 0 {
            return Err(AudioError::InvalidData(
                "max_frames must be greater than zero",
            ));
        }
        let channels = usize::from(self.spec.channels());
        while self
            .pending
            .as_ref()
            .is_none_or(|samples| self.pending_offset == samples.len() / channels)
        {
            self.pending = None;
            self.pending_offset = 0;
            if self.finished || !self.read_next_block()? {
                return Ok(None);
            }
        }
        let pending = self.pending.as_ref().unwrap();
        let frames = (pending.len() / channels - self.pending_offset).min(max_frames);
        let start_sample = self.pending_offset * channels;
        let end_sample = (self.pending_offset + frames) * channels;
        let samples = slice_samples(pending, start_sample, end_sample);
        self.pending_offset += frames;
        PcmBuffer::new(self.spec, samples)
            .map(Some)
            .map_err(AudioError::from)
    }
}

fn decode_lossless_pcm(
    header: &WavPackBlockHeader,
    payload: &[u8],
) -> Result<(AudioSpec, Samples, Option<ChannelInfo>), AudioError> {
    let flags = header.flags;
    if flags & DSD_DATA != 0 {
        return Err(AudioError::Unsupported(
            "WavPack DSD decoding is not supported yet",
        ));
    }
    let hybrid_mode = flags & HYBRID_FLAG != 0;
    if hybrid_mode && flags & HYBRID_SHAPE != 0 {
        return Err(AudioError::Unsupported(
            "WavPack hybrid noise shaping is not supported yet",
        ));
    }
    if hybrid_mode && flags & FLOAT_DATA != 0 {
        return Err(AudioError::Unsupported(
            "hybrid WavPack float decoding is not supported yet",
        ));
    }
    let channels = if flags & MONO_DATA != 0 { 1 } else { 2 };
    let output_channels = if flags & FALSE_STEREO != 0 {
        2
    } else {
        channels
    };
    let sample_format = if flags & FLOAT_DATA != 0 {
        SampleFormat::Float32
    } else {
        match flags & BYTES_STORED {
            0 => SampleFormat::PcmU8,
            1 => SampleFormat::PcmI16,
            2 => SampleFormat::PcmI24,
            _ => SampleFormat::PcmI32,
        }
    };
    let rate_index = ((flags >> 23) & 0x0f) as usize;
    let sample_rate = *SAMPLE_RATES.get(rate_index).ok_or(AudioError::Unsupported(
        "custom WavPack sample rate is not supported yet",
    ))?;
    let spec = AudioSpec::new(sample_rate, output_channels as u16, sample_format)?;

    let mut parts = BlockParts::default();
    let mut input = Cursor::new(payload);
    visit_metadata_sub_blocks(&mut input, payload.len() as u64, |sub_block, data| {
        let slot = match sub_block.id {
            0x02 => &mut parts.terms,
            0x03 => &mut parts.weights,
            0x04 => &mut parts.samples,
            0x05 => &mut parts.entropy,
            0x06 => &mut parts.hybrid_profile,
            0x08 => &mut parts.float_info,
            0x09 => &mut parts.int32_info,
            0x0a => &mut parts.bitstream,
            0x0c | 0x2c => {
                parts.new_wvx |= sub_block.id == 0x2c;
                &mut parts.wvx_bitstream
            }
            0x0d => &mut parts.channel_info,
            _ => return Ok(()),
        };
        if slot.is_some() {
            return Err(AudioError::InvalidData(match sub_block.id {
                0x02 => "duplicate WavPack decorrelation terms",
                0x03 => "duplicate WavPack decorrelation weights",
                0x04 => "duplicate WavPack decorrelation samples",
                0x05 => "duplicate WavPack entropy variables",
                0x06 => "duplicate WavPack hybrid profile",
                0x08 => "duplicate WavPack floating-point metadata",
                0x09 => "duplicate WavPack extended integer metadata",
                0x0a => "duplicate WavPack audio bitstream",
                0x0c => "duplicate WavPack extended bitstream",
                _ => "duplicate WavPack decoder metadata",
            }));
        }
        let mut bytes = Vec::new();
        data.read_to_end(&mut bytes)?;
        *slot = Some(bytes);
        Ok(())
    })?;

    let terms = parts.terms.as_deref().ok_or(AudioError::InvalidData(
        "missing WavPack decorrelation terms",
    ))?;
    let mut passes = parse_terms(terms, channels == 1)?;
    if let Some(weights) = parts.weights.as_deref() {
        if weights.len() > passes.len() * channels || weights.len() % channels != 0 {
            return Err(AudioError::InvalidData(
                "too many WavPack decorrelation weights",
            ));
        }
        for (pass, weights) in passes.iter_mut().rev().zip(weights.chunks_exact(channels)) {
            pass.weight = restore_weight(weights[0] as i8);
            if channels == 2 {
                pass.weight_b = restore_weight(weights[1] as i8);
            }
        }
    }
    if let Some(samples) = parts.samples.as_deref() {
        if channels == 1 {
            parse_mono_samples(samples, &mut passes)?;
        } else {
            parse_stereo_samples(samples, &mut passes)?;
        }
    }
    let entropy = parts
        .entropy
        .as_deref()
        .ok_or(AudioError::InvalidData("missing WavPack entropy variables"))?;
    if entropy.len() != 6 * channels {
        return Err(AudioError::InvalidData(
            "invalid WavPack entropy metadata size",
        ));
    }
    let mut medians = [[0; 3]; 2];
    for (channel, channel_medians) in medians.iter_mut().enumerate().take(channels) {
        for (median, value) in channel_medians.iter_mut().enumerate() {
            let offset = channel * 6 + median * 2;
            *value = u16::from_le_bytes([entropy[offset], entropy[offset + 1]]);
        }
    }
    let bitstream = parts
        .bitstream
        .as_deref()
        .ok_or(AudioError::InvalidData("missing WavPack audio bitstream"))?;
    let hybrid = if hybrid_mode {
        Some(HybridState::parse(
            parts
                .hybrid_profile
                .as_deref()
                .ok_or(AudioError::InvalidData("missing WavPack hybrid profile"))?,
            channels,
            flags,
        )?)
    } else {
        None
    };
    let sample_count = usize::try_from(header.block_samples)
        .map_err(|_| AudioError::InvalidData("WavPack block sample count does not fit memory"))?;
    if sample_count > 131_072 {
        return Err(AudioError::InvalidData(
            "WavPack block exceeds 131072 samples",
        ));
    }
    let mut words = EntropyDecoder::new(bitstream, medians, channels, hybrid)?;
    let value_count = sample_count
        .checked_mul(channels)
        .ok_or(AudioError::InvalidData("WavPack sample count overflow"))?;
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(value_count)
        .map_err(|_| AudioError::InvalidData("decoded WavPack block is too large"))?;
    for _ in 0..sample_count {
        let left = words.read_word(0)?.ok_or(AudioError::InvalidData(
            "WavPack bitstream ended before the block",
        ))?;
        decoded.push(left);
        if channels == 2 {
            let right = words.read_word(1)?.ok_or(AudioError::InvalidData(
                "WavPack bitstream ended before the block",
            ))?;
            decoded.push(right);
        }
    }
    if channels == 1 {
        apply_mono_decorrelation(&mut decoded, &mut passes)?;
    } else {
        apply_stereo_decorrelation(&mut decoded, &mut passes, flags & JOINT_STEREO != 0)?;
    }
    let samples = if flags & FLOAT_DATA != 0 {
        if flags & INT32_DATA != 0 {
            return Err(AudioError::InvalidData(
                "WavPack block sets both float and extended-integer flags",
            ));
        }
        let info = FloatInfo::parse(parts.float_info.as_deref().ok_or(
            AudioError::InvalidData("missing WavPack floating-point metadata"),
        )?)?;
        let mut floats = crate::float::restore(
            &decoded,
            info,
            parts.wvx_bitstream.as_deref(),
            parts.new_wvx,
        )?;
        if output_channels != channels {
            let mut stereo = Vec::new();
            stereo
                .try_reserve_exact(floats.len().saturating_mul(2))
                .map_err(|_| AudioError::InvalidData("decoded WavPack float block is too large"))?;
            for sample in floats {
                stereo.extend([sample, sample]);
            }
            floats = stereo;
        }
        Samples::F32(floats)
    } else {
        if flags & INT32_DATA != 0 {
            let int32_info = parts.int32_info.as_deref().ok_or(AudioError::InvalidData(
                "missing WavPack extended integer metadata",
            ))?;
            if int32_info.len() != 4 {
                return Err(AudioError::InvalidData(
                    "invalid WavPack extended integer metadata size",
                ));
            }
            let sent_bits = int32_info[0] & 0x1f;
            let zeros = int32_info[1] & 0x1f;
            let ones = int32_info[2] & 0x1f;
            let duplicates = int32_info[3] & 0x1f;
            let mut extra_bits = match (sent_bits, parts.wvx_bitstream.as_deref()) {
                (0, _) => None,
                (_, Some(bytes)) if bytes.len() >= 4 => Some(BitReader::new(&bytes[4..])),
                _ => {
                    return Err(AudioError::InvalidData(
                        "missing or truncated WavPack extended integer bitstream",
                    ));
                }
            };
            for sample in &mut decoded {
                let low_bits = match &mut extra_bits {
                    Some(bits) => bits.read_bits(u32::from(sent_bits))?,
                    None => 0,
                };
                *sample =
                    restore_extended_integer(*sample, low_bits, zeros, ones, duplicates, sent_bits);
            }
        }
        let shift = (flags & SHIFT_MASK) >> 13;
        if shift != 0 {
            for sample in &mut decoded {
                *sample = i32::try_from(i64::from(*sample) << shift)
                    .map_err(|_| AudioError::InvalidData("WavPack sample shift overflow"))?;
            }
        }
        if output_channels != channels {
            let mut stereo = Vec::new();
            stereo
                .try_reserve_exact(decoded.len().saturating_mul(2))
                .map_err(|_| AudioError::InvalidData("decoded WavPack block is too large"))?;
            for sample in decoded {
                stereo.extend([sample, sample]);
            }
            decoded = stereo;
        }
        match sample_format {
            SampleFormat::PcmU8 => Samples::U8(
                decoded
                    .into_iter()
                    .map(|sample| {
                        let sample = if hybrid_mode {
                            sample.clamp(-128, 127)
                        } else {
                            sample
                        };
                        sample
                            .checked_add(128)
                            .and_then(|value| u8::try_from(value).ok())
                            .ok_or(AudioError::InvalidData("decoded sample exceeds 8-bit PCM"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            SampleFormat::PcmI16 => Samples::I16(
                decoded
                    .into_iter()
                    .map(|sample| {
                        let sample = if hybrid_mode {
                            sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                        } else {
                            sample
                        };
                        i16::try_from(sample).map_err(|_| {
                            AudioError::InvalidData("decoded sample exceeds 16-bit PCM")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            SampleFormat::PcmI24 => Samples::I24(
                decoded
                    .into_iter()
                    .map(|sample| {
                        let sample = if hybrid_mode {
                            sample.clamp(-8_388_608, 8_388_607)
                        } else {
                            sample
                        };
                        if (-8_388_608..=8_388_607).contains(&sample) {
                            Ok(sample)
                        } else {
                            Err(AudioError::InvalidData("decoded sample exceeds 24-bit PCM"))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            SampleFormat::PcmI32 => Samples::I32(decoded),
            SampleFormat::Float32 | SampleFormat::Float64 => unreachable!(),
        }
    };
    let channel_info = parts
        .channel_info
        .as_deref()
        .map(parse_channel_info)
        .transpose()?;
    Ok((spec, samples, channel_info))
}

fn slice_samples(samples: &Samples, start: usize, end: usize) -> Samples {
    match samples {
        Samples::U8(values) => Samples::U8(values[start..end].to_vec()),
        Samples::I16(values) => Samples::I16(values[start..end].to_vec()),
        Samples::I24(values) => Samples::I24(values[start..end].to_vec()),
        Samples::I32(values) => Samples::I32(values[start..end].to_vec()),
        Samples::F32(values) => Samples::F32(values[start..end].to_vec()),
        Samples::F64(values) => Samples::F64(values[start..end].to_vec()),
    }
}

#[derive(Default)]
struct BlockParts {
    terms: Option<Vec<u8>>,
    weights: Option<Vec<u8>>,
    samples: Option<Vec<u8>>,
    entropy: Option<Vec<u8>>,
    hybrid_profile: Option<Vec<u8>>,
    int32_info: Option<Vec<u8>>,
    float_info: Option<Vec<u8>>,
    bitstream: Option<Vec<u8>>,
    wvx_bitstream: Option<Vec<u8>>,
    new_wvx: bool,
    channel_info: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct ChannelInfo {
    channels: u16,
    speaker_mask: Option<u32>,
}

fn parse_channel_info(bytes: &[u8]) -> Result<ChannelInfo, AudioError> {
    if bytes.is_empty() || bytes.len() > 7 {
        return Err(AudioError::InvalidData(
            "invalid WavPack channel information size",
        ));
    }
    let (channels, max_streams, mask) = if bytes.len() >= 6 {
        let channels = (usize::from(bytes[0]) | (usize::from(bytes[2] & 0x0f) << 8)) + 1;
        let streams = (usize::from(bytes[1]) | (usize::from(bytes[2] & 0xf0) << 4)) + 1;
        let mask_end = bytes.len();
        let mask_start = 3;
        let mask = bytes[mask_start..mask_end]
            .iter()
            .enumerate()
            .fold(0_u32, |mask, (index, byte)| {
                mask | (u32::from(*byte) << (index * 8))
            });
        (channels, streams, mask)
    } else {
        let channels = usize::from(bytes[0]);
        let mask = bytes[1..]
            .iter()
            .enumerate()
            .fold(0_u32, |mask, (index, byte)| {
                mask | (u32::from(*byte) << (index * 8))
            });
        (channels, channels, mask)
    };
    if channels == 0
        || channels > MAX_CHANNELS
        || max_streams == 0
        || channels > max_streams.saturating_mul(2)
        || mask.count_ones() as usize > channels
    {
        return Err(AudioError::InvalidData(
            "invalid WavPack channel count or speaker mask",
        ));
    }
    Ok(ChannelInfo {
        channels: channels as u16,
        speaker_mask: (mask != 0).then_some(mask),
    })
}

fn restore_extended_integer(
    sample: i32,
    low_bits: u32,
    zeros: u8,
    ones: u8,
    duplicates: u8,
    sent_bits: u8,
) -> i32 {
    let mut value = ((sample as u32) << sent_bits) | low_bits;
    if zeros != 0 {
        value <<= zeros;
    } else if ones != 0 {
        value = value
            .wrapping_add(1)
            .wrapping_shl(u32::from(ones))
            .wrapping_sub(1);
    } else if duplicates != 0 {
        let low_bit = value & 1;
        value = value
            .wrapping_add(low_bit)
            .wrapping_shl(u32::from(duplicates))
            .wrapping_sub(low_bit);
    }
    value as i32
}

struct DecorrPass {
    term: i32,
    delta: i32,
    weight: i32,
    weight_b: i32,
    samples: [i32; 8],
    samples_b: [i32; 8],
}

fn parse_terms(bytes: &[u8], mono: bool) -> Result<Vec<DecorrPass>, AudioError> {
    if bytes.len() > 16 {
        return Err(AudioError::InvalidData(
            "too many WavPack decorrelation terms",
        ));
    }
    bytes
        .iter()
        .rev()
        .map(|&value| {
            let term = i32::from(value & 0x1f) - 5;
            if !(1..=8).contains(&term)
                && term != 17
                && term != 18
                && (mono || !(-3..=-1).contains(&term))
            {
                return Err(AudioError::Unsupported(
                    "unsupported WavPack decorrelation term",
                ));
            }
            Ok(DecorrPass {
                term,
                delta: i32::from(value >> 5),
                weight: 0,
                weight_b: 0,
                samples: [0; 8],
                samples_b: [0; 8],
            })
        })
        .collect()
}

fn parse_mono_samples(bytes: &[u8], passes: &mut [DecorrPass]) -> Result<(), AudioError> {
    let mut offset = 0;
    for pass in passes.iter_mut().rev() {
        if offset == bytes.len() {
            break;
        }
        let count = if pass.term > 8 { 2 } else { pass.term as usize };
        let byte_count = count * 2;
        if bytes.len() - offset < byte_count {
            return Err(AudioError::InvalidData(
                "truncated WavPack decorrelation samples",
            ));
        }
        for sample in pass.samples.iter_mut().take(count) {
            let log = i32::from(i16::from_le_bytes([bytes[offset], bytes[offset + 1]]));
            *sample = i32::try_from(crate::entropy::exp2s(log)?).map_err(|_| {
                AudioError::InvalidData("WavPack decorrelation sample is out of range")
            })?;
            offset += 2;
        }
    }
    if offset != bytes.len() {
        return Err(AudioError::InvalidData(
            "unexpected WavPack decorrelation sample bytes",
        ));
    }
    Ok(())
}

fn parse_stereo_samples(bytes: &[u8], passes: &mut [DecorrPass]) -> Result<(), AudioError> {
    let mut offset = 0;
    for pass in passes.iter_mut().rev() {
        if offset == bytes.len() {
            break;
        }
        let count = if pass.term > 8 {
            2
        } else {
            pass.term.unsigned_abs() as usize
        };
        if pass.term > 8 {
            read_history_values(bytes, &mut offset, &mut pass.samples, count)?;
            read_history_values(bytes, &mut offset, &mut pass.samples_b, count)?;
        } else if pass.term < 0 {
            read_history_values(bytes, &mut offset, &mut pass.samples[..1], 1)?;
            read_history_values(bytes, &mut offset, &mut pass.samples_b[..1], 1)?;
        } else {
            for index in 0..count {
                read_history_values(bytes, &mut offset, &mut pass.samples[index..], 1)?;
                read_history_values(bytes, &mut offset, &mut pass.samples_b[index..], 1)?;
            }
        }
    }
    if offset != bytes.len() {
        return Err(AudioError::InvalidData(
            "unexpected WavPack decorrelation sample bytes",
        ));
    }
    Ok(())
}

fn read_history_values(
    bytes: &[u8],
    offset: &mut usize,
    target: &mut [i32],
    count: usize,
) -> Result<(), AudioError> {
    for sample in target.iter_mut().take(count) {
        if bytes.len() - *offset < 2 {
            return Err(AudioError::InvalidData(
                "truncated WavPack decorrelation samples",
            ));
        }
        let log = i32::from(i16::from_le_bytes([bytes[*offset], bytes[*offset + 1]]));
        *sample = i32::try_from(crate::entropy::exp2s(log)?)
            .map_err(|_| AudioError::InvalidData("WavPack decorrelation sample is out of range"))?;
        *offset += 2;
    }
    Ok(())
}

fn apply_mono_decorrelation(
    samples: &mut [i32],
    passes: &mut [DecorrPass],
) -> Result<(), AudioError> {
    let mut ring = 0_usize;
    for sample in samples {
        for pass in passes.iter_mut() {
            let (source, destination) = if pass.term > 8 {
                let source = if pass.term & 1 != 0 {
                    2_i64 * i64::from(pass.samples[0]) - i64::from(pass.samples[1])
                } else {
                    (3_i64 * i64::from(pass.samples[0]) - i64::from(pass.samples[1])) >> 1
                };
                pass.samples[1] = pass.samples[0];
                (source, 0)
            } else {
                (
                    i64::from(pass.samples[ring]),
                    (ring + pass.term as usize) & 7,
                )
            };
            let residual = i64::from(*sample);
            let prediction = (i64::from(pass.weight) * source + 512) >> 10;
            let decoded = prediction + residual;
            let decoded = i32::try_from(decoded)
                .map_err(|_| AudioError::InvalidData("WavPack decorrelation overflow"))?;
            if source != 0 && residual != 0 {
                if (source ^ residual) < 0 {
                    pass.weight -= pass.delta;
                } else {
                    pass.weight += pass.delta;
                }
            }
            pass.samples[destination] = decoded;
            *sample = decoded;
        }
        ring = (ring + 1) & 7;
    }
    Ok(())
}

fn apply_stereo_decorrelation(
    samples: &mut [i32],
    passes: &mut [DecorrPass],
    joint_stereo: bool,
) -> Result<(), AudioError> {
    if samples.len() % 2 != 0 {
        return Err(AudioError::InvalidData(
            "stereo WavPack block has an incomplete sample pair",
        ));
    }
    let mut ring = 0_usize;
    for pair in samples.chunks_exact_mut(2) {
        for pass in passes.iter_mut() {
            let residual_a = i64::from(pair[0]);
            let residual_b = i64::from(pair[1]);
            match pass.term {
                term if term > 0 => {
                    let (source_a, source_b, destination) = if term > 8 {
                        let source = |history: &[i32; 8]| {
                            if term & 1 != 0 {
                                i64::from(history[0].wrapping_mul(2).wrapping_sub(history[1]))
                            } else {
                                i64::from(
                                    history[0]
                                        .wrapping_add(history[0].wrapping_sub(history[1]) >> 1),
                                )
                            }
                        };
                        let source_a = source(&pass.samples);
                        let source_b = source(&pass.samples_b);
                        pass.samples[1] = pass.samples[0];
                        pass.samples_b[1] = pass.samples_b[0];
                        (source_a, source_b, 0)
                    } else {
                        (
                            i64::from(pass.samples[ring]),
                            i64::from(pass.samples_b[ring]),
                            (ring + term as usize) & 7,
                        )
                    };
                    // WavPack decorrelation words use signed 32-bit wraparound.
                    let decoded_a =
                        (residual_a + ((i64::from(pass.weight) * source_a + 512) >> 10)) as i32;
                    let decoded_b =
                        (residual_b + ((i64::from(pass.weight_b) * source_b + 512) >> 10)) as i32;
                    update_weight(&mut pass.weight, pass.delta, source_a, residual_a, false);
                    update_weight(&mut pass.weight_b, pass.delta, source_b, residual_b, false);
                    pass.samples[destination] = decoded_a;
                    pass.samples_b[destination] = decoded_b;
                    pair[0] = decoded_a;
                    pair[1] = decoded_b;
                }
                -1 => {
                    let source = i64::from(pass.samples[0]);
                    let left =
                        i32::try_from(residual_a + ((i64::from(pass.weight) * source + 512) >> 10))
                            .map_err(|_| {
                                AudioError::InvalidData("WavPack decorrelation overflow")
                            })?;
                    update_weight(&mut pass.weight, pass.delta, source, residual_a, true);
                    let right = i32::try_from(
                        residual_b + ((i64::from(pass.weight_b) * i64::from(left) + 512) >> 10),
                    )
                    .map_err(|_| AudioError::InvalidData("WavPack decorrelation overflow"))?;
                    update_weight(
                        &mut pass.weight_b,
                        pass.delta,
                        i64::from(left),
                        residual_b,
                        true,
                    );
                    pass.samples[0] = right;
                    pair[0] = left;
                    pair[1] = right;
                }
                -2 => {
                    let source = i64::from(pass.samples_b[0]);
                    let right = i32::try_from(
                        residual_b + ((i64::from(pass.weight_b) * source + 512) >> 10),
                    )
                    .map_err(|_| AudioError::InvalidData("WavPack decorrelation overflow"))?;
                    update_weight(&mut pass.weight_b, pass.delta, source, residual_b, true);
                    let left = i32::try_from(
                        residual_a + ((i64::from(pass.weight) * i64::from(right) + 512) >> 10),
                    )
                    .map_err(|_| AudioError::InvalidData("WavPack decorrelation overflow"))?;
                    update_weight(
                        &mut pass.weight,
                        pass.delta,
                        i64::from(right),
                        residual_a,
                        true,
                    );
                    pass.samples_b[0] = left;
                    pair[0] = left;
                    pair[1] = right;
                }
                -3 => {
                    let source_a = i64::from(pass.samples[0]);
                    let source_b = i64::from(pass.samples_b[0]);
                    let left = i32::try_from(
                        residual_a + ((i64::from(pass.weight) * source_a + 512) >> 10),
                    )
                    .map_err(|_| AudioError::InvalidData("WavPack decorrelation overflow"))?;
                    let right = i32::try_from(
                        residual_b + ((i64::from(pass.weight_b) * source_b + 512) >> 10),
                    )
                    .map_err(|_| AudioError::InvalidData("WavPack decorrelation overflow"))?;
                    update_weight(&mut pass.weight, pass.delta, source_a, residual_a, true);
                    update_weight(&mut pass.weight_b, pass.delta, source_b, residual_b, true);
                    pass.samples[0] = right;
                    pass.samples_b[0] = left;
                    pair[0] = left;
                    pair[1] = right;
                }
                _ => return Err(AudioError::Unsupported("unsupported stereo WavPack term")),
            }
        }
        ring = (ring + 1) & 7;
        if joint_stereo {
            let mid = i64::from(pair[0]);
            let side = i64::from(pair[1]) - (mid >> 1);
            pair[0] = i32::try_from(mid + side)
                .map_err(|_| AudioError::InvalidData("WavPack joint stereo overflow"))?;
            pair[1] = i32::try_from(side)
                .map_err(|_| AudioError::InvalidData("WavPack joint stereo overflow"))?;
        }
    }
    Ok(())
}

fn update_weight(weight: &mut i32, delta: i32, source: i64, residual: i64, clip: bool) {
    if source != 0 && residual != 0 {
        if (source ^ residual) < 0 {
            *weight -= delta;
        } else {
            *weight += delta;
        }
        if clip {
            *weight = (*weight).clamp(-1024, 1024);
        }
    }
}

fn restore_weight(weight: i8) -> i32 {
    let weight = i32::from(weight);
    let scaled = weight * 8;
    if scaled > 0 {
        scaled + ((scaled + 64) >> 7)
    } else {
        scaled
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_channel_info, restore_extended_integer};

    #[test]
    fn parses_legacy_and_extended_channel_information() {
        assert_eq!(
            parse_channel_info(&[6, 0x3f]).unwrap().speaker_mask,
            Some(0x3f)
        );
        assert_eq!(
            parse_channel_info(&[5, 2, 0, 0x3f, 0, 0]).unwrap().channels,
            6
        );
        assert_eq!(
            parse_channel_info(&[5, 2, 0, 0x3f, 0, 0, 0])
                .unwrap()
                .speaker_mask,
            Some(0x3f)
        );
        assert!(parse_channel_info(&[]).is_err());
        assert!(parse_channel_info(&[2, 0x07]).is_err());
    }

    #[test]
    fn restores_low_bits_and_redundant_integer_bit_patterns() {
        assert_eq!(restore_extended_integer(-1, 0x35, 0, 0, 0, 8), -203);
        assert_eq!(
            restore_extended_integer(0x1234_5678, 0, 3, 0, 0, 0) as u32,
            0x91a2_b3c0
        );
        assert_eq!(restore_extended_integer(0x10, 0, 0, 3, 0, 0), 135);
        assert_eq!(restore_extended_integer(5, 0, 0, 0, 2, 0), 23);
    }
}
