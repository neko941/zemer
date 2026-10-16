//! Native DSD block decoding, independent from the PCM entropy path.

use std::io::{Cursor, Read, Seek, Take};

use zemer_core::{AudioError, DsdBuffer, DsdDecoder, DsdSpec, Metadata, Samples};

use crate::{
    WavPackBlockHeader,
    checksum::verify_dsd_checksum,
    decode::{ChannelInfo, WavPackDecoder, parse_channel_info, parse_sample_rate},
    metadata::visit_metadata_sub_blocks,
    tags::trailing_metadata_and_audio_end,
};

const MONO_DATA: u32 = 1 << 2;
const FALSE_STEREO: u32 = 1 << 30;
const FLOAT_DATA: u32 = 1 << 7;
const INT32_DATA: u32 = 1 << 8;
const DSD_DATA: u32 = 1 << 31;
const MAX_DSD_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const MAX_DSD_HISTORY_BITS: u8 = 8;
const DSD_MODEL_DECAY: u32 = 8;

pub(crate) fn decode_block(
    header: &WavPackBlockHeader,
    payload: &[u8],
) -> Result<(u32, Vec<u8>, Option<ChannelInfo>), AudioError> {
    if header.flags & DSD_DATA == 0
        || header.flags & (FLOAT_DATA | INT32_DATA) != 0
        || header.flags & (1 << 3) != 0
    {
        return Err(AudioError::InvalidData("invalid WavPack DSD block flags"));
    }
    let channels = if header.flags & (MONO_DATA | FALSE_STEREO) != 0 {
        1
    } else {
        2
    };
    let sample_count = usize::try_from(header.block_samples)
        .map_err(|_| AudioError::InvalidData("WavPack DSD block sample count is too large"))?;
    let byte_count = sample_count
        .checked_mul(channels)
        .ok_or(AudioError::InvalidData("WavPack DSD block size overflow"))?;
    if byte_count > MAX_DSD_BLOCK_BYTES {
        return Err(AudioError::InvalidData(
            "WavPack DSD block exceeds the configured size limit",
        ));
    }

    let mut dsd = None;
    let mut channel_info = None;
    let mut sample_rate = None;
    let mut input = Cursor::new(payload);
    visit_metadata_sub_blocks(&mut input, payload.len() as u64, |sub_block, data| {
        let slot = match sub_block.id {
            0x0e => &mut dsd,
            0x0d => &mut channel_info,
            0x27 => &mut sample_rate,
            _ => return Ok(()),
        };
        if slot.is_some() {
            return Err(AudioError::InvalidData(match sub_block.id {
                0x0e => "duplicate WavPack DSD block",
                0x0d => "duplicate WavPack channel information",
                _ => "duplicate WavPack sample-rate metadata",
            }));
        }
        let mut bytes = Vec::new();
        data.read_to_end(&mut bytes)?;
        *slot = Some(bytes);
        Ok(())
    })?;

    let rate_index = ((header.flags >> 23) & 0x0f) as usize;
    let base_rate = parse_sample_rate(rate_index, sample_rate.as_deref())?;
    let dsd = dsd.ok_or(AudioError::InvalidData("missing WavPack DSD block"))?;
    if dsd.len() < 2 || dsd[0] > 31 {
        return Err(AudioError::InvalidData("invalid WavPack DSD block header"));
    }
    let multiplier = 1_u32
        .checked_shl(u32::from(dsd[0]))
        .ok_or(AudioError::InvalidData(
            "invalid WavPack DSD rate multiplier",
        ))?;
    let byte_rate = base_rate
        .checked_mul(multiplier)
        .ok_or(AudioError::InvalidData("WavPack DSD byte rate overflow"))?;
    let samples = match dsd[1] {
        0 if dsd.len() - 2 == byte_count => dsd[2..].to_vec(),
        0 => {
            return Err(AudioError::InvalidData(
                "WavPack raw DSD block has an invalid byte count",
            ));
        }
        1 => decode_fast(&dsd[2..], byte_count, channels)?,
        3 => decode_high(&dsd[2..], byte_count, channels)?,
        _ => return Err(AudioError::InvalidData("unknown WavPack DSD mode")),
    };
    verify_dsd_checksum(header, &samples)?;
    let channel_info = channel_info
        .as_deref()
        .map(parse_channel_info)
        .transpose()?;
    Ok((byte_rate, samples, channel_info))
}

const DSD_DOWN: i32 = 0x0001_0000;
const DSD_UP: i32 = 0x0100_00fe;

fn decode_high(data: &[u8], output_len: usize, channels: usize) -> Result<Vec<u8>, AudioError> {
    if channels == 0 || output_len % channels != 0 {
        return Err(AudioError::InvalidData("invalid WavPack DSD channel count"));
    }
    let (&initial_rate, tail) = data.split_first().ok_or(AudioError::InvalidData(
        "truncated WavPack adaptive DSD model",
    ))?;
    let (&rate_s, tail) = tail.split_first().ok_or(AudioError::InvalidData(
        "truncated WavPack adaptive DSD model",
    ))?;
    if rate_s != 20 {
        return Err(AudioError::InvalidData(
            "invalid WavPack adaptive DSD rate parameter",
        ));
    }
    let state_bytes = channels
        .checked_mul(7)
        .ok_or(AudioError::InvalidData("WavPack DSD model size overflow"))?;
    let initial_state = tail.get(..state_bytes).ok_or(AudioError::InvalidData(
        "truncated WavPack adaptive DSD state",
    ))?;
    let mut filters = Vec::new();
    filters
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("WavPack DSD model is too large"))?;
    for state in initial_state.chunks_exact(7) {
        filters.push(DsdFilter::from_bytes(state));
    }
    let mut model = DsdAdaptiveModel::new(initial_rate, rate_s);
    let mut range = DsdBitRangeDecoder::new(&tail[state_bytes..])?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|_| AudioError::InvalidData("decoded WavPack DSD block is too large"))?;
    output.resize(output_len, 0);
    for frame in 0..output_len / channels {
        for bit_index in 0..8 {
            for channel in 0..channels {
                let bit = model.decode_bit(&mut range, &mut filters[channel])?;
                output[frame * channels + channel] |= bit << (7 - bit_index);
            }
        }
        for filter in &mut filters {
            filter.factor -= (filter.factor + 512) >> 10;
        }
    }
    Ok(output)
}

struct DsdAdaptiveModel {
    probabilities: [i32; 256],
}

impl DsdAdaptiveModel {
    fn new(rate: u8, rate_s: u8) -> Self {
        let mut probabilities = [0; 256];
        let mut estimate = 0x0080_8000_i32;
        let mut acceleration = i32::from(rate) << 8;
        for _ in 0..((acceleration + 128) >> 8) {
            estimate += (DSD_DOWN - estimate) >> 8;
        }
        for index in 0..128 {
            probabilities[index] = estimate;
            probabilities[255 - index] = 0x0100_ffff - estimate;
            if estimate > 0x0001_0000 {
                acceleration += (acceleration * i32::from(rate_s) + 128) >> 8;
                for _ in 0..((acceleration + 64) >> 7) {
                    estimate += (DSD_DOWN - estimate) >> 8;
                }
            }
        }
        Self { probabilities }
    }

    fn decode_bit(
        &mut self,
        range: &mut DsdBitRangeDecoder<'_>,
        filter: &mut DsdFilter,
    ) -> Result<u8, AudioError> {
        let prediction = filter.prediction();
        let bin = ((prediction >> 8) & 0xff) as usize;
        let probability = (self.probabilities[bin] as u32) >> 16;
        let bit = range.decode(probability)?;
        let target = if bit == 0 { DSD_DOWN } else { DSD_UP };
        self.probabilities[bin] += (target - self.probabilities[bin]) >> DSD_MODEL_DECAY;
        filter.update(bit);
        Ok(bit)
    }
}

#[derive(Default)]
struct DsdFilter {
    first: i32,
    second: i32,
    third: i32,
    fourth: i32,
    fifth: i32,
    sixth: i32,
    factor: i32,
    previous_bit: i32,
}

impl DsdFilter {
    fn from_bytes(bytes: &[u8]) -> Self {
        let factor = i16::from_le_bytes([bytes[5], bytes[6]]);
        Self {
            first: i32::from(bytes[0]) << 12,
            second: i32::from(bytes[1]) << 12,
            third: i32::from(bytes[2]) << 12,
            fourth: i32::from(bytes[3]) << 12,
            fifth: i32::from(bytes[4]) << 12,
            factor: i32::from(factor),
            ..Self::default()
        }
    }

    fn prediction(&self) -> i32 {
        self.first
            .wrapping_sub(self.fifth)
            .wrapping_add(self.sixth.wrapping_mul(self.factor) >> 2)
    }

    fn update(&mut self, bit: u8) {
        self.previous_bit = if bit == 0 { 0 } else { -1 };
        let target = self.previous_bit & 0x0010_0000;
        let value = self.prediction().wrapping_add(self.sixth.wrapping_mul(8));
        let polarity = ((value ^ self.previous_bit) >> 31) | 1;
        let crossing = (value ^ value.wrapping_sub(self.sixth.wrapping_mul(16))) >> 31;
        self.factor = self.factor.wrapping_add(polarity & crossing);
        self.first += (target - self.first) >> 6;
        self.second += (target - self.second) >> 4;
        self.third += (self.second - self.third) >> 4;
        self.fourth += (self.third - self.fourth) >> 4;
        let slope = (self.fourth - self.fifth) >> 4;
        self.fifth += slope;
        self.sixth += (slope - self.sixth) >> 3;
    }
}

struct DsdBitRangeDecoder<'a> {
    data: &'a [u8],
    offset: usize,
    low: u32,
    high: u32,
    value: u32,
}

impl<'a> DsdBitRangeDecoder<'a> {
    fn new(data: &'a [u8]) -> Result<Self, AudioError> {
        let initial: [u8; 4] = data
            .get(..4)
            .ok_or(AudioError::InvalidData(
                "truncated WavPack adaptive DSD stream",
            ))?
            .try_into()
            .unwrap();
        Ok(Self {
            data,
            offset: 4,
            low: 0,
            high: u32::MAX,
            value: u32::from_be_bytes(initial),
        })
    }

    fn decode(&mut self, probability: u32) -> Result<u8, AudioError> {
        let scale = self.high.wrapping_sub(self.low) >> 8;
        let split = self.low.wrapping_add(scale.wrapping_mul(probability));
        let bit = u8::from(self.value <= split);
        if bit == 0 {
            self.low = split.wrapping_add(1);
        } else {
            self.high = split;
        }
        while (self.low ^ self.high) & 0xff00_0000 == 0 {
            let byte = *self.data.get(self.offset).ok_or(AudioError::InvalidData(
                "truncated WavPack adaptive DSD stream",
            ))?;
            self.offset += 1;
            self.value = (self.value << 8) | u32::from(byte);
            self.high = (self.high << 8) | 0xff;
            self.low <<= 8;
        }
        Ok(bit)
    }
}

fn decode_fast(data: &[u8], output_len: usize, channels: usize) -> Result<Vec<u8>, AudioError> {
    let (&history_bits, rest) = data
        .split_first()
        .ok_or(AudioError::InvalidData("truncated WavPack DSD fast model"))?;
    if history_bits > MAX_DSD_HISTORY_BITS {
        return Err(AudioError::InvalidData(
            "WavPack DSD fast history exceeds its limit",
        ));
    }
    let bins = 1_usize << history_bits;
    let table_len = bins * 256;
    let (&max_probability, mut encoded_probabilities) = rest
        .split_first()
        .ok_or(AudioError::InvalidData("truncated WavPack DSD fast model"))?;
    let mut probabilities = vec![0_u8; table_len];
    if max_probability < 255 {
        let mut output = 0;
        while output < table_len && !encoded_probabilities.is_empty() {
            let code = encoded_probabilities[0];
            encoded_probabilities = &encoded_probabilities[1..];
            if code > max_probability {
                output = output.saturating_add(usize::from(code - max_probability));
                output = output.min(table_len);
            } else if code != 0 {
                probabilities[output] = code;
                output += 1;
            } else {
                break;
            }
        }
        if output != table_len {
            return Err(AudioError::InvalidData(
                "truncated WavPack DSD fast probability table",
            ));
        }
        if !encoded_probabilities.is_empty() {
            if encoded_probabilities[0] != 0 {
                return Err(AudioError::InvalidData(
                    "invalid WavPack DSD fast probability terminator",
                ));
            }
            encoded_probabilities = &encoded_probabilities[1..];
        }
    } else {
        if encoded_probabilities.len() <= table_len {
            return Err(AudioError::InvalidData(
                "truncated WavPack DSD fast probability table",
            ));
        }
        probabilities.copy_from_slice(&encoded_probabilities[..table_len]);
        encoded_probabilities = &encoded_probabilities[table_len..];
    }

    let mut cumulative = vec![[0_u16; 256]; bins];
    let mut lookup = Vec::new();
    lookup
        .try_reserve_exact(bins)
        .map_err(|_| AudioError::InvalidData("WavPack DSD model is too large"))?;
    for bin in 0..bins {
        let mut total = 0_u32;
        let mut values = Vec::new();
        for code in 0..256 {
            let probability = u32::from(probabilities[bin * 256 + code]);
            total += probability;
            if total > 1280 {
                return Err(AudioError::InvalidData(
                    "WavPack DSD fast probability bin exceeds its limit",
                ));
            }
            cumulative[bin][code] = total as u16;
            values.extend(std::iter::repeat_n(code as u8, probability as usize));
        }
        lookup.push(values);
    }
    let mut range = DsdRangeDecoder::new(encoded_probabilities)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|_| AudioError::InvalidData("decoded WavPack DSD block is too large"))?;
    let mask = bins - 1;
    let mut contexts = [0_usize; 2];
    while output.len() < output_len {
        let context = contexts[0];
        let total = u32::from(cumulative[context][255]);
        let symbol = range.decode_symbol(
            total,
            &cumulative[context],
            &probabilities[context * 256..(context + 1) * 256],
            &lookup[context],
        )?;
        output.push(symbol);
        if channels == 1 {
            contexts[0] = usize::from(symbol) & mask;
        } else {
            contexts[0] = contexts[1];
            contexts[1] = usize::from(symbol) & mask;
        }
    }
    Ok(output)
}

struct DsdRangeDecoder<'a> {
    data: &'a [u8],
    offset: usize,
    low: u32,
    high: u32,
    value: u32,
}

impl<'a> DsdRangeDecoder<'a> {
    fn new(data: &'a [u8]) -> Result<Self, AudioError> {
        let initial: [u8; 4] = data
            .get(..4)
            .ok_or(AudioError::InvalidData(
                "truncated WavPack DSD range stream",
            ))?
            .try_into()
            .unwrap();
        Ok(Self {
            data,
            offset: 4,
            low: 0,
            high: u32::MAX,
            value: u32::from_be_bytes(initial),
        })
    }

    fn decode_symbol(
        &mut self,
        total: u32,
        cumulative: &[u16; 256],
        probabilities: &[u8],
        lookup: &[u8],
    ) -> Result<u8, AudioError> {
        if total == 0 {
            return Err(AudioError::InvalidData(
                "empty WavPack DSD fast probability bin",
            ));
        }
        let mut scale = self.high.wrapping_sub(self.low) / total;
        if scale == 0 {
            self.refill_word()?;
            self.low = 0;
            self.high = u32::MAX;
            scale = self.high / total;
        }
        if scale == 0 || self.value < self.low {
            return Err(AudioError::InvalidData("invalid WavPack DSD range state"));
        }
        let slot = (self.value - self.low) / scale;
        if slot >= total {
            return Err(AudioError::InvalidData("invalid WavPack DSD range code"));
        }
        let symbol = *lookup
            .get(slot as usize)
            .ok_or(AudioError::InvalidData("invalid WavPack DSD range symbol"))?;
        if symbol != 0 {
            self.low = self
                .low
                .wrapping_add(u32::from(cumulative[usize::from(symbol) - 1]) * scale);
        }
        let interval = u32::from(probabilities[usize::from(symbol)]) * scale;
        if interval == 0 {
            return Err(AudioError::InvalidData("empty WavPack DSD range interval"));
        }
        self.high = self.low.wrapping_add(interval).wrapping_sub(1);
        while (self.low ^ self.high) & 0xff00_0000 == 0 {
            let byte = *self.data.get(self.offset).ok_or(AudioError::InvalidData(
                "truncated WavPack DSD range stream",
            ))?;
            self.offset += 1;
            self.value = (self.value << 8) | u32::from(byte);
            self.high = (self.high << 8) | 0xff;
            self.low <<= 8;
        }
        Ok(symbol)
    }

    fn refill_word(&mut self) -> Result<(), AudioError> {
        let word: [u8; 4] = self
            .data
            .get(self.offset..self.offset + 4)
            .ok_or(AudioError::InvalidData(
                "truncated WavPack DSD range stream",
            ))?
            .try_into()
            .unwrap();
        self.offset += 4;
        self.value = u32::from_be_bytes(word);
        Ok(())
    }
}

/// Native one-bit DSD decoder. Output octets are MSB-first and channel-interleaved.
pub struct WavPackDsdDecoder<R> {
    decoder: WavPackDecoder<R>,
    spec: DsdSpec,
}

impl<R: std::io::Read> WavPackDsdDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let decoder = WavPackDecoder::<R>::new_inner(reader, None, true)?;
        Self::from_decoder(decoder)
    }
}

impl<R: Read + Seek> WavPackDsdDecoder<Take<R>> {
    /// Opens a seekable WavPack file and reads its trailing APEv2 metadata.
    pub fn new_seekable(mut reader: R) -> Result<Self, AudioError> {
        let start = reader.stream_position()?;
        let (audio_end, metadata) = trailing_metadata_and_audio_end(&mut reader)?;
        let audio_size = audio_end
            .checked_sub(start)
            .ok_or(AudioError::InvalidData("invalid WavPack audio range"))?;
        reader.seek(std::io::SeekFrom::Start(start))?;
        let mut decoder = WavPackDecoder::new_inner(reader.take(audio_size), None, true)?;
        decoder.metadata = metadata;
        Self::from_decoder(decoder)
    }
}

impl<R: Read> WavPackDsdDecoder<R> {
    fn from_decoder(decoder: WavPackDecoder<R>) -> Result<Self, AudioError> {
        let spec = DsdSpec::new_with_speaker_mask(
            decoder
                .spec
                .sample_rate()
                .checked_mul(8)
                .ok_or(AudioError::InvalidData("WavPack DSD bit rate overflow"))?,
            decoder.spec.channels(),
            decoder.spec.speaker_mask(),
        )
        .map_err(|error| AudioError::InvalidData(error.0))?;
        Ok(Self { decoder, spec })
    }
}

impl<R: std::io::Read> DsdDecoder for WavPackDsdDecoder<R> {
    fn dsd_spec(&self) -> DsdSpec {
        self.spec
    }

    fn metadata(&self) -> &Metadata {
        &self.decoder.metadata
    }

    fn read_dsd_octets(
        &mut self,
        max_octet_frames: usize,
    ) -> Result<Option<DsdBuffer>, AudioError> {
        self.decoder
            .read_samples(max_octet_frames)?
            .map(|samples| match samples {
                Samples::U8(bytes) => DsdBuffer::new(self.spec, bytes)
                    .map_err(|error| AudioError::InvalidData(error.0)),
                _ => Err(AudioError::InvalidData(
                    "WavPack DSD decoder returned a non-byte buffer",
                )),
            })
            .transpose()
    }
}
